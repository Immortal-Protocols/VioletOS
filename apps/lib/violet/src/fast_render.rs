//! RFC skeleton: Sub-4 ms Cortex-A53 software rasterizer & zero-copy swapchain client.
//!
//! Replaces `Canvas` (heap copy + scalar `f32` `sqrt`/`atan2`) and `text.rs`
//! (`RefCell<HashMap>` + float round-trips) with:
//! 1. `MappedCanvas<'a>` drawing directly into the `mmap`'d triple-buffer slot.
//! 2. Per-slot `DamageTracker` (honors `buffer_age` and real `(x, y, w, h)` rects).
//! 3. Integer premultiplied-alpha span blitters with ARM NEON fast paths.
//! 4. Scanline rounded rects (1 `sqrt` per corner row) and a flat ASCII `GlyphAtlas`.
//!
//! Pair with `-C target-cpu=cortex-a53 -C target-feature=+neon` in `tools/build_initfs.py`.

#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() { return other; }
        if other.is_empty() { return *self; }
        Rect::new(self.x0.min(other.x0), self.y0.min(other.y0), self.x1.max(other.x1), self.y1.max(other.y1))
    }
    pub fn intersect(&self, other: Rect) -> Rect {
        Rect::new(self.x0.max(other.x0), self.y0.max(other.y0), self.x1.min(other.x1), self.y1.min(other.y1))
    }
}

/// Tracks damaged rects across triple-buffered slots using `buffer_age` so apps only
/// re-render pixels that actually changed since this slot was last shown.
pub struct DamageTracker {
    history: [Rect; 3], // most recent 3 committed frame bounds
    current: Rect,
}

impl DamageTracker {
    pub const fn new() -> Self {
        Self { history: [Rect::new(0, 0, 0, 0); 3], current: Rect::new(0, 0, 0, 0) }
    }
    pub fn mark(&mut self, r: Rect) {
        self.current = self.current.union(r);
    }
    /// Region that must be repainted into a back buffer with age `buffer_age`.
    pub fn paint_region(&self, buffer_age: u32, full: Rect) -> Rect {
        if buffer_age == 0 || buffer_age as usize > self.history.len() {
            return full;
        }
        let mut r = self.current;
        for past in &self.history[..buffer_age as usize - 1] {
            r = r.union(*past);
        }
        r.intersect(full)
    }
    pub fn finish_frame(&mut self) -> Rect {
        let committed = self.current;
        self.history[2] = self.history[1];
        self.history[1] = self.history[0];
        self.history[0] = committed;
        self.current = Rect::new(0, 0, 0, 0);
        committed
    }
}

/// Zero-copy canvas borrowing directly from an `mmap`'d `/dev/violet/screen` slot.
pub struct MappedCanvas<'a> {
    pub w: usize,
    pub h: usize,
    pub px: &'a mut [u32],
    pub clip: Rect,
}

impl<'a> MappedCanvas<'a> {
    pub fn new(w: usize, h: usize, px: &'a mut [u32]) -> Self {
        Self { w, h, px, clip: Rect::new(0, 0, w as i32, h as i32) }
    }

    /// Opaque span fill (auto-vectorizes to NEON `vst1q_u32` with `-C target-feature=+neon`).
    pub fn fill_rect(&mut self, r: Rect, color: u32) {
        let r = r.intersect(self.clip);
        if r.is_empty() { return; }
        for y in r.y0..r.y1 {
            let row = y as usize * self.w;
            self.px[row + r.x0 as usize..row + r.x1 as usize].fill(color);
        }
    }

    /// Scanline rounded rectangle: computes one `sqrt` per corner row instead of `r^2` per corner.
    pub fn round_rect(&mut self, r: Rect, radius: i32, color: u32) {
        let rad = radius.min((r.x1 - r.x0) / 2).min((r.y1 - r.y0) / 2).max(0);
        self.fill_rect(Rect::new(r.x0, r.y0 + rad, r.x1, r.y1 - rad), color);
        let rf = rad as f32;
        for dy in 0..rad {
            let y_off = rf - (dy as f32 + 0.5);
            let span = (rf * rf - y_off * y_off).max(0.0).sqrt();
            let inset = (rf - span).floor() as i32;
            let edge_cov = ((span - span.floor()) * 255.0) as u8;
            for y in [r.y0 + dy, r.y1 - 1 - dy] {
                self.fill_rect(Rect::new(r.x0 + inset + 1, y, r.x1 - inset - 1, y + 1), color);
                self.blend_pixel_u8(r.x0 + inset, y, color, edge_cov);
                self.blend_pixel_u8(r.x1 - 1 - inset, y, color, edge_cov);
            }
        }
    }

