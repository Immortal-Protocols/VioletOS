//! Drawing Home: everything comes from State, so a preview can be drawn
//! without the devices.
use std::collections::HashSet;

use violet::paper::{self, icons, sdf, Sprite, CREAM, MUTED};
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
    pub warnings: u32,
}

pub struct State {
    pub now: Option<Local>,
    pub apps: Vec<App>,
    pub running: HashSet<String>,
    pub pressed: Option<usize>,
    pub launch_error: Option<String>,
    /// The newest notification an app posted ("notifications" in
    /// /dev/violet/sys, which only Home may read, and not while locked).
    pub notice: Option<String>,
    /// A passcode is set ("passcode" in /dev/violet/sys): Home shows Lock,
    /// which locks the phone now ("lock").
    pub can_lock: bool,
    pub shield: Shield,
    /// The voice agent is listening (in front or in the background).
    pub listening: bool,
}

/// What a press landed on: an app, the Shield pill, or Lock.
pub const PILL: usize = usize::MAX;
pub const LOCK: usize = usize::MAX - 1;

pub const MAX_APPS: usize = 15;
const COLUMNS: [i32; 3] = [144, 360, 572];
const LABEL_WIDTH: i32 = 196;

fn centre(i: usize, apps: &[App]) -> (i32, i32) {
    // In the full factory layout the second row has an open middle slot.
    let full_layout = apps.len() >= BUILT_IN.len()
        && apps.iter().zip(BUILT_IN).all(|(app, (name, _))| app.name == name);
    let slot = i + usize::from(full_layout && i >= 4);
    let row = (slot / 3) as i32;
    let y = if apps.len() > 12 {
        350 + row * 219
    } else {
        [407, 676, 901, 1126, 1351][row as usize]
    };
    (COLUMNS[slot % 3], y)
}

pub fn tile_rect(i: usize, apps: &[App]) -> Rect {
    let (cx, cy) = centre(i, apps);
    Rect::new(cx - 106, cy - 88, cx + 106, cy + 131)
}

pub fn pill_rect() -> Rect {
    Rect::new(398, 100, 674, 182)
}

/// Lock, under the pill, on the notification line's right.
pub fn lock_rect() -> Rect {
    Rect::new(538, 204, 674, 268)
}

pub fn hit(s: &State, x: i32, y: i32) -> Option<usize> {
    if s.shield.known && pill_rect().contains(x, y) {
        return Some(PILL);
    }
    if s.can_lock && lock_rect().contains(x, y) {
        return Some(LOCK);
    }
    (0..s.apps.len()).find(|&i| tile_rect(i, &s.apps).contains(x, y))
}

/// What a press on `pressed` changes on screen.
pub fn region(s: &State, pressed: usize) -> Rect {
    if pressed == PILL {
        pill_rect().inset(-12)
    } else if pressed == LOCK {
        lock_rect().inset(-4)
    } else {
        let tile = tile_rect(pressed, &s.apps);
        Rect::new(tile.x0 - 14, tile.y0, tile.x1 + 14, tile.y1 + 6)
    }
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
            self.icons.push((a.name.clone(), mask.emboss(CREAM, 9.0), mask.emboss(PRESSED_TONE, 4.0)));
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
    let date_face = f.regular(55.0);
    let date_width = pill_rect().x0 - if s.listening { 94 } else { 64 };
    let date_face = f.regular(55.0 * (date_width as f32 / date_face.width(&date) as f32).min(1.0));
    date_face.draw(c, &date, 50, 168, CREAM);
    pill(c, f, s);
    if s.listening {
        // A green microphone left of the pill while the agent hears the room.
        let (mx, top) = (pill_rect().x0 as f32 - 22.0, pill_rect().y0 as f32 + 23.0);
        c.line(mx, top + 6.0, mx, top + 16.0, 12.0, paper::GOOD);
        c.circle(mx, top + 6.0, 6.0, paper::GOOD);
        c.circle(mx, top + 16.0, 6.0, paper::GOOD);
        c.arc(mx, top + 15.0, 11.0, 3.0, -std::f32::consts::PI, 0.0, paper::GOOD);
        c.line(mx, top + 26.0, mx, top + 31.0, 3.0, paper::GOOD);
    }
    // The line under the date stops short of Lock when it is shown.
    let line = if s.can_lock { lock_rect().x0 - 70 } else { W as i32 - 100 };
    if let Some(message) = &s.launch_error {
        let face = f.regular(29.0);
        face.draw(c, &face.fit(message, line), 50, 250, paper::ALERT);
    } else if let Some(notice) = &s.notice {
        let face = f.regular(29.0);
        face.draw(c, &face.fit(notice, line), 50, 250, MUTED);
    }
    if s.can_lock {
        let r = lock_rect();
        let pressed = s.pressed == Some(LOCK);
        c.round_rect(r, 32.0, if pressed { rgb(0x43, 0x3a, 0x4a) } else { rgb(0x28, 0x25, 0x21) });
        let face = f.regular(30.0);
        face.centered(c, "Lock", r.x0, r.x1, r.y0 + 43, if pressed { MUTED } else { CREAM });
    }
    let label = f.regular(32.0);
    for (i, a) in s.apps.iter().enumerate() {
        let (cx, cy) = centre(i, &s.apps);
        let pressed = s.pressed == Some(i);
        if let Some((_, up, down)) = art.icons.iter().find(|(n, _, _)| *n == a.name) {
            if pressed { down.draw_centered(c, cx + 1, cy + 3) } else { up.draw_centered(c, cx, cy) }
        }
        let width = label.width(&a.title).max(LABEL_WIDTH);
        let face = f.regular((32.0 * LABEL_WIDTH as f32 / width as f32).max(24.0));
        let text = face.fit(&a.title, LABEL_WIDTH);
        let (left, right) = (cx - LABEL_WIDTH / 2, cx + LABEL_WIDTH / 2);
        // The label sits on the paper with a soft shadow, like the icon.
        face.centered(c, &text, left + 2, right + 2, cy + 127, rgb(0x12, 0x11, 0x10));
        face.centered(c, &text, left, right, cy + 124, if pressed { MUTED } else { CREAM });
        if s.running.contains(&a.path()) {
            c.circle(cx as f32, cy as f32 + 136.0, 3.0, MUTED);
        }
    }
}

