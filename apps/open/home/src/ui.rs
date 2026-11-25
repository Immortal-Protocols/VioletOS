//! Drawing Home: everything comes from State, so a preview can be drawn
//! without the devices.
use std::collections::HashSet;

use violet::paper::{self, icons, Sprite, CREAM, MUTED};
use violet::{mix, rgb, Canvas, Color, Fonts, Rect, H, W};

use crate::clock::Local;

/// One app on the grid.
#[derive(Clone, PartialEq)]
pub struct App {
    pub name: String,
    pub title: String,
}

impl App {
    pub fn path(&self) -> String {
        format!("/apps/{}", self.name)
    }
}

/// The apps that ship with the phone, in grid order, under these names; every
/// other titled program in /apps (an app the agent installed) follows them.
pub const BUILT_IN: [(&str, &str); 5] = [
    ("ladybird", "Ladybird Browser"),
    ("bitchat", "Bitchat"),
    ("agent", "Violet Agent"),
    ("music", "Locomo Music"),
    ("settings", "Settings"),
];

/// The grid from /apps/.home ("name<TAB>title" lines): the built-in apps that
/// are present, then the rest in the order they were installed.
pub fn apps(listing: &str) -> Vec<App> {
    let listed: Vec<(&str, &str)> = listing.lines().filter_map(|l| l.split_once('\t')).collect();
    let built_in = BUILT_IN.iter().filter(|(n, _)| listed.iter().any(|(l, _)| l == n)).map(|(n, t)| App { name: n.to_string(), title: t.to_string() });
    let others = listed.iter().filter(|(n, _)| *n != "home" && !BUILT_IN.iter().any(|(b, _)| b == n)).map(|(n, t)| App { name: n.to_string(), title: t.to_string() });
    built_in.chain(others).take(MAX_APPS).collect()
}

/// Violet Shield as the Open Runtime reports it ("shield" in /dev/violet/sys).
#[derive(Clone, Default, PartialEq)]
pub struct Shield {
    pub known: bool,
    pub linked: bool,
    pub frozen: Vec<String>,
}

pub struct State {
    pub now: Option<Local>,
    pub apps: Vec<App>,
    pub running: HashSet<String>,
    pub pressed: Option<usize>,
    pub shield: Shield,
}

/// What a press landed on: an app, or the Shield pill.
pub const PILL: usize = usize::MAX;

pub const MAX_APPS: usize = 15;
const COLUMNS: [i32; 3] = [132, 360, 588];
const GRID_TOP: i32 = 410;

fn pitch(count: usize) -> i32 {
    if count <= 12 { 286 } else { 232 }
}

fn centre(i: usize, count: usize) -> (i32, i32) {
    (COLUMNS[i % 3], GRID_TOP + (i / 3) as i32 * pitch(count))
}

pub fn tile_rect(i: usize, count: usize) -> Rect {
    let (cx, cy) = centre(i, count);
    Rect::new(cx - 112, cy - 96, cx + 112, cy + 136)
}

pub fn pill_rect() -> Rect {
    Rect::new(410, 98, 676, 170)
}

pub fn hit(s: &State, x: i32, y: i32) -> Option<usize> {
    if s.shield.known && pill_rect().contains(x, y) {
        return Some(PILL);
    }
    (0..s.apps.len()).find(|&i| tile_rect(i, s.apps.len()).contains(x, y))
}

/// What a press on `pressed` changes on screen.
pub fn region(s: &State, pressed: usize) -> Rect {
    if pressed == PILL { pill_rect().inset(-12) } else { tile_rect(pressed, s.apps.len()) }
}

/// The pictures Home keeps: the wallpaper and each app's icon, up and pressed.
pub struct Art {
    wallpaper: Canvas,
    icons: Vec<(String, Sprite, Sprite)>,
}

const PRESSED_TONE: Color = rgb(0xb9, 0xb1, 0xa2);

