//! Phantom Terminal GPU grid renderer.
//!
//! A hand-rolled wgpu renderer for a terminal [`Snapshot`]: it rasterizes the
//! configured font into a glyph atlas ([`font`]/[`atlas`]) and draws the grid
//! with two instanced pipelines — solid quads (cell backgrounds, cursor,
//! underline/strikethrough) and textured glyph quads (alpha-mask tinted by the
//! foreground colour, plus colour-emoji passthrough). Colours are resolved
//! through [`palette`] (ANSI-16 / 256 / truecolour) and emitted in linear space
//! to match an sRGB surface.

mod atlas;
mod backdrop;
mod font;
#[cfg(feature = "headless")]
pub mod headless;
mod palette;

use std::borrow::Cow;
use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use phantom_core::AppConfig;
use phantom_emu::{CursorShape, Snapshot};

use atlas::{GlyphAtlas, GlyphKey};
use backdrop::BackdropRenderer;
pub use font::available_terminal_font_families;
use font::{face_slot, FontSet, REGULAR};
use palette::{Palette, Rgba};

const ATLAS_SIZE: u32 = 2048;
const INITIAL_INSTANCE_CAP: u64 = 4096;

const LAYER_BASE: usize = 0;
const LAYER_OVERLAY: usize = 1;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SolidInstance {
    pos: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    pos: [f32; 2],
    size: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
    is_color: f32,
    _pad: [f32; 3],
}

pub struct Renderer {
    palette: Palette,
    font: FontSet,
    atlas: GlyphAtlas,
    backdrop: BackdropRenderer,

    solid_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,

    solid_buffer: wgpu::Buffer,
    solid_cap: u64,
    glyph_buffer: wgpu::Buffer,
    glyph_cap: u64,

    // Two draw layers (base = terminal + tab bar, overlay = palette / settings).
    // Each layer accumulates its own instances; `render` draws base then overlay
    // so overlays composite correctly on top of the terminal.
    solids: [Vec<SolidInstance>; 2],
    glyphs: [Vec<GlyphInstance>; 2],
    target: usize,
    solid_counts: [u32; 2],
    glyph_counts: [u32; 2],

