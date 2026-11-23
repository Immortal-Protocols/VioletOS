//! Music: the Glass Orchard album, streamed from the music library.
//!
//! Two full-resolution screens. The library shows the covers fanned like a
//! hand of cards over an aurora in the playing cover's colours, the album, its
//! ten songs and a mini player. The player shows the cover with a glow, a live
//! spectrum from Audio's sixteen bands, a scrubber drawn from the song's
//! loudness peaks, transport, shuffle/repeat and volume. Everything is drawn
//! here (see gfx.rs); only the bands of rows that change are presented.
use crate::{protocol::*, sdk::*};

#[path = "catalog.rs"]
mod catalog;
#[path = "font.rs"]
mod font;
#[path = "gfx.rs"]
mod gfx;

use catalog::{Track, TRACKS};
use font::{HERO, META, ROW, TITLE};
use gfx::*;

/// Fakes the music service (playback, spectrum, progress) to preview the UI.
const DEMO: bool = false;

const NIGHT: u32 = 0x07070b;
const RAISED: u32 = 0x1a1a24;
const TEXT: u32 = 0xf5f2fb;
const MUTED: u32 = 0x9c99ac;
const FAINT: u32 = 0x5e5b6c;
const LIVE: u32 = 0x6cf0b4;
const WARN: u32 = 0xffc56e;

const FRAME_MS: u32 = 16;
/// How long the covers take to deal out.
const DEAL_MS: u32 = 450;
/// Rows repainted per frame after a song change.
const WIPE_ROWS: i32 = 120;

#[derive(Clone, Copy, PartialEq)]
struct Status {
    track: u32,
    state: u32,
    pos: u32,
    bands: [u8; 16],
    ahead: u32,
    len: u32,
}
impl Status {
    const NONE: Status = Status { track: 0, state: MUSIC_STATE_STOPPED, pos: 0, bands: [0; 16], ahead: 0, len: 0 };
    fn playing(&self) -> bool {
        self.track != 0 && matches!(self.state, MUSIC_STATE_PLAYING | MUSIC_STATE_BUFFERING)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Library,
    Player,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Home,
    Close,
    Toggle,
    Next,
    Prev,
    Shuffle,
    Repeat,
    Open,
    Play(usize),
    Seek(i32),
    Volume(i32),
    VolDown,
    VolUp,
}

struct App {
    screen: Screen,
    st: Status,
    sel: usize,
    volume: u32,
    shuffle: bool,
    repeat: bool,
    caps: [i32; 16],
    breath: i32,
    /// The fan deal's start: (where it began, when in ms).
    deal: (i32, u32),
    /// The previous frame's time, for time-based easing.
    last_ms: u32,
    /// The next row of a strip-by-strip repaint (H when none is running).
    wipe: i32,
    /// Off-screen fan card sprites, and the song (plus one) they show.
    tiles: [Tile; 5],
    sprites_for: usize,
    /// The song (plus one) whose colours the cached background has.
    bg_for: usize,
    /// Runtime granted the off-screen buffer (sprites and the cache need it).
    offscreen: bool,
    /// Sprites made so far for the song being prepared (5 when none is), and
    /// the rows of the current one.
    prep: usize,
    prep_row: i32,
    fan: i32,
    frame: u32,
    flash: Hit,
    flash_frames: u32,
    buffering_since: u32,
    advanced: u32,
    seed: u32,
    keys: [u32; 8],
}

// ---------- the music service ----------

struct Demo {
    track: u32,
    state: u32,
    pos: u32,
    stamp: u32,
}
static mut FAKE: Demo = Demo { track: 0, state: 0, pos: 0, stamp: 0 };

fn now_ms() -> u32 {
    unsafe { ticks() / ticks_per_ms.max(1) }
}

fn status(frame: u32) -> Status {
    if DEMO {
        let d = unsafe { &mut *core::ptr::addr_of_mut!(FAKE) };
        let t = now_ms();
        if d.state == MUSIC_STATE_BUFFERING && t.wrapping_sub(d.stamp) > 1500 {
            d.state = MUSIC_STATE_PLAYING;
            d.stamp = t;
        }
        if d.state == MUSIC_STATE_PLAYING {
            d.pos += t.wrapping_sub(d.stamp);
            d.stamp = t;
            if d.track != 0 && d.pos >= TRACKS[d.track as usize - 1].ms {
                d.state = MUSIC_STATE_ENDED;
            }
        }
        let mut bands = [0u8; 16];
        if d.state == MUSIC_STATE_PLAYING {
            for (i, b) in bands.iter_mut().enumerate() {
                let n = noise(i as i32, (frame / 3) as i32);
                let shape = 230 - i as i32 * 9;
                let wob = (n & 127) as i32 - 40;
                *b = (shape + wob - ((frame as i32 * 7 + i as i32 * 23) % 50)).clamp(8, 255) as u8;
            }
        }
        return Status { track: d.track, state: d.state, pos: d.pos, bands, ahead: 63, len: 0 };
    }
    let r = request(GET, MUSIC, 0);
    if r.op != OK {
        return Status { state: MUSIC_STATE_OFFLINE, ..Status::NONE };
    }
    let t = r.text();
    let mut bands = [0u8; 16];
    bands.copy_from_slice(&t[..16]);
    Status {
        track: if (1..=MUSIC_TRACKS).contains(&r.value) { r.value } else { 0 },
        state: r.arg & 15,
        pos: r.arg >> 4,
        bands,
        ahead: u16::from_le_bytes([t[16], t[17]]) as u32,
        len: u32::from_le_bytes([t[20], t[21], t[22], t[23]]),
    }
}

fn music(cmd: u32, arg: u32) {
    if DEMO {
        let d = unsafe { &mut *core::ptr::addr_of_mut!(FAKE) };
        let t = now_ms();
        match cmd {
            MUSIC_PLAY => {
                d.track = arg & 255;
                d.pos = (arg >> 8) * 1000;
                d.state = MUSIC_STATE_BUFFERING;
            }
            MUSIC_PAUSE => d.state = MUSIC_STATE_PAUSED,
            MUSIC_RESUME => d.state = MUSIC_STATE_PLAYING,
            MUSIC_STOP => d.state = MUSIC_STATE_STOPPED,
            MUSIC_SEEK => d.pos = arg * 1000,
            _ => {}
        }
        d.stamp = t;
        return;
    }
    request_with(EXECUTE, MUSIC, cmd, arg);
}

// ---------- text helpers ----------

struct Buf {
    b: [u8; 48],
    n: usize,
}
impl Buf {
    fn new() -> Buf {
        Buf { b: [0; 48], n: 0 }
    }
    fn s(&mut self, s: &str) -> &mut Buf {
        for &c in s.as_bytes() {
            if self.n < 48 {
                self.b[self.n] = c;
                self.n += 1;
            }
        }
        self
    }
    fn num(&mut self, v: u32) -> &mut Buf {
        let mut d = [0u8; 10];
        let mut k = 0;
        let mut v = v;
        loop {
            d[k] = b'0' + (v % 10) as u8;
            k += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while k > 0 {
            k -= 1;
            if self.n < 48 {
                self.b[self.n] = d[k];
                self.n += 1;
            }
        }
        self
    }
    fn time(&mut self, ms: u32) -> &mut Buf {
        let s = ms / 1000;
        self.num(s / 60).s(":");
        if s % 60 < 10 {
            self.s("0");
        }
        self.num(s % 60)
    }
    fn get(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.b[..self.n]) }
    }
}

