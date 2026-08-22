//! Backdrop blur for translucent egui panels.
//!
//! After the terminal has been rendered to the surface but before egui paints
//! its panels, [`BlurPass`] snapshots the bounded source area needed by its
//! separable Gaussian, runs the two passes over the bounded intermediate area,
//! and writes the blurred result back into just the panel regions. The panel's
//! translucent fill then composites over a frosted backdrop instead of the
//! sharp terminal underneath.

use crate::gpu::BlurRegion;

/// Per-tap stride in pixels. With a six-sample half kernel the blur reaches
/// `BLUR_STRIDE_PX * 5` pixels each side, in each axis.
const BLUR_STRIDE_PX: f32 = 2.5;
const TAP_COUNT: u32 = 6;

const BLUR_WGSL: &str = r#"
struct Params {
    // Texel-space offset applied per kernel step (already includes 1/dimension).
    dir: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    // Single oversized triangle covering the whole target.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let xy = corners[vi];
    var out: VsOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // Normalized Gaussian half-kernel (sigma ~2 taps).
    var w = array<f32, 6>(0.20057, 0.17699, 0.12164, 0.06513, 0.02714, 0.008814);
    var col = textureSample(src, samp, in.uv) * w[0];
    for (var i = 1u; i < 6u; i = i + 1u) {
        let off = params.dir * f32(i);
        col = col + textureSample(src, samp, in.uv + off) * w[i];
        col = col + textureSample(src, samp, in.uv - off) * w[i];
    }
    return col;
}
"#;

/// Resources whose dimensions track the surface; rebuilt on resize.
struct Sized {
    width: u32,
    height: u32,
    copy_tex: wgpu::Texture,
    tmp_view: wgpu::TextureView,
    /// Samples `copy_tex` with the horizontal direction.
    bg_horizontal: wgpu::BindGroup,
    /// Samples `tmp_tex` with the vertical direction.
    bg_vertical: wgpu::BindGroup,
}

pub struct BlurPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    ubo_horizontal: wgpu::Buffer,
    ubo_vertical: wgpu::Buffer,
    format: wgpu::TextureFormat,
    sized: Option<Sized>,
}

impl BlurPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phantom-blur-layout"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("phantom-blur-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("phantom-blur"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(BLUR_WGSL)),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("phantom-blur"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("phantom-blur"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Replace: we write the blurred backdrop, not blend over it.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let ubo_horizontal = create_uniform(device, "phantom-blur-ubo-h");
        let ubo_vertical = create_uniform(device, "phantom-blur-ubo-v");
        Self {
            pipeline,
            layout,
            sampler,
            ubo_horizontal,
            ubo_vertical,
            format,
            sized: None,
        }
    }

    fn ensure_sized(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) {
        let width = width.max(1);
        let height = height.max(1);
        if matches!(&self.sized, Some(s) if s.width == width && s.height == height) {
            return;
        }

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let copy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("phantom-blur-copy"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tmp_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("phantom-blur-tmp"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let copy_view = copy_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let tmp_view = tmp_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // The per-step offset is a single texel scaled by the stride; only the
        // surface dimensions change, so refresh the uniforms on resize.
        queue.write_buffer(
            &self.ubo_horizontal,
            0,
            &params_bytes(BLUR_STRIDE_PX / width as f32, 0.0),
        );
        queue.write_buffer(
            &self.ubo_vertical,
            0,
            &params_bytes(0.0, BLUR_STRIDE_PX / height as f32),
        );

        let bg_horizontal = self.bind_group(device, &copy_view, &self.ubo_horizontal, "h");
        let bg_vertical = self.bind_group(device, &tmp_view, &self.ubo_vertical, "v");

        self.sized = Some(Sized {
            width,
            height,
            copy_tex,
            tmp_view,
            bg_horizontal,
            bg_vertical,
        });
    }

    fn bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        ubo: &wgpu::Buffer,
        suffix: &str,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("phantom-blur-bg-{suffix}")),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubo.as_entire_binding(),
                },
            ],
        })
    }

    /// Blur `surface` behind each region in place. `surface` must carry
    /// `COPY_SRC`; `regions` are physical-pixel rects (top-left origin).
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        surface: &wgpu::Texture,
        surface_view: &wgpu::TextureView,
        regions: &[BlurRegion],
        width: u32,
        height: u32,
    ) {
        if regions.is_empty() {
            return;
        }
        let reach = (BLUR_STRIDE_PX * (TAP_COUNT - 1) as f32).ceil() as u32;
        // All regions degenerate (clamped to zero size): nothing to frost —
        // skip the snapshot copy and both passes entirely.
        let Some(work) = blur_work_bounds(regions, width, height, reach) else {
            return;
        };
        self.ensure_sized(device, queue, width, height);
        let sized = self
            .sized
            .as_ref()
            .expect("blur resources created in ensure_sized");

        // Keep source and destination coordinates identical so the full-surface
        // UVs remain valid. The snapshot includes exactly the horizontal
        // kernel support needed by every pixel written to the temp texture.
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: surface,
                mip_level: 0,
                origin: work.snapshot.origin(),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &sized.copy_tex,
                mip_level: 0,
                origin: work.snapshot.origin(),
                aspect: wgpu::TextureAspect::All,
            },
            work.snapshot.extent(),
        );

        // Horizontal pass: copy_tex -> tmp. Its vertical margin supplies every
        // texel sampled by the vertical pass. Loading preserves irrelevant
        // pixels outside the scissor instead of clearing the full attachment.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("phantom-blur-h"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &sized.tmp_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &sized.bg_horizontal, &[]);
            pass.set_scissor_rect(
                work.horizontal.x,
                work.horizontal.y,
                work.horizontal.width,
                work.horizontal.height,
            );
            pass.draw(0..3, 0..1);
        }

        // Vertical pass: tmp -> surface, scissored to each region so only the
        // panel footprint is overwritten with the frosted backdrop.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("phantom-blur-v"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &sized.bg_vertical, &[]);
            for region in regions {
                let (x, y, w, h) = clamp_rect(region, sized.width, sized.height);
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

fn create_uniform(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn params_bytes(dir_x: f32, dir_y: f32) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&dir_x.to_ne_bytes());
    bytes[4..8].copy_from_slice(&dir_y.to_ne_bytes());
    bytes
}

