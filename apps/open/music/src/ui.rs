//! Music's screen: a static layer that changes with the track, and the
//! dynamic band (waveform, times, controls, spectrum) redrawn ten times a second.
use violet::{Canvas, Color, Fonts, Rect, W};
use crate::style::{self, INK, LINE, MUTED, SURFACE};

use crate::player::{Catalog, Player, BANDS};

pub const HOME: Rect = Rect::new(28, 34, 188, 92);
pub const COVER: Rect = Rect::new(110, 150, 610, 650);
const WAVE: Rect = Rect::new(50, 880, 670, 940);
const PREV: (i32, i32) = (180, 1080);
const PLAY: (i32, i32) = (360, 1080);
const NEXT: (i32, i32) = (540, 1080);
const SPEC: Rect = Rect::new(50, 1170, 670, 1240);
pub const DYNAMIC: Rect = Rect::new(0, 860, W as i32, 1250);
/// The parts of DYNAMIC that change while a song plays: the waveform and its
/// times, and the spectrum (with the status line above it). The controls
/// between them change only with play and pause.
pub const PROGRESS: Rect = Rect::new(0, 860, W as i32, 990);
pub const CONTROLS: Rect = Rect::new(0, 990, W as i32, 1140);
pub const SPECTRUM: Rect = Rect::new(0, 1140, W as i32, 1250);

/// A cover, already scaled to the size it is drawn.
pub struct Cover {
    pub px: Vec<Color>, // COVER.width() x COVER.height()
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
            px.push(violet::rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8));
        }
    }
    Some(Cover { px })
}

fn clock(sec: f32) -> String {
    let s = sec as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Everything that changes only with the track.
pub fn draw_static(c: &mut Canvas, f: &Fonts, cat: &Catalog, cover: Option<&Cover>, current: Option<usize>) {
    c.fill(c.bounds(), SURFACE);
    style::outline(c, HOME, 26.0);
    f.medium(23.0).centered(c, "‹ Home", HOME.x0, HOME.x1, 71, INK);
    f.bold(30.0).centered(c, "Locomo Music", 188, 720, 73, INK);
    let (cw, ch) = (COVER.width() as usize, COVER.height() as usize);
    match cover {
        Some(cv) => {
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
        None => {
            style::outline(c, COVER, 28.0);
            style::record(c, 360.0, 400.0, 188.0, INK, SURFACE);
        }
    }
    let (title, body, small) = (f.bold(44.0), f.regular(28.0), f.medium(23.0));
    match current {
        Some(i) => {
            let t = &cat.tracks[i];
            title.centered(c, &title.fit(&t.title, 640), 0, W as i32, 728, INK);
            body.centered(c, &t.artist, 0, W as i32, 774, MUTED);
            let details = if t.bpm == 0 { t.key.clone() } else { format!("{} BPM   ·   {}", t.bpm, t.key) };
            small.centered(c, &details, 0, W as i32, 816, INK);
        }
        None => {
            title.centered(c, &cat.album, 0, W as i32, 728, INK);
            body.centered(c, "Browse the Library to play", 0, W as i32, 774, MUTED);
        }
    }

}

/// The seek bar, times, controls and spectrum over the static layer.
pub fn draw_dynamic(c: &mut Canvas, layer: &Canvas, f: &Fonts, cat: &Catalog, p: &Player, status: &str) {
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
                c.round_rect(r, 3.0, INK);
            } else {
                c.round_rect(r, 3.0, LINE);
            }
        }
        tiny.draw(c, &clock(pos), WAVE.x0, WAVE.y1 + 38, MUTED);
        let end = clock(length);
        tiny.draw(c, &end, WAVE.x1 - tiny.width(&end), WAVE.y1 + 38, MUTED);
    }
    // Previous, play or pause, next.
    let (px, py) = (PLAY.0 as f32, PLAY.1 as f32);
    c.circle(px, py, 62.0, INK);
    if current >= 0 && !p.paused.load(std::sync::atomic::Ordering::Relaxed) {
        c.round_rect(Rect::new(PLAY.0 - 20, PLAY.1 - 24, PLAY.0 - 6, PLAY.1 + 24), 3.0, SURFACE);
        c.round_rect(Rect::new(PLAY.0 + 6, PLAY.1 - 24, PLAY.0 + 20, PLAY.1 + 24), 3.0, SURFACE);
    } else {
        c.triangle([[px - 16.0, py - 27.0], [px - 16.0, py + 27.0], [px + 28.0, py]], SURFACE);
    }
    for (at, dir) in [(PREV, -1.0f32), (NEXT, 1.0)] {
        let (x, y) = (at.0 as f32, at.1 as f32);
        c.triangle([[x - 18.0 * dir, y - 20.0], [x - 18.0 * dir, y + 20.0], [x + 14.0 * dir, y]], INK);
        let bx = (x + 14.0 * dir) as i32 + if dir > 0.0 { 2 } else { -2 };
        c.round_rect(Rect::new(bx - 3, y as i32 - 20, bx + 3, y as i32 + 20), 2.0, INK);
    }
    // The spectrum.
    let bw = SPEC.width() as f32 / BANDS as f32;
    for (b, v) in p.bands().iter().enumerate() {
        let h = (v * SPEC.height() as f32).max(6.0);
        let x0 = SPEC.x0 as f32 + b as f32 * bw;
        let r = Rect::new((x0 + 5.0) as i32, SPEC.y1 - h as i32, (x0 + bw - 5.0) as i32, SPEC.y1);
        c.round_rect_alpha(r, 5.0, INK, 0.55 + 0.45 * v);
    }
    if !status.is_empty() {
        tiny.centered(c, status, 0, W as i32, 1166, MUTED);
    }
}

pub enum Hit {
    Home,
    Play,
    Prev,
    Next,
}

/// Which control or song a touch lands on.
pub fn hit(x: i32, y: i32) -> Option<Hit> {
    let near = |at: (i32, i32), r: i32| (x - at.0).pow(2) + (y - at.1).pow(2) < r * r;
    if HOME.inset(-12).contains(x, y) {
        Some(Hit::Home)
    } else if near(PLAY, 80) {
        Some(Hit::Play)
    } else if near(PREV, 70) {
        Some(Hit::Prev)
    } else if near(NEXT, 70) {
        Some(Hit::Next)
    } else {
        None
    }
}