fn center_text(f: &font::Font, cx: i32, y: i32, s: &str, c: u32, a: u32, track: i32) {
    let w = measure(f, s, track);
    text(f, cx - w / 2, y, s, c, a, track);
}
fn right_text(f: &font::Font, x1: i32, y: i32, s: &str, c: u32, a: u32, track: i32) {
    let w = measure(f, s, track);
    text(f, x1 - w, y, s, c, a, track);
}

fn visible(y0: i32, y1: i32) -> bool {
    let (a, b) = unsafe { CLIP };
    y1 > a && y0 < b
}
static mut CLIP: (i32, i32) = (0, H);
fn set_clip(y0: i32, y1: i32) {
    unsafe { CLIP = (y0, y1) };
    clip(y0, y1);
}

// ---------- icons (centre and size in pixels) ----------

fn q(c: i32, s: i32, k: i32) -> i32 {
    // c px + k/100 of s px, in Q4
    c * 16 + s * k * 16 / 100
}

fn icon_play(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    shape(&[Prim::Poly([p(-24, -32), p(36, 0), p(-24, 32), p(-24, 32)], s * 16 * 7 / 100)], c, a);
}
fn icon_pause(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let (hw, hh, r) = (s * 16 * 10 / 100, s * 16 * 34 / 100, s * 16 * 5 / 100);
    shape(&[Prim::Box(q(cx, s, -17), cy * 16, hw, hh, r), Prim::Box(q(cx, s, 17), cy * 16, hw, hh, r)], c, a);
}
fn icon_skip(cx: i32, cy: i32, s: i32, dir: i32, c: u32, a: u32) {
    let p = |x: i32, y| (q(cx, s, x * dir), q(cy, s, y));
    let tri = if dir > 0 { [p(-30, -30), p(16, 0), p(-30, 30), p(-30, 30)] } else { [p(-30, 30), p(16, 0), p(-30, -30), p(-30, -30)] };
    shape(&[Prim::Poly(tri, s * 16 * 5 / 100), Prim::Box(q(cx, s, 27 * dir), cy * 16, s * 16 * 6 / 100, s * 16 * 32 / 100, s * 16 * 5 / 100)], c, a);
}
fn icon_home(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    let r = s * 16 * 6 / 100;
    let seg = |a: (i32, i32), b: (i32, i32)| Prim::Seg(a.0, a.1, b.0, b.1, r);
    shape(
        &[
            seg(p(-40, -2), p(0, -38)),
            seg(p(0, -38), p(40, -2)),
            seg(p(-28, -12), p(-28, 34)),
            seg(p(-28, 34), p(28, 34)),
            seg(p(28, 34), p(28, -12)),
            Prim::Box(q(cx, s, 0), q(cy, s, 22), s * 16 * 8 / 100, s * 16 * 14 / 100, r),
        ],
        c,
        a,
    );
}
fn icon_chevron(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    let r = s * 16 * 6 / 100;
    let (l, m, rt) = (p(-30, -12), p(0, 18), p(30, -12));
    shape(&[Prim::Seg(l.0, l.1, m.0, m.1, r), Prim::Seg(m.0, m.1, rt.0, rt.1, r)], c, a);
}
fn icon_shuffle(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    let r = s * 16 * 5 / 100;
    let seg = |a: (i32, i32), b: (i32, i32)| Prim::Seg(a.0, a.1, b.0, b.1, r);
    shape(
        &[
            seg(p(-40, -24), p(-14, -24)),
            seg(p(-14, -24), p(14, 24)),
            seg(p(14, 24), p(38, 24)),
            seg(p(-40, 24), p(-14, 24)),
            seg(p(-14, 24), p(14, -24)),
            seg(p(14, -24), p(38, -24)),
            seg(p(26, -36), p(40, -24)),
            seg(p(40, -24), p(26, -12)),
            seg(p(26, 12), p(40, 24)),
            seg(p(40, 24), p(26, 36)),
        ],
        c,
        a,
    );
}
fn icon_repeat(cx: i32, cy: i32, s: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    let r = s * 16 * 5 / 100;
    let seg = |a: (i32, i32), b: (i32, i32)| Prim::Seg(a.0, a.1, b.0, b.1, r);
    shape(
        &[
            seg(p(-38, 8), p(-38, -22)),
            seg(p(-38, -22), p(34, -22)),
            seg(p(22, -34), p(36, -22)),
            seg(p(36, -22), p(22, -10)),
            seg(p(38, -8), p(38, 22)),
            seg(p(38, 22), p(-34, 22)),
            seg(p(-22, 10), p(-36, 22)),
            seg(p(-36, 22), p(-22, 34)),
        ],
        c,
        a,
    );
}
fn icon_speaker(cx: i32, cy: i32, s: i32, waves: i32, c: u32, a: u32) {
    let p = |x, y| (q(cx, s, x), q(cy, s, y));
    let r = s * 16 * 4 / 100;
    let mut prims = [Prim::Box(q(cx, s, -26), cy * 16, s * 16 * 12 / 100, s * 16 * 13 / 100, r); 4];
    prims[1] = Prim::Poly([p(-22, -12), p(6, -36), p(6, 36), p(-22, 12)], r);
    let mut n = 2;
    for w in 0..waves {
        prims[n] = Prim::Seg(q(cx, s, 18 + w * 14), q(cy, s, -(12 + w * 9)), q(cx, s, 18 + w * 14), q(cy, s, 12 + w * 9), r);
        n += 1;
    }
    shape(&prims[..n], c, a);
}

/// A glass circle button.
fn glass_button(cx: i32, cy: i32, r: i32, lit: bool) {
    let a = if lit { 70 } else { 22 };
    rbox(cx - r, cy - r, 2 * r, 2 * r, r, 0xffffff, a);
    rbox_line(cx - r, cy - r, 2 * r, 2 * r, r, 0xffffff, 30);
}

// ---------- background fields ----------

fn chan(c: u32, s: u32) -> i32 {
    ((c >> s) & 255) as i32
}
fn over(base: &mut [i32; 3], c: u32, a: i32) {
    // a in 0..=1024
    for (k, s) in [16, 8, 0].iter().enumerate() {
        base[k] += (chan(c, *s) * 256 - base[k]) * a / 1024;
    }
}
/// A radial blob's weight (0..=1024) at distance from its centre.
fn blob(x: i32, y: i32, cx: i32, cy: i32, r: i32) -> i32 {
    let (dx, dy) = (x - cx, y - cy);
    let d2 = (dx * dx + dy * dy) / ((r * r) >> 10);
    if d2 >= 1024 {
        return 0;
    }
    let u = 1024 - d2;
    u * u / 1024
}