    cell_w: f32,
    cell_h: f32,
    ascent: f32,
    underline_offset: f32,
    underline_size: f32,
    strikeout_offset: f32,
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        config: &AppConfig,
        scale_factor: f32,
    ) -> Self {
        let palette = Palette::from_theme(&config.theme);
        let font = FontSet::new(
            &config.font_family,
            (config.font_size as f32 * scale_factor).max(1.0),
            config.line_height,
        );
        let metrics = font.metrics();
        let atlas = GlyphAtlas::new(device, ATLAS_SIZE);

        // Uniform: screen size in physical px.
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phantom-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phantom-uniform-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("phantom-uniform-bg"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phantom-atlas-layout"),
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
            label: Some("phantom-sampler"),
            min_filter: wgpu::FilterMode::Nearest,
            mag_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("phantom-atlas-bg"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let backdrop = BackdropRenderer::new(device, queue, format, &uniform_layout);
        let solid_pipeline = build_pipeline(
            device,
            format,
            "phantom-solid",
            include_str!("shaders/solid.wgsl"),
            &[&uniform_layout],
            &solid_vertex_layout(),
        );
        let glyph_pipeline = build_pipeline(
            device,
            format,
            "phantom-glyph",
            include_str!("shaders/glyph.wgsl"),
            &[&uniform_layout, &atlas_layout],
            &glyph_vertex_layout(),
        );

        let solid_buffer =
            create_instance_buffer(device, "phantom-solid-inst", INITIAL_INSTANCE_CAP);
        let glyph_buffer =
            create_instance_buffer(device, "phantom-glyph-inst", INITIAL_INSTANCE_CAP);

        let renderer = Self {
            palette,
            font,
            atlas,
            backdrop,
            solid_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_bind_group,
            atlas_bind_group,
            solid_buffer,
            solid_cap: INITIAL_INSTANCE_CAP,
            glyph_buffer,
            glyph_cap: INITIAL_INSTANCE_CAP,
            solids: [Vec::new(), Vec::new()],
            glyphs: [Vec::new(), Vec::new()],
            target: LAYER_BASE,
            solid_counts: [0, 0],
            glyph_counts: [0, 0],
            cell_w: metrics.cell_w,
            cell_h: metrics.cell_h,
            ascent: metrics.ascent,
            underline_offset: metrics.underline_offset,
            underline_size: metrics.underline_size,
            strikeout_offset: metrics.strikeout_offset,
        };
        renderer.write_uniforms(queue, [1.0, 1.0]);
        renderer
    }

    /// Cell size in physical px.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// Number of whole cells `(rows, cols)` for a window of the given physical
    /// pixel size.
    pub fn grid_size(&self, width: u32, height: u32) -> (u16, u16) {
        let cols = ((width as f32 / self.cell_w).floor() as i64).clamp(1, 1000) as u16;
        let rows = ((height as f32 / self.cell_h).floor() as i64).clamp(1, 1000) as u16;
        (rows, cols)
    }

    /// Window-background clear colour in linear space.
    pub fn clear_color(&self) -> wgpu::Color {
        let [r, g, b, _] = self.palette.background();
        wgpu::Color {
            r: srgb_to_linear(r),
            g: srgb_to_linear(g),
            b: srgb_to_linear(b),
            a: 1.0,
        }
    }

    pub fn resize(&self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.write_uniforms(queue, [width.max(1) as f32, height.max(1) as f32]);
    }

    fn write_uniforms(&self, queue: &wgpu::Queue, screen: [f32; 2]) {
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                screen,
                _pad: [0.0, 0.0],
            }),
        );
    }

    pub fn ascent(&self) -> f32 {
        self.ascent
    }

    pub fn foreground_color(&self) -> Rgba {
        self.palette.foreground()
    }

    pub fn background_color(&self) -> Rgba {
        self.palette.background()
    }

    /// One of the 16 ANSI palette slots (clamped), for chrome accents.
    pub fn ansi_color(&self, index: usize) -> Rgba {
        self.palette.ansi(index.min(15))
    }

    /// Advance width of `s` rendered as monospace text.
    pub fn text_width(&self, s: &str) -> f32 {
        s.chars().count() as f32 * self.cell_w
    }

    /// Start a frame: clear both layers and target the base layer.
    pub fn begin(&mut self) {
        self.backdrop.clear();
        for layer in 0..2 {
            self.solids[layer].clear();
            self.glyphs[layer].clear();
        }
        self.target = LAYER_BASE;
    }

    /// Direct subsequent draws to the overlay layer (composited on top of the
    /// base layer). Used by the command palette and settings panel.
    pub fn begin_overlay(&mut self) {
        self.target = LAYER_OVERLAY;
    }

    /// Fill an axis-aligned rectangle (physical px) in the current layer.
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        self.solids[self.target].push(solid(x, y, w, h, color));
    }

    /// Draw the selected local terminal backdrop behind terminal cells.
    pub fn draw_terminal_backdrop(
        &mut self,
        name: &str,
        opacity_percent: u8,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        self.backdrop.draw(name, opacity_percent, x, y, w, h);
    }

    /// Draw monospace `s` with its top-left at `(x, y)` in the current layer.
    /// Returns the advance width.
    pub fn text(&mut self, queue: &wgpu::Queue, x: f32, y: f32, s: &str, color: Rgba) -> f32 {
        let baseline = y + self.ascent;
        let mut pen = x;
        for ch in s.chars() {
            if ch != ' ' {
                self.emit_glyph(queue, REGULAR, ch, pen, baseline, color);
            }
            pen += self.cell_w;
        }
        pen - x
    }

    /// Draw the terminal grid with its top-left at `(ox, oy)`. `cursor_on` is the
    /// current blink phase.
    pub fn draw_terminal(
        &mut self,
        queue: &wgpu::Queue,
        snap: &Snapshot,
        cursor_on: bool,
        ox: f32,
        oy: f32,
    ) {
        let (cw, ch) = (self.cell_w, self.cell_h);
        let default_bg = self.palette.background();
        let cursor_color = self.palette.cursor();
        let selection_color = self.palette.selection();
        let cursor = snap.cursor;
        let show_cursor = cursor.visible && cursor_on;

        for row in 0..snap.rows as usize {
            for col in 0..snap.cols as usize {
                let Some(cell) = snap.cell(row, col) else {
                    continue;
                };
                let attrs = cell.attrs;

                // Inverse swaps fg/bg before resolution.
                let (fg_color, bg_color) = if attrs.inverse {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };
                let mut fg = self.palette.resolve(fg_color);
                let bg = self.palette.resolve(bg_color);
                if attrs.dim {
                    fg = dim(fg);
                }

                let wide = cell.width.max(1) as f32;
                let x = ox + col as f32 * cw;
                let y = oy + row as f32 * ch;
                let is_cursor = show_cursor && row == cursor.row && col == cursor.col;
                let block_cursor = is_cursor && matches!(cursor.shape, CursorShape::Block);

                // Background (or block cursor fill).
                if block_cursor {
                    self.fill_rect(x, y, cw * wide, ch, cursor_color);
                    // Glyph drawn in the cell background colour for contrast.
                    fg = bg;
                } else if bg != default_bg {
                    self.fill_rect(x, y, cw * wide, ch, bg);
                }

                // Selection highlight (over the bg, under the glyph).
                if cell.selected {
                    self.fill_rect(x, y, cw * wide, ch, selection_color);
                }

                // Glyph.
                if !attrs.hidden && cell.c != ' ' {
                    let slot = face_slot(attrs.bold, attrs.italic);
                    self.emit_glyph(queue, slot, cell.c, x, y + self.ascent, fg);
                }

                // Underline / strikethrough.
                if attrs.underline {
                    let uy = y + self.ascent - self.underline_offset;
                    self.fill_rect(x, uy, cw * wide, self.underline_size, fg);
                }
                if attrs.strikeout {
                    let sy = y + self.ascent - self.strikeout_offset;
                    self.fill_rect(x, sy, cw * wide, self.underline_size, fg);
                }

                // Bar / underline cursor (block handled above).
                if is_cursor && !block_cursor {
                    match cursor.shape {
                        CursorShape::Beam => {
                            self.fill_rect(x, y, (cw * 0.15).max(1.0), ch, cursor_color);
                        }
                        CursorShape::Underline => {
                            let h = (ch * 0.12).max(1.0);
                            self.fill_rect(x, y + ch - h, cw * wide, h, cursor_color);
                        }
                        CursorShape::Block => {}
                    }
                }
            }
        }
    }

    fn emit_glyph(
        &mut self,
        queue: &wgpu::Queue,
        slot: usize,
        ch: char,
        pen_x: f32,
        baseline_y: f32,
        color: Rgba,
    ) {
        let Some(resolved) = self.font.resolve_glyph(slot, ch) else {
            return;
        };
        let key = GlyphKey {
            face: resolved.face,
            glyph_id: resolved.glyph_id,
        };
        let entry = match self.atlas.get(key) {
            Some(entry) => Some(entry),
            None => self
                .font
                .rasterize(resolved)
                .map(|raster| self.atlas.insert(queue, key, &raster)),
        };
        if let Some(entry) = entry {
            if !entry.empty {
                let gx = pen_x + entry.left as f32;
                let gy = baseline_y - entry.top as f32;
                self.glyphs[self.target].push(glyph(
                    gx,
                    gy,
                    entry.width as f32,
                    entry.height as f32,
                    entry.uv_min,
                    entry.uv_max,
                    color,
                    entry.is_color,
                ));
            }
        }
    }

    /// Upload the frame's instance data. Call once after all draws.
    pub fn end(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.backdrop.end(queue);
        self.solid_counts = [self.solids[0].len() as u32, self.solids[1].len() as u32];
        self.glyph_counts = [self.glyphs[0].len() as u32, self.glyphs[1].len() as u32];

        let mut all_solids = Vec::with_capacity(self.solids[0].len() + self.solids[1].len());
        all_solids.extend_from_slice(&self.solids[0]);
        all_solids.extend_from_slice(&self.solids[1]);
        let solid_bytes = bytemuck::cast_slice(&all_solids);
        if solid_bytes.len() as u64 > self.solid_cap {
            self.solid_cap = (solid_bytes.len() as u64).next_power_of_two();
            self.solid_buffer =
                create_instance_buffer(device, "phantom-solid-inst", self.solid_cap);
        }
        if !all_solids.is_empty() {
            queue.write_buffer(&self.solid_buffer, 0, solid_bytes);
        }

        let mut all_glyphs = Vec::with_capacity(self.glyphs[0].len() + self.glyphs[1].len());
        all_glyphs.extend_from_slice(&self.glyphs[0]);
        all_glyphs.extend_from_slice(&self.glyphs[1]);
        let glyph_bytes = bytemuck::cast_slice(&all_glyphs);
        if glyph_bytes.len() as u64 > self.glyph_cap {
            self.glyph_cap = (glyph_bytes.len() as u64).next_power_of_two();
            self.glyph_buffer =
                create_instance_buffer(device, "phantom-glyph-inst", self.glyph_cap);
        }
        if !all_glyphs.is_empty() {
            queue.write_buffer(&self.glyph_buffer, 0, glyph_bytes);
        }
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        let [base_solids, overlay_solids] = self.solid_counts;
        let [base_glyphs, overlay_glyphs] = self.glyph_counts;

        // Draw order = compositing order: terminal backdrop, base solids, base
        // glyphs, then the overlay layer on top.
        self.backdrop.render(pass, &self.uniform_bind_group);
        self.draw_solids(pass, 0..base_solids);
        self.draw_glyphs(pass, 0..base_glyphs);
        self.draw_solids(pass, base_solids..base_solids + overlay_solids);
        self.draw_glyphs(pass, base_glyphs..base_glyphs + overlay_glyphs);
    }

    fn draw_solids(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.solid_pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_vertex_buffer(0, self.solid_buffer.slice(..));
        pass.draw(0..4, range);
    }

    fn draw_glyphs(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.glyph_pipeline);
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_bind_group(1, &self.atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, self.glyph_buffer.slice(..));
        pass.draw(0..4, range);
    }
}