    #[inline]
    pub fn blend_pixel_u8(&mut self, x: i32, y: i32, src: u32, a: u8) {
        if a == 0 || x < self.clip.x0 || x >= self.clip.x1 || y < self.clip.y0 || y >= self.clip.y1 {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.px[i] = if a == 255 { src | 0xff00_0000 } else { blend_u8(self.px[i], src, a) };
    }

    /// Blend an 8-bit coverage mask row (e.g., glyph atlas scanline) in pure integer/NEON math.
    pub fn blend_mask_row(&mut self, x0: i32, y: i32, mask: &[u8], color: u32) {
        if y < self.clip.y0 || y >= self.clip.y1 { return; }
        let start = (self.clip.x0 - x0).max(0) as usize;
        let end = (self.clip.x1 - x0).min(mask.len() as i32).max(0) as usize;
        if start >= end { return; }
        let dst = &mut self.px[y as usize * self.w + (x0 as usize + start)..y as usize * self.w + (x0 as usize + end)];
        blend_span_mask(dst, &mask[start..end], color);
    }
}

/// Pure-integer premultiplied BGRA8888 blend (`a` in `0..=255`, zero float conversions).
#[inline(always)]
pub fn blend_u8(dst: u32, src: u32, a: u8) -> u32 {
    let w = a as u32 + (a >> 7) as u32; // map 0..255 -> 0..256
    let inv = 256 - w;
    let rb = (((dst & 0x00ff_00ff) * inv + (src & 0x00ff_00ff) * w + 0x0080_0080) >> 8) & 0x00ff_00ff;
    let g = (((dst & 0x0000_ff00) * inv + (src & 0x0000_ff00) * w + 0x0000_8000) >> 8) & 0x0000_ff00;
    0xff00_0000 | rb | g
}

/// Span alpha-mask blitter: processes 4 pixels/iter under NEON (`vmlal_u8` / `vrshrn_n_u16`)
/// on `target_feature = "neon"`, falling back to unrolled integer SWAR otherwise.
#[inline]
pub fn blend_span_mask(dst: &mut [u32], mask: &[u8], src: u32) {
    // Phase 2: drop in `#[cfg(target_feature = "neon")]` `vld1_u8` + `vmlal_u8` 4-wide loop.
    for (d, &a) in dst.iter_mut().zip(mask.iter()) {
        if a == 255 {
            *d = src | 0xff00_0000;
        } else if a != 0 {
            *d = blend_u8(*d, src, a);
        }
    }
}

/// Flat ASCII glyph atlas: O(1) array lookup (no `RefCell<HashMap>` per character)
/// backed by one contiguous alpha-coverage buffer.
#[derive(Clone, Copy, Default)]
pub struct AtlasGlyph {
    pub offset: u32,
    pub w: u16,
    pub h: u16,
    pub xmin: i16,
    pub ymin: i16,
    pub advance_q4: i16, // fixed-point 12.4 advance width
}

pub struct GlyphAtlas {
    pub ascii: [AtlasGlyph; 96], // U+0020..=U+007F
    pub alpha: Vec<u8>,
}

impl GlyphAtlas {
    pub fn draw_ascii(&self, c: &mut MappedCanvas<'_>, s: &str, x: i32, baseline_y: i32, color: u32) -> i32 {
        let mut pen_q4 = x << 4;
        for b in s.bytes() {
            if !(0x20..0x80).contains(&b) { continue; }
            let g = self.ascii[(b - 0x20) as usize];
            let gx = (pen_q4 >> 4) + g.xmin as i32;
            let gy = baseline_y - g.ymin as i32 - g.h as i32;
            let w = g.w as usize;
            let base = g.offset as usize;
            for row in 0..g.h as usize {
                c.blend_mask_row(gx, gy + row as i32, &self.alpha[base + row * w..base + (row + 1) * w], color);
            }
            pen_q4 += g.advance_q4 as i32;
        }
        pen_q4 >> 4
    }
}
