//! Music's renderer for the 720x1600 surface: a dithered background field,
//! anti-aliased shapes from signed distances (rounded boxes, circles, capsules,
//! convex polygons), soft glows, smoothly scaled cover art and 4-bit text.
//! Geometry is fixed point with 4 fractional bits ("Q4": 16 = one pixel).
//! Drawing is clipped to a band of rows so any part of a screen can be redrawn
//! from the background up and presented on its own.
use super::font::Font;

pub const W: i32 = 720;
pub const H: i32 = 1600;
pub const ONE: i32 = 16;

static mut PX: *mut u32 = core::ptr::null_mut();
static mut CY0: i32 = 0;
static mut CY1: i32 = H;

pub fn init(surface: *mut u32) {
    unsafe { PX = surface };
}
pub fn clip(y0: i32, y1: i32) {
    unsafe {
        CY0 = y0.max(0);
        CY1 = y1.min(H);
    }
}
pub fn clip_get() -> (i32, i32) {
    unsafe { (CY0, CY1) }
}
#[inline(always)]
fn rows(y0: i32, y1: i32) -> (i32, i32) {
    unsafe { (y0.max(CY0), y1.min(CY1)) }
}
#[inline(always)]
fn at(x: i32, y: i32) -> *mut u32 {
    unsafe { PX.add((y * W + x) as usize) }
}

