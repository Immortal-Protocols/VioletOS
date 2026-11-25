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
        let height = blur(&self.a, w, h, 3);
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
                let lit = (slope * 1.8).clamp(-1.0, 1.0);
                let grade = 1.04 - 0.14 * y as f32 / h as f32 - 0.04 * x as f32 / w as f32;
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

/// Paint a sheet with rounded, overlapping sections, from the shadow up to
/// the lit stock. The rectangles overlap so there are no seams within a sheet.
fn sheet(c: &mut Canvas, panels: &[Rect], tone: Color, lift: f32) {
    let bounds = panels.iter().copied().reduce(|a, b| a.union(b)).unwrap();
    let area = bounds.inset(-((lift * 2.0) as i32)).intersect(c.bounds());
    let panels: Vec<_> = panels.iter().map(|r| {
        let (hw, hh) = (r.width() as f32 * 0.5, r.height() as f32 * 0.5);
        (r.x0 as f32 + hw, r.y0 as f32 + hh, hw, hh)
    }).collect();
    let distance = |x: f32, y: f32| {
        panels.iter().map(|&(cx, cy, hw, hh)| sdf::rbox(x, y, cx, cy, hw, hh, 7.0))
            .fold(f32::MAX, f32::min)
    };
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let edge = distance(px, py);
            if edge > 0.0 {
                let shadow = distance(px - lift * 0.38, py - lift * 0.70);
                let ambient = (1.0 - edge / (lift * 0.7)).clamp(0.0, 1.0);
                let cast = (1.0 - shadow / lift).clamp(0.0, 1.0);
                c.blend(x, y, BLACK, 0.22 * ambient + 0.48 * cast * cast);
            }
            if edge < 0.5 {
                let rim = (1.0 + edge / 5.0).clamp(0.0, 1.0);
                let diffuse = (1.0 + edge / 72.0).clamp(0.0, 1.0);
                let lit = mix(tone, rgb(0x83, 0x7e, 0x71), rim * 0.19 + diffuse * 0.085);
                c.blend(x, y, lit, cover(edge));
            }
        }
    }
}