fn solid(x: f32, y: f32, w: f32, h: f32, color: Rgba) -> SolidInstance {
    SolidInstance {
        pos: [x, y],
        size: [w, h],
        color: to_linear(color),
    }
}

#[allow(clippy::too_many_arguments)]
fn glyph(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: Rgba,
    is_color: bool,
) -> GlyphInstance {
    GlyphInstance {
        pos: [x, y],
        size: [w, h],
        uv_min,
        uv_max,
        color: to_linear(color),
        is_color: if is_color { 1.0 } else { 0.0 },
        _pad: [0.0; 3],
    }
}

fn create_instance_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &str,
    wgsl: &str,
    bind_group_layouts: &[&wgpu::BindGroupLayout],
    vertex_layout: &wgpu::VertexBufferLayout<'_>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(wgsl)),
    });
    let layout_refs: Vec<Option<&wgpu::BindGroupLayout>> =
        bind_group_layouts.iter().map(|l| Some(*l)).collect();
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &layout_refs,
        ..Default::default()
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            buffers: std::slice::from_ref(vertex_layout),
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

fn solid_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x2, // pos
        1 => Float32x2, // size
        2 => Float32x4, // color
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SolidInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

fn glyph_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x2, // pos
        1 => Float32x2, // size
        2 => Float32x2, // uv_min
        3 => Float32x2, // uv_max
        4 => Float32x4, // color
        5 => Float32,   // is_color
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GlyphInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

fn to_linear(c: Rgba) -> [f32; 4] {
    [
        srgb_to_linear(c[0]) as f32,
        srgb_to_linear(c[1]) as f32,
        srgb_to_linear(c[2]) as f32,
        c[3] as f32 / 255.0,
    ]
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn dim(c: Rgba) -> Rgba {
    [
        (c[0] as f32 * 0.66) as u8,
        (c[1] as f32 * 0.66) as u8,
        (c[2] as f32 * 0.66) as u8,
        c[3],
    ]
}
