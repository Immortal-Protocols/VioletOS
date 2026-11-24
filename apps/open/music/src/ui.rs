//! Music's screen: a static layer that changes with the track, and the
//! dynamic band (waveform, times, controls, spectrum) redrawn ten times a second.
use violet::{mix, rgb, Canvas, Color, Fonts, Rect, H, W};

use crate::player::{Catalog, Player, BANDS};

pub const HOME: Rect = Rect::new(28, 34, 188, 92);
pub const COVER: Rect = Rect::new(110, 150, 610, 650);
const WAVE: Rect = Rect::new(50, 880, 670, 940);
const PREV: (i32, i32) = (180, 1080);
const PLAY: (i32, i32) = (360, 1080);
const NEXT: (i32, i32) = (540, 1080);
const SPEC: Rect = Rect::new(50, 1170, 670, 1240);
const LIST_TOP: i32 = 1262;
const ROW_H: i32 = 33;
pub const DYNAMIC: Rect = Rect::new(0, 860, W as i32, 1250);
/// The parts of DYNAMIC that change while a song plays: the waveform and its
/// times, and the spectrum (with the status line above it). The controls
/// between them change only with play and pause.
pub const PROGRESS: Rect = Rect::new(0, 860, W as i32, 990);
pub const CONTROLS: Rect = Rect::new(0, 990, W as i32, 1140);
pub const SPECTRUM: Rect = Rect::new(0, 1140, W as i32, 1250);
const WHITE: Color = rgb(255, 255, 255);

/// A cover, already scaled to the size it is drawn.
pub struct Cover {
    pub px: Vec<Color>, // COVER.width() x COVER.height()
    pub palette: Palette,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub deep: Color,
    pub mid: Color,
    pub accent: Color,
    pub glow: Color,
}

impl Default for Palette {
    fn default() -> Palette {
        Palette { deep: rgb(20, 14, 30), mid: rgb(40, 28, 60), accent: rgb(0xe6, 0x8c, 0xc8), glow: rgb(90, 50, 110) }
    }
}

fn channels(c: Color) -> [f32; 3] {
    [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32]
}

/// Decode a JPEG cover, scale it to the drawn size and pick its colours: the
/// average for the background and the most vivid light colour for accents.
pub fn cover(jpeg: &[u8]) -> Option<Cover> {
    let mut dec = jpeg_decoder::Decoder::new(jpeg);
    let data = dec.decode().ok()?;
    let info = dec.info()?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    let px_at = |x: usize, y: usize| -> [f32; 3] {
        let i = y * sw + x;
        match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => [data[3 * i] as f32, data[3 * i + 1] as f32, data[3 * i + 2] as f32],
            jpeg_decoder::PixelFormat::L8 => [data[i] as f32; 3],
            _ => [128.0; 3],
        }
    };
    let (dw, dh) = (COVER.width() as usize, COVER.height() as usize);
    let mut px = Vec::with_capacity(dw * dh);
    // Box filter: average the source pixels under each destination pixel.
    for y in 0..dh {
        let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1));
        for x in 0..dw {
            let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1));
            let mut sum = [0.0f32; 3];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = px_at(xx, yy);
                    (0..3).for_each(|k| sum[k] += p[k]);
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            px.push(rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8));
        }
    }
    let (mut avg, mut count) = ([0.0f32; 3], 0.0);
    let (mut best, mut best_score) = (rgb(230, 120, 170), -1.0f32);
    for y in (0..dh).step_by(4) {
        for x in (0..dw).step_by(4) {
            let c = px[y * dw + x];
            let p = channels(c);
            (0..3).for_each(|k| avg[k] += p[k]);
            count += 1.0;
            let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            let score = (mx - mn) * (0.4 + mx / 255.0);
            if mx > 110.0 && score > best_score {
                best = c;
                best_score = score;
            }
        }
    }
    let avg = rgb((avg[0] / count) as u8, (avg[1] / count) as u8, (avg[2] / count) as u8);
    let black = rgb(8, 8, 14);
    let palette = Palette { deep: mix(avg, black, 0.78), mid: mix(avg, black, 0.55), accent: mix(best, WHITE, 0.15), glow: mix(best, black, 0.35) };
    Some(Cover { px, palette })
}

