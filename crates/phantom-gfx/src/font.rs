//! Font loading and glyph rasterization.
//!
//! Discovers up to four primary faces (regular / bold / italic / bold-italic)
//! via `fontdb`, resolves each codepoint through the primary face first and
//! then through system symbol/emoji/Nerd Font fallbacks, and rasterizes glyphs
//! with `swash`. Cell metrics are derived from the primary regular face so the
//! grid is sized to the user's text font, not a fallback symbol face.

use std::collections::HashMap;

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

pub const REGULAR: usize = 0;
pub const BOLD: usize = 1;
pub const ITALIC: usize = 2;
pub const BOLD_ITALIC: usize = 3;
const STYLE_SLOTS: usize = 4;

const TERMINAL_FONT_FALLBACKS: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "MesloLGS NF",
    "Hack Nerd Font Mono",
    "Hack Nerd Font",
    "FiraCode Nerd Font Mono",
    "FiraCode Nerd Font",
    "CaskaydiaCove Nerd Font Mono",
    "CaskaydiaCove Nerd Font",
    "SauceCodePro Nerd Font Mono",
    "SauceCodePro Nerd Font",
];

const SYMBOL_FONT_FALLBACKS: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "Symbols Nerd Font Propo",
    "Noto Color Emoji",
    "Apple Color Emoji",
    "Segoe UI Emoji",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols",
    "Apple Symbols",
    "DejaVu Sans",
];

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

/// A glyph resolved to a concrete font face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedGlyph {
    pub face: u32,
    pub glyph_id: u16,
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

#[derive(Debug, Clone, Copy)]
struct FaceRef {
    id: fontdb::ID,
}

pub struct FontSet {
    db: fontdb::Database,
    faces: Vec<FaceRef>,
    primary: [usize; STYLE_SLOTS],
    fallback_order: Vec<usize>,
    px: f32,
    metrics: FontMetrics,
    ctx: ScaleContext,
    glyphs: HashMap<(usize, char), Option<ResolvedGlyph>>,
}

impl FontSet {
    /// Load the family at `size_px` (already scaled for HiDPI) with the given
    /// line-height multiplier.
    pub fn new(family: &str, size_px: f32, line_height: f32) -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        let mut faces = Vec::new();
        let regular_id =
            resolve_primary_face(&db, family, fontdb::Weight::NORMAL, fontdb::Style::Normal)
                .expect("no usable font found on this system");
        let regular = push_face(&mut faces, regular_id);
        let bold = resolve_primary_face(&db, family, fontdb::Weight::BOLD, fontdb::Style::Normal)
            .map(|id| push_face(&mut faces, id))
            .unwrap_or(regular);
        let italic =
            resolve_primary_face(&db, family, fontdb::Weight::NORMAL, fontdb::Style::Italic)
                .map(|id| push_face(&mut faces, id))
                .unwrap_or(regular);
        let bold_italic =
            resolve_primary_face(&db, family, fontdb::Weight::BOLD, fontdb::Style::Italic)
                .map(|id| push_face(&mut faces, id))
                .unwrap_or(bold);

        let fallback_order = fallback_faces(&db, &mut faces, family);
        let metrics = metrics_for_face(&db, faces[regular].id, size_px, line_height)
            .expect("regular face failed to parse");

