//! A 720x1600 canvas of 0xAARRGGBB pixels (the screen's BGRA8888 in memory).
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Blend two colours: t = 0 gives a, 1 gives b. Integer arithmetic: this
/// runs for every anti-aliased pixel.
#[inline]
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let w = (t.clamp(0.0, 1.0) * 256.0) as u32;
    let v = 256 - w;
    // Red and blue together, then green.
    let rb = ((a & 0x00ff_00ff) * v + (b & 0x00ff_00ff) * w + 0x0080_0080) >> 8 & 0x00ff_00ff;
    let g = ((a & 0x0000_ff00) * v + (b & 0x0000_ff00) * w + 0x0000_8000) >> 8 & 0x0000_ff00;
    0xff00_0000 | rb | g
}

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
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Color>,
}

fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![rgb(0, 0, 0); w * h] }
    }
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }
    pub fn get(&self, x: i32, y: i32) -> Color {
        self.px[y as usize * self.w + x as usize]
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let r = r.intersect(self.bounds());
        if r.is_empty() {
            return;
        }
        for y in r.y0..r.y1 {
            let row = y as usize * self.w;
            self.px[row + r.x0 as usize..row + r.x1 as usize].fill(c);
        }
    }

    /// Draw c over a pixel with coverage a (0..1).
    pub fn blend(&mut self, x: i32, y: i32, c: Color, a: f32) {
        if a <= 0.0 || x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.px[i] = if a >= 1.0 { c | 0xff00_0000 } else { mix(self.px[i], c, a) };
    }

    /// A vertical gradient from top to bottom.
    pub fn gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let r = r.intersect(self.bounds());
        for y in r.y0..r.y1 {
            let t = (y - r.y0) as f32 / (r.height() - 1).max(1) as f32;
            self.fill(Rect::new(r.x0, y, r.x1, y + 1), mix(top, bottom, t));
        }
    }

    /// A rectangle with anti-aliased rounded corners, drawn with opacity alpha.
    pub fn round_rect_alpha(&mut self, r: Rect, radius: f32, c: Color, alpha: f32) {
        let ri = (radius.ceil() as i32).min(r.width() / 2).min(r.height() / 2).max(0);
        let radius = radius.min(ri as f32);
        let solid = |s: &mut Canvas, q: Rect| {
            if alpha >= 1.0 {
                s.fill(q, c)
            } else {
                let q = q.intersect(s.bounds());
                for y in q.y0..q.y1 {
                    for x in q.x0..q.x1 {
                        s.blend(x, y, c, alpha);
                    }
                }
            }
        };
        solid(self, Rect::new(r.x0 + ri, r.y0, r.x1 - ri, r.y1));
        solid(self, Rect::new(r.x0, r.y0 + ri, r.x0 + ri, r.y1 - ri));
        solid(self, Rect::new(r.x1 - ri, r.y0 + ri, r.x1, r.y1 - ri));
        // The four corners.
        for (cx, cy, xs, ys) in [
            (r.x0 as f32 + radius, r.y0 as f32 + radius, r.x0, r.y0),
            (r.x1 as f32 - radius, r.y0 as f32 + radius, r.x1 - ri, r.y0),
            (r.x0 as f32 + radius, r.y1 as f32 - radius, r.x0, r.y1 - ri),
            (r.x1 as f32 - radius, r.y1 as f32 - radius, r.x1 - ri, r.y1 - ri),
        ] {
            for y in ys..ys + ri {
                for x in xs..xs + ri {
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() - radius;
                    self.blend(x, y, c, alpha * coverage(d));
                }
            }
        }
    }

    pub fn round_rect(&mut self, r: Rect, radius: f32, c: Color) {
        self.round_rect_alpha(r, radius, c, 1.0)
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        for y in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
            for x in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() - r;
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
        for y in (cy - e) as i32..=(cy + e) as i32 {
            for x in (cx - e) as i32..=(cx + e) as i32 {
                let (fx, fy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let a = (-fy).atan2(fx);
                if a < from || a > to {
                    continue;
                }
                let d = ((fx * fx + fy * fy).sqrt() - r).abs() - width / 2.0;
                self.blend(x, y, c, coverage(d));
            }
        }
    }

    /// A segment with round ends.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Color) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let l2 = dx * dx + dy * dy;
        for y in (y0.min(y1) - width) as i32..=(y0.max(y1) + width) as i32 {
            for x in (x0.min(x1) - width) as i32..=(x0.max(x1) + width) as i32 {
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
        for y in miny as i32 - 1..=maxy as i32 + 1 {
            for x in minx as i32 - 1..=maxx as i32 + 1 {
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

    /// Copy rows y0..y1 from another canvas of the same size.
    pub fn copy_rows(&mut self, from: &Canvas, y0: i32, y1: i32) {
        let (a, b) = (y0.max(0) as usize * self.w, y1.min(self.h as i32) as usize * self.w);
        self.px[a..b].copy_from_slice(&from.px[a..b]);
    }
}
