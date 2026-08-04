//! Glyph atlas: an RGBA texture packed with rasterized glyphs, plus a cache
//! keyed by `(resolved face, glyph id)`. Packing uses a simple shelf allocator,
//! which is a good fit for the near-uniform heights of monospace glyphs.

use std::collections::HashMap;

use crate::font::RasterGlyph;

/// Shelf (row) allocator over a fixed `width` x `height` area. Glyphs are placed
/// left-to-right on the current shelf; when one doesn't fit, a new shelf opens
/// above the tallest glyph so far.
#[derive(Debug)]
pub struct ShelfPacker {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    shelf_height: u32,
}

impl ShelfPacker {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            x: 0,
            y: 0,
            shelf_height: 0,
        }
    }

    /// Allocate a `w` x `h` rectangle, returning its top-left origin, or `None`
    /// if it does not fit. A 1px gutter is left to avoid bilinear bleed.
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.width || h > self.height {
            return None;
        }
        // Compute the candidate position without committing, so a failed
        // allocation doesn't abandon the remaining width of the open shelf.
        let (mut x, mut y, mut shelf_height) = (self.x, self.y, self.shelf_height);
        if x + w > self.width {
            // Advance to a new shelf.
            x = 0;
            y += shelf_height + 1;
            shelf_height = 0;
        }
        if y + h > self.height {
            return None;
        }
        self.x = x + w + 1;
        self.y = y;
        self.shelf_height = shelf_height.max(h);
        Some((x, y))
    }
}

/// Cache key: which resolved face the glyph came from, and its glyph id within
/// that face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: u32,
    pub glyph_id: u16,
}

/// A placed glyph: atlas UVs plus bitmap placement relative to the pen origin.
#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Bitmap offset from the pen origin (swash placement).
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub is_color: bool,
    /// True for whitespace / unplaceable glyphs that contribute no quad.
    pub empty: bool,
}

impl GlyphEntry {
    fn empty(left: i32, top: i32, is_color: bool) -> Self {
        Self {
            uv_min: [0.0, 0.0],
            uv_max: [0.0, 0.0],
            left,
            top,
            width: 0,
            height: 0,
            is_color,
            empty: true,
        }
    }
}

/// CPU-side atlas state: packing, cache, and the deferred-eviction flag. Kept
/// separate from the GPU texture so placement policy is unit-testable.
struct AtlasIndex {
    size: u32,
    packer: ShelfPacker,
    cache: HashMap<GlyphKey, GlyphEntry>,
    /// Set when an allocation failed because the atlas is full. The
    /// evict-and-repack is deferred to the next `begin_frame`: glyph instances
    /// already emitted this frame hold UVs into the current packing, so
    /// repacking mid-frame would have them sample the wrong texels.
    pending_reset: bool,
}

impl AtlasIndex {
    fn new(size: u32) -> Self {
        Self {
            size,
            packer: ShelfPacker::new(size, size),
            cache: HashMap::new(),
            pending_reset: false,
        }
    }

    /// Apply a deferred evict-and-repack. Must run before any glyph is placed
    /// this frame so no emitted instance ever references the old packing.
    fn begin_frame(&mut self) {
        if self.pending_reset {
            self.pending_reset = false;
            self.packer = ShelfPacker::new(self.size, self.size);
            self.cache.clear();
        }
    }

    /// Decide placement for a glyph. Returns the entry to hand back and, when
    /// `Some`, the atlas origin the caller must upload the bitmap to.
    fn place(&mut self, key: GlyphKey, glyph: &RasterGlyph) -> (GlyphEntry, Option<(u32, u32)>) {
        if let Some(entry) = self.cache.get(&key) {
            return (*entry, None);
        }
        if glyph.width == 0 || glyph.height == 0 {
            // Whitespace / blank glyph: cache it, contributes no quad.
            let entry = GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color);
            self.cache.insert(key, entry);
            return (entry, None);
        }
        if let Some((x, y)) = self.packer.alloc(glyph.width, glyph.height) {
            let s = self.size as f32;
            let entry = GlyphEntry {
                uv_min: [x as f32 / s, y as f32 / s],
                uv_max: [(x + glyph.width) as f32 / s, (y + glyph.height) as f32 / s],
                left: glyph.left,
                top: glyph.top,
                width: glyph.width,
                height: glyph.height,
                is_color: glyph.is_color,
                empty: false,
            };
            self.cache.insert(key, entry);
            return (entry, Some((x, y)));
        }
        if glyph.width > self.size || glyph.height > self.size {
            // Can never fit any packing: cache the blank so the glyph isn't
            // re-rasterized (and the atlas isn't repacked) every frame.
            let entry = GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color);
            self.cache.insert(key, entry);
            return (entry, None);
        }
        // Atlas full: schedule the evict-and-repack for the next frame
        // boundary, and do NOT cache the failure — the glyph renders blank
        // for this one frame and is retried once the repack frees space.
        if !self.pending_reset {
            self.pending_reset = true;
            eprintln!(
                "glyph atlas full ({0}x{0}); evicting all cached glyphs next frame",
                self.size
            );
        }
        (
            GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color),
            None,
        )
    }
}