/// Per-pixel noise used to dither every soft gradient (stable across redraws).
#[inline(always)]
pub fn noise(x: i32, y: i32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

/// Blends colour `c` over `d` with alpha `a` (0..=256), rounding with dither `n` (0..=255).
#[inline(always)]
pub fn mix(d: u32, c: u32, a: u32, n: u32) -> u32 {
    let na = 256 - a;
    let rb = (((c & 0xff00ff) * a + (d & 0xff00ff) * na + (n | n << 16)) >> 8) & 0xff00ff;
    let g = (((c & 0xff00) * a + (d & 0xff00) * na + (n << 8)) >> 8) & 0xff00;
    0xff00_0000 | rb | g
}
#[inline(always)]
pub fn plot(x: i32, y: i32, c: u32, a: u32) {
    if a == 0 {
        return;
    }
    let p = at(x, y);
    unsafe { *p = if a >= 256 { c | 0xff00_0000 } else { mix(*p, c, a, noise(x, y) & 255) } };
}
pub fn lerp_rgb(a: u32, b: u32, t: u32) -> u32 {
    mix(a, b, t.min(256), 128)
}
pub fn scale_rgb(c: u32, k: u32) -> u32 {
    let ch = |s: u32| ((((c >> s) & 255) * k) >> 8).min(255) << s;
    0xff00_0000 | ch(16) | ch(8) | ch(0)
}

/// Integer square root: Newton's method from a power of two above the root,
/// which the hardware divide makes a few cycles per step.
pub fn isqrt(v: u32) -> u32 {
    if v < 2 {
        return v;
    }
    let mut x = 1u32 << ((33 - v.leading_zeros()) / 2);
    loop {
        let y = (x + v / x) >> 1;
        if y >= x {
            return x;
        }
        x = y;
    }
}
#[inline(always)]
fn len(dx: i32, dy: i32) -> i32 {
    isqrt((dx * dx + dy * dy) as u32) as i32
}
/// Coverage (0..=256) of a pixel whose centre is `d` (Q4) outside an edge.
#[inline(always)]
pub fn cover(d: i32) -> u32 {
    (128 - d * 16).clamp(0, 256) as u32
}

// ---------- background ----------

/// The background is a coarse grid of colours (one every 64 px), interpolated
/// and dithered per pixel. Screens fill it with any smooth field: aurora blobs,
/// a blurred cover, a darkening veil.
pub const GW: usize = 13;
pub const GH: usize = 26;
pub static mut GRID: [u32; GW * GH] = [0; GW * GH];

pub fn background(y0: i32, y1: i32) {
    let (y0, y1) = rows(y0, y1);
    let grid = unsafe { &*core::ptr::addr_of!(GRID) };
    for y in y0..y1 {
        let gy = (y >> 6) as usize;
        let fy = (y & 63) as u32;
        // This row's colours at each grid column, 8.8 fixed per channel.
        let mut col = [[0u32; 3]; GW];
        for (gx, c) in col.iter_mut().enumerate() {
            let a = grid[gy * GW + gx];
            let b = grid[(gy + 1).min(GH - 1) * GW + gx];
            for (k, s) in [16, 8, 0].iter().enumerate() {
                let va = (a >> s) & 255;
                let vb = (b >> s) & 255;
                c[k] = (va * (64 - fy) + vb * fy) << 2; // 8.8
            }
        }
        let mut p = at(0, y);
        for gx in 0..GW - 1 {
            let (a, b) = (col[gx], col[gx + 1]);
            let mut r = (a[0] << 6) as i32;
            let mut g = (a[1] << 6) as i32;
            let mut bl = (a[2] << 6) as i32;
            let dr = b[0] as i32 - a[0] as i32;
            let dg = b[1] as i32 - a[1] as i32;
            let db = b[2] as i32 - a[2] as i32;
            let x0 = (gx as i32) << 6;
            let x1 = (x0 + 64).min(W);
            for x in x0..x1 {
                let n = noise(x, y);
                // Dither plus a touch of film grain.
                let grain = (((n >> 8) & 15) as i32 - 8) * 3072;
                let d = ((n & 255) << 6) as i32 + grain;
                let cr = ((r + d) >> 14).clamp(0, 255) as u32;
                let cg = ((g + d) >> 14).clamp(0, 255) as u32;
                let cb = ((bl + d) >> 14).clamp(0, 255) as u32;
                unsafe {
                    *p = 0xff00_0000 | cr << 16 | cg << 8 | cb;
                    p = p.add(1);
                }
                r += dr;
                g += dg;
                bl += db;
            }
        }
    }
}

// ---------- shapes ----------

/// A shape primitive in Q4 coordinates.
#[derive(Clone, Copy)]
pub enum Prim {
    /// Rounded box: centre, half size, corner radius.
    Box(i32, i32, i32, i32, i32),
    /// Capsule: segment a-b with radius.
    Seg(i32, i32, i32, i32, i32),
    /// Convex polygon (clockwise on screen, up to 4 points; repeat the last for a triangle), rounded by r.
    Poly([(i32, i32); 4], i32),
    /// Ring: centre, radius, half width.
    Ring(i32, i32, i32, i32),
}

fn bbox(p: &Prim) -> (i32, i32, i32, i32) {
    match *p {
        Prim::Box(cx, cy, hw, hh, _) => (cx - hw, cy - hh, cx + hw, cy + hh),
        Prim::Seg(ax, ay, bx, by, r) => (ax.min(bx) - r, ay.min(by) - r, ax.max(bx) + r, ay.max(by) + r),
        Prim::Poly(v, r) => {
            let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for (x, y) in v {
                b = (b.0.min(x - r), b.1.min(y - r), b.2.max(x + r), b.3.max(y + r));
            }
            b
        }
        Prim::Ring(cx, cy, r, w) => (cx - r - w, cy - r - w, cx + r + w, cy + r + w),
    }
}

pub fn box_d(px: i32, py: i32, cx: i32, cy: i32, hw: i32, hh: i32, r: i32) -> i32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let ox = qx.max(0);
    let oy = qy.max(0);
    let out = if ox == 0 {
        oy
    } else if oy == 0 {
        ox
    } else {
        len(ox, oy)
    };
    out + qx.max(qy).min(0) - r
}

fn seg_d(px: i32, py: i32, ax: i32, ay: i32, bx: i32, by: i32) -> i32 {
    let (dx, dy) = (bx - ax, by - ay);
    let (qx, qy) = (px - ax, py - ay);
    let l2 = dx * dx + dy * dy;
    let dot = qx * dx + qy * dy;
    let (cx, cy) = if l2 == 0 || dot <= 0 {
        (qx, qy)
    } else if dot >= l2 {
        (px - bx, py - by)
    } else {
        // Projection without a per-pixel divide when the segment is axis-aligned.
        if dx == 0 {
            (qx, 0)
        } else if dy == 0 {
            (0, qy)
        } else {
            let t = (dot << 6) / (l2 >> 2).max(1);
            (qx - ((dx * t) >> 8), qy - ((dy * t) >> 8))
        }
    };
    len(cx, cy)
}

