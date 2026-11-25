//! The VioletOS look: dark layered paper with cream shapes pressed out of it.
//!
//! Shapes are built as coverage masks from signed distance functions (and
//! text), then embossed: lit from the top left, with a soft shadow below.
//! The result is a sprite an app draws as often as it likes. Slabs and wells
//! are the flat pieces: a raised card and a pressed-in field.
use crate::canvas::{mix, rgb, Canvas, Color, Rect};
use crate::text::Face;

pub const PAPER: Color = rgb(0x2b, 0x2a, 0x28);
pub const PAPER_DEEP: Color = rgb(0x1f, 0x1e, 0x1d);
pub const PAPER_RAISED: Color = rgb(0x36, 0x34, 0x31);
pub const PAPER_PRESSED: Color = rgb(0x43, 0x40, 0x3b);
pub const CREAM: Color = rgb(0xec, 0xe4, 0xd4);
pub const MUTED: Color = rgb(0xa8, 0xa1, 0x93);
pub const DIM: Color = rgb(0x6e, 0x69, 0x60);
pub const VIOLET: Color = rgb(0xb3, 0x9a, 0xf0);
pub const ALERT: Color = rgb(0xe0, 0x6c, 0x5f);
pub const GOOD: Color = rgb(0x9c, 0xc7, 0x9a);
const BLACK: Color = rgb(0, 0, 0);
const WHITE: Color = rgb(0xff, 0xff, 0xff);

/// Signed distances (negative inside) for building masks.
pub mod sdf {
    pub fn circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
        ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
    }
    /// A box of half-size (hw, hh) around (cx, cy) with rounded corners.
    pub fn rbox(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
        let (qx, qy) = ((x - cx).abs() - hw + r, (y - cy).abs() - hh + r);
        qx.max(qy).min(0.0) + (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() - r
    }
    /// A line from a to b, `width` thick, with round ends.
    pub fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32), width: f32) -> f32 {
        let (px, py, bx, by) = (x - a.0, y - a.1, b.0 - a.0, b.1 - a.1);
        let t = ((px * bx + py * by) / (bx * bx + by * by).max(1e-6)).clamp(0.0, 1.0);
        ((px - bx * t).powi(2) + (py - by * t).powi(2)).sqrt() - width / 2.0
    }
    /// An ellipse with radii (rx, ry), turned by `angle` radians (approximate distance).
    pub fn ellipse(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32, angle: f32) -> f32 {
        let (s, c) = angle.sin_cos();
        let (dx, dy) = (x - cx, y - cy);
        let (px, py) = (dx * c + dy * s, -dx * s + dy * c);
        let k1 = ((px / rx).powi(2) + (py / ry).powi(2)).sqrt();
        let k2 = ((px / (rx * rx)).powi(2) + (py / (ry * ry)).powi(2)).sqrt();
        if k2 < 1e-6 { -rx.min(ry) } else { k1 * (k1 - 1.0) / k2 }
    }
    /// The outline of a shape: `width` around where its distance is zero.
    pub fn outline(d: f32, width: f32) -> f32 {
        d.abs() - width / 2.0
    }
    /// A closed polygon.
    pub fn polygon(x: f32, y: f32, p: &[(f32, f32)]) -> f32 {
        let mut d = f32::MAX;
        let mut inside = false;
        let mut j = p.len() - 1;
        for i in 0..p.len() {
            let (a, b) = (p[j], p[i]);
            d = d.min(segment(x, y, a, b, 0.0));
            if (a.1 > y) != (b.1 > y) && x < (b.0 - a.0) * (y - a.1) / (b.1 - a.1) + a.0 {
                inside = !inside;
            }
            j = i;
        }
        if inside { -d } else { d }
    }
}

/// Coverage (0..1) of a shape, one value per pixel.
pub struct Mask {
    pub w: usize,
    pub h: usize,
    a: Vec<f32>,
}

fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let pass = |src: &[f32], horizontal: bool| {
        let mut out = vec![0.0; src.len()];
        let (n, m) = if horizontal { (w, h) } else { (h, w) };
        let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
        for line in 0..m {
            let mut sum = 0.0;
            for i in 0..n.min(radius + 1) {
                sum += src[at(line, i)];
            }
            for i in 0..n {
                out[at(line, i)] = sum / (2 * radius + 1) as f32;
                if i + radius + 1 < n {
                    sum += src[at(line, i + radius + 1)];
                }
                if i >= radius {
                    sum -= src[at(line, i - radius)];
                }
            }
        }
        out
    };
    let once = pass(&pass(src, true), false);
    pass(&pass(&once, true), false)
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Mask {
        Mask { w, h, a: vec![0.0; w * h] }
    }
    fn each(&mut self, f: impl Fn(f32, f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let i = y * self.w + x;
                self.a[i] = f(x as f32 + 0.5, y as f32 + 0.5, self.a[i]);
            }
        }
    }
    /// Add a shape.
    pub fn add(&mut self, d: impl Fn(f32, f32) -> f32) -> &mut Mask {
        self.each(|x, y, a| a.max(cover(d(x, y))));
        self
    }
    /// Cut a shape out.
    pub fn cut(&mut self, d: impl Fn(f32, f32) -> f32) -> &mut Mask {
        self.each(|x, y, a| a.min(1.0 - cover(d(x, y))));
        self
    }
    /// Text centred on x with its baseline at y, added or (cut) removed.
    pub fn text(&mut self, face: &Face, s: &str, x: i32, y: i32, cut: bool) -> &mut Mask {
        let mut c = Canvas::new(self.w, self.h);
        face.centered(&mut c, s, x - self.w as i32, x + self.w as i32, y, WHITE);
        for (a, p) in self.a.iter_mut().zip(c.px.iter()) {
            let t = (p & 0xff) as f32 / 255.0;
            *a = if cut { a.min(1.0 - t) } else { a.max(t) };
        }
        self
    }

    /// The shape in `tone`, raised from the paper: lit from the top left,
    /// with a shadow of `depth` pixels below and to the right.
    pub fn emboss(&self, tone: Color, depth: f32) -> Sprite {
        let (w, h) = (self.w, self.h);
        let height = blur(&self.a, w, h, 2);
        let shadow = blur(&self.a, w, h, (depth * 0.9).max(1.0) as usize);
        let (ox, oy) = ((depth * 0.6).round() as i32, depth.round() as i32);
        let mut px = vec![0u32; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let at = |x: i32, y: i32| {
                    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 { 0.0 } else { height[y as usize * w + x as usize] }
                };
                let (xi, yi) = (x as i32, y as i32);
                // The surface rises into the shape: where it faces the light it is brighter.
                let slope = (at(xi + 1, yi) - at(xi - 1, yi)) + (at(xi, yi + 1) - at(xi, yi - 1)) * 1.3;
                let lit = (slope * 1.5).clamp(-1.0, 1.0);
                let grade = 1.0 - 0.10 * y as f32 / h as f32;
                let mut body = mix(BLACK, tone, grade);
                body = if lit > 0.0 { mix(body, WHITE, lit * 0.55) } else { mix(body, rgb(0x3a, 0x32, 0x28), -lit * 0.6) };
                let a = self.a[i];
                let (sx, sy) = (xi - ox, yi - oy);
                let s = if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 { 0.0 } else { shadow[sy as usize * w + sx as usize] * 0.6 };
                let alpha = 1.0 - (1.0 - a) * (1.0 - s);
                if alpha <= 0.004 {
                    continue;
                }
                // The body over its black shadow, as one colour and one alpha.
                let color = mix(BLACK, body, a / alpha);
                px[i] = (color & 0x00ff_ffff) | (((alpha * 255.0).round() as u32) << 24);
            }
        }
        Sprite { w, h, px }
    }
}

/// A picture with its own transparency.
pub struct Sprite {
    pub w: usize,
    pub h: usize,
    px: Vec<u32>,
}

impl Sprite {
    /// Draw with its top left corner at (x, y).
    pub fn draw(&self, c: &mut Canvas, x: i32, y: i32) {
        let clip = c.clip().intersect(Rect::new(x, y, x + self.w as i32, y + self.h as i32));
        for py in clip.y0..clip.y1 {
            let row = (py - y) as usize * self.w;
            for px in clip.x0..clip.x1 {
                let p = self.px[row + (px - x) as usize];
                c.blend8(px, py, p | 0xff00_0000, (p >> 24) as u8);
            }
        }
    }
    /// Draw centred on (cx, cy).
    pub fn draw_centered(&self, c: &mut Canvas, cx: i32, cy: i32) {
        self.draw(c, cx - self.w as i32 / 2, cy - self.h as i32 / 2);
    }
}

/// A soft shadow under r.
fn shadow(c: &mut Canvas, r: Rect, radius: f32, depth: i32) {
    for k in 1..=depth {
        c.round_rect_alpha(r.offset(k / 2, k + 1).inset(-(k / 2)), radius + k as f32 / 2.0, BLACK, 0.5 / depth as f32);
    }
}

/// A raised piece of paper: a card, a button, a row.
pub fn slab(c: &mut Canvas, r: Rect, radius: f32, tone: Color) {
    shadow(c, r, radius, 6);
    c.round_rect(r, radius, mix(tone, WHITE, 0.16));
    c.round_rect(Rect::new(r.x0, r.y0 + 2, r.x1, r.y1), radius, tone);
}