fn paint_grid(app: &App) {
    let t = &TRACKS[app.current()];
    let player = app.screen == Screen::Player;
    if player {
        load_cover(app.current());
    }
    let grid = unsafe { &mut *core::ptr::addr_of_mut!(GRID) };
    let deep_red = {
        let r = (chan(t.deep, 16) * 16 / 10).min(255) as u32;
        r << 16 | t.deep & 0xffff
    };
    for gy in 0..GH {
        for gx in 0..GW {
            let (x, y) = (gx as i32 * 64, gy as i32 * 64);
            let mut c = [0i32; 3];
            over(&mut c, NIGHT, 1024);
            over(&mut c, t.deep, 563);
            over(&mut c, t.glow, blob(x, y, 144 + 40, 192, 1120) * 55 / 100);
            over(&mut c, t.accent, blob(x, y, 612, 560, 880) * 28 / 100);
            over(&mut c, deep_red, blob(x, y, 360, 1520, 960) * 70 / 100);
            if player {
                // The cover, cropped to fill the screen at 1.3x and heavily blurred.
                let side = SIDE as i32;
                let tx = (x - 360) * side * 64 / 2080 + side * 32; // texels, Q6
                let ty = (y - 800) * side * 64 / 2080 + side * 32;
                let mut acc = [0i32; 3];
                let mut wsum = 0;
                for j in -4..=4 {
                    for i in -4..=4 {
                        let sx = ((tx >> 6) + i).clamp(0, side - 1) as usize;
                        let sy = ((ty >> 6) + j).clamp(0, side - 1) as usize;
                        let w = (5 - i.abs()) * (5 - j.abs());
                        let v = texel(sy * SIDE + sx);
                        acc[0] += chan(v, 16) * w;
                        acc[1] += chan(v, 8) * w;
                        acc[2] += chan(v, 0) * w;
                        wsum += w;
                    }
                }
                let v = ((acc[0] / wsum) as u32) << 16 | ((acc[1] / wsum) as u32) << 8 | (acc[2] / wsum) as u32;
                over(&mut c, v, 563);
                // Veil: 25% at the top, 35% mid, 90% at the bottom.
                let f = y * 1024 / H;
                let veil = if f < 512 { 256 + (f * 102 / 512) } else { 358 + (f - 512) * 563 / 512 };
                over(&mut c, NIGHT, veil);
            } else {
                let f = y * 1024 / H;
                let veil = if f < 563 { f * 563 / 563 * 55 / 100 } else { 563 + (f - 563) * 379 / 461 };
                over(&mut c, NIGHT, veil.min(950));
            }
            grid[gy * GW + gx] = ((c[0] >> 8) as u32) << 16 | ((c[1] >> 8) as u32) << 8 | (c[2] >> 8) as u32;
        }
    }
}

// ---------- the library ----------

const LIST_Y: i32 = 676;
const ROW_H: i32 = 76;
const MINI_Y: i32 = 1450;
/// Width of the mini player's progress hairline (its panel is 680 wide).
const MINI_PROGRESS_W: i32 = 600;
const FAN_CY: i32 = 300;
const FAN_SIZE: i32 = 232;
const HERO_PLAY: (i32, i32) = (626, 562);

fn status_line(app: &App) -> (u32, &'static str) {
    let st = &app.st;
    match st.state {
        _ if st.track == 0 => (MUTED, "Ready \u{b7} 10 songs to stream"),
        MUSIC_STATE_OFFLINE => (WARN, "Offline \u{b7} connect Wi-Fi to stream"),
        MUSIC_STATE_BUFFERING if app.frame.wrapping_sub(app.buffering_since) > 90 => (WARN, "Library is waking up"),
        MUSIC_STATE_BUFFERING => (MUTED, "Connecting to the library"),
        MUSIC_STATE_PAUSED => (MUTED, "Paused"),
        MUSIC_STATE_ENDED => (MUTED, "Finished"),
        MUSIC_STATE_STOPPED => (MUTED, "Stopped"),
        _ => (LIVE, "Live from the library"),
    }
}

fn status_pill(app: &App, x1: i32, cy: i32) {
    let (c, s) = status_line(app);
    let w = measure(&META, s, 0) + 64;
    let x = x1 - w;
    rbox(x, cy - 26, w, 52, 26, 0xffffff, 16);
    rbox_line(x, cy - 26, w, 52, 26, 0xffffff, 22);
    let animate = matches!(app.st.state, MUSIC_STATE_PLAYING | MUSIC_STATE_BUFFERING) && app.st.track != 0;
    if animate {
        let p = (app.frame % 48) as i32;
        let r = 8 + p * 12 / 48;
        rbox(x + 30 - r, cy - r, 2 * r, 2 * r, r, c, ((48 - p) * 128 / 48) as u32);
    }
    rbox(x + 22, cy - 8, 16, 16, 8, c, 256);
    text(&META, x + 48, cy + 9, s, TEXT, 220, 0);
}

fn fan_positions(app: &App) -> [(usize, i32, i32, i32, i32); 5] {
    fan_at(app.current(), app.fan)
}

/// The fan for song `current` at deal progress `o` (0..=256).
fn fan_at(current: usize, o: i32) -> [(usize, i32, i32, i32, i32); 5] {
    // (track, centre x, centre y, size, angle in tenths of a degree), back to front.
    let mut out = [(0, 0, 0, 0, 0); 5];
    let order = [-2, 2, -1, 1, 0];
    for (k, d) in order.iter().enumerate() {
        let t = (current as i32 + d).rem_euclid(10) as usize;
        let cx = 360 + d * 126 * o / 256;
        let lift = if *d == 0 { 22 } else { 0 };
        let cy = FAN_CY + (d.abs() * 26 - lift) * o / 256;
        let s = FAN_SIZE * (100 - d.abs() * 9) / 100 * (218 + 38 * o / 256) / 256 * if *d == 0 { 106 } else { 100 } / 100;
        out[k] = (t, cx, cy, s, d * 80 * o / 256);
    }
    out
}

/// One fan card (cover and shadow) at its resting size and angle, centred on
/// (cx, cy). The deal slides these finished cards rather than redrawing them.
fn draw_card(t: usize, cx: i32, cy: i32, s: i32, ang: i32) {
    let hs = s / 2;
    // Coloured shadow that lifts the card off the aurora.
    glow(cx - hs + 24, cy - hs + 36, s - 48, s - 48, 40, 90, scale_rgb(TRACKS[t].glow, 110), 210);
    load_cover(t);
    if ang == 0 {
        cover_art(cx - hs, cy - hs, s, 30, 256);
    } else {
        soften();
        let (sn, cs) = sincos(ang);
        cover_turned(cx, cy, s, 30, sn, cs, 190 + 8 * o_abs(ang));
    }
}

/// A card sprite's reach from its centre: left/right, up, down (pixels).
fn card_reach(s: i32) -> (i32, i32, i32) {
    let (hs, ext) = (s / 2, (s * 3) / 4 + 2);
    (ext.max(hs + 66), ext.max(hs + 54), ext.max(hs + 78))
}

/// The off-screen tile for resting card `k`: cards pack in shelves, each
/// after the one before.
fn tile_for(app: &App, k: usize) -> Tile {
    let s = fan_at(app.current(), 256)[k].3;
    let (side, up, down) = card_reach(s);
    let (w, h) = (2 * side, up + down);
    if k == 0 {
        return Tile { x: 0, y: CACHE_Y1 + 4, w, h };
    }
    let p = app.tiles[k - 1];
    if p.x + p.w + w <= W {
        return Tile { x: p.x + p.w, y: p.y, w, h };
    }
    // A new shelf below the tallest tile on the previous one.
    let y = app.tiles[..k].iter().filter(|q| q.y == p.y).map(|q| q.y + q.h).max().unwrap_or(p.y);
    Tile { x: 0, y, w, h }
}