fn poly_d(px: i32, py: i32, v: &[(i32, i32); 4]) -> i32 {
    let mut inside = i32::MIN;
    let mut out = i32::MAX;
    let mut any_out = false;
    for i in 0..4 {
        let (ax, ay) = v[i];
        let (bx, by) = v[(i + 1) % 4];
        if ax == bx && ay == by {
            continue;
        }
        // Outward normal for clockwise points on a y-down screen.
        let (ex, ey) = (bx - ax, by - ay);
        let l = len(ex, ey).max(1);
        let d = ((px - ax) * ey - (py - ay) * ex) / l;
        if d > 0 {
            any_out = true;
            out = out.min(seg_d(px, py, ax, ay, bx, by));
        }
        inside = inside.max(d);
    }
    if any_out {
        out
    } else {
        inside
    }
}

#[inline(always)]
fn prim_d(p: &Prim, x: i32, y: i32) -> i32 {
    match *p {
        Prim::Box(cx, cy, hw, hh, r) => box_d(x, y, cx, cy, hw, hh, r),
        Prim::Seg(ax, ay, bx, by, r) => seg_d(x, y, ax, ay, bx, by) - r,
        Prim::Poly(ref v, r) => poly_d(x, y, v) - r,
        Prim::Ring(cx, cy, r, w) => (len(x - cx, y - cy) - r).abs() - w,
    }
}

/// Fills the union of `prims` with colour `c` at opacity `a` (0..=256).
pub fn shape(prims: &[Prim], c: u32, a: u32) {
    let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for p in prims {
        let q = bbox(p);
        b = (b.0.min(q.0), b.1.min(q.1), b.2.max(q.2), b.3.max(q.3));
    }
    let x0 = ((b.0 >> 4) - 1).max(0);
    let x1 = ((b.2 >> 4) + 2).min(W);
    let (y0, y1) = rows((b.1 >> 4) - 1, (b.3 >> 4) + 2);
    for y in y0..y1 {
        let py = y * 16 + 8;
        for x in x0..x1 {
            let px = x * 16 + 8;
            let mut d = i32::MAX;
            for p in prims {
                d = d.min(prim_d(p, px, py));
            }
            let cv = cover(d);
            if cv != 0 {
                plot(x, y, c, (cv * a) >> 8);
            }
        }
    }
}

/// Rounded box in pixels (fast path for cards and pills).
pub fn rbox(x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, a: u32) {
    let (cx, cy, hw, hh, rr) = (x * 16 + w * 8, y * 16 + h * 8, w * 8, h * 8, r * 16);
    let (y0, y1) = rows(y, y + h);
    let x0 = x.max(0);
    let x1 = (x + w).min(W);
    for py in y0..y1 {
        let qy = (py * 16 + 8 - cy).abs() - (hh - rr);
        for px in x0..x1 {
            let qx = (px * 16 + 8 - cx).abs() - (hw - rr);
            // Most pixels are well inside: skip the distance.
            let d = if qx < -16 && qy < -16 {
                -16
            } else {
                box_d(px * 16 + 8, py * 16 + 8, cx, cy, hw, hh, rr)
            };
            let cv = cover(d);
            if cv != 0 {
                plot(px, py, c, (cv * a) >> 8);
            }
        }
    }
}

/// Vertical gradient inside a rounded box.
/// Colour and opacity both run from the top value to the bottom one.
pub fn rbox_v(x: i32, y: i32, w: i32, h: i32, r: i32, top: u32, bottom: u32, a_top: u32, a_bottom: u32) {
    let (cx, cy, hw, hh, rr) = (x * 16 + w * 8, y * 16 + h * 8, w * 8, h * 8, r * 16);
    let (y0, y1) = rows(y, y + h);
    for py in y0..y1 {
        let f = ((py - y) * 256 / h.max(1)) as u32;
        let c = lerp_rgb(top, bottom, f);
        let a = (a_top * (256 - f) + a_bottom * f) >> 8;
        for px in x.max(0)..(x + w).min(W) {
            let cv = cover(box_d(px * 16 + 8, py * 16 + 8, cx, cy, hw, hh, rr));
            if cv != 0 {
                plot(px, py, c, (cv * a) >> 8);
            }
        }
    }
}

