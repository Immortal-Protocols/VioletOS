//! Drawing Home: everything comes from State, so a preview can be drawn
//! without the devices.
use std::collections::HashSet;
use std::f32::consts::PI;

use violet::{colors::INK, mix, rgb, Canvas, Color, Fonts, Rect, W};

use crate::clock::Local;

pub struct State {
    pub now: Option<Local>,
    pub online: bool,
    pub battery: i32, // percent, -1 unknown
    pub volume: i32,
    pub phones: bool,
    pub temp: String,
    pub sky: String,
    pub city: String,
    pub running: HashSet<String>,
    pub pressed: Option<usize>,
    pub shield: Shield,
    /// The joined Wi-Fi network's name ("" for none), or Phone Internet.
    pub wifi: String,
}

/// The Wi-Fi bar under the weather; a tap opens the Wi-Fi app.
pub const WIFI_BAR: Rect = Rect::new(40, 660, W as i32 - 40, 720);
pub const WIFI_COMMAND: &str = "open /apps/wifi";

/// Violet Shield as the Open Runtime reports it ("shield" in /dev/violet/sys).
#[derive(Clone, Default, PartialEq)]
pub struct Shield {
    pub known: bool,
    pub linked: bool,
    pub rules: u32,
    pub frozen: Vec<String>,
}

pub struct Tile {
    pub name: &'static str,
    pub title: &'static str,
    pub command: &'static str, // written to /dev/violet/sys on a tap
    tint: Color,
    icon: fn(&mut Canvas, f32, f32, Color),
}

pub const TILES: [Tile; 7] = [
    Tile { name: "music", title: "Music", command: "open /apps/music", tint: rgb(0xe6, 0x5c, 0x9a), icon: icon_music },
    Tile { name: "weather", title: "Weather", command: "open /apps/weather", tint: rgb(0x3d, 0x9b, 0xe9), icon: icon_weather },
    Tile { name: "stopwatch", title: "Stopwatch", command: "open /apps/stopwatch", tint: rgb(0xf2, 0x9e, 0x38), icon: icon_stopwatch },
    Tile { name: "sounds", title: "Sounds", command: "open /apps/sounds", tint: rgb(0x45, 0xc4, 0x8f), icon: icon_sounds },
    Tile { name: "programs", title: "Programs", command: "apps", tint: rgb(0x8f, 0x7a, 0xe6), icon: icon_programs },
    Tile { name: "system", title: "System", command: "system", tint: rgb(0x7d, 0x86, 0x99), icon: icon_system },
    Tile { name: "camera", title: "Camera", command: "open /apps/camera", tint: rgb(0x5b, 0x6f, 0xe0), icon: icon_camera },
];

// Three rows; the second row still covers y = 1141, where tests tap.
const TILE_W: i32 = 196;
const TILE_H: i32 = 200;
const GRID_TOP: i32 = 740;
const GAP: i32 = 22;

pub fn tile_rect(i: usize) -> Rect {
    let (col, row) = (i as i32 % 3, i as i32 / 3);
    let x = (W as i32 - 3 * TILE_W - 2 * GAP) / 2 + col * (TILE_W + GAP);
    let y = GRID_TOP + row * (TILE_H + GAP);
    Rect::new(x, y, x + TILE_W, y + TILE_H)
}

pub fn tile_at(x: i32, y: i32) -> Option<usize> {
    (0..TILES.len()).find(|&i| tile_rect(i).contains(x, y))
}

pub const TOP: Color = rgb(0x2a, 0x17, 0x40);
const BOTTOM: Color = rgb(0x0e, 0x0b, 0x16);
const DIM: Color = rgb(0xb8, 0xae, 0xd2);
const CARD: Color = rgb(0x24, 0x1b, 0x36);
const WHITE: Color = rgb(0xff, 0xff, 0xff);

/// What a press on tile i (or the Wi-Fi bar, usize::MAX) changes on screen.
pub fn region(pressed: usize) -> Rect {
    if pressed == usize::MAX { WIFI_BAR } else { tile_rect(pressed) }
}

/// Redraw only r (a pressed or released tile): the same scene, clipped, so
/// it costs a fraction of a full redraw and only r needs presenting.
pub fn draw_clipped(c: &mut Canvas, f: &Fonts, s: &State, r: Rect) {
    c.set_clip(r);
    draw(c, f, s);
    c.reset_clip();
}

