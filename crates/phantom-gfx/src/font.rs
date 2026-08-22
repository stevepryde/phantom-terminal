//! Font loading and glyph rasterization.
//!
//! Discovers up to four primary faces (regular / bold / italic / bold-italic)
//! via `fontique`, resolves each codepoint through the primary face first and
//! then through system symbol/emoji/Nerd Font fallbacks, and rasterizes glyphs
//! with `swash`. Cell metrics are derived from the primary regular face so the
//! grid is sized to the user's text font, not a fallback symbol face.

use std::collections::{HashMap, HashSet};

use fontique::{
    Collection, CollectionOptions, FamilyId, FontInfo, FontStyle, FontWeight, FontWidth,
    GenericFamily, SourceCache,
};
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

/// Return installed families that are suitable primary terminal fonts.
///
/// The generic `monospace` choice stays first because it lets the platform pick
/// a sensible terminal face and keeps old configs valid on every machine.
pub fn available_terminal_font_families() -> Vec<String> {
    let (mut collection, mut source_cache) = load_font_collection();
    available_terminal_font_families_from_collection(&mut collection, &mut source_cache)
}

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

#[derive(Debug, Clone)]
struct FaceRef {
    font: FontInfo,
}

pub struct FontSet {
    source_cache: SourceCache,
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
    ///
    /// Fallible because it runs not only at startup but on every renderer
    /// rebuild (font settings change, monitor scale change): a corrupt font
    /// file mid-session must surface as an error, not a panic.
    pub fn new(family: &str, size_px: f32, line_height: f32) -> Result<Self, String> {
        let (mut collection, mut source_cache) = load_font_collection();

        let mut faces = Vec::new();
        let regular_font = resolve_primary_face(
            &mut collection,
            family,
            FontWeight::NORMAL,
            FontStyle::Normal,
        )
        .ok_or_else(|| "no usable font found on this system".to_string())?;
        let regular = push_face(&mut faces, regular_font);
        let bold =
            resolve_primary_face(&mut collection, family, FontWeight::BOLD, FontStyle::Normal)
                .map(|font| push_face(&mut faces, font))
                .unwrap_or(regular);
        let italic = resolve_primary_face(
            &mut collection,
            family,
            FontWeight::NORMAL,
            FontStyle::Italic,
        )
        .map(|font| push_face(&mut faces, font))
        .unwrap_or(regular);
        let bold_italic =
            resolve_primary_face(&mut collection, family, FontWeight::BOLD, FontStyle::Italic)
                .map(|font| push_face(&mut faces, font))
                .unwrap_or(bold);

        let fallback_order = fallback_faces(&mut collection, &mut faces, family);
        let mut primary = [regular, bold, italic, bold_italic];
        let metrics = match metrics_for_face(
            &faces[regular].font,
            &mut source_cache,
            size_px,
            line_height,
        ) {
            Some(metrics) => metrics,
            None => {
                // The picked face exists but fails to parse (corrupt file):
                // fall back to the first installed face that does parse, and
                // point any style slot that resolved to the broken face at it.
                let family_names = collection
                    .family_names()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                let (font, metrics) = family_names
                    .iter()
                    .filter_map(|name| {
                        query_named_face(
                            &mut collection,
                            name,
                            FontWeight::NORMAL,
                            FontStyle::Normal,
                        )
                    })
                    .find_map(|font| {
                        metrics_for_face(&font, &mut source_cache, size_px, line_height)
                            .map(|metrics| (font, metrics))
                    })
                    .ok_or_else(|| "no installed font face could be parsed".to_string())?;
                let parsed = push_face(&mut faces, font);
                for slot in &mut primary {
                    if *slot == regular {
                        *slot = parsed;
                    }
                }
                metrics
            }
        };

        Ok(Self {
            source_cache,
            faces,
            primary,
            fallback_order,
            px: size_px,
            metrics,
            ctx: ScaleContext::new(),
            glyphs: HashMap::new(),
        })
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

        // `fallback_order` is already deduplicated, so only the two primary
        // faces need skipping — no per-push `contains` scan (which made the
        // first resolution of an uncovered codepoint O(faces²) on systems
        // with many installed fonts).
        let primary_slot = self.primary[slot];
        let primary_regular = self.primary[REGULAR];
        let mut resolved = self.glyph_in_face(primary_slot, ch);
        if resolved.is_none() && primary_slot != primary_regular {
            resolved = self.glyph_in_face(primary_regular, ch);
        }
        if resolved.is_none() {
            for index in 0..self.fallback_order.len() {
                let face = self.fallback_order[index];
                if face == primary_slot || face == primary_regular {
                    continue;
                }
                if let Some(glyph) = self.glyph_in_face(face, ch) {
                    resolved = Some(glyph);
                    break;
                }
            }
        }
        self.glyphs.insert((slot, ch), resolved);
        resolved
    }