fn clock(sec: f32) -> String {
    let s = sec as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Everything that changes only with the track.
pub fn draw_static(c: &mut Canvas, f: &Fonts, cat: &Catalog, cover: Option<&Cover>, current: Option<usize>) {
    let pal = cover.map_or(Palette::default(), |c| c.palette);
    let (cw, ch) = (COVER.width() as usize, COVER.height() as usize);
    // Background: the cover blurred to 9x20 cells, darkened towards the bottom.
    match cover {
        Some(cv) => {
            let (gw, gh) = (9usize, 20usize);
            let mut grid = vec![[0.0f32; 3]; gw * gh];
            for (gi, cell) in grid.iter_mut().enumerate() {
                let (gx, gy) = (gi % gw, gi / gw);
                let mut n = 0.0f32;
                for y in (gy * ch / gh..(gy + 1) * ch / gh).step_by(3) {
                    for x in (gx * cw / gw..(gx + 1) * cw / gw).step_by(3) {
                        let p = channels(cv.px[y * cw + x]);
                        (0..3).for_each(|k| cell[k] += p[k]);
                        n += 1.0;
                    }
                }
                (0..3).for_each(|k| cell[k] /= n.max(1.0));
            }
            for y in 0..H {
                let fy = (y as f32 + 0.5) / H as f32 * gh as f32 - 0.5;
                let (y0, ty) = (fy.floor().clamp(0.0, (gh - 1) as f32) as usize, (fy - fy.floor()).clamp(0.0, 1.0));
                let y1 = (y0 + 1).min(gh - 1);
                let dark = 0.45 + 0.5 * y as f32 / H as f32;
                for x in 0..W {
                    let fx = (x as f32 + 0.5) / W as f32 * gw as f32 - 0.5;
                    let (x0, tx) = (fx.floor().clamp(0.0, (gw - 1) as f32) as usize, (fx - fx.floor()).clamp(0.0, 1.0));
                    let x1 = (x0 + 1).min(gw - 1);
                    let v = |k: usize| {
                        let top = grid[y0 * gw + x0][k] * (1.0 - tx) + grid[y0 * gw + x1][k] * tx;
                        let bottom = grid[y1 * gw + x0][k] * (1.0 - tx) + grid[y1 * gw + x1][k] * tx;
                        top * (1.0 - ty) + bottom * ty
                    };
                    c.px[y * W + x] = mix(rgb(v(0) as u8, v(1) as u8, v(2) as u8), pal.deep, dark);
                }
            }
        }
        None => {
            let b = c.bounds();
            c.gradient(b, pal.mid, pal.deep);
        }
    }
    c.round_rect_alpha(HOME, 29.0, WHITE, 0.13);
    f.medium(23.0).draw(c, "‹  Home", HOME.x0 + 28, HOME.y0 + 38, rgb(245, 240, 250));
    let tiny = f.medium(20.0);
    let label = format!("{}  ·  {}", cat.name, cat.album);
    tiny.draw(c, &label, W as i32 - 36 - tiny.width(&label), 70, pal.accent);
    match cover {
        Some(cv) => {
            for k in (4..=24).rev().step_by(4) {
                c.round_rect_alpha(COVER.inset(-k).offset(0, 14), 34.0 + k as f32, rgb(0, 0, 0), 0.05);
            }
            // The cover with rounded corners (radius 30).
            let r = 30.0f32;
            for y in 0..ch {
                for x in 0..cw {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let dx = (r - fx).max(fx - (cw as f32 - r)).max(0.0);
                    let dy = (r - fy).max(fy - (ch as f32 - r)).max(0.0);
                    let a = (0.5 - ((dx * dx + dy * dy).sqrt() - r)).clamp(0.0, 1.0);
                    c.blend(COVER.x0 + x as i32, COVER.y0 + y as i32, cv.px[y * cw + x], a);
                }
            }
        }
        None => c.round_rect(COVER, 30.0, pal.glow),
    }
    let (title, body, small) = (f.bold(44.0), f.regular(28.0), f.medium(23.0));
    match current {
        Some(i) => {
            let t = &cat.tracks[i];
            title.centered(c, &title.fit(&t.title, 640), 0, W as i32, 728, WHITE);
            body.centered(c, &t.artist, 0, W as i32, 774, rgb(225, 220, 235));
            small.centered(c, &format!("{} BPM   ·   {}", t.bpm, t.key), 0, W as i32, 816, pal.accent);
        }
        None => {
            title.centered(c, &cat.album, 0, W as i32, 728, WHITE);
            body.centered(c, "Tap a song to play", 0, W as i32, 774, rgb(225, 220, 235));
        }
    }
    // The album, one row per song.
    for (i, t) in cat.tracks.iter().enumerate() {
        let y = LIST_TOP + i as i32 * ROW_H;
        let mut ink = rgb(232, 228, 240);
        if Some(i) == current {
            c.round_rect_alpha(Rect::new(36, y - 2, W as i32 - 36, y + ROW_H - 2), 12.0, pal.accent, 0.22);
            ink = WHITE;
        }
        tiny.draw(c, &format!("{:02}", t.number), 54, y + 24, pal.accent);
        tiny.draw(c, &tiny.fit(&t.title, 380), 104, y + 24, ink);
        let meta = format!("{} BPM  {}", t.bpm, clock(t.duration_ms as f32 / 1000.0));
        tiny.draw(c, &meta, W as i32 - 54 - tiny.width(&meta), y + 24, rgb(185, 178, 200));
    }
}

/// The seek bar, times, controls and spectrum over the static layer.
pub fn draw_dynamic(c: &mut Canvas, layer: &Canvas, f: &Fonts, cat: &Catalog, p: &Player, pal: Palette, status: &str) {
    c.copy_rows(layer, DYNAMIC.y0, DYNAMIC.y1);
    let current = p.track.load(std::sync::atomic::Ordering::Relaxed);
    let tiny = f.medium(20.0);
    if current >= 0 {
        let t = &cat.tracks[current as usize];
        let length = t.duration_ms as f32 / 1000.0;
        let pos = p.position().min(length);
        let played = pos / length;
        let bars = 62;
        let w = WAVE.width() as f32 / bars as f32;
        let mid = (WAVE.y0 + WAVE.y1) as f32 / 2.0;
        for b in 0..bars {
            let v = if t.peaks.is_empty() { 0.25 } else { t.peaks[b * t.peaks.len() / bars] };
            let h = (v * WAVE.height() as f32 * 1.6).max(4.0).min(WAVE.height() as f32);
            let x0 = WAVE.x0 as f32 + b as f32 * w;
            let r = Rect::new((x0 + 1.5) as i32, (mid - h / 2.0) as i32, (x0 + w - 1.5) as i32, (mid + h / 2.0) as i32);
            if (b as f32) / (bars as f32) < played {
                c.round_rect(r, 3.0, pal.accent);
            } else {
                c.round_rect_alpha(r, 3.0, WHITE, 0.22);
            }
        }
        tiny.draw(c, &clock(pos), WAVE.x0, WAVE.y1 + 38, rgb(225, 220, 235));
        let end = clock(length);
        tiny.draw(c, &end, WAVE.x1 - tiny.width(&end), WAVE.y1 + 38, rgb(225, 220, 235));
    }
    // Previous, play or pause, next.
    let (px, py) = (PLAY.0 as f32, PLAY.1 as f32);
    c.circle(px, py, 62.0, WHITE);
    if current >= 0 && !p.paused.load(std::sync::atomic::Ordering::Relaxed) {
        c.round_rect(Rect::new(PLAY.0 - 20, PLAY.1 - 24, PLAY.0 - 6, PLAY.1 + 24), 3.0, pal.deep);
        c.round_rect(Rect::new(PLAY.0 + 6, PLAY.1 - 24, PLAY.0 + 20, PLAY.1 + 24), 3.0, pal.deep);
    } else {
        c.triangle([[px - 16.0, py - 27.0], [px - 16.0, py + 27.0], [px + 28.0, py]], pal.deep);
    }
    for (at, dir) in [(PREV, -1.0f32), (NEXT, 1.0)] {
        let (x, y) = (at.0 as f32, at.1 as f32);
        c.triangle([[x - 18.0 * dir, y - 20.0], [x - 18.0 * dir, y + 20.0], [x + 14.0 * dir, y]], WHITE);
        let bx = (x + 14.0 * dir) as i32 + if dir > 0.0 { 2 } else { -2 };
        c.round_rect(Rect::new(bx - 3, y as i32 - 20, bx + 3, y as i32 + 20), 2.0, WHITE);
    }
    // The spectrum.
    let bw = SPEC.width() as f32 / BANDS as f32;
    for (b, v) in p.bands().iter().enumerate() {
        let h = (v * SPEC.height() as f32).max(6.0);
        let x0 = SPEC.x0 as f32 + b as f32 * bw;
        let r = Rect::new((x0 + 5.0) as i32, SPEC.y1 - h as i32, (x0 + bw - 5.0) as i32, SPEC.y1);
        c.round_rect_alpha(r, 5.0, mix(pal.accent, WHITE, 0.15 * v), 0.55 + 0.45 * v);
    }
    if !status.is_empty() {
        tiny.centered(c, status, 0, W as i32, 1166, rgb(255, 200, 200));
    }
}

pub enum Hit {
    Home,
    Play,
    Prev,
    Next,
    Song(usize),
}

/// Which control or song a touch lands on.
pub fn hit(x: i32, y: i32, songs: usize) -> Option<Hit> {
    let near = |at: (i32, i32), r: i32| (x - at.0).pow(2) + (y - at.1).pow(2) < r * r;
    if HOME.inset(-12).contains(x, y) {
        Some(Hit::Home)
    } else if near(PLAY, 80) {
        Some(Hit::Play)
    } else if near(PREV, 70) {
        Some(Hit::Prev)
    } else if near(NEXT, 70) {
        Some(Hit::Next)
    } else if y >= LIST_TOP && y < LIST_TOP + songs as i32 * ROW_H {
        Some(Hit::Song(((y - LIST_TOP) / ROW_H) as usize))
    } else {
        None
    }
}