/// Rows of a card sprite drawn per step; steps run until the frame's budget.
const SLICE: i32 = 64;
const PREP_MS: u32 = 12;

/// Draws the next slices of the resting cards into the off-screen buffer
/// until about PREP_MS have passed. Returns true once all five are done.
fn prep_sprites(app: &mut App) -> bool {
    let start = now_ms();
    while app.prep < 5 {
        let k = app.prep;
        if app.prep_row == 0 {
            app.tiles[k] = tile_for(app, k);
        }
        let tile = app.tiles[k];
        let (t, _, _, s, ang) = fan_at(app.current(), 256)[k];
        let (side, up, _) = card_reach(s);
        let y0 = tile.y + app.prep_row;
        sprite(&tile, y0, y0 + SLICE, || draw_card(t, tile.x + side, tile.y + up, s, ang));
        app.prep_row += SLICE;
        if app.prep_row >= tile.h {
            app.prep += 1;
            app.prep_row = 0;
        }
        if now_ms().wrapping_sub(start) >= PREP_MS {
            break;
        }
    }
    app.prep == 5
}

fn draw_fan(app: &App) {
    if !visible(FAN_CY - 230, FAN_CY + 230) {
        return;
    }
    let ready = app.sprites_for == app.current() + 1;
    let rest = fan_at(app.current(), 256);
    for (k, (t, cx, cy, s, ang)) in fan_positions(app).into_iter().enumerate() {
        if ready {
            // The resting card, slid to where the deal has it now.
            let (side, up, _) = card_reach(rest[k].3);
            blit(&app.tiles[k], cx - side, cy - up);
        } else {
            draw_card(t, cx, cy, s, ang);
        }
    }
}

fn o_abs(ang: i32) -> u32 {
    (8 - ang.abs() / 20).max(0) as u32
}

fn draw_row(app: &App, i: usize) {
    let y = LIST_Y + i as i32 * ROW_H;
    if !visible(y, y + ROW_H) {
        return;
    }
    let t = &TRACKS[i];
    let current = i == app.current() && app.st.track != 0;
    if current {
        rbox(16, y + 2, 688, ROW_H - 4, 22, t.accent, 30);
    }
    load_cover(i);
    cover_art(36, y + 8, 60, 14, 256);
    if current {
        rbox(36, y + 8, 60, 60, 14, 0x000000, 110);
        // Four bars that dance with the music's real bands.
        for k in 0..4 {
            let level = if app.st.state == MUSIC_STATE_PLAYING { app.st.bands[k * 4 + 1] as i32 } else { 30 };
            let h = 8 + level * 30 / 255;
            rbox(46 + k as i32 * 11, y + 54 - h, 7, h, 3, 0xffffff, 256);
        }
    }
    let title_c = if current { t.accent } else { TEXT };
    text(&ROW, 118, y + 36, t.title, title_c, 256, -1);
    let mut b = Buf::new();
    b.num(t.bpm as u32).s(" BPM \u{b7} ").s(t.key);
    text(&META, 118, y + 64, b.get(), MUTED, 256, 0);
    let mut d = Buf::new();
    d.time(t.ms);
    right_text(&META, 680, y + 46, d.get(), if current { t.accent } else { MUTED }, 256, 0);
    if i < 9 && !current && i + 1 != app.current() {
        rbox(118, y + ROW_H - 1, 562, 1, 0, 0xffffff, 14);
    }
}

fn draw_mini(app: &App) {
    if !visible(MINI_Y - 40, H) {
        return;
    }
    let i = app.current();
    let t = &TRACKS[i];
    let (x, y, w, h) = (20, MINI_Y, 680, 120);
    glow(x + 20, y + 10, w - 40, h - 10, 36, 50, t.glow, 110);
    let left = lerp_rgb(t.deep, RAISED, 90);
    rbox(x, y, w, h, 34, RAISED, 256);
    // Horizontal tint from the cover's deep colour, strongest on the right.
    rbox_h(x, y, w, h, 34, left, 70, 148);
    rbox_line(x, y, w, h, 34, 0xffffff, 26);
    load_cover(i);
    let breathe = if app.st.playing() { 92 } else { 84 };
    let off = (92 - breathe) / 2;
    cover_art(x + 16 + off, y + 14 + off, breathe, 20, 256);
    text(&ROW, x + 128, y + 52, t.title, TEXT, 256, -1);
    let (c, s) = if app.st.track == 0 { (MUTED, catalog::ARTIST) } else { status_line(app) };
    let c = if c == LIVE { MUTED } else { c };
    text(&META, x + 128, y + 86, if c == MUTED && app.st.playing() { catalog::ARTIST } else { s }, c, 256, 0);
    let (bx, by) = (x + w - 150, y + 58);
    let lit = app.flash == Hit::Toggle && app.flash_frames > 0;
    glass_button(bx, by, 40, lit);
    if app.st.playing() {
        icon_pause(bx, by, 40, TEXT, 256);
    } else {
        icon_play(bx + 2, by, 40, TEXT, 256);
    }
    let lit = app.flash == Hit::Next && app.flash_frames > 0;
    if lit {
        rbox(x + w - 90, by - 36, 72, 72, 36, 0xffffff, 60);
    }
    icon_skip(x + w - 54, by, 40, 1, TEXT, 256);
    // Progress hairline.
    let frac = app.progress();
    let (px, pw) = (x + 40, MINI_PROGRESS_W);
    rbox(px, y + h - 9, pw, 4, 2, 0xffffff, 26);
    let fill = (pw * frac) >> 12;
    if fill > 0 {
        rbox(px, y + h - 9, fill.max(4), 4, 2, t.accent, 256);
    }
}

fn draw_library(app: &App) {
    // Top bar.
    if visible(30, 140) {
        let lit = app.flash == Hit::Home && app.flash_frames > 0;
        glass_button(82, 84, 40, lit);
        icon_home(82, 84, 40, TEXT, 256);
        status_pill(app, 692, 84);
    }
    draw_fan(app);
    let t = &TRACKS[app.current()];
    if visible(480, 660) {
        text(&META, 40, 512, "ALBUM \u{b7} 10 SONGS \u{b7} 25 MIN", t.accent, 235, 10);
        text(&HERO, 36, 590, catalog::ALBUM, TEXT, 256, -10);
        text(&META, 40, 636, catalog::ARTIST, MUTED, 256, 0);
        // The big play button.
        let (cx, cy) = HERO_PLAY;
        glow(cx - 40, cy - 30, 80, 80, 40, 40, t.glow, 160);
        let r = if app.flash == Hit::Toggle && app.flash_frames > 0 { 46 } else { 50 };
        rbox(cx - r, cy - r, 2 * r, 2 * r, r, t.accent, 256);
        if app.st.playing() {
            icon_pause(cx, cy, 46, NIGHT, 256);
        } else {
            icon_play(cx + 3, cy, 46, NIGHT, 256);
        }
    }
    for i in 0..10 {
        draw_row(app, i);
    }
    draw_mini(app);
}

