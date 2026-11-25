//! A 720x1600 canvas of 0xAARRGGBB pixels (the screen's BGRA8888 in memory).
//!
//! Drawing is clipped to `clip` (the whole canvas unless set), so an app can
//! redraw only what changed: set the clip to the damaged rectangle, draw the
//! scene as usual, and present that rectangle. Every primitive skips work
//! outside the clip, and anti-aliasing is computed per row: shapes fill their
//! solid spans directly and evaluate the exact edge coverage only for the few
//! pixels on their boundary (see docs/8ms-frame-pipeline-rfc.md).
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Blend two colours: t = 0 gives a, 1 gives b.
#[inline]
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    mix_w(a, b, (t.clamp(0.0, 1.0) * 256.0) as u32)
}

/// Blend with an integer weight w in 0..=256 (256 is all b). Red and blue
/// together, then green: two multiplies per channel pair, no floats.
#[inline(always)]
pub fn mix_w(a: Color, b: Color, w: u32) -> Color {
    let v = 256 - w;
    let rb = ((a & 0x00ff_00ff) * v + (b & 0x00ff_00ff) * w + 0x0080_0080) >> 8 & 0x00ff_00ff;
    let g = ((a & 0x0000_ff00) * v + (b & 0x0000_ff00) * w + 0x0000_8000) >> 8 & 0x0000_ff00;
    0xff00_0000 | rb | g
}

/// The weight `mix` uses for an 8-bit coverage a (exactly (a / 255 * 256) as u32).
const fn coverage_weights() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut a = 0;
    while a < 256 {
        // (a * 256) / 255 rounded down, which is what the float path produces.
        t[a] = ((a * 256) / 255) as u16;
        a += 1;
    }
    t
}
static WEIGHT: [u16; 256] = coverage_weights();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }
    pub fn width(&self) -> i32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> i32 {
        self.y1 - self.y0
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x0 + d, self.y0 + d, self.x1 - d, self.y1 - d)
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
    pub fn intersect(&self, o: Rect) -> Rect {
        Rect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }
    /// The smallest rectangle holding both (an empty one adds nothing).
    pub fn union(&self, o: Rect) -> Rect {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }
    /// Whether the two overlap.
    pub fn touches(&self, o: Rect) -> bool {
        !self.intersect(o).is_empty()
    }
}

/// A canvas's pixels: its own memory, or memory it borrows (the app's mapped
/// screen surface, so drawing needs no copy before presenting).
pub struct Pixels {
    ptr: *mut Color,
    len: usize,
    _owned: Option<Vec<Color>>,
}

// The pointer is either into `_owned` or into a mapping that lives as long as
// the process; a canvas moves between threads like the Vec it replaced.
unsafe impl Send for Pixels {}

impl std::ops::Deref for Pixels {
    type Target = [Color];
    fn deref(&self) -> &[Color] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
// `for p in &canvas.px`, as with the Vec it replaced.
impl<'a> IntoIterator for &'a Pixels {
    type Item = &'a Color;
    type IntoIter = std::slice::Iter<'a, Color>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl std::ops::DerefMut for Pixels {
    fn deref_mut(&mut self) -> &mut [Color] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Pixels,
    clip: Rect,
}

fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Rows [y0, y1) of the pixel centres within `half` of `c` (empty when half < 0).
fn span(c: f32, half: f32) -> (i32, i32) {
    if !(half >= 0.0) {
        return (0, 0);
    }
    // Pixel x has its centre at x + 0.5: |x + 0.5 - c| <= half.
    ((c - half - 0.5).ceil() as i32, (c + half - 0.5).floor() as i32 + 1)
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        let mut v = vec![rgb(0, 0, 0); w * h];
        let ptr = v.as_mut_ptr();
        Canvas { w, h, px: Pixels { ptr, len: w * h, _owned: Some(v) }, clip: Rect::new(0, 0, w as i32, h as i32) }
    }

