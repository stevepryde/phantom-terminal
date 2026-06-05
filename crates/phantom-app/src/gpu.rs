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

pub trait FrameOverlay {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    );

    fn paint(&mut self, pass: &mut wgpu::RenderPass<'static>);

    fn after_submit(&mut self) {}
}

pub struct GpuContext {
    pub window: Arc<Window>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    surface_config: SurfaceConfiguration,
}

impl GpuContext {
    pub async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self, String> {
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
        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: TextureFormat::Bgra8UnormSrgb,
            width: physical.width.max(1),
            height: physical.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        Ok(Self {
            window,
            device,
            queue,
            instance,
            surface,
            surface_config,
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

    /// Acquire the next frame, clear it, and let `renderer` draw the prepared
    /// instances. Transient surface states trigger a redraw rather than a panic.
    pub fn present(&mut self, renderer: &Renderer) -> bool {
        self.present_with_overlay(renderer, None)
    }

    pub fn present_with_overlay(
        &mut self,
        renderer: &Renderer,
        mut overlay: Option<&mut dyn FrameOverlay>,
    ) -> bool {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return false;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return false;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("recreate surface");
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return false;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("surface validation error");
                return false;
            }
        };

        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        if let Some(overlay) = overlay.as_deref_mut() {
            overlay.prepare(&self.device, &self.queue, &mut encoder);
        }
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("phantom-frame"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(renderer.clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass);
            let mut pass = pass.forget_lifetime();
            if let Some(overlay) = overlay.as_deref_mut() {
                overlay.paint(&mut pass);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        if let Some(overlay) = overlay {
            overlay.after_submit();
        }
        true
    }
}