fn library_hit(app: &App, x: i32, y: i32) -> Hit {
    if y < 140 {
        return if x < 160 { Hit::Home } else { Hit::None };
    }
    if y < 470 {
        // Front to back.
        let fan = fan_positions(app);
        for k in (0..5).rev() {
            let (t, cx, cy, s, _) = fan[k];
            if (x - cx).abs() < s / 2 && (y - cy).abs() < s / 2 {
                return if t == app.current() && app.st.track != 0 { Hit::Open } else { Hit::Play(t) };
            }
        }
        return Hit::None;
    }
    if y < LIST_Y {
        let (cx, cy) = HERO_PLAY;
        return if (x - cx).abs() < 70 && (y - cy).abs() < 70 { Hit::Toggle } else { Hit::None };
    }
    if y < LIST_Y + 10 * ROW_H {
        let i = ((y - LIST_Y) / ROW_H) as usize;
        return if i == app.current() && app.st.track != 0 { Hit::Open } else { Hit::Play(i) };
    }
    if y >= MINI_Y - 10 {
        if x > 700 - 100 {
            return Hit::Next;
        }
        if x > 700 - 200 {
            return Hit::Toggle;
        }
        return Hit::Open;
    }
    Hit::None
}

// ---------- the player ----------

const COVER_Y: i32 = 162;
const COVER: i32 = 512;
const VIS_Y: i32 = 856;
const VIS_BASE: i32 = 1000;
const SCRUB_Y: i32 = 1080;
const SCRUB_MID: i32 = 1122;
const TIME_Y: i32 = 1206;
const TRANSPORT_Y: i32 = 1330;
const VOL_Y: i32 = 1500;
const SCRUB_X: i32 = 56;
const SCRUB_W: i32 = 608;
const VOL_X: i32 = 136;
const VOL_W: i32 = 448;

const BANDS: [(i32, i32); 6] = [
    (130, 850),        // cover, title
    (VIS_Y, 1062),     // spectrum
    (1062, 1226),      // scrubber, times, status
    (1220, 1452),      // transport and its buffering dots
    (1440, 1560),      // volume
    (30, 140),         // top bar
];

fn draw_player(app: &App) {
    let i = app.current();
    let t = &TRACKS[i];
    if visible(30, 140) {
        let lit = app.flash == Hit::Close && app.flash_frames > 0;
        glass_button(82, 84, 40, lit);
        icon_chevron(82, 86, 40, TEXT, 256);
        center_text(&META, 360, 74, "PLAYING FROM", TEXT, 150, 10);
        center_text(&META, 360, 108, catalog::ALBUM, TEXT, 256, 0);
        let lit = app.flash == Hit::Home && app.flash_frames > 0;
        glass_button(638, 84, 40, lit);
        icon_home(638, 84, 40, TEXT, 256);
    }
    // Cover with its glow; it breathes in while playing.
    if visible(COVER_Y - 120, COVER_Y + COVER + 120) {
        let s = app.breath;
        let x = 360 - s / 2;
        let y = COVER_Y + (COVER - s) / 2;
        glow(x + 24, y + 40, s - 48, s - 48, 48, 110, t.glow, 170);
        load_cover(i);
        cover_art(x, y, s, 40, 256);
    }
    if visible(730, 850) {
        text(&TITLE, 52, 784, t.title, TEXT, 256, -6);
        let x = text(&META, 56, 830, catalog::ARTIST, TEXT, 180, 0);
        let mut b = Buf::new();
        b.num(t.bpm as u32).s(" BPM");
        let mut cx = x + 18;
        for s in [b.get(), t.key] {
            let w = measure(&META, s, 0) + 28;
            rbox(cx, 802, w, 38, 19, TEXT, 30);
            text(&META, cx + 14, 830, s, TEXT, 235, 0);
            cx += w + 10;
        }
    }
    draw_spectrum(app, t);
    draw_scrubber(app, t);
    draw_transport(app, t);
    draw_volume(app, t);
}

fn draw_spectrum(app: &App, t: &Track) {
    if !visible(VIS_Y, 1062) {
        return;
    }
    let (bw, gap) = (28, 13);
    let x0 = (W - (16 * bw + 15 * gap)) / 2;
    let on = app.st.track != 0 && app.st.state == MUSIC_STATE_PLAYING;
    // A soft pool of light under the bars that swells with the music.
    let energy: i32 = app.st.bands.iter().map(|&b| b as i32).sum::<i32>() / 16;
    if on {
        // Kept inside the spectrum's band so the band redraws cleanly.
        light(360, VIS_BASE - 50, 360, 100, t.glow, (50 + energy / 2) as u32);
    }
    for k in 0..16 {
        let x = x0 + k as i32 * (bw + gap);
        // At rest the bars show the song's loudness shape, dimmed.
        let level = if on { app.st.bands[k] as i32 } else { t.bars[k * 3 + 1] as i32 / 2 };
        let h = 10 + level * 126 / 255;
        // Bar: accent at the top fading to the glow colour at the base.
        let top = lerp_rgb(t.accent, TEXT, (40 + level as u32 / 3).min(256));
        let bottom = lerp_rgb(t.glow, t.accent, 60);
        let a = if on { 256 } else { 70 };
        rbox_v(x, VIS_BASE - h, bw, h, bw / 2, top, bottom, a, a);
        // Reflection.
        let rh = h * 2 / 5;
        rbox_v(x, VIS_BASE + 6, bw, rh, bw / 2, bottom, t.glow, a * 3 / 10, 0);
        // Falling peak cap.
        let cap = app.caps[k] >> 4;
        if on && cap > h + 4 {
            rbox(x + 4, VIS_BASE - cap - 8, bw - 8, 6, 3, TEXT, 200);
        }
    }
}

fn draw_scrubber(app: &App, t: &Track) {
    if !visible(1062, 1226) {
        return;
    }
    let frac = app.progress(); // Q12
    let n = catalog::BARS as i32;
    let head = SCRUB_X + ((SCRUB_W * frac) >> 12);
    for k in 0..n {
        let x = SCRUB_X + k * SCRUB_W / n;
        let w = SCRUB_W / n - 5;
        let h = (84 * (26 + 160 * t.bars[k as usize] as i32 / 255) / 256).max(6);
        let centre = SCRUB_X + (k * 2 + 1) * SCRUB_W / (2 * n);
        let played = centre <= head;
        let near = (48 - (centre - head).abs()).max(0);
        let h = h + h * near / 160;
        if played {
            rbox(x, SCRUB_MID - h / 2, w, h, w / 2, t.accent, 256);
        } else {
            rbox(x, SCRUB_MID - h / 2, w, h, w / 2, 0xffffff, 56 + near as u32);
        }
    }
    if app.st.track != 0 {
        light(head, SCRUB_MID, 40, 40, t.accent, 120);
        rbox(head - 2, SCRUB_MID - 42, 5, 84, 2, 0xffffff, 230);
    }
    let len = app.length();
    let pos = app.st.pos.min(len);
    let mut a = Buf::new();
    a.time(pos);
    text(&META, SCRUB_X, TIME_Y, a.get(), TEXT, 190, 0);
    let mut b = Buf::new();
    b.s("-").time(len - pos);
    right_text(&META, SCRUB_X + SCRUB_W, TIME_Y, b.get(), TEXT, 190, 0);
    // What the stream is doing.
    let (c, s) = status_line(app);
    let mut m = Buf::new();
    if app.st.state == MUSIC_STATE_PLAYING && app.st.ahead > 0 {
        m.s("Live \u{b7} ").num(app.st.ahead / 10).s(".").num(app.st.ahead % 10).s(" s buffered");
    } else {
        m.s(s);
    }
    let w = measure(&META, m.get(), 0);
    let x = 360 - (w + 22) / 2;
    rbox(x, TIME_Y - 15, 12, 12, 6, c, 256);
    text(&META, x + 22, TIME_Y, m.get(), if c == WARN { WARN } else { TEXT }, if c == WARN { 256 } else { 170 }, 0);
}