pub fn draw(c: &mut Canvas, f: &Fonts, s: &State) {
    let b = c.bounds();
    c.gradient(b, TOP, BOTTOM);
    status(c, f, s);
    let body = f.regular(32.0);
    match &s.now {
        Some(t) => {
            let h = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
            let end = f.bold(168.0).draw(c, &format!("{}:{:02}", h, t.minute), 56, 360, INK);
            body.draw(c, if t.hour >= 12 { "PM" } else { "AM" }, end + 14, 360, DIM);
            body.draw(c, &t.date(), 62, 430, DIM);
        }
        None => {
            f.bold(168.0).draw(c, "Violet", 56, 360, INK);
            body.draw(c, if s.online { "Starting…" } else { "Offline – choose a Wi-Fi network below" }, 62, 430, DIM);
        }
    }
    // The weather glance.
    let card = Rect::new(40, 500, W as i32 - 40, 640);
    c.round_rect(card, 30.0, CARD);
    icon_weather(c, 112.0, 570.0, INK);
    if !s.temp.is_empty() {
        let end = f.bold(46.0).draw(c, &s.temp, 186, 588, INK);
        let x = (end + 36).max(370);
        let (label, small) = (f.medium(28.0), f.medium(26.0));
        label.draw(c, &label.fit(&s.sky, card.x1 - 30 - x), x, 560, INK);
        small.draw(c, &small.fit(&s.city, card.x1 - 30 - x), x, 604, DIM);
    } else {
        f.medium(26.0).draw(c, "Weather appears here when online", 186, 580, DIM);
    }
    wifi_bar(c, f, s);
    for (i, t) in TILES.iter().enumerate() {
        tile(c, f, i, t, s.running.contains(&format!("/apps/{}", t.name)), s.pressed == Some(i));
    }
    shield(c, f, &s.shield);
    f.regular(21.0).centered(c, "Hold the top edge in any app to come back here", 0, W as i32, 1550, rgb(0x6c, 0x63, 0x84));
}

fn wifi_bar(c: &mut Canvas, f: &Fonts, s: &State) {
    let r = WIFI_BAR;
    c.round_rect(r, 30.0, if s.pressed == Some(usize::MAX) { rgb(0x3a, 0x2c, 0x55) } else { CARD });
    let on = !s.wifi.is_empty();
    let ink = if on { INK } else { DIM };
    let (cx, cy) = (86.0, (r.y0 + 44) as f32);
    c.circle(cx, cy, 4.0, ink);
    for rad in [14.0, 24.0] {
        c.arc(cx, cy, rad, 5.0, PI / 4.0, 3.0 * PI / 4.0, ink);
    }
    let label = f.medium(28.0);
    let x = label.draw(c, "Wi-Fi", 126, r.y0 + 40, INK);
    let text = if on { s.wifi.as_str() } else { "Not connected" };
    let value = f.regular(27.0);
    value.draw(c, &value.fit(text, r.x1 - 70 - (x + 24)), x + 24, r.y0 + 40, DIM);
    let (ax, ay) = ((r.x1 - 40) as f32, (r.y0 + r.height() / 2) as f32);
    c.line(ax - 6.0, ay - 11.0, ax + 4.0, ay, 4.0, DIM);
    c.line(ax + 4.0, ay, ax - 6.0, ay + 11.0, 4.0, DIM);
}

const ALERT: Color = rgb(0xf0, 0x60, 0x60);
const SAFE: Color = rgb(0x45, 0xc4, 0x8f);

/// A card under the tiles: the Shield's link and rules, or the apps it froze.
fn shield(c: &mut Canvas, f: &Fonts, s: &Shield) {
    if !s.known {
        return;
    }
    let r = Rect::new(40, 1400, W as i32 - 40, 1500);
    let alert = !s.frozen.is_empty();
    c.round_rect(r, 30.0, if alert { rgb(0x4a, 0x1a, 0x26) } else { CARD });
    let tint = if alert { ALERT } else if s.linked { SAFE } else { DIM };
    // A shield: a rounded top and a point at the bottom.
    let (cx, cy) = (112.0, 1450.0);
    c.round_rect(Rect::new(cx as i32 - 22, cy as i32 - 30, cx as i32 + 22, cy as i32 + 3), 8.0, tint);
    c.triangle([[cx - 22.0, cy], [cx + 22.0, cy], [cx, cy + 30.0]], tint);
    let x = 180;
    let title = f.bold(32.0);
    let line = f.medium(26.0);
    if alert {
        let names: Vec<String> = s.frozen.iter().map(|n| title_of(n)).collect();
        title.draw(c, &title.fit(&format!("{} frozen", names.join(", ")), r.x1 - 30 - x), x, 1440, INK);
        line.draw(c, "Violet Shield stopped it mid-call", x, 1480, rgb(0xf4, 0xb8, 0xc0));
    } else {
        title.draw(c, "Violet Shield", x, 1440, INK);
        let state = if s.linked {
            format!("On · linked to your iPhone · {} rule{}", s.rules, if s.rules == 1 { "" } else { "s" })
        } else {
            "Waiting for your iPhone".to_string()
        };
        line.draw(c, &line.fit(&state, r.x1 - 30 - x), x, 1480, DIM);
    }
}