/// "Shield · Linked" in the corner: the phone's link to its companion.
fn pill(c: &mut Canvas, f: &Fonts, s: &State) {
    if !s.shield.known {
        return;
    }
    let r = pill_rect();
    let alert = s.shield.warnings > 0;
    let pressed = s.pressed == Some(PILL);
    let body = if alert { rgb(0x43, 0x31, 0x1f) } else { rgb(0x28, 0x25, 0x21) };
    let glow = if alert { rgb(0xee, 0xb2, 0x64) } else { rgb(0xa0, 0x72, 0xb4) };
    c.round_rect_alpha(r.offset(1, 5), 41.0, rgb(0, 0, 0), 0.25);
    c.round_rect(r, 41.0, rgb(0x12, 0x11, 0x10));
    let cx = (r.x0 + 43) as f32;
    let cy = (r.y0 + 39) as f32;
    let area = r.intersect(c.clip());
    for y in area.y0..area.y1 {
        let v = (y - r.y0) as f32 / r.height() as f32;
        for x in area.x0..area.x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let edge = sdf::rbox(px, py, (r.x0 + r.x1) as f32 / 2.0, cy + 2.0, 138.0, 41.0, 41.0);
            let light = (1.0 - ((px - cx).hypot(py - cy) / 54.0)).max(0.0);
            let shade = mix(rgb(0x12, 0x11, 0x10), body, v + if pressed { 0.2 } else { 0.0 });
            let fill = mix(shade, glow, light * light * if s.shield.linked || alert { 0.64 } else { 0.36 });
            let rim = mix(fill, CREAM, v * v * 0.34);
            c.blend(x, y, fill, (-edge - 1.0).clamp(0.0, 1.0));
            c.blend(x, y, rim, (1.0 - (edge + 0.5).abs()).max(0.0));
        }
    }

    let shield = [(-15.0, -13.0), (0.0, -19.0), (15.0, -13.0), (14.0, -1.0),
                  (10.0, 9.0), (5.0, 15.0), (0.0, 18.0), (-5.0, 15.0),
                  (-10.0, 9.0), (-14.0, -1.0)];
    let tint = if alert { rgb(0xee, 0xb2, 0x64) } else if s.shield.linked { CREAM } else { MUTED };
    let icon = Rect::new(cx as i32 - 18, cy as i32 - 21, cx as i32 + 18, cy as i32 + 21).intersect(area);
    for y in icon.y0..icon.y1 {
        for x in icon.x0..icon.x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let edge = sdf::polygon(dx, dy, &shield);
            let light = (0.58 - (dy + dx * 0.5) / 57.0).clamp(0.0, 1.0);
            let color = mix(mix(tint, glow, 0.35), tint, light);
            c.blend(x, y, color, (0.9 - edge).clamp(0.0, 1.0));
        }
    }
    let text = if alert {
        let count = s.shield.warnings;
        let suffix = if count == 1 { "" } else { "s" };
        format!("Shield · {count} warning{suffix}")
    } else if s.shield.linked {
        "Shield · Linked".to_string()
    } else {
        "Shield · Waiting".to_string()
    };
    let face = f.regular(28.0);
    let max_width = r.width() - 87;
    let face = f.regular(28.0 * (max_width as f32 / face.width(&text) as f32).min(1.0));
    face.draw(c, &text, r.x0 + 70, r.y0 + 52, if s.shield.linked || alert { CREAM } else { MUTED });
}