/// Clamp a region to the surface, returning `(x, y, w, h)` in physical pixels.
fn clamp_rect(region: &BlurRegion, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let x = region.x.min(width);
    let y = region.y.min(height);
    let w = region.width.min(width - x);
    let h = region.height.min(height - y);
    (x, y, w, h)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    fn origin(self) -> wgpu::Origin3d {
        wgpu::Origin3d {
            x: self.x,
            y: self.y,
            z: 0,
        }
    }

    fn extent(self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlurWorkBounds {
    /// Temp-texture pixels produced by the horizontal pass.
    horizontal: Rect,
    /// Surface pixels copied into the source texture.
    snapshot: Rect,
}

/// Plan the minimal bounding rectangles needed by the separable kernel.
///
/// The horizontal pass needs a vertical margin because the vertical pass
/// samples it above and below each output pixel. The snapshot needs a
/// horizontal margin because the horizontal pass samples it left and right.
fn blur_work_bounds(
    regions: &[BlurRegion],
    width: u32,
    height: u32,
    reach: u32,
) -> Option<BlurWorkBounds> {
    let output = union_bounds(regions, width, height)?;
    let horizontal = expand_bounds(output, 0, reach, width, height);
    let snapshot = expand_bounds(horizontal, reach, 0, width, height);
    Some(BlurWorkBounds {
        horizontal,
        snapshot,
    })
}

/// Bounding box of all non-degenerate regions, clamped to the surface.
fn union_bounds(regions: &[BlurRegion], width: u32, height: u32) -> Option<Rect> {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for region in regions {
        let (x, y, w, h) = clamp_rect(region, width, height);
        if w == 0 || h == 0 {
            continue;
        }
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    Some(Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

fn expand_bounds(rect: Rect, reach_x: u32, reach_y: u32, width: u32, height: u32) -> Rect {
    let x = rect.x.saturating_sub(reach_x);
    let y = rect.y.saturating_sub(reach_y);
    let right = rect
        .x
        .saturating_add(rect.width)
        .saturating_add(reach_x)
        .min(width);
    let bottom = rect
        .y
        .saturating_add(rect.height)
        .saturating_add(reach_y)
        .min(height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: u32, y: u32, width: u32, height: u32) -> BlurRegion {
        BlurRegion {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn clamp_rect_trims_to_surface() {
        let r = region(900, 0, 400, 600);
        assert_eq!(clamp_rect(&r, 1000, 600), (900, 0, 100, 600));
    }

    #[test]
    fn blur_work_bounds_include_each_kernel_axis_and_clamp() {
        let regions = [region(900, 100, 100, 100)];
        let bounds = blur_work_bounds(&regions, 1000, 600, 20);
        assert_eq!(
            bounds,
            Some(BlurWorkBounds {
                // Vertical samples need twenty pixels above and below.
                horizontal: Rect {
                    x: 900,
                    y: 80,
                    width: 100,
                    height: 140,
                },
                // Horizontal samples additionally need twenty pixels left;
                // the right edge clamps to the surface.
                snapshot: Rect {
                    x: 880,
                    y: 80,
                    width: 120,
                    height: 140,
                },
            })
        );
    }

    #[test]
    fn union_bounds_is_none_when_nothing_to_blur() {
        // No regions, and regions that clamp to zero size, both mean the
        // whole blur pass can be skipped.
        assert_eq!(blur_work_bounds(&[], 800, 480, 10), None);
        assert_eq!(
            blur_work_bounds(&[region(800, 0, 50, 50)], 800, 480, 10),
            None
        );
        assert_eq!(
            blur_work_bounds(&[region(10, 10, 0, 50)], 800, 480, 10),
            None
        );
    }

    #[test]
    fn union_bounds_ignores_degenerate_regions_in_a_mix() {
        let regions = [region(10, 10, 0, 50), region(100, 100, 50, 50)];
        assert_eq!(
            union_bounds(&regions, 800, 480),
            Some(Rect {
                x: 100,
                y: 100,
                width: 50,
                height: 50,
            })
        );
    }

    #[test]
    fn blur_work_bounds_cover_disjoint_region_union_without_full_surface_work() {
        let regions = [region(100, 120, 40, 30), region(300, 220, 50, 60)];
        let bounds = blur_work_bounds(&regions, 1920, 1080, 13).unwrap();
        assert_eq!(
            bounds.horizontal,
            Rect {
                x: 100,
                y: 107,
                width: 250,
                height: 186,
            }
        );
        assert_eq!(
            bounds.snapshot,
            Rect {
                x: 87,
                y: 107,
                width: 276,
                height: 186,
            }
        );
        assert!(bounds.snapshot.width < 1920);
        assert!(bounds.snapshot.height < 1080);
    }
}