/// A field pressed into the paper: text entry, a track, a list.
pub fn well(c: &mut Canvas, r: Rect, radius: f32) {
    c.round_rect(Rect::new(r.x0, r.y0, r.x1, r.y1 + 2), radius, mix(PAPER, WHITE, 0.10));
    c.round_rect(r, radius, rgb(0x12, 0x11, 0x10));
    c.round_rect(Rect::new(r.x0, r.y0 + 3, r.x1, r.y1), radius, PAPER_DEEP);
}

/// A switch, on or off, in a 104 x 56 box at (x, y).
pub fn switch(c: &mut Canvas, x: i32, y: i32, on: bool) {
    let r = Rect::new(x, y, x + 104, y + 56);
    well(c, r, 28.0);
    if on {
        c.round_rect(r.inset(3), 25.0, mix(PAPER_DEEP, VIOLET, 0.55));
    }
    let kx = if on { x + 104 - 28 } else { x + 28 };
    let knob = Rect::new(kx - 21, y + 7, kx + 21, y + 49);
    shadow(c, knob, 21.0, 4);
    c.circle(kx as f32, y as f32 + 28.0, 21.0, CREAM);
}
pub const SWITCH_W: i32 = 104;

fn hash(x: u32, y: u32) -> u32 {
    let mut v = x.wrapping_mul(0x9e37_79b1) ^ y.wrapping_mul(0x85eb_ca6b);
    v ^= v >> 15;
    v = v.wrapping_mul(0x2c1b_3c6d);
    v ^ (v >> 13)
}

/// Home's wallpaper: stepped sheets of dark paper with a little grain.
pub fn wallpaper(w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h);
    c.fill(Rect::new(0, 0, w as i32, h as i32), rgb(0x24, 0x23, 0x22));
    // Each sheet: where it lies, its tone, and how far it stands off the one below.
    let sheets: [(Rect, Color, f32); 9] = [
        (Rect::new(596, -40, 760, 1236), rgb(0x21, 0x20, 0x1f), 10.0),
        (Rect::new(-40, -40, 596, 1010), rgb(0x2c, 0x2b, 0x29), 18.0),
        (Rect::new(566, -40, 690, 168), rgb(0x30, 0x2f, 0x2d), 12.0),
        (Rect::new(-40, 1010, 470, 1112), rgb(0x28, 0x27, 0x26), 14.0),
        (Rect::new(-40, 1112, 214, 1262), rgb(0x2a, 0x29, 0x27), 14.0),
        (Rect::new(470, 830, 596, 1112), rgb(0x26, 0x25, 0x24), 12.0),
        (Rect::new(214, 1236, 760, 1340), rgb(0x2e, 0x2d, 0x2b), 16.0),
        (Rect::new(-40, 1262, 214, 1640), rgb(0x23, 0x22, 0x21), 10.0),
        (Rect::new(214, 1340, 760, 1640), rgb(0x27, 0x26, 0x25), 12.0),
    ];
    for (r, tone, lift) in sheets {
        // The shadow falls down and to the right, softer the higher the sheet.
        let area = Rect::new(r.x0, r.y0, r.x1 + (lift * 3.0) as i32, r.y1 + (lift * 3.0) as i32).intersect(c.bounds());
        let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0 + lift * 0.5, (r.y0 + r.y1) as f32 / 2.0 + lift * 0.8);
        let (hw, hh) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                if r.contains(x, y) {
                    continue;
                }
                let d = sdf::rbox(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, 0.0);
                let t = (1.0 - (d + lift) / (2.0 * lift)).clamp(0.0, 1.0);
                c.blend(x, y, BLACK, 0.55 * t * t);
            }
        }
        c.fill(r, tone);
        // A lit top and left edge, a dark lower and right one.
        c.fill(Rect::new(r.x0, r.y0, r.x1, r.y0 + 2), mix(tone, WHITE, 0.10));
        c.fill(Rect::new(r.x0, r.y0, r.x0 + 2, r.y1), mix(tone, WHITE, 0.07));
        c.fill(Rect::new(r.x0, r.y1 - 2, r.x1, r.y1), mix(tone, BLACK, 0.25));
        c.fill(Rect::new(r.x1 - 2, r.y0, r.x1, r.y1), mix(tone, BLACK, 0.25));
    }
    // Grain, and the light falling off towards the bottom.
    for y in 0..h {
        let fade = 1.0 - 0.12 * y as f32 / h as f32;
        for x in 0..w {
            let n = (hash(x as u32, y as u32) % 7) as i32 - 3;
            let p = c.px[y * w + x];
            let ch = |shift: u32| ((((p >> shift) & 0xff) as f32 * fade) as i32 + n).clamp(0, 255) as u32;
            c.px[y * w + x] = 0xff00_0000 | ch(16) << 16 | ch(8) << 8 | ch(0);
        }
    }
    c
}

/// The app icons, each in a 168 x 168 box.
pub mod icons {
    use super::{sdf::*, Mask};
    use crate::text::Fonts;
    use std::f32::consts::PI;

