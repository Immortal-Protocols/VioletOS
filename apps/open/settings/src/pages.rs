//! The Settings pages other than Wi-Fi. Everything is drawn from `View`, so a
//! preview can be drawn without the devices.
use violet::keypad::{self, PadKey};
use violet::paper::{slab, ALERT, CREAM, GOOD, MUTED, PAPER, PAPER_PRESSED, PAPER_RAISED, VIOLET};

use crate::passcode::{Passcode, Step};
use violet::{Canvas, Fonts, Rect, CITIES, HOME_BOX, W};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Menu,
    Wifi,
    Bluetooth,
    PhoneLink,
    Time,
    AgentApps,
    Passcode,
    Power,
    About,
}

/// The menu, in order.
pub const MENU: [(Section, &str); 8] = [
    (Section::Wifi, "Wi-Fi"),
    (Section::Bluetooth, "Bluetooth"),
    (Section::PhoneLink, "Phone Link"),
    (Section::Time, "Date & Time"),
    (Section::AgentApps, "Agent Apps"),
    (Section::Passcode, "Passcode"),
    (Section::Power, "Power"),
    (Section::About, "About"),
];

/// Something under the finger, drawn pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Back,
    Row(usize),
    Relay,
    NewCode,
    City(usize),
    Delete(usize),
    DeleteAll,
    /// Passcode: choose or change it, turn it off, a pad key.
    PasscodeSet,
    PasscodeOff,
    Pad(PadKey),
    PowerOff,
}

/// An app Violet Agent installed.
#[derive(Clone, PartialEq, Debug)]
pub struct Installed {
    pub name: String,
    pub title: String,
    pub bytes: u64,
}

/// "name<TAB>bytes<TAB>title[<TAB>package]" lines from "installed": the apps,
/// leaving out packages (Python, which the apps need) and the ".free" line.
pub fn parse_installed(text: &str) -> Vec<Installed> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, bytes, title) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            if name.starts_with('.') || f.next() == Some("package") {
                return None;
            }
            Some(Installed { name: name.into(), bytes: bytes.parse().ok()?, title: if title.is_empty() { name.into() } else { title.into() } })
        })
        .collect()
}

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    pub wifi: String,
    pub bt_on: bool,
    pub bt_open: bool,
    pub shield_known: bool,
    pub shield_linked: bool,
    pub shield_rules: u32,
    pub frozen: Vec<String>,
    /// Phone Internet: "off", "starting", "waiting", "joined", "online", "unavailable" or "busy".
    pub relay: String,
    pub relay_failure: u32,
    pub battery: i32,
    pub volume: i32,
    pub city: usize,
    pub installed: Vec<Installed>,
    /// A passcode locks the phone.
    pub passcode: bool,
}

pub struct View<'a> {
    pub section: Section,
    pub status: &'a Status,
    /// The saved Phone Internet pairing code and whether it should be on.
    pub code: &'a str,
    pub relay_on: bool,
    pub pressed: Option<Hit>,
    /// The app whose Delete was tapped once (usize::MAX: Delete all).
    pub confirm: Option<usize>,
    pub note: &'a str,
    pub passcode: &'a Passcode,
}

const MENU_TOP: i32 = 250;
const MENU_ROW: i32 = 124;

pub fn menu_rect(i: usize) -> Rect {
    let y = MENU_TOP + i as i32 * MENU_ROW;
    Rect::new(40, y, W as i32 - 40, y + MENU_ROW - 16)
}
pub const RELAY_BUTTON: Rect = Rect::new(40, 1010, W as i32 - 40, 1110);
pub const NEW_CODE: Rect = Rect::new(40, 1140, W as i32 - 40, 1240);
const CITY_TOP: i32 = 250;
const CITY_ROW: i32 = 118;
pub fn city_rect(i: usize) -> Rect {
    let (col, row) = (i as i32 % 2, i as i32 / 2);
    let x = 40 + col * 330;
    let y = CITY_TOP + row * CITY_ROW;
    Rect::new(x, y, x + 310, y + CITY_ROW - 16)
}
const APPS_TOP: i32 = 250;
const APP_ROW: i32 = 124;
pub const MAX_LISTED: usize = 8;
pub fn app_rect(i: usize) -> Rect {
    let y = APPS_TOP + i as i32 * APP_ROW;
    Rect::new(40, y, W as i32 - 40, y + APP_ROW - 16)
}
pub fn delete_rect(i: usize) -> Rect {
    let r = app_rect(i);
    Rect::new(r.x1 - 196, r.y0 + 20, r.x1 - 20, r.y1 - 20)
}
pub const DELETE_ALL: Rect = Rect::new(40, 1300, W as i32 - 40, 1400);
pub const PASSCODE_SET: Rect = Rect::new(40, 540, W as i32 - 40, 650);
pub const PASSCODE_OFF: Rect = Rect::new(40, 680, W as i32 - 40, 790);
pub const POWER_OFF: Rect = Rect::new(40, 540, W as i32 - 40, 650);
/// Confirming Power off (View::confirm).
pub const CONFIRM_POWER: usize = usize::MAX - 1;