/// Hairline border of a rounded box.
pub fn rbox_line(x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, a: u32) {
    let (cx, cy, hw, hh, rr) = (x * 16 + w * 8, y * 16 + h * 8, w * 8, h * 8, r * 16);
    let (y0, y1) = rows(y, y + h);
    for py in y0..y1 {
        let edge_row = py < y + r + 2 || py >= y + h - r - 2;
        for px in x.max(0)..(x + w).min(W) {
            if !edge_row && px > x + 2 && px < x + w - 3 {
                continue;
            }
            let d = box_d(px * 16 + 8, py * 16 + 8, cx, cy, hw, hh, rr);
            let cv = cover((d + 12).abs() - 12);
            if cv != 0 {
                plot(px, py, c, (cv * a) >> 8);
            }
        }
    }
}

/// Smooth falloff 0..=256 for t in 0..=256 (a Gaussian-like curve).
#[inline(always)]
fn falloff(t: i32) -> u32 {
    if t >= 256 {
        return 0;
    }
    let u = (256 - t.max(0)) as u32;
    // u^2 * (3 - 2u) smoothstep, then squared for a soft tail.
    let s = (u * u >> 8) * (768 - 2 * u) >> 8;
    (s * s) >> 8
}

/// A soft shadow or glow around a rounded box (pixels): `spread` px of falloff.
pub fn glow(x: i32, y: i32, w: i32, h: i32, r: i32, spread: i32, c: u32, a: u32) {
    let (y0, y1) = rows(y - spread, y + h + spread);
    let (x0, x1) = ((x - spread).max(0), (x + w + spread).min(W));
    if x0 >= x1 {
        return;
    }
    let inv = (256 * 65536) / spread.max(1);
    let fall = |q: i32| falloff((q.max(0) * inv) >> 16);
    if 2 * r >= w.min(h) {
        // A circle: falloff by distance from the edge.
        let (cx2, cy2, rad) = (2 * x + w, 2 * y + h, w.min(h));
        for py in y0..y1 {
            let dy = 2 * py + 1 - cy2;
            for px in x0..x1 {
                let dx = 2 * px + 1 - cx2;
                let f = fall((len(dx, dy) - rad) / 2);
                if f != 0 {
                    let p = at(px, py);
                    unsafe { *p = mix(*p, c, (f * a) >> 8, noise(px, py) & 255) };
                }
            }
        }
        return;
    }
    // Cards: the falloff across times the falloff down. With this much spread
    // it reads as a rounded shadow, at a multiply per pixel.
    let mut across = [0u16; W as usize];
    for px in x0..x1 {
        across[px as usize] = fall((x - px).max(px - (x + w - 1))) as u16;
    }
    for py in y0..y1 {
        let fy = fall((y - py).max(py - (y + h - 1)));
        if fy == 0 {
            continue;
        }
        let k = (fy * a) >> 8;
        for px in x0..x1 {
            let f = (across[px as usize] as u32 * k) >> 8;
            if f != 0 {
                let p = at(px, py);
                unsafe { *p = mix(*p, c, f, noise(px, py) & 255) };
            }
        }
    }
}

/// A soft elliptical light: radii r (across) and ry (down), centre (cx, cy) pixels.
pub fn light(cx: i32, cy: i32, r: i32, ry: i32, c: u32, a: u32) {
    let (y0, y1) = rows(cy - ry, cy + ry);
    let inv = (256 * 65536) / (r * r).max(1);
    for py in y0..y1 {
        let dy = (py - cy) * r / ry;
        for px in (cx - r).max(0)..(cx + r).min(W) {
            let dx = px - cx;
            let t = ((dx * dx + dy * dy) * inv) >> 16;
            if t < 256 {
                let u = (256 - t) as u32;
                let f = (u * u) >> 8;
                let p = at(px, py);
                unsafe { *p = mix(*p, c, (f * a) >> 8, noise(px, py) & 255) };
            }
        }
    }
}

// ---------- images ----------

/// Decoded cover art (SIDE x SIDE, 0xRRGGBB).
pub const SIDE: usize = super::catalog::SIDE;
pub static mut TEX: [u16; SIDE * SIDE] = [0; SIDE * SIDE];
/// A decoded texel as 0xRRGGBB.
#[inline(always)]
pub fn texel(i: usize) -> u32 {
    let v = unsafe { *core::ptr::addr_of!(TEX[i]) } as u32;
    let c = c565(v);
    c[0] << 16 | c[1] << 8 | c[2]
}
static mut LOADED: u8 = 0;