    pub const SIZE: usize = 168;
    const C: f32 = 84.0;

    /// Ladybird Browser: three orbits.
    pub fn ladybird() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        for angle in [PI / 4.0, -PI / 4.0] {
            m.add(|x, y| outline(ellipse(x, y, C, C, 62.0, 25.0, angle), 9.0));
        }
        m.add(|x, y| outline(ellipse(x, y, C, C, 64.0, 40.0, 0.0), 9.0));
        m
    }

    /// Bitchat: a tile with BIT pressed into it.
    pub fn bitchat(fonts: &Fonts) -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| rbox(x, y, C, C, 56.0, 56.0, 16.0));
        m.text(&fonts.bold(46.0), "BIT", C as i32, 100, true);
        m
    }

    /// Violet Agent: a head in profile, looking right.
    pub fn agent() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        let face: [(f32, f32); 15] = [
            (84.0, 36.0), (108.0, 42.0), (118.0, 60.0), (117.0, 72.0), (130.0, 92.0), (119.0, 96.0), (121.0, 104.0),
            (116.0, 108.0), (119.0, 116.0), (112.0, 128.0), (96.0, 130.0), (92.0, 150.0), (60.0, 150.0), (62.0, 110.0), (60.0, 60.0),
        ];
        m.add(|x, y| polygon(x, y, &face) - 3.0);
        // Hair swept back into a knot.
        m.add(|x, y| circle(x, y, 76.0, 62.0, 40.0));
        m.add(|x, y| circle(x, y, 42.0, 96.0, 19.0));
        m.add(|x, y| ellipse(x, y, 56.0, 84.0, 22.0, 34.0, 0.3));
        // The hairline, a band, an eye and the parting of the lips.
        m.cut(|x, y| outline(circle(x, y, 56.0, 58.0, 52.0), 4.5).max(-(x - 84.0)).max(y - 104.0));
        m.cut(|x, y| outline(circle(x, y, 70.0, 66.0, 30.0), 3.5).max(x - 84.0).max(y - 70.0));
        m.cut(|x, y| segment(x, y, (107.0, 73.0), (114.0, 74.0), 4.5));
        m.cut(|x, y| segment(x, y, (112.0, 105.0), (120.0, 104.0), 2.5));
        m
    }

    /// Locomo Music: a dog in headphones.
    pub fn music() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        // The band over the head and the two cups.
        m.add(|x, y| outline(circle(x, y, C, 84.0, 54.0), 9.0).max(y - 84.0));
        for cx in [28.0, 140.0] {
            m.add(|x, y| rbox(x, y, cx, 96.0, 12.0, 22.0, 9.0));
        }
        // Head, muzzle and ears.
        m.add(|x, y| ellipse(x, y, C, 90.0, 37.0, 40.0, 0.0));
        m.add(|x, y| ellipse(x, y, C, 116.0, 24.0, 20.0, 0.0));
        for (cx, a) in [(50.0, 0.35), (118.0, -0.35)] {
            m.add(|x, y| ellipse(x, y, cx, 96.0, 13.0, 30.0, a));
        }
        m.cut(|x, y| outline(ellipse(x, y, 50.0, 96.0, 13.0, 30.0, 0.35), 3.0).max(x - 58.0));
        m.cut(|x, y| outline(ellipse(x, y, 118.0, 96.0, 13.0, 30.0, -0.35), 3.0).max(110.0 - x));
        for cx in [70.0, 98.0] {
            m.cut(|x, y| circle(x, y, cx, 84.0, 5.5));
        }
        m.cut(|x, y| ellipse(x, y, C, 106.0, 9.0, 6.5, 0.0));
        m.cut(|x, y| segment(x, y, (C, 112.0), (C, 120.0), 3.0));
        m.cut(|x, y| outline(circle(x, y, 76.0, 118.0, 8.0), 3.0).max(118.0 - y).max(x - 84.0));
        m.cut(|x, y| outline(circle(x, y, 92.0, 118.0, 8.0), 3.0).max(118.0 - y).max(84.0 - x));
        m
    }

    /// Settings: a gear.
    pub fn settings() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| circle(x, y, C, C, 44.0));
        for k in 0..8 {
            let a = k as f32 * PI / 4.0;
            let tip = (C + a.cos() * 50.0, C + a.sin() * 50.0);
            m.add(|x, y| segment(x, y, (C, C), tip, 20.0) - 3.0);
        }
        m.cut(|x, y| circle(x, y, C, C, 20.0));
        m
    }

    /// An installed app without its own icon: a disc with its initial.
    pub fn initial(fonts: &Fonts, title: &str) -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| circle(x, y, C, C, 58.0));
        let letter: String = title.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
        m.text(&fonts.bold(72.0), &letter, C as i32, 110, true);
        m
    }
}