impl Art {
    pub fn new() -> Art {
        Art { wallpaper: paper::wallpaper(W, H), icons: Vec::new() }
    }
    /// Make the icons for apps that do not have one yet.
    pub fn prepare(&mut self, fonts: &Fonts, apps: &[App]) {
        self.icons.retain(|(n, _, _)| apps.iter().any(|a| a.name == *n));
        for a in apps {
            if self.icons.iter().any(|(n, _, _)| *n == a.name) {
                continue;
            }
            let mask = match a.name.as_str() {
                "ladybird" => icons::ladybird(),
                "bitchat" => icons::bitchat(fonts),
                "agent" => icons::agent(),
                "music" => icons::music(),
                "settings" => icons::settings(),
                _ => icons::initial(fonts, &a.title),
            };
            self.icons.push((a.name.clone(), mask.emboss(CREAM, 7.0), mask.emboss(PRESSED_TONE, 3.0)));
        }
    }
}

/// Redraw only r (a pressed or released icon): the same scene, clipped.
pub fn draw_clipped(c: &mut Canvas, f: &Fonts, art: &Art, s: &State, r: Rect) {
    c.set_clip(r);
    draw(c, f, art, s);
    c.reset_clip();
}

pub fn draw(c: &mut Canvas, f: &Fonts, art: &Art, s: &State) {
    c.copy_rows(&art.wallpaper, 0, H as i32);
    let date = s.now.as_ref().map(|t| t.date()).unwrap_or_else(|| "VioletOS".to_string());
    f.regular(50.0).draw(c, &date, 56, 152, CREAM);
    pill(c, f, s);
    let label = f.regular(27.0);
    let count = s.apps.len();
    for (i, a) in s.apps.iter().enumerate() {
        let (cx, cy) = centre(i, count);
        let pressed = s.pressed == Some(i);
        if let Some((_, up, down)) = art.icons.iter().find(|(n, _, _)| *n == a.name) {
            if pressed { down.draw_centered(c, cx + 1, cy + 3) } else { up.draw_centered(c, cx, cy) }
        }
        let text = label.fit(&a.title, 230);
        // The label sits on the paper with a soft shadow, like the icon.
        label.centered(c, &text, cx - 112 + 2, cx + 112 + 2, cy + 113, rgb(0x12, 0x11, 0x10));
        label.centered(c, &text, cx - 112, cx + 112, cy + 110, if pressed { MUTED } else { CREAM });
        if s.running.contains(&a.path()) {
            c.circle(cx as f32, cy as f32 + 128.0, 4.0, MUTED);
        }
    }
}

/// "Shield · Linked" in the corner: the phone's link to its companion.
fn pill(c: &mut Canvas, f: &Fonts, s: &State) {
    if !s.shield.known {
        return;
    }
    let r = pill_rect();
    let alert = !s.shield.frozen.is_empty();
    let pressed = s.pressed == Some(PILL);
    let base = if alert { rgb(0x4a, 0x1f, 0x1c) } else { rgb(0x1d, 0x1a, 0x24) };
    for k in 1..=6 {
        c.round_rect_alpha(r.offset(k / 2, k + 1).inset(-(k / 2)), 38.0 + k as f32 / 2.0, rgb(0, 0, 0), 0.08);
    }
    c.round_rect(r, 38.0, mix(base, paper::VIOLET, 0.28));
    c.round_rect(r.inset(2), 36.0, if pressed { mix(base, paper::VIOLET, 0.18) } else { base });
    let tint = if alert { paper::ALERT } else if s.shield.linked { paper::VIOLET } else { paper::DIM };
    // A shield: a rounded top and a point at the bottom.
    let (cx, cy) = (r.x0 as f32 + 42.0, r.y0 as f32 + 35.0);
    c.round_rect(Rect::new(cx as i32 - 14, cy as i32 - 17, cx as i32 + 14, cy as i32 + 3), 5.0, tint);
    c.triangle([[cx - 14.0, cy + 1.0], [cx + 14.0, cy + 1.0], [cx, cy + 19.0]], tint);
    c.circle(cx, cy - 4.0, 4.5, base);
    let text = if alert {
        format!("Shield · {} frozen", s.shield.frozen.len())
    } else if s.shield.linked {
        "Shield · Linked".to_string()
    } else {
        "Shield · Waiting".to_string()
    };
    let face = f.medium(24.0);
    face.draw(c, &face.fit(&text, r.x1 - 20 - (r.x0 + 68)), r.x0 + 68, r.y0 + 45, if s.shield.linked || alert { CREAM } else { MUTED });
}