fn draw_transport(app: &App, t: &Track) {
    if !visible(1190, 1460) {
        return;
    }
    let cy = TRANSPORT_Y;
    let lit = |h: Hit| app.flash == h && app.flash_frames > 0;
    for (h, cx, on) in [(Hit::Shuffle, 92, app.shuffle), (Hit::Repeat, 628, app.repeat)] {
        if lit(h) {
            rbox(cx - 40, cy - 40, 80, 80, 40, 0xffffff, 40);
        }
        let c = if on { t.accent } else { TEXT };
        let a = if on { 256 } else { 150 };
        if h == Hit::Shuffle {
            icon_shuffle(cx, cy, 52, c, a);
        } else {
            icon_repeat(cx, cy, 52, c, a);
        }
        if on {
            rbox(cx - 4, cy + 34, 8, 8, 4, t.accent, 256);
        }
    }
    for (h, cx, dir) in [(Hit::Prev, 222, -1), (Hit::Next, 498, 1)] {
        if lit(h) {
            rbox(cx - 48, cy - 48, 96, 96, 48, 0xffffff, 40);
        }
        icon_skip(cx, cy, 60, dir, TEXT, 256);
    }
    let r = if lit(Hit::Toggle) { 74 } else { 82 };
    glow(360 - r + 10, cy - r + 16, 2 * r - 20, 2 * r - 20, r, 60, t.glow, 210);
    rbox(360 - r, cy - r, 2 * r, 2 * r, r, TEXT, 256);
    if app.st.state == MUSIC_STATE_BUFFERING && app.st.track != 0 {
        // Eight dots chasing around the button.
        for k in 0..8 {
            let ang = (k as u32 * 8 + 64 - (app.frame % 64)) % 64;
            let a = 40 + ang * 3;
            let (dx, dy) = DOTS[k as usize];
            rbox(360 + dx - 7, cy + dy - 7, 14, 14, 7, t.accent, a.min(256));
        }
    }
    if app.st.playing() && app.st.state != MUSIC_STATE_BUFFERING {
        icon_pause(360, cy, 76, NIGHT, 256);
    } else {
        icon_play(364, cy, 76, NIGHT, 256);
    }
}
/// Points on a circle of radius 100 at 45 degree steps.
const DOTS: [(i32, i32); 8] = [(0, -100), (71, -71), (100, 0), (71, 71), (0, 100), (-71, 71), (-100, 0), (-71, -71)];

fn draw_volume(app: &App, t: &Track) {
    if !visible(1440, 1560) {
        return;
    }
    let cy = VOL_Y;
    let lit = |h: Hit| app.flash == h && app.flash_frames > 0;
    if lit(Hit::VolDown) {
        rbox(48, cy - 40, 80, 80, 40, 0xffffff, 40);
    }
    if lit(Hit::VolUp) {
        rbox(592, cy - 40, 80, 80, 40, 0xffffff, 40);
    }
    icon_speaker(84, cy, 44, 0, TEXT, 170);
    icon_speaker(632, cy, 44, 2, TEXT, 170);
    rbox(VOL_X, cy - 5, VOL_W, 10, 5, 0xffffff, 40);
    let fill = VOL_W * app.volume as i32 / VOLUME_MAX as i32;
    if fill > 0 {
        rbox(VOL_X, cy - 5, fill, 10, 5, t.accent, 256);
    }
    let kx = VOL_X + fill;
    glow(kx - 10, cy - 10, 20, 20, 10, 18, t.accent, 120);
    rbox(kx - 14, cy - 14, 28, 28, 14, TEXT, 256);
}

fn player_hit(x: i32, y: i32) -> Hit {
    if y < 140 {
        return if x < 160 {
            Hit::Close
        } else if x > 560 {
            Hit::Home
        } else {
            Hit::None
        };
    }
    if y < COVER_Y + COVER {
        return if (100..620).contains(&x) { Hit::Toggle } else { Hit::None };
    }
    if (1062..1180).contains(&y) {
        let f = ((x - SCRUB_X).clamp(0, SCRUB_W) << 12) / SCRUB_W;
        return Hit::Seek(f);
    }
    if (1240..1424).contains(&y) {
        return match x {
            0..=156 => Hit::Shuffle,
            157..=290 => Hit::Prev,
            291..=430 => Hit::Toggle,
            431..=562 => Hit::Next,
            _ => Hit::Repeat,
        };
    }
    if (1440..1570).contains(&y) {
        return match x {
            0..=VOL_X => Hit::VolDown,
            _ if x >= VOL_X + VOL_W => Hit::VolUp,
            _ => Hit::Volume(((x - VOL_X) * VOLUME_MAX as i32 + VOL_W / 2) / VOL_W),
        };
    }
    Hit::None
}

// ---------- the app ----------

impl App {
    /// Audio advances to the next song itself, so the album keeps playing
    /// while Music is off screen.
    fn send_mode(&self) {
        let mode = MUSIC_ADVANCE
            | if self.repeat { MUSIC_REPEAT } else { 0 }
            | if self.shuffle { MUSIC_SHUFFLE } else { 0 };
        music(MUSIC_MODE, mode);
    }
    fn current(&self) -> usize {
        if self.st.track != 0 {
            self.st.track as usize - 1
        } else {
            self.sel
        }
    }
    fn length(&self) -> u32 {
        if self.st.len != 0 && self.st.track != 0 {
            self.st.len
        } else {
            TRACKS[self.current()].ms
        }
    }
    /// Playback position as a fraction in Q12.
    fn progress(&self) -> i32 {
        if self.st.track == 0 {
            return 0;
        }
        let len = self.length().max(1);
        (self.st.pos.min(len) * 4096 / len) as i32
    }
    fn target_breath(&self) -> i32 {
        if self.st.playing() {
            COVER
        } else {
            COVER * 88 / 100
        }
    }
    fn next_track(&mut self, step: i32) -> Option<usize> {
        let cur = self.current() as i32;
        if self.shuffle && step > 0 {
            self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let jump = 1 + (self.seed >> 16) % 9;
            return Some(((cur + jump as i32) % 10) as usize);
        }
        let n = cur + step;
        if (0..10).contains(&n) {
            Some(n as usize)
        } else if self.repeat || step < 0 {
            Some(n.rem_euclid(10) as usize)
        } else {
            None
        }
    }
    fn play(&mut self, i: usize) {
        self.sel = i;
        music(MUSIC_PLAY, i as u32 + 1);
    }
    fn toggle(&mut self) {
        let st = self.st;
        if st.track == 0 || matches!(st.state, MUSIC_STATE_STOPPED | MUSIC_STATE_ENDED | MUSIC_STATE_OFFLINE) {
            let start = if st.track != 0 && st.state == MUSIC_STATE_OFFLINE { st.pos / 1000 } else { 0 };
            self.sel = self.current();
            music(MUSIC_PLAY, (self.sel as u32 + 1) | start << 8);
        } else if st.state == MUSIC_STATE_PAUSED {
            music(MUSIC_RESUME, 0);
        } else {
            music(MUSIC_PAUSE, 0);
        }
    }
}