pub fn hit(v: &View, x: i32, y: i32) -> Option<Hit> {
    if HOME_BOX.contains(x, y) {
        return Some(Hit::Back);
    }
    match v.section {
        Section::Menu => (0..MENU.len()).find(|&i| menu_rect(i).contains(x, y)).map(Hit::Row),
        Section::PhoneLink if relay_usable(v.status) => {
            if RELAY_BUTTON.contains(x, y) {
                Some(Hit::Relay)
            } else if !v.relay_on && NEW_CODE.contains(x, y) {
                Some(Hit::NewCode)
            } else {
                None
            }
        }
        Section::Time => (0..CITIES.len()).find(|&i| city_rect(i).contains(x, y)).map(Hit::City),
        Section::AgentApps => {
            let n = v.status.installed.len().min(MAX_LISTED);
            if n > 0 && DELETE_ALL.contains(x, y) {
                return Some(Hit::DeleteAll);
            }
            (0..n).find(|&i| delete_rect(i).contains(x, y)).map(Hit::Delete)
        }
        Section::Passcode if v.passcode.step == Step::Overview => {
            if PASSCODE_SET.contains(x, y) {
                Some(Hit::PasscodeSet)
            } else if v.passcode.set && PASSCODE_OFF.contains(x, y) {
                Some(Hit::PasscodeOff)
            } else {
                None
            }
        }
        Section::Passcode => keypad::key_at(x, y).map(Hit::Pad),
        Section::Power => POWER_OFF.contains(x, y).then_some(Hit::PowerOff),
        _ => None,
    }
}

fn relay_usable(s: &Status) -> bool {
    s.relay != "unavailable"
}

fn header(c: &mut Canvas, f: &Fonts, v: &View, back: &str, title: &str) {
    let bg = if v.pressed == Some(Hit::Back) { PAPER_PRESSED } else { PAPER_RAISED };
    violet::button(c, &f.medium(24.0), HOME_BOX, back, bg, CREAM);
    f.regular(48.0).draw(c, title, 40, 180, CREAM);
}

fn chevron(c: &mut Canvas, x: f32, y: f32) {
    c.line(x - 6.0, y - 13.0, x + 5.0, y, 4.0, MUTED);
    c.line(x + 5.0, y, x - 6.0, y + 13.0, 4.0, MUTED);
}

/// A paragraph wrapped to `width`; returns the y after it.
fn paragraph(c: &mut Canvas, f: &violet::Face, text: &str, x: i32, mut y: i32, width: i32, ink: violet::Color) -> i32 {
    let mut line = String::new();
    for word in text.split_whitespace() {
        let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if f.width(&next) > width && !line.is_empty() {
            f.draw(c, &line, x, y, ink);
            y += (f.size() * 1.4) as i32;
            line = word.to_string();
        } else {
            line = next;
        }
    }
    if !line.is_empty() {
        f.draw(c, &line, x, y, ink);
        y += (f.size() * 1.4) as i32;
    }
    y
}

