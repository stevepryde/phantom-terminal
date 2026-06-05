//! Font loading and glyph rasterization.
//!
//! Discovers up to four faces (regular / bold / italic / bold-italic) of the
//! configured family via `fontdb`, falling back to a generic monospace, and
//! rasterizes glyphs with `swash`. Cell metrics are derived from the regular
//! face so the grid is sized to the real font, not a heuristic.

use std::collections::HashMap;

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

pub const REGULAR: usize = 0;
pub const BOLD: usize = 1;
pub const ITALIC: usize = 2;
pub const BOLD_ITALIC: usize = 3;

/// Pick the face slot for a cell's bold/italic attributes.
pub fn face_slot(bold: bool, italic: bool) -> usize {
    match (bold, italic) {
        (false, false) => REGULAR,
        (true, false) => BOLD,
        (false, true) => ITALIC,
        (true, true) => BOLD_ITALIC,
    }
}

/// A rasterized glyph bitmap as RGBA (mask glyphs are white with coverage in
/// alpha; colour glyphs carry real RGBA).
pub struct RasterGlyph {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub is_color: bool,
    pub data: Vec<u8>,
}

/// Pixel metrics derived from the regular face at the render size.
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Baseline distance from the top of the cell.
    pub ascent: f32,
    pub underline_offset: f32,
    pub underline_size: f32,
    /// Strikethrough distance above the baseline.
    pub strikeout_offset: f32,
}

pub struct FontSet {
    faces: [Option<(Vec<u8>, u32)>; 4],
    px: f32,
    metrics: FontMetrics,
    ctx: ScaleContext,
    glyph_ids: HashMap<(usize, char), u16>,
}

impl FontSet {
    /// Load the family at `size_px` (already scaled for HiDPI) with the given
    /// line-height multiplier.
    pub fn new(family: &str, size_px: f32, line_height: f32) -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        let load = |weight: fontdb::Weight, style: fontdb::Style| -> Option<(Vec<u8>, u32)> {
            let query = fontdb::Query {
                families: &[fontdb::Family::Name(family), fontdb::Family::Monospace],
                weight,
                stretch: fontdb::Stretch::Normal,
                style,
            };
            let id = db.query(&query)?;
            db.with_face_data(id, |data, index| (data.to_vec(), index))
        };

        let regular = load(fontdb::Weight::NORMAL, fontdb::Style::Normal)
            .or_else(|| {
                // Last resort: any monospace, else any installed face at all.
                let id = db
                    .query(&fontdb::Query {
                        families: &[fontdb::Family::Monospace],
                        weight: fontdb::Weight::NORMAL,
                        stretch: fontdb::Stretch::Normal,
                        style: fontdb::Style::Normal,
                    })
                    .or_else(|| db.faces().next().map(|f| f.id))?;
                db.with_face_data(id, |data, index| (data.to_vec(), index))
            })
            .expect("no usable font found on this system");

        let bold = load(fontdb::Weight::BOLD, fontdb::Style::Normal);
        let italic = load(fontdb::Weight::NORMAL, fontdb::Style::Italic);
        let bold_italic = load(fontdb::Weight::BOLD, fontdb::Style::Italic);

        let metrics = {
            let font = FontRef::from_index(&regular.0, regular.1 as usize)
                .expect("regular face failed to parse");
            let m = font.metrics(&[]).scale(size_px);
            let gm = font.glyph_metrics(&[]).scale(size_px);
            let zero = font.charmap().map('0');
            let advance = if zero != 0 {
                gm.advance_width(zero)
            } else {
                0.0
            };
            let cell_w = if advance > 0.0 {
                advance
            } else if m.average_width > 0.0 {
                m.average_width
            } else {
                size_px * 0.6
            };
            let natural = m.ascent + m.descent + m.leading;
            let cell_h = (natural * line_height).max(1.0);
            // Centre the natural line box within the (possibly taller) cell.
            let top_pad = (cell_h - natural) / 2.0;
            FontMetrics {
                cell_w: cell_w.ceil().max(1.0),
                cell_h: cell_h.ceil().max(1.0),
                ascent: (top_pad + m.ascent).round(),
                underline_offset: m.underline_offset,
                underline_size: m.stroke_size.max(1.0),
                strikeout_offset: (m.x_height / 2.0).max(1.0),
            }
        };

        Self {
            faces: [Some(regular), bold, italic, bold_italic],
            px: size_px,
            metrics,
            ctx: ScaleContext::new(),
            glyph_ids: HashMap::new(),
        }
    }

    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// Glyph id for `ch` in `slot` (falling back to the regular face when that
    /// style is missing). Cached after the first lookup.
    pub fn glyph_id(&mut self, slot: usize, ch: char) -> u16 {
        if let Some(&id) = self.glyph_ids.get(&(slot, ch)) {
            return id;
        }
        let face = self.faces[slot]
            .as_ref()
            .or(self.faces[REGULAR].as_ref())
            .expect("regular face always present");
        let id = FontRef::from_index(&face.0, face.1 as usize)
            .map(|font| font.charmap().map(ch))
            .unwrap_or(0);
        self.glyph_ids.insert((slot, ch), id);
        id
    }

    /// Rasterize a glyph from `slot`. Returns `None` only if the face fails to
    /// parse; whitespace / empty glyphs return a zero-size [`RasterGlyph`].
    pub fn rasterize(&mut self, slot: usize, glyph_id: u16) -> Option<RasterGlyph> {
        let face = self.faces[slot]
            .as_ref()
            .or(self.faces[REGULAR].as_ref())
            .expect("regular face always present");
        let font = FontRef::from_index(&face.0, face.1 as usize)?;
        let mut scaler = self.ctx.builder(font).size(self.px).hint(true).build();

        let sources = [
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ];
        let image = Render::new(&sources)
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id)?;

        let width = image.placement.width;
        let height = image.placement.height;
        let (left, top) = (image.placement.left, image.placement.top);

        if width == 0 || height == 0 {
            return Some(RasterGlyph {
                width: 0,
                height: 0,
                left,
                top,
                is_color: false,
                data: Vec::new(),
            });
        }

        let (is_color, data) = match image.content {
            Content::Color => (true, image.data),
            Content::Mask => {
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for coverage in image.data {
                    rgba.extend_from_slice(&[255, 255, 255, coverage]);
                }
                (false, rgba)
            }
            Content::SubpixelMask => {
                // We request Alpha, so this is unexpected; average the channels
                // back into a coverage mask defensively.
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for px in image.data.chunks_exact(4) {
                    let coverage = ((px[0] as u16 + px[1] as u16 + px[2] as u16) / 3) as u8;
                    rgba.extend_from_slice(&[255, 255, 255, coverage]);
                }
                (false, rgba)
            }
        };

        Some(RasterGlyph {
            width,
            height,
            left,
            top,
            is_color,
            data,
        })
    }
}