fn present(y0: i32, y1: i32) {
    let (y0, y1) = (y0.clamp(0, H), y1.clamp(0, H));
    if y0 < y1 {
        request(EXECUTE, PRESENT, HD_PRESENT | (y0 as u32) << 16 | y1 as u32);
    }
}

/// Rows of the library whose background is cached off screen (the covers'
/// band); card sprites are packed below it.
const CACHE_Y0: i32 = 60;
const CACHE_Y1: i32 = 476;

fn render(app: &App, y0: i32, y1: i32) {
    set_clip(y0, y1);
    if app.screen == Screen::Library && app.bg_for == app.current() + 1 && y0 >= CACHE_Y0 && y1 <= CACHE_Y1 {
        restore_rows(y0, y1);
    } else {
        background(y0, y1);
    }
    match app.screen {
        Screen::Library => draw_library(app),
        Screen::Player => draw_player(app),
    }
    if DEMO && visible(1570, 1600) {
        let mut b = Buf::new();
        unsafe { b.s("full ").num(PERF.0).s(" ms upd ").num(PERF.1).s(" ms") };
        text(&META, 250, 1590, b.get(), TEXT, 256, 0);
    }
}
static mut PERF: (u32, u32) = (0, 0);

fn full(app: &mut App) {
    let t0 = now_ms();
    paint_grid(app);
    // Draw in slices so the screen fills top-down quickly.
    let mut y = 0;
    while y < H {
        render(app, y, y + 400);
        present(y, y + 400);
        y += 400;
    }
    app.keys = [u32::MAX; 8];
    // Everything is fresh: no strip repaint needs to follow.
    app.wipe = H;
    unsafe { PERF.0 = now_ms().wrapping_sub(t0) };
}

/// A cheap digest of what a band shows, to skip redrawing unchanged bands.
fn digest(parts: &[u32]) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &p in parts {
        h = (h ^ p).wrapping_mul(0x0100_0193);
    }
    h
}

fn update(app: &mut App) {
    let st = app.st;
    let bands = u32::from_le_bytes([st.bands[0], st.bands[3], st.bands[7], st.bands[11]])
        ^ u32::from_le_bytes([st.bands[1], st.bands[5], st.bands[9], st.bands[15]]).rotate_left(7)
        ^ u32::from_le_bytes([st.bands[2], st.bands[6], st.bands[10], st.bands[13]]).rotate_left(13)
        ^ u32::from_le_bytes([st.bands[4], st.bands[8], st.bands[12], st.bands[14]]).rotate_left(19);
    let caps = app.caps.iter().fold(0u32, |h, &c| h.wrapping_mul(31).wrapping_add(c as u32));
    let flash = if app.flash_frames > 0 { hit_id(app.flash) + 1 } else { 0 };
    let buffering = st.state == MUSIC_STATE_BUFFERING;
    let anim = |on: bool| if on { app.frame } else { 0 };
    let line = status_line(app).1.as_ptr() as u32;
    match app.screen {
        Screen::Player => {
            let keys = [
                digest(&[app.breath as u32, st.track]),
                digest(&[bands, caps, st.state, st.track]),
                digest(&[app.progress() as u32, st.pos / 1000, st.state, line, st.ahead, st.track]),
                digest(&[st.state, flash, anim(buffering), app.shuffle as u32, app.repeat as u32 * 2, st.track]),
                digest(&[app.volume, flash]),
                digest(&[flash]),
            ];
            for (k, &(y0, y1)) in BANDS.iter().enumerate() {
                // A strip repaint still to reach this band will draw it anyway.
                if app.keys[k] != keys[k] {
                    app.keys[k] = keys[k];
                    if y0 < app.wipe {
                        render(app, y0, y1);
                        present(y0, y1);
                    }
                }
            }
        }
        Screen::Library => {
            let cur = app.current() as u32;
            let pulse = anim(st.playing());
            let keys = [
                digest(&[line, flash, st.state]),
                digest(&[st.playing() as u32, flash, cur, st.state]),
                digest(&[cur, st.track, if st.state == MUSIC_STATE_PLAYING { bands } else { 0 }]),
                digest(&[st.state, line, flash, cur, st.playing() as u32]),
                digest(&[app.fan as u32]),
                // The status dot's pulse alone: its rows clear the covers' shadows.
                digest(&[pulse]),
                // The mini player's progress hairline alone, by filled pixels.
                digest(&[(MINI_PROGRESS_W * app.progress()) as u32 >> 12, cur]),
            ];
            let row_y = LIST_Y + cur as i32 * ROW_H;
            let bands_y: [(i32, i32); 7] = [
                (30, 140),
                (480, 662),
                (row_y, row_y + ROW_H),
                (MINI_Y - 40, H),
                (60, 476),
                (62, 106),
                (MINI_Y + 108, MINI_Y + 118),
            ];
            for (k, &(y0, y1)) in bands_y.iter().enumerate() {
                // A strip repaint still to reach this band will draw it anyway.
                if app.keys[k] != keys[k] {
                    app.keys[k] = keys[k];
                    if k == 4 || y0 < app.wipe {
                        render(app, y0, y1);
                        present(y0, y1);
                    }
                }
            }
        }
    }
}

fn act(app: &mut App, hit: Hit) {
    app.flash = hit;
    app.flash_frames = 4;
    match hit {
        Hit::None => {}
        Hit::Home => {
            update(app);
            request(EXECUTE, OPEN, LAUNCHER);
        }
        Hit::Close => {
            app.screen = Screen::Library;
            app.flash = Hit::None;
            app.fan = 0;
            app.deal = (0, now_ms());
            full(app);
        }
        Hit::Open => {
            app.screen = Screen::Player;
            app.breath = app.target_breath();
            app.flash = Hit::None;
            full(app);
        }
        Hit::Toggle => {
            update(app);
            app.toggle();
        }
        Hit::Next | Hit::Prev => {
            update(app);
            let step = if hit == Hit::Next { 1 } else { -1 };
            if step < 0 && app.st.pos > 3000 && app.st.track != 0 {
                music(MUSIC_SEEK, 0);
            } else if let Some(n) = app.next_track(step).or(Some(0)) {
                app.play(n);
            }
        }
        Hit::Play(i) => app.play(i),
        Hit::Seek(f) => {
            if app.st.track == 0 {
                app.play(app.sel);
            } else {
                let s = app.length() / 1000 * f as u32 / 4096;
                music(MUSIC_SEEK, s);
            }
        }
        Hit::Shuffle => {
            app.shuffle = !app.shuffle;
            app.send_mode();
        }
        Hit::Repeat => {
            app.repeat = !app.repeat;
            app.send_mode();
        }
        Hit::Volume(v) => {
            let r = request(EXECUTE, VOLUME, v.clamp(0, VOLUME_MAX as i32) as u32);
            if r.op == OK {
                app.volume = r.value;
            }
        }
        Hit::VolDown | Hit::VolUp => {
            let r = request(EXECUTE, VOLUME, if hit == Hit::VolUp { VOLUME_UP } else { VOLUME_DOWN });
            if r.op == OK {
                app.volume = r.value;
            }
        }
    }
}