    /// A canvas drawing straight into `w * h` pixels at `ptr`.
    ///
    /// # Safety
    /// The memory must stay mapped and writable for the canvas's lifetime.
    pub unsafe fn from_raw(ptr: *mut Color, w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: Pixels { ptr, len: w * h, _owned: None }, clip: Rect::new(0, 0, w as i32, h as i32) }
    }

    pub(crate) fn pixels_ptr(&self) -> *const Color {
        self.px.ptr
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }
    /// Where drawing lands; everything outside is left alone.
    pub fn clip(&self) -> Rect {
        self.clip
    }
    pub fn set_clip(&mut self, r: Rect) {
        self.clip = r.intersect(self.bounds());
    }
    pub fn reset_clip(&mut self) {
        self.clip = self.bounds();
    }
    pub fn get(&self, x: i32, y: i32) -> Color {
        self.px[y as usize * self.w + x as usize]
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let r = r.intersect(self.clip);
        if r.is_empty() {
            return;
        }
        let (w, x0, x1) = (self.w, r.x0 as usize, r.x1 as usize);
        for y in r.y0..r.y1 {
            let row = y as usize * w;
            self.px[row + x0..row + x1].fill(c);
        }
    }

    /// Draw c over a pixel with coverage a (0..1).
    pub fn blend(&mut self, x: i32, y: i32, c: Color, a: f32) {
        if a <= 0.0 {
            return;
        }
        if a >= 1.0 {
            self.blend_w(x, y, c, 256)
        } else {
            self.blend_w(x, y, c, (a * 256.0) as u32)
        }
    }

    /// Draw c over a pixel with 8-bit coverage (255 is opaque).
    #[inline]
    pub fn blend8(&mut self, x: i32, y: i32, c: Color, a: u8) {
        if a != 0 {
            self.blend_w(x, y, c, if a == 255 { 256 } else { WEIGHT[a as usize] as u32 });
        }
    }

    /// Draw c over a pixel with weight w in 0..=256 (256 replaces it).
    #[inline(always)]
    fn blend_w(&mut self, x: i32, y: i32, c: Color, w: u32) {
        if !self.clip.contains(x, y) {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.px[i] = if w >= 256 { c | 0xff00_0000 } else { mix_w(self.px[i], c, w) };
    }

    /// One row of 8-bit coverage `mask`, its first value at (x, y), in colour c.
    pub fn blend_row(&mut self, x: i32, y: i32, mask: &[u8], c: Color) {
        if y < self.clip.y0 || y >= self.clip.y1 {
            return;
        }
        let from = (self.clip.x0 - x).max(0) as usize;
        let to = ((self.clip.x1 - x).max(0) as usize).min(mask.len());
        if from >= to {
            return;
        }
        let row = y as usize * self.w + (x + from as i32) as usize;
        let dst = &mut self.px[row..row + (to - from)];
        let solid = c | 0xff00_0000;
        for (d, &a) in dst.iter_mut().zip(&mask[from..to]) {
            match a {
                0 => {}
                255 => *d = solid,
                a => *d = mix_w(*d, c, WEIGHT[a as usize] as u32),
            }
        }
    }

    /// A vertical gradient from top to bottom.
    pub fn gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let r = r.intersect(self.bounds());
        let visible = r.intersect(self.clip);
        for y in visible.y0..visible.y1 {
            let t = (y - r.y0) as f32 / (r.height() - 1).max(1) as f32;
            self.fill(Rect::new(r.x0, y, r.x1, y + 1), mix(top, bottom, t));
        }
    }

    /// A rectangle with anti-aliased rounded corners, drawn with opacity alpha.
    pub fn round_rect_alpha(&mut self, r: Rect, radius: f32, c: Color, alpha: f32) {
        if !r.touches(self.clip) || alpha <= 0.0 {
            return;
        }
        let ri = (radius.ceil() as i32).min(r.width() / 2).min(r.height() / 2).max(0);
        let radius = radius.min(ri as f32);
        let solid = |s: &mut Canvas, q: Rect| {
            if alpha >= 1.0 {
                s.fill(q, c)
            } else {
                let w = (alpha * 256.0) as u32;
                let q = q.intersect(s.clip);
                for y in q.y0..q.y1 {
                    for x in q.x0..q.x1 {
                        s.blend_w(x, y, c, w);
                    }
                }
            }
        };
        solid(self, Rect::new(r.x0 + ri, r.y0, r.x1 - ri, r.y1));
        solid(self, Rect::new(r.x0, r.y0 + ri, r.x0 + ri, r.y1 - ri));
        solid(self, Rect::new(r.x1 - ri, r.y0 + ri, r.x1, r.y1 - ri));
        // The four corners: per row, the pixels well inside the arc are solid
        // and only the edge pixels need their distance to the centre.
        for (cx, cy, xs, ys) in [
            (r.x0 as f32 + radius, r.y0 as f32 + radius, r.x0, r.y0),
            (r.x1 as f32 - radius, r.y0 as f32 + radius, r.x1 - ri, r.y0),
            (r.x0 as f32 + radius, r.y1 as f32 - radius, r.x0, r.y1 - ri),
            (r.x1 as f32 - radius, r.y1 as f32 - radius, r.x1 - ri, r.y1 - ri),
        ] {
            let corner = Rect::new(xs, ys, xs + ri, ys + ri).intersect(self.clip);
            for y in corner.y0..corner.y1 {
                let dy = y as f32 + 0.5 - cy;
                let (in0, in1) = span(cx, inner_half(radius, dy));
                let (in0, in1) = (in0.clamp(corner.x0, corner.x1), in1.clamp(corner.x0, corner.x1));
                if in0 < in1 {
                    if alpha >= 1.0 {
                        self.fill(Rect::new(in0, y, in1, y + 1), c | 0xff00_0000);
                    } else {
                        let w = (alpha * 256.0) as u32;
                        for x in in0..in1 {
                            self.blend_w(x, y, c, w);
                        }
                    }
                }
                for x in (corner.x0..in0.max(corner.x0)).chain(in1.max(in0)..corner.x1) {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + dy.powi(2)).sqrt() - radius;
                    self.blend(x, y, c, alpha * coverage(d));
                }
            }
        }
    }

    pub fn round_rect(&mut self, r: Rect, radius: f32, c: Color) {
        self.round_rect_alpha(r, radius, c, 1.0)
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        let area = Rect::new((cx - r - 1.0) as i32, (cy - r - 1.0) as i32, (cx + r + 1.0) as i32 + 1, (cy + r + 1.0) as i32 + 1)
            .intersect(self.clip);
        for y in area.y0..area.y1 {
            let dy = y as f32 + 0.5 - cy;
            // Pixels whose centre is within r - 0.5 are fully covered; beyond
            // r + 0.5 not at all. Only the ones between get a square root.
            let (out0, out1) = span(cx, ((r + 0.5) * (r + 0.5) - dy * dy).max(-1.0).sqrt() + 1.0);
            let (out0, out1) = (out0.max(area.x0), out1.min(area.x1));
            if out0 >= out1 {
                continue;
            }
            let (in0, in1) = span(cx, inner_half(r, dy));
            let (in0, in1) = (in0.clamp(out0, out1), in1.clamp(out0, out1));
            if in0 < in1 {
                self.fill(Rect::new(in0, y, in1, y + 1), c | 0xff00_0000);
            }
            for x in (out0..in0.max(out0)).chain(in1.max(in0)..out1) {
                let d = ((x as f32 + 0.5 - cx).powi(2) + dy.powi(2)).sqrt() - r;
                self.blend(x, y, c, coverage(d));
            }
        }
    }

    /// A circle's outline of the given width.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: Color) {
        self.arc(cx, cy, r, width, -std::f32::consts::PI, std::f32::consts::PI, c)
    }

    /// Part of a ring between two angles (radians; 0 is right, positive is up).
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, width: f32, from: f32, to: f32, c: Color) {
        let e = r + width;
        let area = Rect::new((cx - e) as i32, (cy - e) as i32, (cx + e) as i32 + 1, (cy + e) as i32 + 1).intersect(self.clip);
        let whole = from <= -std::f32::consts::PI && to >= std::f32::consts::PI;
        // Coverage is non-zero only within width / 2 + 0.5 of the radius.
        let (outer, inner) = (r + width / 2.0 + 0.5, r - width / 2.0 - 0.5);
        for y in area.y0..area.y1 {
            let fy = y as f32 + 0.5 - cy;
            let (o0, o1) = span(cx, (outer * outer - fy * fy).max(-1.0).sqrt() + 1.0);
            let (o0, o1) = (o0.max(area.x0), o1.min(area.x1));
            if o0 >= o1 {
                continue;
            }
            // The hole in the middle, shrunk by a pixel so no edge is skipped.
            let hole = if inner > 0.0 { inner * inner - fy * fy } else { -1.0 };
            let (h0, h1) = span(cx, if hole > 0.0 { hole.sqrt() - 1.0 } else { -1.0 });
            let (h0, h1) = (h0.clamp(o0, o1), h1.clamp(o0, o1));
            for x in (o0..h0.max(o0)).chain(h1.max(h0)..o1) {
                let fx = x as f32 + 0.5 - cx;
                let d = ((fx * fx + fy * fy).sqrt() - r).abs() - width / 2.0;
                let cov = coverage(d);
                if cov <= 0.0 {
                    continue;
                }
                if !whole {
                    let a = (-fy).atan2(fx);
                    if a < from || a > to {
                        continue;
                    }
                }
                self.blend(x, y, c, cov);
            }
        }
    }

    /// A segment with round ends.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let area = Rect::new(
            (x0.min(x1) - width) as i32,
            (y0.min(y1) - width) as i32,
            (x0.max(x1) + width) as i32 + 1,
            (y0.max(y1) + width) as i32 + 1,
        )
        .intersect(self.clip);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let l2 = dx * dx + dy * dy;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if l2 > 0.0 { (((px - x0) * dx + (py - y0) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let d = ((px - x0 - t * dx).powi(2) + (py - y0 - t * dy).powi(2)).sqrt() - width / 2.0;
                self.blend(x, y, c, coverage(d));
            }
        }
    }

    /// A filled triangle, anti-aliased by 4x4 supersampling.
    pub fn triangle(&mut self, p: [[f32; 2]; 3], c: Color) {
        let edge = |a: [f32; 2], b: [f32; 2], x: f32, y: f32| (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        let (minx, maxx) = (p[0][0].min(p[1][0]).min(p[2][0]), p[0][0].max(p[1][0]).max(p[2][0]));
        let (miny, maxy) = (p[0][1].min(p[1][1]).min(p[2][1]), p[0][1].max(p[1][1]).max(p[2][1]));
        let area = Rect::new(minx as i32 - 1, miny as i32 - 1, maxx as i32 + 2, maxy as i32 + 2).intersect(self.clip);
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (fx, fy) = (x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                        let (e0, e1, e2) = (edge(p[0], p[1], fx, fy), edge(p[1], p[2], fx, fy), edge(p[2], p[0], fx, fy));
                        if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                            hits += 1;
                        }
                    }
                }
                self.blend(x, y, c, hits as f32 / 16.0);
            }
        }
    }

    /// Copy rows y0..y1 from another canvas of the same size (within the clip).
    pub fn copy_rows(&mut self, from: &Canvas, y0: i32, y1: i32) {
        let r = Rect::new(0, y0, self.w as i32, y1).intersect(self.clip);
        if r.is_empty() {
            return;
        }
        for y in r.y0..r.y1 {
            let (a, b) = (y as usize * self.w + r.x0 as usize, y as usize * self.w + r.x1 as usize);
            self.px[a..b].copy_from_slice(&from.px[a..b]);
        }
    }
}