fn c565(v: u32) -> [u32; 3] {
    let r = (v >> 11) & 31;
    let g = (v >> 5) & 63;
    let b = v & 31;
    [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
}
/// Decodes track `n`'s 4x4 colour blocks into TEX.
pub fn load_cover(n: usize) {
    if unsafe { LOADED } as usize == n + 1 {
        return;
    }
    let src = &super::catalog::TRACKS[n].cover;
    let tex = unsafe { &mut *core::ptr::addr_of_mut!(TEX) };
    let blocks = SIDE / 4;
    for b in 0..blocks * blocks {
        let o = b * 8;
        let a = src[o] as u32 | (src[o + 1] as u32) << 8;
        let c = src[o + 2] as u32 | (src[o + 3] as u32) << 8;
        let bits = u32::from_le_bytes([src[o + 4], src[o + 5], src[o + 6], src[o + 7]]);
        let (ca, cb) = (c565(a), c565(c));
        let mut pal = [0u32; 4];
        for k in 0..4 {
            let (wa, wb) = [(3, 0), (0, 3), (2, 1), (1, 2)][k];
            let ch = |i: usize| (ca[i] * wa + cb[i] * wb) / 3;
            pal[k] = (ch(0) * 31 + 127) / 255 << 11 | (ch(1) * 63 + 127) / 255 << 5 | (ch(2) * 31 + 127) / 255;
        }
        let (bx, by) = (b % blocks * 4, b / blocks * 4);
        for i in 0..16 {
            tex[(by + i / 4) * SIDE + bx + i % 4] = pal[((bits >> (2 * i)) & 3) as usize] as u16;
        }
    }
    unsafe { LOADED = n as u8 + 1 };
}

/// Cubic weights (Q8, summing to 256) for phase f (0..64): Catmull-Rom for
/// small sharp thumbnails, a B-spline for large soft-focus art.
#[inline(always)]
fn cubic(f: i32, soft: bool) -> [i32; 4] {
    let t = f * 4;
    let t2 = (t * t) >> 8;
    let t3 = (t2 * t) >> 8;
    if soft {
        let s = 256 - t;
        let w0 = ((s * s >> 8) * s >> 8) / 6;
        let w3 = t3 / 6;
        let w1 = (3 * t3 - 6 * t2 + 1024) / 6;
        return [w0, w1, 256 - w0 - w1 - w3, w3];
    }
    let w0 = (-t3 + 2 * t2 - t) >> 1;
    let w1 = (3 * t3 - 5 * t2 + 512) >> 1;
    let w2 = (-3 * t3 + 4 * t2 + t) >> 1;
    let w3 = (t3 - t2) >> 1;
    [w0, w1, w2, w3]
}

/// Softens the loaded cover with a 1-2-1 blur (for small turned cards).
pub fn soften() {
    let tex = unsafe { &mut *core::ptr::addr_of_mut!(TEX) };
    for pass in 0..2 {
        for j in 0..SIDE {
            let mut line = [0u32; SIDE];
            for (i, l) in line.iter_mut().enumerate() {
                *l = texel(if pass == 0 { j * SIDE + i } else { i * SIDE + j });
            }
            for i in 0..SIDE {
                let (a, b, c) = (line[i.saturating_sub(1)], line[i], line[(i + 1).min(SIDE - 1)]);
                let ch = |s: u32| (((a >> s) & 255) + 2 * ((b >> s) & 255) + ((c >> s) & 255) + 2) / 4;
                let v = (ch(16) >> 3) << 11 | (ch(8) >> 2) << 5 | ch(0) >> 3;
                tex[if pass == 0 { j * SIDE + i } else { i * SIDE + j }] = v as u16;
            }
        }
    }
    unsafe { LOADED = 0 };
}

/// Draws the loaded cover, scaled smoothly (separable Catmull-Rom) into a
/// rounded square at (x, y) of side `size` px. `shade` darkens it (256 = none).
pub fn cover_art(x: i32, y: i32, size: i32, radius: i32, shade: u32) {
    let soft = size > 150;
    let (cx, cy, h, rr) = (x * 16 + size * 8, y * 16 + size * 8, size * 8, radius * 16);
    let step = ((SIDE as i32) << 16) / size; // texels per pixel, Q16
    let weights: [[i32; 4]; 64] = core::array::from_fn(|f| cubic(f as i32, soft));
    let sheen_k = 96 * 65536 / size;
    let (y0, y1) = rows(y, y + size);
    for py in y0..y1 {
        // Vertical pass: one row of SIDE texels, Q8 per channel.
        let v = ((py - y) * step + step / 2 - 32768).max(0);
        let (ti, f) = ((v >> 16) as i32, (v >> 10) & 63);
        let w = weights[f as usize];
        let mut line = [[0i32; 3]; SIDE];
        for (tx, l) in line.iter_mut().enumerate() {
            for k in 0..4 {
                let ty = (ti - 1 + k as i32).clamp(0, SIDE as i32 - 1) as usize;
                let t = texel(ty * SIDE + tx);
                l[0] += ((t >> 16) & 255) as i32 * w[k];
                l[1] += ((t >> 8) & 255) as i32 * w[k];
                l[2] += (t & 255) as i32 * w[k];
            }
        }
        let qy = (py * 16 + 8 - cy).abs() - (h - rr);
        let mut u = step / 2 - 32768;
        for px in x..x + size {
            let uu = u.max(0);
            u += step;
            if px < 0 || px >= W {
                continue;
            }
            let qx = (px * 16 + 8 - cx).abs() - (h - rr);
            let d = if qx < -16 && qy < -16 { -999 } else { box_d(px * 16 + 8, py * 16 + 8, cx, cy, h, h, rr) };
            let cv = cover(d);
            if cv == 0 {
                continue;
            }
            let (ti, f) = ((uu >> 16) as i32, (uu >> 10) & 63);
            let w = weights[f as usize];
            let mut acc = [0i32; 3];
            for k in 0..4 {
                let tx = (ti - 1 + k as i32).clamp(0, SIDE as i32 - 1) as usize;
                let l = &line[tx];
                acc[0] += l[0] * w[k];
                acc[1] += l[1] * w[k];
                acc[2] += l[2] * w[k];
            }
            let n = (noise(px, py) & 255) as i32 * 256;
            let ch = |a: i32| (((a + n) >> 16) * shade as i32 >> 8).clamp(0, 255) as u32;
            let mut col = ch(acc[0]) << 16 | ch(acc[1]) << 8 | ch(acc[2]);
            // Glass: a soft sheen from the top-left corner and a faint rim.
            let sheen = 64 - (((px - x) + (py - y)) * sheen_k >> 16);
            if sheen > 0 {
                col = mix(col, 0xffffff, sheen as u32, 128);
            }
            if d > -40 {
                col = mix(col, 0xffffff, 26, 128);
            }
            plot(px, py, col, cv);
        }
    }
}

/// Bilinear sample of TEX at (u, v) in Q16 texels.
#[inline(always)]
fn sample(u: i32, v: i32) -> u32 {
    let m = (SIDE as i32 - 1) << 16;
    let (u, v) = ((u - 32768).clamp(0, m), (v - 32768).clamp(0, m));
    let (x, y) = ((u >> 16) as usize, (v >> 16) as usize);
    let (fx, fy) = (((u >> 8) & 255) as u32, ((v >> 8) & 255) as u32);
    let x1 = (x + 1).min(SIDE - 1);
    let y1 = (y + 1).min(SIDE - 1);
    let top = mix(texel(y * SIDE + x), texel(y * SIDE + x1), fx, 128);
    let bot = mix(texel(y1 * SIDE + x), texel(y1 * SIDE + x1), fx, 128);
    mix(top, bot, fy, 128)
}

/// The loaded cover as a rotated rounded card: centre (cx, cy) px, side `size`
/// px, rotation given by sin/cos in Q16, with a dimming `shade`.
pub fn cover_turned(cx: i32, cy: i32, size: i32, radius: i32, sin: i32, cos: i32, shade: u32) {
    let ext = (size * 3) / 4 + 2; // covers the rotated square's reach
    let (y0, y1) = rows(cy - ext, cy + ext);
    let half = size * 8; // Q4
    let rr = radius * 16;
    let tscale = ((SIDE as i32) << 16) / size; // Q16 texels per pixel
    for py in y0..y1 {
        let dy = (py - cy) * 16 + 8;
        for px in (cx - ext).max(0)..(cx + ext).min(W) {
            let dx = (px - cx) * 16 + 8;
            // Inverse rotation into the card's frame (Q4).
            let lx = ((dx * cos + dy * sin) >> 16) as i32;
            let ly = ((-dx * sin + dy * cos) >> 16) as i32;
            if lx.abs() > half + 16 || ly.abs() > half + 16 {
                continue;
            }
            let d = box_d(lx, ly, 0, 0, half, half, rr);
            let cv = cover(d);
            if cv == 0 {
                continue;
            }
            let u = ((lx + half) * tscale) >> 4;
            let v = ((ly + half) * tscale) >> 4;
            let mut col = scale_rgb(sample(u, v), shade);
            if d > -36 {
                col = mix(col, 0xffffff, 30, 128);
            }
            plot(px, py, col, cv);
        }
    }
}

// ---------- text ----------

fn glyph_index(f: &Font, b: u8) -> Option<usize> {
    let code = match b {
        0xB7 => 127,
        32..=126 => b,
        _ => return None,
    };
    let g = f.map[(code - 32) as usize];
    (g != 255).then_some(g as usize)
}

fn kern(f: &Font, a: usize, b: usize) -> i32 {
    for &(x, y, k) in f.kern {
        if x as usize == a && y as usize == b {
            return k as i32;
        }
    }
    0
}

/// Width in pixels of `s` with extra `track` quarter-pixels between letters.
pub fn measure(f: &Font, s: &str, track: i32) -> i32 {
    let mut pen = 0;
    let mut prev = None;
    for b in s.bytes() {
        if let Some(g) = glyph_index(f, b) {
            if let Some(p) = prev {
                pen += kern(f, p, g) + track;
            }
            pen += f.glyphs[g].adv as i32;
            prev = Some(g);
        }
    }
    (pen + 2) >> 2
}

/// Draws `s` with its baseline at `y`; returns the pen's end x.
pub fn text(f: &Font, x: i32, y: i32, s: &str, c: u32, a: u32, track: i32) -> i32 {
    let mut pen = x * 4;
    let mut prev = None;
    for b in s.bytes() {
        let Some(gi) = glyph_index(f, b) else { continue };
        if let Some(p) = prev {
            pen += kern(f, p, gi) + track;
        }
        prev = Some(gi);
        let g = &f.glyphs[gi];
        let gx = ((pen + 2) >> 2) + g.x as i32;
        let gy = y - g.top as i32;
        pen += g.adv as i32;
        let (w, h) = (g.w as i32, g.h as i32);
        let (r0, r1) = rows(gy, gy + h);
        if w == 0 || r0 >= r1 {
            continue;
        }
        let mut k = g.off as usize * 2; // nibble cursor
        let nib = |k: usize| (f.data[k >> 1] >> if k & 1 == 0 { 4 } else { 0 }) & 15;
        let mut i = 0;
        let total = w * h;
        while i < total {
            let v = nib(k);
            k += 1;
            let (alpha, run) = if v == 0 || v == 15 {
                let n = nib(k) as i32 + 1;
                k += 1;
                (v as u32, n)
            } else {
                (v as u32, 1)
            };
            if alpha != 0 {
                for j in i..i + run {
                    let (px, py) = (gx + j % w, gy + j / w);
                    if py >= r0 && py < r1 && px >= 0 && px < W {
                        plot(px, py, c, ((alpha * 17 + (alpha >> 3)) * a) >> 8);
                    }
                }
            }
            i += run;
        }
    }
    (pen + 2) >> 2
}

/// Fixed-point sine and cosine (Q16) of an angle in tenths of a degree.
pub fn sincos(deci: i32) -> (i32, i32) {
    // Small-angle polynomial, plenty for the cover fan's few degrees.
    let x = deci * 114; // radians in Q16 (pi/1800 * 65536 = 114.4)
    let x2 = (x * x) >> 16;
    let x3 = (x2 * x) >> 16;
    let x4 = (x2 * x2) >> 16;
    let s = x - x3 / 6 + ((x3 * x2) >> 16) / 120;
    let c = 65536 - x2 / 2 + x4 / 24;
    (s, c)
}