        Self {
            db,
            faces,
            primary: [regular, bold, italic, bold_italic],
            fallback_order,
            px: size_px,
            metrics,
            ctx: ScaleContext::new(),
            glyphs: HashMap::new(),
        }
    }

    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// Resolve `ch` in `slot`, using the selected style face first, then the
    /// regular face, then installed symbol/emoji/Nerd Font fallbacks. Returns
    /// `None` only when no installed face covers the codepoint.
    pub fn resolve_glyph(&mut self, slot: usize, ch: char) -> Option<ResolvedGlyph> {
        let slot = slot.min(STYLE_SLOTS - 1);
        if let Some(glyph) = self.glyphs.get(&(slot, ch)) {
            return *glyph;
        }

        let mut candidates = Vec::with_capacity(self.fallback_order.len() + 2);
        push_unique(&mut candidates, self.primary[slot]);
        if self.primary[slot] != self.primary[REGULAR] {
            push_unique(&mut candidates, self.primary[REGULAR]);
        }
        for face in &self.fallback_order {
            push_unique(&mut candidates, *face);
        }

        let resolved = candidates
            .into_iter()
            .find_map(|face| self.glyph_in_face(face, ch));
        self.glyphs.insert((slot, ch), resolved);
        resolved
    }

    /// Rasterize a glyph from `slot`. Returns `None` only if the face fails to
    /// parse; whitespace / empty glyphs return a zero-size [`RasterGlyph`].
    pub fn rasterize(&mut self, glyph: ResolvedGlyph) -> Option<RasterGlyph> {
        let face = self.faces.get(glyph.face as usize)?;
        let font = self
            .db
            .with_face_data(face.id, |data, index| {
                FontRef::from_index(data, index as usize)
                    .map(|font| rasterize_font(&mut self.ctx, font, self.px, glyph.glyph_id))
            })
            .flatten()?;
        Some(font)
    }

    fn glyph_in_face(&self, face: usize, ch: char) -> Option<ResolvedGlyph> {
        let face_ref = self.faces.get(face)?;
        let glyph_id = self
            .db
            .with_face_data(face_ref.id, |data, index| {
                FontRef::from_index(data, index as usize)
                    .map(|font| font.charmap().map(ch))
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        (glyph_id != 0).then_some(ResolvedGlyph {
            face: face as u32,
            glyph_id,
        })
    }
}

fn rasterize_font(
    ctx: &mut ScaleContext,
    font: FontRef<'_>,
    px: f32,
    glyph_id: u16,
) -> RasterGlyph {
    let mut scaler = ctx.builder(font).size(px).hint(true).build();

    let sources = [
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ];
    let image = Render::new(&sources)
        .format(Format::Alpha)
        .render(&mut scaler, glyph_id);

    let Some(image) = image else {
        return RasterGlyph {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            is_color: false,
            data: Vec::new(),
        };
    };

    let width = image.placement.width;
    let height = image.placement.height;
    let (left, top) = (image.placement.left, image.placement.top);

    if width == 0 || height == 0 {
        return RasterGlyph {
            width: 0,
            height: 0,
            left,
            top,
            is_color: false,
            data: Vec::new(),
        };
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

    RasterGlyph {
        width,
        height,
        left,
        top,
        is_color,
        data,
    }
}

fn resolve_primary_face(
    db: &fontdb::Database,
    family: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<fontdb::ID> {
    let generic_mono = [fontdb::Family::Monospace];
    if is_generic_monospace(family) {
        for name in TERMINAL_FONT_FALLBACKS {
            let families = [fontdb::Family::Name(name)];
            if let Some(id) = query_face(db, &families, weight, style) {
                return Some(id);
            }
        }
        return query_face(db, &generic_mono, weight, style).or_else(|| first_face(db));
    }

    let named = [fontdb::Family::Name(family), fontdb::Family::Monospace];
    query_face(db, &named, weight, style)
        .or_else(|| query_face(db, &generic_mono, weight, style))
        .or_else(|| first_face(db))
}

fn query_face(
    db: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families,
        weight,
        stretch: fontdb::Stretch::Normal,
        style,
    })
}

fn first_face(db: &fontdb::Database) -> Option<fontdb::ID> {
    db.faces().next().map(|face| face.id)
}

fn push_face(faces: &mut Vec<FaceRef>, id: fontdb::ID) -> usize {
    if let Some(index) = faces.iter().position(|face| face.id == id) {
        return index;
    }
    let index = faces.len();
    faces.push(FaceRef { id });
    index
}

fn fallback_faces(
    db: &fontdb::Database,
    faces: &mut Vec<FaceRef>,
    configured_family: &str,
) -> Vec<usize> {
    let mut order = Vec::new();
    for name in TERMINAL_FONT_FALLBACKS
        .iter()
        .chain(SYMBOL_FONT_FALLBACKS.iter())
    {
        if !configured_family.eq_ignore_ascii_case(name) {
            let families = [fontdb::Family::Name(name)];
            if let Some(id) =
                query_face(db, &families, fontdb::Weight::NORMAL, fontdb::Style::Normal)
            {
                push_ordered_face(faces, &mut order, id);
            }
        }
    }
    let generic = [fontdb::Family::Monospace];
    if let Some(id) = query_face(db, &generic, fontdb::Weight::NORMAL, fontdb::Style::Normal) {
        push_ordered_face(faces, &mut order, id);
    }
    let generic = [fontdb::Family::SansSerif];
    if let Some(id) = query_face(db, &generic, fontdb::Weight::NORMAL, fontdb::Style::Normal) {
        push_ordered_face(faces, &mut order, id);
    }
    for face in db.faces() {
        push_ordered_face(faces, &mut order, face.id);
    }
    order
}

fn push_ordered_face(faces: &mut Vec<FaceRef>, order: &mut Vec<usize>, id: fontdb::ID) {
    let index = push_face(faces, id);
    push_unique(order, index);
}

fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn metrics_for_face(
    db: &fontdb::Database,
    id: fontdb::ID,
    size_px: f32,
    line_height: f32,
) -> Option<FontMetrics> {
    db.with_face_data(id, |data, index| {
        let font = FontRef::from_index(data, index as usize)?;
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
        let top_pad = (cell_h - natural) / 2.0;
        Some(FontMetrics {
            cell_w: cell_w.ceil().max(1.0),
            cell_h: cell_h.ceil().max(1.0),
            ascent: (top_pad + m.ascent).round(),
            underline_offset: m.underline_offset,
            underline_size: m.stroke_size.max(1.0),
            strikeout_offset: (m.x_height / 2.0).max(1.0),
        })
    })
    .flatten()
}

fn is_generic_monospace(family: &str) -> bool {
    let family = family.trim();
    family.is_empty()
        || family.eq_ignore_ascii_case("monospace")
        || family.eq_ignore_ascii_case("mono")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ascii_to_real_glyph() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2);

        let glyph = fonts
            .resolve_glyph(REGULAR, '>')
            .expect("monospace font should cover ASCII");

        assert_ne!(glyph.glyph_id, 0);
        let raster = fonts.rasterize(glyph).expect("glyph should rasterize");
        assert!(raster.width > 0 && raster.height > 0);
    }

    #[test]
    fn resolved_glyphs_never_return_notdef() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2);

        for ch in ['>', '\u{276f}', '\u{e0b0}', '\u{1f680}', '\u{10fffd}'] {
            if let Some(glyph) = fonts.resolve_glyph(REGULAR, ch) {
                assert_ne!(glyph.glyph_id, 0, "resolved {ch:?} to .notdef");
            }
        }
    }

    #[test]
    fn starship_symbols_resolve_when_installed() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2);

        // Starship/Nerd Font prompts commonly use one or more of these. This is
        // optional because a clean CI image may not have Nerd Fonts installed.
        let resolved = ['\u{276f}', '\u{e0b0}', '\u{f0e7}']
            .into_iter()
            .filter_map(|ch| fonts.resolve_glyph(REGULAR, ch).map(|glyph| (ch, glyph)))
            .collect::<Vec<_>>();

        for (ch, glyph) in resolved {
            assert_ne!(glyph.glyph_id, 0, "resolved {ch:?} to .notdef");
        }
    }
}