/// Home's wallpaper: staggered sheets of charcoal paper and their cast shadows.
pub fn wallpaper(w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h);
    c.fill(c.bounds(), rgb(0x22, 0x22, 0x20));
    sheet(&mut c, &[Rect::new(583, -40, 762, 1287)], rgb(0x29, 0x28, 0x25), 23.0);
    sheet(&mut c, &[Rect::new(202, 1286, 762, 1379)], rgb(0x2b, 0x2a, 0x27), 27.0);
    sheet(&mut c, &[Rect::new(-40, -40, 205, 1318)], rgb(0x2d, 0x2c, 0x28), 33.0);
    sheet(&mut c, &[Rect::new(-40, -40, 472, 1200)], rgb(0x30, 0x2e, 0x2a), 26.0);
    sheet(&mut c, &[
        Rect::new(-40, -40, 676, 831),
        Rect::new(-40, -40, 608, 990),
        Rect::new(-40, -40, 472, 1112),
    ], rgb(0x31, 0x30, 0x2c), 24.0);
    sheet(&mut c, &[Rect::new(585, -40, 762, 190)], rgb(0x28, 0x27, 0x23), 18.0);

    // Vary the stock at two scales, with softer light towards the screen edges.
    for y in 0..h {
        let v = y as f32 / h as f32;
        for x in 0..w {
            let u = x as f32 / w as f32;
            let grain = (hash(x as u32, y as u32) % 7) as i32 - 3;
            let fibres = (hash((x / 2) as u32, (y / 3) as u32) % 5) as i32 - 2;
            let falloff = 1.02 - 0.18 * (u - 0.38).abs() - 0.10 * v;
            let p = c.px[y * w + x];
            let ch = |shift: u32| {
                let channel = ((p >> shift) & 0xff) as f32 * falloff;
                (channel as i32 + grain + fibres).clamp(0, 255) as u32
            };
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
        for angle in [PI / 4.7, -PI / 4.7] {
            m.add(|x, y| outline(ellipse(x, y, C, C, 84.0, 27.0, angle), 9.5));
        }
        m.add(|x, y| outline(ellipse(x, y, C, C, 58.0, 62.0, 0.0), 9.5));
        m
    }

    /// Bitchat: a tile with BIT pressed into it.
    pub fn bitchat(fonts: &Fonts) -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| rbox(x, y, C, C, 62.0, 62.0, 22.0));
        m.text(&fonts.bold(44.0), "BIT", C as i32, 100, true);
        m
    }

    /// Violet Agent: a head in profile, looking right.
    pub fn agent() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        let shape = |(x, y): (f32, f32)| (C + (x - C) * 1.08, C + (y - C) * 0.86);
        let profile = [
            (40.0, 60.0), (49.0, 39.0), (64.0, 27.0), (84.0, 16.0),
            (103.0, 15.0), (119.0, 20.0), (125.0, 29.0), (125.0, 42.0),
            (132.0, 57.0), (133.0, 69.0), (145.0, 91.0), (144.0, 96.0),
            (135.0, 99.0), (135.0, 106.0), (131.0, 110.0), (132.0, 118.0),
            (127.0, 130.0), (108.0, 132.0), (101.0, 150.0), (64.0, 150.0),
            (65.0, 141.0), (47.0, 135.0), (38.0, 123.0), (37.0, 109.0),
            (42.0, 96.0), (35.0, 82.0), (35.0, 73.0),
        ].map(shape);
        m.add(|x, y| polygon(x, y, &profile) - 2.5);
        // Swept hair, modelled as shallow grooves across the silhouette.
        let waves: [&[(f32, f32)]; 2] = [
            &[(36.0, 93.0), (49.0, 90.0), (65.0, 81.0), (77.0, 69.0),
              (87.0, 52.0), (98.0, 40.0), (111.0, 34.0), (126.0, 35.0)],
            &[(62.0, 145.0), (59.0, 132.0), (64.0, 119.0), (74.0, 108.0),
              (90.0, 100.0), (102.0, 93.0), (111.0, 79.0), (122.0, 69.0), (132.0, 68.0)],
        ];
        for wave in waves {
            m.each(|x, y, a| {
                let d = wave.windows(2).map(|p| segment(x, y, shape(p[0]), shape(p[1]), 1.0)).fold(f32::MAX, f32::min);
                a * (0.55 + 0.45 * (d / 4.0).clamp(0.0, 1.0))
            });
        }
        m.cut(|x, y| segment(x, y, shape((115.0, 81.0)), shape((126.0, 82.0)), 5.0));
        m.cut(|x, y| segment(x, y, shape((129.0, 108.0)), shape((134.0, 107.0)), 2.5));
        m
    }

    /// Locomo Music: a dog in headphones.
    pub fn music() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| outline(ellipse(x, y, C, 81.0, 59.0, 57.0, 0.0), 8.0).max(y - 90.0));
        for cx in [28.0, 140.0] {
            m.add(|x, y| rbox(x, y, cx, 86.0, 13.0, 23.0, 12.0));
        }
        m.add(|x, y| ellipse(x, y, C, 102.0, 41.0, 45.0, 0.0));
        m.add(|x, y| ellipse(x, y, C, 137.0, 17.0, 14.0, 0.0));
        for cx in [43.0, 125.0] {
            m.cut(|x, y| segment(x, y, (cx, 76.0), (cx, 106.0), 4.5));
        }
        for cx in [64.0, 104.0] {
            m.cut(|x, y| circle(x, y, cx, 94.0, 6.0));
        }
        m.cut(|x, y| ellipse(x, y, C, 116.0, 11.0, 8.0, 0.0));
        m.cut(|x, y| segment(x, y, (C, 120.0), (C, 134.0), 5.0));
        for cx in [74.0, 94.0] {
            m.cut(|x, y| outline(circle(x, y, cx, 131.0, 10.0), 4.5).max(131.0 - y));
        }
        m
    }

    /// Settings: a six-tooth gear.
    pub fn settings() -> Mask {
        let mut m = Mask::new(SIZE, SIZE);
        m.add(|x, y| circle(x, y, C, C, 41.0));
        for k in 0..6 {
            let a = k as f32 * PI / 3.0 - PI / 2.0;
            let tooth = [(31.0, -14.0), (53.0, -10.0), (53.0, 10.0), (31.0, 14.0)]
                .map(|(radius, tangent)| (C + a.cos() * radius - a.sin() * tangent,
                                          C + a.sin() * radius + a.cos() * tangent));
            m.add(|x, y| polygon(x, y, &tooth) - 2.5);
        }
        m.cut(|x, y| circle(x, y, C, C, 25.0));
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