pub struct GlyphAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    index: AtlasIndex,
}

impl GlyphAtlas {
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("phantom-glyph-atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB: alpha (coverage) stays linear; colour-emoji bytes are sRGB
            // and get linearised on sample, matching the sRGB surface.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            index: AtlasIndex::new(size),
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn get(&self, key: GlyphKey) -> Option<GlyphEntry> {
        self.index.cache.get(&key).copied()
    }

    /// Apply any eviction deferred from an overflow last frame. Call at the
    /// start of each frame, before any glyph is emitted.
    pub fn begin_frame(&mut self) {
        self.index.begin_frame();
    }

    /// Cache a rasterization failure as an empty entry so it isn't retried —
    /// a retry re-maps and re-parses the font file every frame for every
    /// visible occurrence of the glyph.
    pub fn insert_failed(&mut self, key: GlyphKey) -> GlyphEntry {
        let entry = GlyphEntry::empty(0, 0, false);
        self.index.cache.insert(key, entry);
        entry
    }

    /// Upload a rasterized glyph and cache its placement. Idempotent per key.
    /// When the atlas is full, the glyph renders blank for one frame (the
    /// failure is not cached) and every cached glyph is evicted at the next
    /// frame boundary, so live glyphs re-pack on demand instead of newly seen
    /// characters rendering as permanently invisible cells.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        glyph: &RasterGlyph,
    ) -> GlyphEntry {
        let (entry, upload) = self.index.place(key, glyph);
        if let Some((x, y)) = upload {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &glyph.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(glyph.width * 4),
                    rows_per_image: Some(glyph.height),
                },
                wgpu::Extent3d {
                    width: glyph.width,
                    height: glyph.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_advances_left_to_right_with_gutter() {
        let mut p = ShelfPacker::new(100, 100);
        assert_eq!(p.alloc(10, 20), Some((0, 0)));
        assert_eq!(p.alloc(10, 20), Some((11, 0))); // +1 gutter
    }

    #[test]
    fn alloc_wraps_to_new_shelf() {
        let mut p = ShelfPacker::new(20, 100);
        assert_eq!(p.alloc(15, 10), Some((0, 0)));
        // 15 + 1 + 15 > 20, so the next glyph starts a new shelf below.
        assert_eq!(p.alloc(15, 8), Some((0, 11)));
    }

    #[test]
    fn alloc_returns_none_when_exhausted() {
        let mut p = ShelfPacker::new(20, 20);
        assert!(p.alloc(20, 20).is_some());
        assert!(p.alloc(1, 1).is_none()); // no vertical room left
    }

    #[test]
    fn alloc_rejects_oversized() {
        let mut p = ShelfPacker::new(16, 16);
        assert!(p.alloc(17, 4).is_none());
        assert!(p.alloc(4, 17).is_none());
    }

    #[test]
    fn failed_alloc_keeps_open_shelf_usable() {
        let mut p = ShelfPacker::new(20, 25);
        assert_eq!(p.alloc(10, 20), Some((0, 0)));
        // Doesn't fit horizontally, and the new shelf fails the height check:
        // the open shelf must not be abandoned.
        assert!(p.alloc(15, 20).is_none());
        assert_eq!(p.alloc(8, 20), Some((11, 0)));
    }

    fn raster(w: u32, h: u32) -> RasterGlyph {
        RasterGlyph {
            width: w,
            height: h,
            left: 0,
            top: 0,
            is_color: false,
            data: Vec::new(),
        }
    }

    fn key(glyph_id: u16) -> GlyphKey {
        GlyphKey { face: 0, glyph_id }
    }

    #[test]
    fn full_atlas_failure_is_transient_and_succeeds_after_repack() {
        let mut idx = AtlasIndex::new(32);
        let (entry, upload) = idx.place(key(1), &raster(32, 32));
        assert!(!entry.empty && upload.is_some());
        // No space left: blank entry, no upload, and crucially not cached —
        // a cached blank would never be retried.
        let (entry, upload) = idx.place(key(2), &raster(8, 8));
        assert!(entry.empty && upload.is_none());
        assert!(!idx.cache.contains_key(&key(2)));
        // The repack is deferred: the current packing (and its cached UVs)
        // stays valid for the rest of this frame.
        assert!(idx.pending_reset);
        assert!(idx.cache.contains_key(&key(1)));
        // Next frame the repack frees space and the same glyph places.
        idx.begin_frame();
        assert!(!idx.pending_reset);
        assert!(idx.cache.is_empty());
        let (entry, upload) = idx.place(key(2), &raster(8, 8));
        assert!(!entry.empty && upload.is_some());
    }

    #[test]
    fn oversized_glyph_is_cached_blank_without_scheduling_eviction() {
        let mut idx = AtlasIndex::new(32);
        // Wider than the whole atlas: no packing can ever hold it, so the
        // blank is cached and no futile repack is scheduled.
        let (entry, upload) = idx.place(key(1), &raster(64, 4));
        assert!(entry.empty && upload.is_none());
        assert!(idx.cache.contains_key(&key(1)));
        assert!(!idx.pending_reset);
    }
}
