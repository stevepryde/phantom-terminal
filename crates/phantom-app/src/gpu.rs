//! The per-window GPU context: wgpu instance/device/queue/surface. Rendering of
//! actual content is the renderer's job; this owns the surface lifecycle and
//! frame presentation.

use std::sync::Arc;

use phantom_gfx::Renderer;
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, Operations, PresentMode, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, SurfaceConfiguration, TextureFormat, TextureUsages,
    TextureViewDescriptor,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::blur::BlurPass;

/// A rectangle (physical pixels, top-left origin) whose backdrop should be
/// blurred before the overlay paints over it. Emitted by translucent panels so
/// the renderer can frost whatever shows through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub trait FrameOverlay {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    );

    fn paint(&mut self, pass: &mut wgpu::RenderPass<'static>);

    /// Regions whose backdrop should be blurred before `paint`. Empty (the
    /// default) means no backdrop blur this frame.
    fn blur_regions(&self) -> &[BlurRegion] {
        &[]
    }

    fn after_submit(&mut self) {}
}

pub struct GpuContext {
    pub window: Arc<Window>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    surface_config: SurfaceConfiguration,
    blur: BlurPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentStatus {
    Presented,
    RetrySoon,
    Occluded,
    Fatal,
}

impl GpuContext {
    pub async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        transparent: bool,
    ) -> Result<Self, String> {
        let physical = window.inner_size();

        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let adapter = instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
            .map_err(|e| format!("no suitable GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .map_err(|e| format!("failed to request device: {e}"))?;

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("failed to create surface: {e}"))?;
        let capabilities = surface.get_capabilities(&adapter);
        let alpha_mode = if transparent {
            capabilities
                .alpha_modes
                .iter()
                .copied()
                .find(|mode| {
                    matches!(
                        mode,
                        CompositeAlphaMode::PreMultiplied | CompositeAlphaMode::PostMultiplied
                    )
                })
                .unwrap_or(CompositeAlphaMode::Opaque)
        } else {
            CompositeAlphaMode::Opaque
        };
        // Bgra8UnormSrgb is universal on Metal/DX12/desktop Vulkan, but GL/GLES
        // fallbacks may only expose Rgba8UnormSrgb; configuring an unsupported
        // format is a validation panic, so pick from what the surface offers.
        let format = if capabilities
            .formats
            .contains(&TextureFormat::Bgra8UnormSrgb)
        {
            TextureFormat::Bgra8UnormSrgb
        } else {
            capabilities
                .formats
                .iter()
                .copied()
                .find(|format| format.is_srgb())
                .or_else(|| capabilities.formats.first().copied())
                .ok_or_else(|| "surface reports no supported texture formats".to_string())?
        };
        let surface_config = SurfaceConfiguration {
            // COPY_SRC lets the backdrop-blur pass snapshot the rendered frame.
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            format,
            width: physical.width.max(1),
            height: physical.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &surface_config);

        let blur = BlurPass::new(&device, surface_config.format);

        Ok(Self {
            window,
            device,
            queue,
            instance,
            surface,
            surface_config,
            blur,
        })
    }

    pub fn format(&self) -> TextureFormat {
        self.surface_config.format
    }

    pub fn scale_factor(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    /// Current surface size in physical px.
    pub fn size(&self) -> (u32, u32) {
        (self.surface_config.width, self.surface_config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width.max(1);
        self.surface_config.height = height.max(1);
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn sync_to_window_size(&mut self) -> bool {
        let size = self.window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        if self.surface_config.width == width && self.surface_config.height == height {
            return false;
        }
        self.resize(width, height);
        true
    }

    /// Acquire the next frame, clear it, and let `renderer` draw the prepared
    /// instances. Transient surface states trigger a redraw rather than a panic.
    pub fn present(&mut self, renderer: &Renderer) -> PresentStatus {
        self.present_with_overlay(renderer, None, false)
    }

    pub fn present_with_overlay(
        &mut self,
        renderer: &Renderer,
        mut overlay: Option<&mut dyn FrameOverlay>,
        transparent_clear: bool,
    ) -> PresentStatus {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout => return PresentStatus::RetrySoon,
            wgpu::CurrentSurfaceTexture::Occluded => return PresentStatus::Occluded,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                return PresentStatus::RetrySoon;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // Lost usually accompanies a GPU/driver reset — the worst
                // moment to assume surface creation succeeds. Keep the old
                // surface on failure and retry on the next attempt.
                match self.instance.create_surface(self.window.clone()) {
                    Ok(surface) => {
                        self.surface = surface;
                        self.surface.configure(&self.device, &self.surface_config);
                    }
                    Err(error) => eprintln!("failed to recreate lost surface: {error}"),
                }
                return PresentStatus::RetrySoon;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("surface validation error");
                return PresentStatus::Fatal;
            }
        };

        let frame_size = frame.texture.size();
        renderer.resize(&self.queue, frame_size.width, frame_size.height);

        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        if let Some(overlay) = overlay.as_deref_mut() {
            overlay.prepare(&self.device, &self.queue, &mut encoder);
        }

        // Translucent panels ask for their backdrop to be frosted. When present,
        // the terminal pass and the overlay paint are split so the blur can run
        // in between, reading the rendered frame and writing it back.
        let blur_regions: Vec<_> = overlay
            .as_deref()
            .map(|overlay| overlay.blur_regions().to_vec())
            .unwrap_or_default();

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("phantom-frame"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(if transparent_clear {
                            wgpu::Color::TRANSPARENT
                        } else {
                            renderer.clear_color()
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass);
            if blur_regions.is_empty() {
                // No frosted panels: paint the overlay in the same pass.
                let mut pass = pass.forget_lifetime();
                if let Some(overlay) = overlay.as_deref_mut() {
                    overlay.paint(&mut pass);
                }
            }
        }

        if !blur_regions.is_empty() {
            let (width, height) = (self.surface_config.width, self.surface_config.height);
            self.blur.run(
                &self.device,
                &self.queue,
                &mut encoder,
                &frame.texture,
                &view,
                &blur_regions,
                width,
                height,
            );
            let mut pass = encoder
                .begin_render_pass(&RenderPassDescriptor {
                    label: Some("phantom-overlay"),
                    color_attachments: &[Some(RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: Operations {
                            load: LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            if let Some(overlay) = overlay.as_deref_mut() {
                overlay.paint(&mut pass);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        if let Some(overlay) = overlay {
            overlay.after_submit();
        }
        PresentStatus::Presented
    }
}