pub fn menu_value(s: &Status, section: Section) -> (String, bool) {
    match section {
        Section::Wifi => (s.wifi.clone(), s.wifi != "Not connected"),
        Section::Bluetooth => ((if s.bt_on { "On" } else { "Off" }).into(), s.bt_on),
        Section::PhoneLink => {
            let shield = if !s.shield_known { "" } else if s.shield_linked { "Shield linked" } else { "Shield waiting" };
            let relay = match s.relay.as_str() {
                "online" => "Internet on",
                "waiting" | "joined" | "starting" => "Internet starting",
                _ => "",
            };
            let parts: Vec<&str> = [shield, relay].into_iter().filter(|p| !p.is_empty()).collect();
            (if parts.is_empty() { "Not linked".into() } else { parts.join(" · ") }, s.shield_linked || s.relay == "online")
        }
        Section::Time => (CITIES[s.city.min(CITIES.len() - 1)].into(), false),
        Section::Passcode => ((if s.passcode { "On" } else { "Off" }).into(), s.passcode),
        Section::AgentApps => {
            let n = s.installed.len();
            (if n == 0 { "None".into() } else { format!("{n} installed") }, false)
        }
        _ => (String::new(), false),
    }
}

pub fn draw(c: &mut Canvas, f: &Fonts, v: &View) {
    c.fill(c.bounds(), PAPER);
    let s = v.status;
    match v.section {
        Section::Menu => {
            header(c, f, v, "‹ Home", "Settings");
            let (title, value) = (f.medium(34.0), f.regular(27.0));
            for (i, (section, name)) in MENU.iter().enumerate() {
                let r = menu_rect(i);
                slab(c, r, 26.0, if v.pressed == Some(Hit::Row(i)) { PAPER_PRESSED } else { PAPER_RAISED });
                let base = r.y0 + r.height() / 2 + 12;
                title.draw(c, name, r.x0 + 32, base, CREAM);
                let (text, lit) = menu_value(s, *section);
                let w = value.width(&text).min(300);
                value.draw(c, &value.fit(&text, 300), r.x1 - 60 - w, base - 1, if lit { VIOLET } else { MUTED });
                chevron(c, (r.x1 - 34) as f32, (r.y0 + r.height() / 2) as f32);
            }
        }
        Section::Wifi => {}
        Section::Bluetooth => {
            header(c, f, v, "‹ Settings", "Bluetooth");
            let card = Rect::new(40, 230, W as i32 - 40, 390);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(36.0).draw(c, if s.bt_on { "On" } else { "Off" }, 80, 300, if s.bt_on { VIOLET } else { CREAM });
            let line = if s.bt_open { "In use by Bitchat" } else if s.bt_on { "Ready for nearby chat" } else { "The radio is not running" };
            f.regular(27.0).draw(c, line, 80, 346, MUTED);
            paragraph(c, &f.regular(28.0), "Bluetooth shares the phone's connectivity chip with Wi-Fi, so it runs while Wi-Fi is on. Bitchat uses it to chat with phones nearby, with no internet needed.", 44, 460, W as i32 - 88, MUTED);
        }
        Section::PhoneLink => {
            header(c, f, v, "‹ Settings", "Phone Link");
            // Violet Shield.
            let card = Rect::new(40, 230, W as i32 - 40, 420);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(23.0).draw(c, "VIOLET SHIELD", 80, 284, MUTED);
            let (head, tint) = if !s.frozen.is_empty() {
                (format!("Froze {}", s.frozen.join(", ")), ALERT)
            } else if s.shield_linked {
                ("Linked to your Mac".to_string(), VIOLET)
            } else {
                ("Waiting for your Mac".to_string(), CREAM)
            };
            let big = f.medium(34.0);
            big.draw(c, &big.fit(&head, card.width() - 80), 80, 336, tint);
            let rules = format!("{} rule{} in force", s.shield_rules, if s.shield_rules == 1 { "" } else { "s" });
            f.regular(26.0).draw(c, if s.shield_known { &rules } else { "Not running on this phone" }, 80, 382, MUTED);
            // Phone Internet.
            let card = Rect::new(40, 450, W as i32 - 40, 960);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(23.0).draw(c, "PHONE INTERNET", 80, 504, MUTED);
            let state = match s.relay.as_str() {
                "online" => "Online through your phone",
                "joined" => "Your phone joined",
                "waiting" => "Waiting for your phone",
                "starting" => "Starting…",
                "unavailable" => "Not available on this phone",
                "busy" => "Checking…",
                _ if v.relay_on && s.relay_failure != 0 => "Couldn't start",
                _ => "Off",
            };
            big.draw(c, state, 80, 556, if s.relay == "online" { GOOD } else { CREAM });
            let mut y = 620;
            if relay_usable(s) {
                f.medium(23.0).draw(c, "PAIRING CODE", 80, y, MUTED);
                let spaced = if v.code.len() == 12 { format!("{} {} {}", &v.code[..4], &v.code[4..8], &v.code[8..]) } else { "—".into() };
                f.medium(52.0).draw(c, &spaced, 80, y + 70, CREAM);
                y += 130;
            }
            paragraph(c, &f.regular(26.0), "Open Violet Relay on your iPhone, enter the code and tap Join. This phone then reaches the internet over your iPhone's cellular data.", 80, y, card.width() - 80, MUTED);
            if relay_usable(s) {
                let on = v.relay_on;
                let bg = if v.pressed == Some(Hit::Relay) { PAPER_PRESSED } else if on { PAPER_RAISED } else { VIOLET };
                violet::button(c, &f.medium(34.0), RELAY_BUTTON, if on { "Turn off" } else { "Turn on" }, bg, if on { CREAM } else { PAPER });
                if !on {
                    let bg = if v.pressed == Some(Hit::NewCode) { PAPER_PRESSED } else { PAPER_RAISED };
                    violet::button(c, &f.medium(30.0), NEW_CODE, "New code", bg, CREAM);
                }
            }
            if !v.note.is_empty() {
                f.regular(26.0).centered(c, v.note, 0, W as i32, 1290, ALERT);
            }
        }
        Section::Time => {
            header(c, f, v, "‹ Settings", "Date & Time");
            f.regular(26.0).draw(c, "Home shows the date where you are", 44, 228, MUTED);
            let face = f.medium(28.0);
            for (i, city) in CITIES.iter().enumerate() {
                let r = city_rect(i);
                let chosen = s.city == i;
                slab(c, r, 22.0, if v.pressed == Some(Hit::City(i)) { PAPER_PRESSED } else { PAPER_RAISED });
                face.draw(c, &face.fit(city, r.width() - 80), r.x0 + 26, r.y0 + r.height() / 2 + 10, if chosen { VIOLET } else { CREAM });
                if chosen {
                    let (cx, cy) = ((r.x1 - 36) as f32, (r.y0 + r.height() / 2) as f32);
                    c.line(cx - 12.0, cy, cx - 3.0, cy + 9.0, 5.0, VIOLET);
                    c.line(cx - 3.0, cy + 9.0, cx + 13.0, cy - 10.0, 5.0, VIOLET);
                }
            }
        }
        Section::AgentApps => {
            header(c, f, v, "‹ Settings", "Agent Apps");
            f.regular(26.0).draw(c, "Apps Violet Agent built for you", 44, 228, MUTED);
            if s.installed.is_empty() {
                paragraph(c, &f.regular(30.0), "No apps yet. Ask Violet Agent for one and it appears on Home.", 44, 340, W as i32 - 88, MUTED);
            }
            let (title, small) = (f.medium(32.0), f.regular(24.0));
            for (i, a) in s.installed.iter().take(MAX_LISTED).enumerate() {
                let r = app_rect(i);
                slab(c, r, 26.0, PAPER_RAISED);
                let d = delete_rect(i);
                title.draw(c, &title.fit(&a.title, d.x0 - r.x0 - 60), r.x0 + 30, r.y0 + 50, CREAM);
                small.draw(c, &format!("{} KB", (a.bytes + 1023) / 1024), r.x0 + 30, r.y0 + 88, MUTED);
                let confirm = v.confirm == Some(i);
                let bg = if v.pressed == Some(Hit::Delete(i)) { PAPER_PRESSED } else if confirm { ALERT } else { PAPER };
                violet::button(c, &f.medium(26.0), d, if confirm { "Confirm" } else { "Delete" }, bg, if confirm { PAPER } else { ALERT });
            }
            if s.installed.len() > MAX_LISTED {
                small.draw(c, &format!("and {} more", s.installed.len() - MAX_LISTED), 44, app_rect(MAX_LISTED).y0 + 30, MUTED);
            }
            if !s.installed.is_empty() {
                let confirm = v.confirm == Some(usize::MAX);
                let bg = if v.pressed == Some(Hit::DeleteAll) { PAPER_PRESSED } else if confirm { ALERT } else { PAPER_RAISED };
                let label = if confirm { "Tap again to delete all".to_string() } else { format!("Delete all {}", s.installed.len()) };
                violet::button(c, &f.medium(32.0), DELETE_ALL, &label, bg, if confirm { PAPER } else { ALERT });
            }
            if !v.note.is_empty() {
                f.regular(26.0).centered(c, v.note, 0, W as i32, 1460, ALERT);
            }
        }
        Section::Passcode => {
            let p = v.passcode;
            header(c, f, v, "‹ Settings", "Passcode");
            if p.step == Step::Overview {
                let card = Rect::new(40, 230, W as i32 - 40, 500);
                slab(c, card, 30.0, PAPER_RAISED);
                f.medium(36.0).draw(c, if p.set { "On" } else { "Off" }, 80, 300, if p.set { VIOLET } else { CREAM });
                let about = if p.set {
                    "The power button locks the phone, and it starts locked. Your passcode unlocks it."
                } else {
                    "With a passcode, the power button locks the phone and it starts locked. A passcode is 4 to 8 digits."
                };
                paragraph(c, &f.regular(28.0), about, 80, 360, card.width() - 80, MUTED);
                let bg = if v.pressed == Some(Hit::PasscodeSet) { PAPER_PRESSED } else if p.set { PAPER_RAISED } else { VIOLET };
                violet::button(c, &f.medium(34.0), PASSCODE_SET, if p.set { "Change passcode" } else { "Set a passcode" }, bg, if p.set { CREAM } else { PAPER });
                if p.set {
                    let bg = if v.pressed == Some(Hit::PasscodeOff) { PAPER_PRESSED } else { PAPER_RAISED };
                    violet::button(c, &f.medium(34.0), PASSCODE_OFF, "Turn off passcode", bg, ALERT);
                }
                if !p.note.is_empty() {
                    f.regular(30.0).centered(c, &p.note, 0, W as i32, 880, if p.good { GOOD } else { ALERT });
                }
            } else {
                f.regular(36.0).centered(c, p.prompt(), 0, W as i32, 560, CREAM);
                keypad::dots(c, 650, p.typed.len());
                if !p.note.is_empty() {
                    f.regular(30.0).centered(c, &p.note, 0, W as i32, 740, ALERT);
                }
                let pressed = match v.pressed {
                    Some(Hit::Pad(k)) => Some(k),
                    _ => None,
                };
                keypad::draw(c, f, p.typed.len(), pressed);
            }
        }
        Section::Power => {
            header(c, f, v, "‹ Settings", "Power");
            let card = Rect::new(40, 230, W as i32 - 40, 500);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(36.0).draw(c, if s.battery >= 0 { format!("Battery {}%", s.battery) } else { "Power button".into() }.as_str(), 80, 300, CREAM);
            paragraph(c, &f.regular(28.0), "Press the power button to turn the screen off, and again to turn it on. Hold it on any screen to power off.", 80, 360, card.width() - 80, MUTED);
            let confirm = v.confirm == Some(CONFIRM_POWER);
            let bg = if v.pressed == Some(Hit::PowerOff) { PAPER_PRESSED } else if confirm { ALERT } else { PAPER_RAISED };
            violet::button(c, &f.medium(34.0), POWER_OFF, if confirm { "Tap again to power off" } else { "Power off" }, bg, if confirm { PAPER } else { ALERT });
            if !v.note.is_empty() {
                f.regular(26.0).centered(c, v.note, 0, W as i32, 720, ALERT);
            }
        }
        Section::About => {
            header(c, f, v, "‹ Settings", "About");
            let card = Rect::new(40, 230, W as i32 - 40, 700);
            slab(c, card, 30.0, PAPER_RAISED);
            let (label, value) = (f.regular(27.0), f.medium(30.0));
            let rows = [
                ("System", "VioletOS".to_string()),
                ("Kernel", "seL4 microkernel".to_string()),
                ("Battery", if s.battery >= 0 { format!("{}%", s.battery) } else { "—".into() }),
                ("Volume", if s.volume >= 0 { s.volume.to_string() } else { "—".into() }),
                ("Agent apps", s.installed.len().to_string()),
                ("Wi-Fi", s.wifi.clone()),
            ];
            for (k, (name, v)) in rows.iter().enumerate() {
                let y = 300 + k as i32 * 66;
                label.draw(c, name, 80, y, MUTED);
                value.draw(c, &value.fit(v, card.x1 - 40 - 300), 300, y, CREAM);
            }
        }
    }
}