/// Half-width, at height dy from a circle's centre, of the pixels that are
/// fully covered (coverage exactly 1): those within radius - 0.5, less a
/// margin so float rounding never promotes an edge pixel.
fn inner_half(radius: f32, dy: f32) -> f32 {
    let r = radius - 0.5 - 0.01;
    if r <= 0.0 {
        return -1.0;
    }
    let h = r * r - dy * dy;
    if h <= 0.0 { -1.0 } else { h.sqrt() - 0.01 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-pixel reference the row-based shapes must match exactly.
    fn reference_circle(c: &mut Canvas, cx: f32, cy: f32, r: f32, col: Color) {
        for y in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
            for x in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() - r;
                c.blend(x, y, col, coverage(d));
            }
        }
    }
    fn reference_arc(c: &mut Canvas, cx: f32, cy: f32, r: f32, width: f32, from: f32, to: f32, col: Color) {
        let e = r + width;
        for y in (cy - e) as i32..=(cy + e) as i32 {
            for x in (cx - e) as i32..=(cx + e) as i32 {
                let (fx, fy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let a = (-fy).atan2(fx);
                if a < from || a > to {
                    continue;
                }
                let d = ((fx * fx + fy * fy).sqrt() - r).abs() - width / 2.0;
                c.blend(x, y, col, coverage(d));
            }
        }
    }
    fn reference_round_rect(c: &mut Canvas, r: Rect, radius: f32, col: Color, alpha: f32) {
        let ri = (radius.ceil() as i32).min(r.width() / 2).min(r.height() / 2).max(0);
        let radius = radius.min(ri as f32);
        let solid = |s: &mut Canvas, q: Rect| {
            let q = q.intersect(s.bounds());
            for y in q.y0..q.y1 {
                for x in q.x0..q.x1 {
                    s.blend(x, y, col, alpha);
                }
            }
        };
        solid(c, Rect::new(r.x0 + ri, r.y0, r.x1 - ri, r.y1));
        solid(c, Rect::new(r.x0, r.y0 + ri, r.x0 + ri, r.y1 - ri));
        solid(c, Rect::new(r.x1 - ri, r.y0 + ri, r.x1, r.y1 - ri));
        for (cx, cy, xs, ys) in [
            (r.x0 as f32 + radius, r.y0 as f32 + radius, r.x0, r.y0),
            (r.x1 as f32 - radius, r.y0 as f32 + radius, r.x1 - ri, r.y0),
            (r.x0 as f32 + radius, r.y1 as f32 - radius, r.x0, r.y1 - ri),
            (r.x1 as f32 - radius, r.y1 as f32 - radius, r.x1 - ri, r.y1 - ri),
        ] {
            for y in ys..ys + ri {
                for x in xs..xs + ri {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() - radius;
                    c.blend(x, y, col, alpha * coverage(d));
                }
            }
        }
    }
    fn pair() -> (Canvas, Canvas) {
        let mut a = Canvas::new(200, 200);
        a.gradient(a.bounds(), rgb(10, 20, 30), rgb(200, 100, 50));
        let mut b = Canvas::new(200, 200);
        b.px.copy_from_slice(&a.px);
        (a, b)
    }

    #[test]
    fn circles_match_reference() {
        for &(cx, cy, r) in &[(100.0, 100.0, 66.0), (50.3, 61.7, 4.0), (120.5, 80.25, 0.7), (0.0, 0.0, 30.0), (100.0, 100.0, 5.0)] {
            let (mut a, mut b) = pair();
            a.circle(cx, cy, r, rgb(255, 200, 10));
            reference_circle(&mut b, cx, cy, r, rgb(255, 200, 10));
            assert!(a.px[..] == b.px[..], "circle {cx} {cy} {r}");
        }
    }

    #[test]
    fn arcs_match_reference() {
        use std::f32::consts::PI;
        for &(cx, cy, r, w, from, to) in &[
            (100.0, 100.0, 40.0, 5.0, PI / 4.0, 3.0 * PI / 4.0),
            (100.0, 100.0, 24.0, 5.0, -PI, PI),
            (90.5, 99.5, 3.0, 4.0, -PI, PI),
            (100.0, 100.0, 60.0, 12.0, -PI / 2.0, PI),
        ] {
            let (mut a, mut b) = pair();
            a.arc(cx, cy, r, w, from, to, rgb(5, 250, 100));
            reference_arc(&mut b, cx, cy, r, w, from, to, rgb(5, 250, 100));
            assert!(a.px[..] == b.px[..], "arc {r} {w}");
        }
    }

    #[test]
    fn round_rects_match_reference() {
        for &(r, radius, alpha) in &[
            (Rect::new(10, 20, 180, 150), 30.0, 1.0),
            (Rect::new(10, 20, 180, 150), 22.0, 0.5),
            (Rect::new(40, 40, 50, 46), 9.0, 1.0),
            (Rect::new(-10, -5, 60, 70), 38.0, 1.0),
        ] {
            let (mut a, mut b) = pair();
            a.round_rect_alpha(r, radius, rgb(90, 80, 200), alpha);
            reference_round_rect(&mut b, r, radius, rgb(90, 80, 200), alpha);
            assert!(a.px[..] == b.px[..], "round rect {r:?} {radius} {alpha}");
        }
    }

    #[test]
    fn clip_limits_drawing() {
        let (mut a, b) = pair();
        a.set_clip(Rect::new(50, 50, 60, 60));
        a.fill(a.bounds(), 0xff123456);
        a.circle(55.0, 55.0, 40.0, 0xff00ff00);
        for y in 0..200 {
            for x in 0..200 {
                let inside = (50..60).contains(&x) && (50..60).contains(&y);
                assert_eq!(a.get(x, y) == b.get(x, y), !inside, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn coverage_weights_match_float_path() {
        for a in 0..=255u8 {
            let float = (a as f32 / 255.0 * 256.0) as u32;
            assert_eq!(WEIGHT[a as usize] as u32, float, "coverage {a}");
        }
    }
}