    /// Rasterize a glyph from `slot`. Returns `None` only if the face fails to
    /// parse; whitespace / empty glyphs return a zero-size [`RasterGlyph`].
    pub fn rasterize(&mut self, glyph: ResolvedGlyph) -> Option<RasterGlyph> {
        let face = self.faces.get(glyph.face as usize)?.font.clone();
        let data = face.load(Some(&mut self.source_cache))?;
        let font = FontRef::from_index(data.as_ref(), face.index() as usize)?;
        Some(rasterize_font(&mut self.ctx, font, self.px, glyph.glyph_id))
    }

    fn glyph_in_face(&mut self, face: usize, ch: char) -> Option<ResolvedGlyph> {
        let face_ref = self.faces.get(face)?.font.clone();
        let data = face_ref.load(Some(&mut self.source_cache))?;
        let glyph_id = FontRef::from_index(data.as_ref(), face_ref.index() as usize)?
            .charmap()
            .map(ch);
        (glyph_id != 0).then_some(ResolvedGlyph {
            face: face as u32,
            glyph_id,
        })
    }
}

fn load_font_collection() -> (Collection, SourceCache) {
    #[cfg(target_os = "linux")]
    let mut collection = Collection::new(CollectionOptions::default());
    #[cfg(not(target_os = "linux"))]
    let collection = Collection::new(CollectionOptions::default());
    #[cfg(target_os = "linux")]
    let mut source_cache = SourceCache::default();
    #[cfg(not(target_os = "linux"))]
    let source_cache = SourceCache::default();

    #[cfg(target_os = "linux")]
    ensure_linux_monospace(&mut collection, &mut source_cache);

    (collection, source_cache)
}

