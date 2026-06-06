use std::borrow::Cow;

use bytemuck::{Pod, Zeroable};
use image::GenericImageView;

const PHANTOM_BYTES: &[u8] = include_bytes!("../assets/backgrounds/phantom-terminal-phantom.webp");
const DRAGON_BYTES: &[u8] = include_bytes!("../assets/backgrounds/phantom-terminal-dragon.webp");

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct BackdropInstance {
    pos: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    opacity: f32,
    _pad: [f32; 3],
}

#[derive(Clone, Copy)]
enum BackdropKind {
    Phantom,
    Dragon,
}

struct BackdropTexture {
    bind_group: wgpu::BindGroup,
    size: [f32; 2],
}

pub(crate) struct BackdropRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    phantom: Option<BackdropTexture>,
    dragon: Option<BackdropTexture>,
    draw: Option<(BackdropKind, BackdropInstance)>,
    count: u32,
}

impl BackdropRenderer {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        uniform_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phantom-backdrop-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("phantom-backdrop-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let pipeline = build_pipeline(device, format, uniform_layout, &texture_layout);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phantom-backdrop-inst"),
            size: std::mem::size_of::<BackdropInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let phantom = load_texture(
            device,
            queue,
            &texture_layout,
            &sampler,
            "phantom-backdrop-phantom",
            PHANTOM_BYTES,
        );
        let dragon = load_texture(
            device,
            queue,
            &texture_layout,
            &sampler,
            "phantom-backdrop-dragon",
            DRAGON_BYTES,
        );
        Self {
            pipeline,
            buffer,
            phantom,
            dragon,
            draw: None,
            count: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.draw = None;
        self.count = 0;
    }

    pub(crate) fn draw(&mut self, name: &str, opacity_percent: u8, x: f32, y: f32, w: f32, h: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let kind = match name {
            "phantom" => BackdropKind::Phantom,
            "dragon" => BackdropKind::Dragon,
            _ => return,
        };
        let Some(texture) = self.texture(kind) else {
            return;
        };
        let [uv_min, uv_max] = aspect_fill_uv(texture.size, [w, h]);
        let opacity = (opacity_percent.min(60) as f32) / 100.0;
        self.draw = Some((
            kind,
            BackdropInstance {
                pos: [x, y],
                size: [w, h],
                uv_min,
                uv_max,
                opacity,
                _pad: [0.0; 3],
            },
        ));
    }

    pub(crate) fn end(&mut self, queue: &wgpu::Queue) {
        let Some((_, instance)) = self.draw else {
            self.count = 0;
            return;
        };
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&instance));
        self.count = 1;
    }

    pub(crate) fn render(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        uniform_bind_group: &wgpu::BindGroup,
    ) {
        if self.count == 0 {
            return;
        }
        let Some((kind, _)) = self.draw else {
            return;
        };
        let Some(texture) = self.texture(kind) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, uniform_bind_group, &[]);
        pass.set_bind_group(1, &texture.bind_group, &[]);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..4, 0..self.count);
    }

    fn texture(&self, kind: BackdropKind) -> Option<&BackdropTexture> {
        match kind {
            BackdropKind::Phantom => self.phantom.as_ref(),
            BackdropKind::Dragon => self.dragon.as_ref(),
        }
    }
}

fn aspect_fill_uv(image_size: [f32; 2], rect_size: [f32; 2]) -> [[f32; 2]; 2] {
    let image_aspect = image_size[0] / image_size[1];
    let rect_aspect = rect_size[0] / rect_size[1];
    if rect_aspect > image_aspect {
        let visible_h = image_aspect / rect_aspect;
        let offset = (1.0 - visible_h) * 0.5;
        [[0.0, offset], [1.0, 1.0 - offset]]
    } else {
        let visible_w = rect_aspect / image_aspect;
        let offset = (1.0 - visible_w) * 0.5;
        [[offset, 0.0], [1.0 - offset, 1.0]]
    }
}

fn load_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    label: &str,
    bytes: &[u8],
) -> Option<BackdropTexture> {
    let decoded = match image::load_from_memory_with_format(bytes, image::ImageFormat::WebP) {
        Ok(decoded) => decoded,
        Err(e) => {
            eprintln!("failed to decode {label}: {e}");
            return None;
        }
    };
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let rgba = decoded.to_rgba8();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&format!("{label}-bg")),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Some(BackdropTexture {
        bind_group,
        size: [width as f32, height as f32],
    })
}

fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    uniform_layout: &wgpu::BindGroupLayout,
    texture_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("phantom-backdrop"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("shaders/backdrop.wgsl"))),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("phantom-backdrop"),
        bind_group_layouts: &[Some(uniform_layout), Some(texture_layout)],
        ..Default::default()
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("phantom-backdrop"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            buffers: &[backdrop_vertex_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::default(),
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn backdrop_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
        0 => Float32x2, // pos
        1 => Float32x2, // size
        2 => Float32x2, // uv_min
        3 => Float32x2, // uv_max
        4 => Float32,   // opacity
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<BackdropInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}