pub fn run() -> ! {
    init(request(GET, SURFACE, 0).value as *mut u32);
    let me = request(GET, WHOAMI, 0).value;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let volume_events = submit(SUBSCRIBE, VOLUME, 0).ok();
    let v = request(GET, VOLUME, 0);
    let mut app = App {
        screen: Screen::Library,
        st: Status::NONE,
        sel: 0,
        volume: if v.op == OK { v.value } else { 8 },
        shuffle: false,
        repeat: true,
        caps: [0; 16],
        breath: COVER,
        deal: (0, 0),
        last_ms: 0,
        wipe: H,
        tiles: [Tile::default(); 5],
        sprites_for: 0,
        bg_for: 0,
        offscreen: {
            let r = request(GET, OFFSCREEN, 0);
            r.op == OK && r.value as usize == SCRATCH
        },
        prep: 5,
        prep_row: 0,
        fan: 0,
        frame: 0,
        flash: Hit::None,
        flash_frames: 0,
        buffering_since: 0,
        advanced: 0,
        seed: 0x2545_f491,
        keys: [0; 8],
    };
    app.send_mode();
    let mut shown = false;
    loop {
        let start = now_ms();
        app.frame = app.frame.wrapping_add(1);
        let st = status(app.frame);
        if st.state == MUSIC_STATE_BUFFERING && app.st.state != MUSIC_STATE_BUFFERING {
            app.buffering_since = app.frame;
        }
        let changed_track = st.track != 0 && st.track != app.st.track;
        app.st = st;
        if st.track != 0 {
            app.sel = st.track as usize - 1;
        }
        if let Some(s) = &volume_events {
            let r = s.poll();
            if r.op == OK {
                app.volume = r.value.min(VOLUME_MAX);
            }
        }
        // Peak caps fall slowly.
        for k in 0..16 {
            let h = (10 + st.bands[k] as i32 * 126 / 255) << 4;
            app.caps[k] = if h > app.caps[k] { h } else { (app.caps[k] - 40).max(0) };
        }
        let active = request(GET, FOCUS, 0).value == me;
        if active {
            // A finished song moves on to the next one.
            if st.state == MUSIC_STATE_ENDED && app.advanced != st.track {
                app.advanced = st.track;
                match app.next_track(1) {
                    Some(n) => app.play(n),
                    None => music(MUSIC_STOP, 0),
                }
            }
            if st.state != MUSIC_STATE_ENDED {
                app.advanced = 0;
            }
            if !shown || changed_track {
                // Deal the covers out on arrival; gather and re-deal on a new song.
                if !(shown && app.screen == Screen::Library) {
                    app.fan = if shown { 120 } else { 0 };
                    app.deal = (app.fan, now_ms());
                }
                app.breath = app.target_breath();
                if shown {
                    // A new song: the colours follow its cover. Repaint in
                    // strips over the next frames (after the deal, on the
                    // library) instead of stalling on a full redraw.
                    paint_grid(&app);
                    app.wipe = 0;
                } else {
                    full(&mut app);
                }
            }
            if app.offscreen && app.screen == Screen::Library && app.sprites_for != app.current() + 1 {
                // One card per frame, while the old fan stays on screen; the
                // deal and the colour repaint start once all five are ready.
                if app.prep == 5 {
                    app.prep = 0;
                    app.prep_row = 0;
                    // Cache the covers' band background in the new colours.
                    offscreen(|| {
                        set_clip(CACHE_Y0, CACHE_Y1);
                        background(CACHE_Y0, CACHE_Y1);
                    });
                    app.bg_for = app.current() + 1;
                } else if prep_sprites(&mut app) {
                    app.sprites_for = app.current() + 1;
                    app.deal = (app.fan.min(120), now_ms());
                    app.fan = app.deal.0;
                }
            }
            // The colour repaint follows the deal, so the two never share a frame.
            let dealt = app.screen == Screen::Player || (app.prep == 5 && app.fan >= 256);
            if app.wipe < H && dealt {
                // The covers' band (60..476) redraws every frame while dealing.
                let (y0, y1) = (app.wipe, (app.wipe + WIPE_ROWS).min(H));
                app.wipe = y1;
                if app.screen == Screen::Player || y0 < 60 || y1 > 476 {
                    render(&app, y0, y1);
                    present(y0, y1);
                }
            }
            // Animations run on the clock, so a slow frame never stretches or
            // stalls them: the deal eases out over DEAL_MS.
            let now = now_ms();
            let dt = now.wrapping_sub(app.last_ms).min(100) as i32;
            app.last_ms = now;
            if app.fan < 256 && app.screen == Screen::Library {
                let x = (now.wrapping_sub(app.deal.1).min(DEAL_MS) * 256 / DEAL_MS) as i32;
                let rest = 256 - x;
                let eased = 256 - rest * rest / 256 * rest / 256;
                app.fan = app.deal.0 + (256 - app.deal.0) * eased / 256;
            }
            let target = app.target_breath();
            if app.breath != target {
                let d = target - app.breath;
                let step = d * (dt * 256 / 90).min(256) / 256;
                app.breath += if d.abs() <= 2 { d } else if step == 0 { d.signum() } else { step };
            }
            for _ in 0..4 {
                let e = touch.poll();
                if e.op != OK {
                    break;
                }
                let (x, y) = (e.value as i32, e.arg as i32);
                let hit = match app.screen {
                    Screen::Library => library_hit(&app, x, y),
                    Screen::Player => player_hit(x, y),
                };
                act(&mut app, hit);
                if request(GET, FOCUS, 0).value != me {
                    break;
                }
            }
            if app.flash_frames > 0 {
                app.flash_frames -= 1;
            }
            if request(GET, FOCUS, 0).value == me {
                let t0 = now_ms();
                update(&mut app);
                if DEMO {
                    let spent = now_ms().wrapping_sub(t0);
                    if spent > 0 {
                        unsafe { PERF.1 = spent };
                        render(&app, 1570, 1600);
                        present(1570, 1600);
                    }
                }
            }
        } else {
            // Drain touches meant for other screens.
            let _ = touch.poll();
        }
        shown = active;
        let spent = now_ms().wrapping_sub(start);
        unsafe { delay_ms(FRAME_MS.saturating_sub(spent).max(2)) };
    }
}

fn hit_id(h: Hit) -> u32 {
    match h {
        Hit::None => 0,
        Hit::Home => 1,
        Hit::Close => 2,
        Hit::Toggle => 3,
        Hit::Next => 4,
        Hit::Prev => 5,
        Hit::Shuffle => 6,
        Hit::Repeat => 7,
        Hit::Open => 8,
        Hit::Play(i) => 9 + i as u32,
        Hit::Seek(_) => 30,
        Hit::Volume(_) => 31,
        Hit::VolDown => 32,
        Hit::VolUp => 33,
    }
}