#[cfg(target_os = "linux")]
fn ensure_linux_monospace(collection: &mut Collection, source_cache: &mut SourceCache) {
    if query_generic_face(
        collection,
        GenericFamily::Monospace,
        FontWeight::NORMAL,
        FontStyle::Normal,
    )
    .is_some_and(|font| font_is_monospace(&font, source_cache))
    {
        return;
    }

    // Some Linux fontconfig configurations omit a usable `monospace` alias.
    // Prefer a genuinely fixed-width installed family before using embedded
    // data so normal desktop font configuration remains authoritative.
    let names = collection
        .family_names()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Some(family_id) = names.iter().find_map(|name| {
        let family_id = collection.family_id(name)?;
        let font = match_family(collection, family_id, FontWeight::NORMAL, FontStyle::Normal)?;
        font_is_monospace(&font, source_cache).then_some(family_id)
    }) {
        collection.set_generic_families(GenericFamily::Monospace, [family_id].into_iter());
        return;
    }

    // Minimal distributions and containers may ship no fonts at all. Hack is
    // already present in the locked egui dependency tree and gives the
    // terminal a deterministic, offline-safe last resort.
    let registered =
        collection.register_fonts(epaint_default_fonts::HACK_REGULAR.to_vec().into(), None);
    collection.set_generic_families(
        GenericFamily::Monospace,
        registered.into_iter().map(|(family, _)| family),
    );
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
            for px in image.data.as_chunks::<4>().0 {
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
    collection: &mut Collection,
    family: &str,
    weight: FontWeight,
    style: FontStyle,
) -> Option<FontInfo> {
    if is_generic_monospace(family) {
        for name in TERMINAL_FONT_FALLBACKS {
            if let Some(font) = query_named_face(collection, name, weight, style) {
                return Some(font);
            }
        }
        return query_generic_face(collection, GenericFamily::Monospace, weight, style)
            .or_else(|| first_face(collection));
    }

    query_named_face(collection, family, weight, style)
        .or_else(|| query_generic_face(collection, GenericFamily::Monospace, weight, style))
        .or_else(|| first_face(collection))
}

fn query_named_face(
    collection: &mut Collection,
    family: &str,
    weight: FontWeight,
    style: FontStyle,
) -> Option<FontInfo> {
    let family = collection.family_id(family)?;
    match_family(collection, family, weight, style)
}

fn query_generic_face(
    collection: &mut Collection,
    generic: GenericFamily,
    weight: FontWeight,
    style: FontStyle,
) -> Option<FontInfo> {
    let families = collection.generic_families(generic).collect::<Vec<_>>();
    families
        .into_iter()
        .find_map(|family| match_family(collection, family, weight, style))
}

fn match_family(
    collection: &mut Collection,
    family: FamilyId,
    weight: FontWeight,
    style: FontStyle,
) -> Option<FontInfo> {
    collection
        .family(family)?
        .match_font(FontWidth::NORMAL, style, weight, true)
        .cloned()
}

fn first_face(collection: &mut Collection) -> Option<FontInfo> {
    let names = collection
        .family_names()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    names
        .iter()
        .find_map(|name| query_named_face(collection, name, FontWeight::NORMAL, FontStyle::Normal))
}

fn same_face(left: &FontInfo, right: &FontInfo) -> bool {
    left.source().id() == right.source().id() && left.index() == right.index()
}

fn push_face(faces: &mut Vec<FaceRef>, font: FontInfo) -> usize {
    if let Some(index) = faces.iter().position(|face| same_face(&face.font, &font)) {
        return index;
    }
    let index = faces.len();
    faces.push(FaceRef { font });
    index
}

fn fallback_faces(
    collection: &mut Collection,
    faces: &mut Vec<FaceRef>,
    configured_family: &str,
) -> Vec<usize> {
    let mut order = Vec::new();
    for name in TERMINAL_FONT_FALLBACKS
        .iter()
        .chain(SYMBOL_FONT_FALLBACKS.iter())
    {
        if !configured_family.eq_ignore_ascii_case(name) {
            if let Some(font) =
                query_named_face(collection, name, FontWeight::NORMAL, FontStyle::Normal)
            {
                push_ordered_face(faces, &mut order, font);
            }
        }
    }
    for generic in [
        GenericFamily::Monospace,
        GenericFamily::SansSerif,
        GenericFamily::Emoji,
    ] {
        if let Some(font) =
            query_generic_face(collection, generic, FontWeight::NORMAL, FontStyle::Normal)
        {
            push_ordered_face(faces, &mut order, font);
        }
    }

    push_all_family_faces(collection, faces, &mut order);
    order
}

fn push_all_family_faces(
    collection: &mut Collection,
    faces: &mut Vec<FaceRef>,
    order: &mut Vec<usize>,
) {
    let names = collection
        .family_names()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for name in names {
        let Some(fonts) = collection
            .family_id(&name)
            .and_then(|family| collection.family(family))
            .map(|family| family.fonts().to_vec())
        else {
            continue;
        };
        for font in fonts {
            push_ordered_face(faces, order, font);
        }
    }
}

fn push_ordered_face(faces: &mut Vec<FaceRef>, order: &mut Vec<usize>, font: FontInfo) {
    let index = push_face(faces, font);
    push_unique(order, index);
}

fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn metrics_for_face(
    face: &FontInfo,
    source_cache: &mut SourceCache,
    size_px: f32,
    line_height: f32,
) -> Option<FontMetrics> {
    let data = face.load(Some(source_cache))?;
    let font = FontRef::from_index(data.as_ref(), face.index() as usize)?;
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
}

fn font_is_monospace(face: &FontInfo, source_cache: &mut SourceCache) -> bool {
    let Some(data) = face.load(Some(source_cache)) else {
        return false;
    };
    let Some(font) = FontRef::from_index(data.as_ref(), face.index() as usize) else {
        return false;
    };
    font.metrics(&[]).is_monospace
}

fn is_generic_monospace(family: &str) -> bool {
    let family = family.trim();
    family.is_empty()
        || family.eq_ignore_ascii_case("monospace")
        || family.eq_ignore_ascii_case("mono")
}

fn available_terminal_font_families_from_collection(
    collection: &mut Collection,
    source_cache: &mut SourceCache,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut families = Vec::new();
    push_family(&mut families, &mut seen, "monospace");

    for name in TERMINAL_FONT_FALLBACKS {
        if query_named_face(collection, name, FontWeight::NORMAL, FontStyle::Normal).is_some() {
            push_family(&mut families, &mut seen, name);
        }
    }

    let names = collection
        .family_names()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut installed = names
        .into_iter()
        .filter(|name| {
            query_named_face(collection, name, FontWeight::NORMAL, FontStyle::Normal)
                .is_some_and(|font| font_is_monospace(&font, source_cache))
        })
        .collect::<Vec<_>>();
    installed.sort_by_key(|name| name.to_ascii_lowercase());
    for name in &installed {
        push_family(&mut families, &mut seen, name);
    }

    families
}

fn push_family(families: &mut Vec<String>, seen: &mut HashSet<String>, family: &str) {
    let family = family.trim();
    if family.is_empty() {
        return;
    }
    if seen.insert(family.to_ascii_lowercase()) {
        families.push(family.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ascii_to_real_glyph() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2).expect("load fonts");

        let glyph = fonts
            .resolve_glyph(REGULAR, '>')
            .expect("monospace font should cover ASCII");

        assert_ne!(glyph.glyph_id, 0);
        let raster = fonts.rasterize(glyph).expect("glyph should rasterize");
        assert!(raster.width > 0 && raster.height > 0);
    }

    #[test]
    fn resolved_glyphs_never_return_notdef() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2).expect("load fonts");

        for ch in ['>', '\u{276f}', '\u{e0b0}', '\u{1f680}', '\u{10fffd}'] {
            if let Some(glyph) = fonts.resolve_glyph(REGULAR, ch) {
                assert_ne!(glyph.glyph_id, 0, "resolved {ch:?} to .notdef");
            }
        }
    }

    #[test]
    fn starship_symbols_resolve_when_installed() {
        let mut fonts = FontSet::new("monospace", 14.0, 1.2).expect("load fonts");

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

    #[test]
    fn terminal_font_choices_always_include_generic_monospace_first() {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let mut source_cache = SourceCache::default();
        let families =
            available_terminal_font_families_from_collection(&mut collection, &mut source_cache);

        assert_eq!(families.first().map(String::as_str), Some("monospace"));
    }

    #[test]
    fn terminal_font_choices_are_case_insensitively_deduplicated() {
        let mut families = Vec::new();
        let mut seen = HashSet::new();
        push_family(&mut families, &mut seen, "Hack");
        push_family(&mut families, &mut seen, "hack");
        push_family(&mut families, &mut seen, "HACK");

        assert_eq!(families, ["Hack"]);
    }

    #[test]
    fn fallback_enumeration_keeps_every_face_in_a_family() {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        collection.register_fonts(epaint_default_fonts::HACK_REGULAR.to_vec().into(), None);
        collection.register_fonts(epaint_default_fonts::HACK_REGULAR.to_vec().into(), None);

        let hack_face_count = collection
            .family_by_name("Hack")
            .expect("embedded Hack family")
            .fonts()
            .len();
        let mut faces = Vec::new();
        let mut order = Vec::new();
        push_all_family_faces(&mut collection, &mut faces, &mut order);

        assert_eq!(hack_face_count, 2);
        assert_eq!(faces.len(), hack_face_count);
        assert_eq!(order.len(), hack_face_count);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_without_system_fonts_gets_embedded_monospace_fallback() {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let mut source_cache = SourceCache::default();
        ensure_linux_monospace(&mut collection, &mut source_cache);

        let font = query_generic_face(
            &mut collection,
            GenericFamily::Monospace,
            FontWeight::NORMAL,
            FontStyle::Normal,
        )
        .expect("embedded Linux fallback should resolve");

        assert!(font_is_monospace(&font, &mut source_cache));
        assert!(collection.family_id("Hack").is_some());
    }
}