/// "photosync" -> "PhotoSync" for the demo apps, else the name capitalised.
fn title_of(name: &str) -> String {
    match name {
        "photosync" => "PhotoSync".to_string(),
        n => {
            let mut ch = n.chars();
            ch.next().map(|f| f.to_uppercase().chain(ch).collect()).unwrap_or_default()
        }
    }
}

fn tile(c: &mut Canvas, f: &Fonts, i: usize, t: &Tile, running: bool, pressed: bool) {
    let r = tile_rect(i);
    c.round_rect(r, 38.0, if pressed { rgb(0x3a, 0x2c, 0x55) } else { CARD });
    let (cx, cy) = ((r.x0 + r.width() / 2) as f32, (r.y0 + 82) as f32);
    // The icon sits on a disc in the app's colour, lighter at the top.
    for k in 0..8 {
        c.circle(cx, cy - k as f32 * 0.6, 58.0 - k as f32 * 0.4, mix(t.tint, WHITE, k as f32 * 0.03));
    }
    (t.icon)(c, cx, cy, WHITE);
    f.medium(28.0).centered(c, t.title, r.x0, r.x1, r.y0 + 178, INK);
    if running {
        c.circle(cx, (r.y1 - 14) as f32, 5.0, t.tint);
    }
}

fn status(c: &mut Canvas, f: &Fonts, s: &State) {
    let y = 78;
    let small = f.medium(26.0);
    small.draw(c, "VioletOS", 56, y, DIM);
    let mut x = W as i32 - 56;
    if s.battery >= 0 {
        let label = format!("{}%", s.battery);
        x -= small.width(&label);
        small.draw(c, &label, x, y, INK);
        x -= 66;
        let body = Rect::new(x, y - 24, x + 50, y - 2);
        c.round_rect(body, 6.0, INK);
        c.round_rect(body.inset(3), 4.0, BOTTOM);
        let mut level = body.inset(5);
        level.x1 = level.x0 + level.width() * s.battery.clamp(0, 100) / 100;
        c.fill(level, if s.battery <= 15 { rgb(0xf0, 0x60, 0x60) } else { INK });
        c.fill(Rect::new(x + 50, y - 17, x + 54, y - 9), INK);
        x -= 30;
    }
    // Wi-Fi: three arcs, dim when offline.
    let wc = if s.online { INK } else { rgb(0x5a, 0x52, 0x6e) };
    let (cx, cy) = ((x - 22) as f32, (y - 5) as f32);
    c.circle(cx, cy, 4.0, wc);
    for r in [18.0, 28.0] {
        c.arc(cx, cy, r, 5.0, PI / 4.0, 3.0 * PI / 4.0, wc);
    }
    x -= 80;
    let label = s.volume.to_string();
    x -= small.width(&label);
    small.draw(c, &label, x, y, INK);
    if s.phones {
        c.arc((x - 26) as f32, (y - 8) as f32, 14.0, 4.0, 0.0, PI, INK);
        c.round_rect(Rect::new(x - 44, y - 12, x - 36, y), 3.0, INK);
        c.round_rect(Rect::new(x - 16, y - 12, x - 8, y), 3.0, INK);
    } else {
        speaker(c, (x - 34) as f32, (y - 20) as f32, 0.75, INK);
    }
}

// ---- Icons, drawn around (cx, cy) in about a 90 px box. ----

fn icon_music(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    c.circle(cx - 16.0, cy + 20.0, 14.0, ink);
    c.circle(cx + 20.0, cy + 12.0, 14.0, ink);
    c.line(cx - 4.0, cy + 20.0, cx - 4.0, cy - 26.0, 7.0, ink);
    c.line(cx + 32.0, cy + 12.0, cx + 32.0, cy - 34.0, 7.0, ink);
    c.line(cx - 4.0, cy - 26.0, cx + 32.0, cy - 34.0, 12.0, ink);
}

