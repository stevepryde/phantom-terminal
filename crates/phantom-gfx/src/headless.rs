//! Headless offscreen rendering.
//!
//! Renders the terminal [`Renderer`](crate::Renderer) into an offscreen texture
//! and reads the pixels back to the CPU — no window, surface, or display
//! required. This powers pixel-level tests and image dumps for visual review,
//! and works anywhere wgpu can find an adapter (a real GPU, or a software
//! rasterizer such as lavapipe in CI).

use phantom_core::AppConfig;
use phantom_emu::Snapshot;

use crate::Renderer;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn block<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

/// A read-back RGBA image (tightly packed, top-left origin).
pub struct Image {
    pub width: u32,
    pub height: u32,
    pixels: Vec<u8>,
}

impl Image {
    /// Tightly-packed RGBA bytes (`width * height * 4`).
    pub fn rgba(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Average colour over a rectangle (reduces anti-aliasing noise in tests).
    pub fn avg(&self, x0: u32, y0: u32, w: u32, h: u32) -> [u32; 3] {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in y0..(y0 + h).min(self.height) {
            for x in x0..(x0 + w).min(self.width) {
                let p = self.pixel(x, y);
                r += p[0] as u32;
                g += p[1] as u32;
                b += p[2] as u32;
                n += 1;
            }
        }
        let n = n.max(1);
        [r / n, g / n, b / n]
    }
}

/// Offscreen render harness wrapping the real renderer.
pub struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Harness {
    /// Build a harness, or `None` if no GPU adapter is available (so callers can
    /// skip rather than fail in environments without one).
    pub fn new(config: &AppConfig, width: u32, height: u32) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            block(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let (device, queue) =
            block(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;

        let renderer = Renderer::new(&device, &queue, FORMAT, config, 1.0);
        renderer.resize(&queue, width, height);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("phantom-offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Some(Self {
            device,
            queue,
            renderer,
            texture,
            view,
            width,
            height,
        })
    }

    pub fn cell_size(&self) -> (f32, f32) {
        self.renderer.cell_size()
    }

    pub fn grid(&self) -> (u16, u16) {
        self.renderer.grid_size(self.width, self.height)
    }

    /// Render a snapshot (terminal grid at the origin) and read it back.
    pub fn render_snapshot(&mut self, snapshot: &Snapshot, cursor_on: bool) -> Image {
        self.renderer.begin();
        self.renderer
            .draw_terminal(&self.queue, snapshot, cursor_on, 0.0, 0.0);
        self.renderer.end(&self.device, &self.queue);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.renderer.clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.render(&mut pass);
        }

        // Copy with a 256-byte-aligned row stride, then un-pad on the CPU.
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phantom-readback"),
            size: (padded * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map readback buffer"));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let mapped = slice.get_mapped_range();

        let mut pixels = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            pixels.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        Image {
            width: self.width,
            height: self.height,
            pixels,
        }
    }
}
