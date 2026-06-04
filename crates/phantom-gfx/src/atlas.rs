//! Glyph atlas: an RGBA texture packed with rasterized glyphs, plus a cache
//! keyed by `(face slot, glyph id)`. Packing uses a simple shelf allocator,
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
        if self.x + w > self.width {
            // Advance to a new shelf.
            self.x = 0;
            self.y += self.shelf_height + 1;
            self.shelf_height = 0;
        }
        if self.y + h > self.height {
            return None;
        }
        let pos = (self.x, self.y);
        self.x += w + 1;
        self.shelf_height = self.shelf_height.max(h);
        Some(pos)
    }
}

/// Cache key: which face the glyph came from, and its glyph id within that face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub slot: u8,
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

pub struct GlyphAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: u32,
    packer: ShelfPacker,
    cache: HashMap<GlyphKey, GlyphEntry>,
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
            size,
            packer: ShelfPacker::new(size, size),
            cache: HashMap::new(),
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn get(&self, key: GlyphKey) -> Option<GlyphEntry> {
        self.cache.get(&key).copied()
    }

    /// Upload a rasterized glyph and cache its placement. Idempotent per key.
    /// If the atlas is full, the glyph is cached as empty (skipped) so we don't
    /// retry every frame.
    pub fn insert(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        glyph: &RasterGlyph,
    ) -> GlyphEntry {
        if let Some(entry) = self.cache.get(&key) {
            return *entry;
        }

        let entry = if glyph.width == 0 || glyph.height == 0 {
            GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color)
        } else if let Some((x, y)) = self.packer.alloc(glyph.width, glyph.height) {
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
            let s = self.size as f32;
            GlyphEntry {
                uv_min: [x as f32 / s, y as f32 / s],
                uv_max: [(x + glyph.width) as f32 / s, (y + glyph.height) as f32 / s],
                left: glyph.left,
                top: glyph.top,
                width: glyph.width,
                height: glyph.height,
                is_color: glyph.is_color,
                empty: false,
            }
        } else {
            GlyphEntry::empty(glyph.left, glyph.top, glyph.is_color)
        };

        self.cache.insert(key, entry);
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
}