fn icon_weather(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    let sun = rgb(0xff, 0xd2, 0x5c);
    c.circle(cx + 12.0, cy - 12.0, 20.0, sun);
    for k in 0..8 {
        let a = k as f32 * PI / 4.0;
        c.line(cx + 12.0 + a.cos() * 28.0, cy - 12.0 + a.sin() * 28.0, cx + 12.0 + a.cos() * 36.0, cy - 12.0 + a.sin() * 36.0, 5.0, sun);
    }
    c.circle(cx - 14.0, cy + 12.0, 18.0, ink);
    c.circle(cx + 8.0, cy + 4.0, 22.0, ink);
    c.circle(cx + 28.0, cy + 16.0, 14.0, ink);
    c.round_rect(Rect::new((cx - 32.0) as i32, (cy + 12.0) as i32, (cx + 42.0) as i32, (cy + 30.0) as i32), 9.0, ink);
}

fn icon_stopwatch(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    c.ring(cx, cy + 6.0, 30.0, 8.0, ink);
    c.line(cx, cy - 30.0, cx, cy - 36.0, 10.0, ink);
    c.line(cx - 10.0, cy - 40.0, cx + 10.0, cy - 40.0, 8.0, ink);
    c.line(cx + 26.0, cy - 22.0, cx + 32.0, cy - 28.0, 7.0, ink);
    c.line(cx, cy + 6.0, cx + 12.0, cy - 10.0, 6.0, ink);
    c.circle(cx, cy + 6.0, 5.0, ink);
}

fn speaker(c: &mut Canvas, x: f32, y: f32, s: f32, ink: Color) {
    c.round_rect(Rect::new(x as i32, (y + 8.0 * s) as i32, (x + 12.0 * s) as i32, (y + 24.0 * s) as i32), 2.0, ink);
    c.triangle([[x + 8.0 * s, y + 8.0 * s], [x + 26.0 * s, y - 4.0 * s], [x + 8.0 * s, y + 16.0 * s]], ink);
    c.triangle([[x + 8.0 * s, y + 24.0 * s], [x + 26.0 * s, y + 36.0 * s], [x + 8.0 * s, y + 16.0 * s]], ink);
    c.fill(Rect::new((x + 8.0 * s) as i32, (y + 8.0 * s) as i32, (x + 27.0 * s) as i32, (y + 25.0 * s) as i32), ink);
    c.triangle([[x + 26.0 * s, y - 4.0 * s], [x + 27.0 * s, y + 36.0 * s], [x + 20.0 * s, y + 16.0 * s]], ink);
}

fn icon_sounds(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    speaker(c, cx - 38.0, cy - 16.0, 1.1, ink);
    c.arc(cx - 6.0, cy + 2.0, 18.0, 6.0, -PI / 3.0, PI / 3.0, ink);
    c.arc(cx - 6.0, cy + 2.0, 32.0, 6.0, -PI / 3.0, PI / 3.0, ink);
}

fn icon_programs(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    for k in 0..4 {
        let (x, y) = (cx - 30.0 + (k % 2) as f32 * 34.0, cy - 30.0 + (k / 2) as f32 * 34.0);
        c.round_rect(Rect::new(x as i32, y as i32, x as i32 + 26, y as i32 + 26), 7.0, ink);
    }
}

fn icon_camera(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    c.round_rect(Rect::new((cx - 36.0) as i32, (cy - 20.0) as i32, (cx + 36.0) as i32, (cy + 28.0) as i32), 10.0, ink);
    c.round_rect(Rect::new((cx - 14.0) as i32, (cy - 30.0) as i32, (cx + 14.0) as i32, (cy - 14.0) as i32), 5.0, ink);
    c.circle(cx, cy + 4.0, 17.0, rgb(0x5b, 0x6f, 0xe0));
    c.circle(cx, cy + 4.0, 10.0, ink);
}

fn icon_system(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    for k in 0..8 {
        let a = k as f32 * PI / 4.0;
        c.line(cx + a.cos() * 22.0, cy + a.sin() * 22.0, cx + a.cos() * 34.0, cy + a.sin() * 34.0, 13.0, ink);
    }
    c.ring(cx, cy, 22.0, 14.0, ink);
    c.circle(cx, cy, 9.0, ink);
}
