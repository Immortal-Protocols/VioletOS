//! The Settings pages other than Wi-Fi. Everything is drawn from `View`, so a
//! preview can be drawn without the devices.
use violet::paper::{slab, ALERT, CREAM, GOOD, MUTED, PAPER, PAPER_PRESSED, PAPER_RAISED, VIOLET};
use violet::{Canvas, Fonts, Rect, CITIES, HOME_BOX, W};

use crate::nearby::Scan;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Menu,
    Wifi,
    Bluetooth,
    /// What the radio is doing and who made it so; reached from Bluetooth.
    BluetoothDetails,
    /// A passive scan of the devices nearby; reached from Details.
    BluetoothNearby,
    PhoneLink,
    Time,
    AgentApps,
    About,
}

/// The menu, in order.
pub const MENU: [(Section, &str); 6] = [
    (Section::Wifi, "Wi-Fi"),
    (Section::Bluetooth, "Bluetooth"),
    (Section::PhoneLink, "Phone Link"),
    (Section::Time, "Date & Time"),
    (Section::AgentApps, "Agent Apps"),
    (Section::About, "About"),
];

/// Something under the finger, drawn pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Back,
    Row(usize),
    Details,
    /// The app holding Bluetooth, on Details: opens it.
    HolderApp,
    /// Scan nearby, on Details; then Cancel, Proceed and Stop on its page.
    ScanNearby,
    Cancel,
    Proceed,
    Stop,
    Relay,
    NewCode,
    City(usize),
    Delete(usize),
    DeleteAll,
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

/// Bluetooth as the Open Runtime saw it ("bluetooth" in /dev/violet/sys): the
/// commands the holding app sent, not what the app says about itself.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Bluetooth {
    pub on: bool,
    pub open: bool,
    /// The title of the app holding the controller, empty when none does.
    pub app: String,
    /// Its program name in /apps, to open it.
    pub name: String,
    pub advertising: bool,
    pub scanning: bool,
    /// The scan asks devices nearby for more, which transmits.
    pub active: bool,
    pub random_adv: bool,
    pub random_scan: bool,
    /// "off", "starting", "on" or "failed".
    pub state: String,
    pub error: u32,
    pub rx: u32,
    pub tx: u32,
    pub bad: u32,
    pub dropped: u32,
    /// The controller's HCI version code and Bluetooth SIG company id; 0 before it answered.
    pub hci: u32,
    pub maker: u32,
}

impl Bluetooth {
    /// From the runtime's answer, with titles from /apps/.home ("name<TAB>title" lines).
    pub fn parse(answer: &str, titles: &str) -> Self {
        let f = violet::fields(answer);
        let flag = |k: &str| f.get(k).is_some_and(|v| v == "1");
        let num = |k: &str| {
            let v = f.get(k).map_or("", |v| v.as_str());
            match v.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).unwrap_or(0),
                None => v.parse().unwrap_or(0),
            }
        };
        let name = f.get("app").filter(|a| *a != "-").cloned().unwrap_or_default();
        let app = titles.lines().filter_map(|l| l.split_once('\t')).find(|(n, _)| *n == name).map_or(name.clone(), |(_, t)| t.to_string());
        Bluetooth {
            on: flag("on"),
            open: flag("open"),
            app,
            name,
            advertising: flag("advertising"),
            scanning: flag("scanning"),
            active: flag("active"),
            random_adv: flag("random_adv"),
            random_scan: flag("random_scan"),
            state: f.get("state").cloned().unwrap_or_else(|| "off".into()),
            error: num("error"),
            rx: num("rx"),
            tx: num("tx"),
            bad: num("bad"),
            dropped: num("dropped"),
            hci: num("hci"),
            maker: num("maker"),
        }
    }

    /// Who is using it, for the line under the title.
    fn holder(&self) -> String {
        match (self.open, self.app.is_empty()) {
            (false, _) => "No app is using it".into(),
            (true, true) => "In use by an app".into(),
            (true, false) => format!("In use by {}", self.app),
        }
    }

    /// The Bluetooth version the controller's HCI speaks, as people know it.
    fn version(&self) -> &'static str {
        match self.hci {
            6 => "4.0",
            7 => "4.1",
            8 => "4.2",
            9 => "5.0",
            10 => "5.1",
            11 => "5.2",
            12 => "5.3",
            13 => "5.4",
            _ => "",
        }
    }

    /// The Bluetooth SIG company name for the controller's maker.
    fn manufacturer(&self) -> String {
        match self.maker {
            2 => "Intel".into(),
            15 => "Broadcom".into(),
            29 => "Qualcomm".into(),
            70 => "MediaTek".into(),
            93 => "Realtek".into(),
            n => format!("Company {n}"),
        }
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    pub wifi: String,
    pub bt: Bluetooth,
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
}

/// Scan nearby: asking first, then the scan (running or stopped), or why it couldn't start.
#[derive(Clone, Copy)]
pub enum Nearby<'a> {
    Ask,
    Scan { scan: &'a Scan, running: bool, seconds: u64 },
    Failed(&'a str),
}

pub struct View<'a> {
    pub section: Section,
    pub nearby: Nearby<'a>,
    pub status: &'a Status,
    /// The saved Phone Internet pairing code and whether it should be on.
    pub code: &'a str,
    pub relay_on: bool,
    pub pressed: Option<Hit>,
    /// The app whose Delete was tapped once (usize::MAX: Delete all).
    pub confirm: Option<usize>,
    pub note: &'a str,
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
pub const BT_DETAILS: Rect = Rect::new(40, 700, W as i32 - 40, 800);
pub const BT_HOLDER: Rect = Rect::new(40, 230, W as i32 - 40, 370);
pub const SCAN_NEARBY: Rect = Rect::new(40, 1440, W as i32 - 40, 1540);
pub const CANCEL: Rect = Rect::new(40, 1440, W as i32 / 2 - 12, 1540);
pub const PROCEED: Rect = Rect::new(W as i32 / 2 + 12, 1440, W as i32 - 40, 1540);
/// Stop, or Done once the scan stopped.
pub const STOP: Rect = Rect::new(40, 1440, W as i32 - 40, 1540);

pub fn hit(v: &View, x: i32, y: i32) -> Option<Hit> {
    if HOME_BOX.contains(x, y) {
        return Some(Hit::Back);
    }
    match v.section {
        Section::Menu => (0..MENU.len()).find(|&i| menu_rect(i).contains(x, y)).map(Hit::Row),
        Section::Bluetooth if BT_DETAILS.contains(x, y) => Some(Hit::Details),
        Section::BluetoothDetails if v.status.bt.open && !v.status.bt.name.is_empty() && BT_HOLDER.contains(x, y) => Some(Hit::HolderApp),
        Section::BluetoothDetails if SCAN_NEARBY.contains(x, y) => Some(Hit::ScanNearby),
        Section::BluetoothNearby => match v.nearby {
            Nearby::Ask if CANCEL.contains(x, y) => Some(Hit::Cancel),
            Nearby::Ask if PROCEED.contains(x, y) && can_scan(&v.status.bt).is_ok() => Some(Hit::Proceed),
            Nearby::Scan { .. } | Nearby::Failed(_) if STOP.contains(x, y) => Some(Hit::Stop),
            _ => None,
        },
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
        Section::Bluetooth => ((if s.bt.on { "On" } else { "Off" }).into(), s.bt.on),
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
            f.medium(36.0).draw(c, if s.bt.on { "On" } else { "Off" }, 80, 300, if s.bt.on { VIOLET } else { CREAM });
            let line = if s.bt.open { s.bt.holder() } else if s.bt.on { "Ready for nearby chat".into() } else { "The radio is not running".into() };
            f.regular(27.0).draw(c, &line, 80, 346, MUTED);
            paragraph(c, &f.regular(28.0), "Bluetooth shares the phone's connectivity chip with Wi-Fi, so it runs while Wi-Fi is on. Bitchat uses it to chat with phones nearby, with no internet needed.", 44, 460, W as i32 - 88, MUTED);
            let bg = if v.pressed == Some(Hit::Details) { PAPER_PRESSED } else { PAPER_RAISED };
            violet::button(c, &f.medium(32.0), BT_DETAILS, "Details", bg, CREAM);
        }
        Section::BluetoothDetails => bluetooth_details(c, f, v, &s.bt),
        Section::BluetoothNearby => bluetooth_nearby(c, f, v, &s.bt),
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

/// Who can hear this phone, who it can hear, and the controller, from what
/// the runtime saw the holding app tell it.
fn bluetooth_details(c: &mut Canvas, f: &Fonts, v: &View, bt: &Bluetooth) {
    header(c, f, v, "‹ Bluetooth", "Details");
    // Tapping the app opens it.
    let held = BT_HOLDER;
    let opens = bt.open && !bt.name.is_empty();
    slab(c, held, 30.0, if v.pressed == Some(Hit::HolderApp) { PAPER_PRESSED } else { PAPER_RAISED });
    if opens {
        chevron(c, (held.x1 - 34) as f32, (held.y0 + held.height() / 2) as f32);
    }
    f.medium(23.0).draw(c, "IN USE BY", 80, 282, MUTED);
    let app = if !bt.open { "No app" } else if bt.app.is_empty() { "An app" } else { &bt.app };
    let big = f.medium(34.0);
    big.draw(c, &big.fit(app, held.width() - 120), 80, 336, if bt.open { VIOLET } else { CREAM });
    let (label, value) = (f.regular(26.0), f.medium(28.0));
    let card = |c: &mut Canvas, top: i32, title: &str, rows: &[(&str, String, violet::Color)]| {
        let r = Rect::new(40, top, W as i32 - 40, top + 86 + rows.len() as i32 * 60);
        slab(c, r, 30.0, PAPER_RAISED);
        f.medium(23.0).draw(c, title, 80, top + 52, MUTED);
        for (k, (name, text, ink)) in rows.iter().enumerate() {
            let y = top + 112 + k as i32 * 60;
            label.draw(c, name, 80, y, MUTED);
            value.draw(c, &value.fit(text, r.x1 - 40 - 300), 300, y, *ink);
        }
        r.y1
    };
    let lit = |on: bool| if on { VIOLET } else { CREAM };
    // The Core Specification's names: a public or random device address.
    let address = |random: bool| if random { "Random" } else { "Public" };
    let y = card(c, held.y1 + 24, "WHO CAN HEAR ME?", &[
        ("Advertising", if bt.advertising { "On".into() } else { "Off".into() }, lit(bt.advertising)),
        ("Address type", if bt.advertising { address(bt.random_adv).into() } else { "—".into() }, CREAM),
    ]);
    // A passive scan only listens; an active one sends scan requests from an address.
    let y = card(c, y + 24, "WHO CAN I HEAR?", &[
        ("Scanning", match (bt.scanning, bt.active) {
            (false, _) => "Off".into(),
            (true, false) => "Passive".into(),
            (true, true) => "Active · sends scan requests".into(),
        }, lit(bt.scanning)),
        ("Address type", if bt.scanning && bt.active { address(bt.random_scan).into() } else if bt.scanning { "None sent".into() } else { "—".into() }, CREAM),
    ]);
    let state = match bt.state.as_str() {
        "on" => "On".to_string(),
        "starting" => "Starting…".into(),
        "failed" => format!("Failed ({:#x})", bt.error),
        _ => "Off · runs while Wi-Fi is on".into(),
    };
    let dash = |t: String| if bt.hci == 0 { "—".to_string() } else { t };
    let y = card(c, y + 24, "CONTROLLER", &[
        ("State", state, if bt.state == "failed" { ALERT } else { lit(bt.state == "on") }),
        ("Manufacturer", dash(bt.manufacturer()), CREAM),
        ("HCI version", dash(match bt.version() { "" => format!("{:#x}", bt.hci), v => format!("Bluetooth {v}") }), CREAM),
        ("Frames", format!("{} received · {} sent", bt.rx, bt.tx), CREAM),
        ("Errors", format!("{} rejected · {} dropped", bt.bad, bt.dropped), if bt.bad != 0 || bt.dropped != 0 { ALERT } else { CREAM }),
    ]);
    paragraph(c, &f.regular(26.0), "VioletOS never makes this phone visible by itself. Only an app that opens Bluetooth can, and it shows here. When the app closes, the radio stops.", 44, y + 60, W as i32 - 88, MUTED);
    let bg = if v.pressed == Some(Hit::ScanNearby) { PAPER_PRESSED } else { PAPER_RAISED };
    violet::button(c, &f.medium(32.0), SCAN_NEARBY, "Scan nearby", bg, CREAM);
}

/// Whether Settings can take the controller for a scan, or why not.
pub fn can_scan(bt: &Bluetooth) -> Result<(), String> {
    if !bt.on {
        return Err("Bluetooth is off. It runs while Wi-Fi is on.".into());
    }
    if bt.open {
        let app = if bt.app.is_empty() { "An app" } else { &bt.app };
        return Err(format!("{app} is using Bluetooth. Close it to scan."));
    }
    Ok(())
}

/// Scan nearby: what the scan does to whom, asked before it starts; then
/// the devices heard, strongest first.
fn bluetooth_nearby(c: &mut Canvas, f: &Fonts, v: &View, bt: &Bluetooth) {
    header(c, f, v, "‹ Details", "Scan nearby");
    let (name, text) = (f.medium(30.0), f.regular(26.0));
    match v.nearby {
        Nearby::Ask => {
            f.regular(26.0).draw(c, "For as long as the scan runs:", 44, 228, MUTED);
            let card = Rect::new(40, 260, W as i32 - 40, 690);
            slab(c, card, 30.0, PAPER_RAISED);
            let mut y = 320;
            for (who, what) in [
                ("This phone", "Listens with a passive scan. It receives advertisements and transmits nothing."),
                ("Devices nearby", "Can't see this phone or tell that it is listening."),
                ("Bitchat and other apps", "Can't use Bluetooth until you stop the scan."),
            ] {
                name.draw(c, who, 80, y, CREAM);
                y = paragraph(c, &text, what, 80, y + 44, card.width() - 80, MUTED) + 36;
            }
            if let Err(why) = can_scan(bt) {
                paragraph(c, &f.regular(26.0), &why, 44, 750, W as i32 - 88, ALERT);
            }
            let bg = if v.pressed == Some(Hit::Cancel) { PAPER_PRESSED } else { PAPER_RAISED };
            violet::button(c, &f.medium(32.0), CANCEL, "Cancel", bg, CREAM);
            let ok = can_scan(bt).is_ok();
            let bg = if v.pressed == Some(Hit::Proceed) { PAPER_PRESSED } else if ok { VIOLET } else { PAPER_RAISED };
            violet::button(c, &f.medium(32.0), PROCEED, "Proceed", bg, if ok { PAPER } else { MUTED });
        }
        Nearby::Failed(why) => {
            paragraph(c, &f.regular(30.0), why, 44, 260, W as i32 - 88, ALERT);
            let bg = if v.pressed == Some(Hit::Stop) { PAPER_PRESSED } else { PAPER_RAISED };
            violet::button(c, &f.medium(32.0), STOP, "Done", bg, CREAM);
        }
        Nearby::Scan { scan, running, seconds } => {
            let card = Rect::new(40, 230, W as i32 - 40, 380);
            slab(c, card, 30.0, PAPER_RAISED);
            let (head, tint) = if !running { ("Stopped", CREAM) } else if scan.listening { ("Passive scan", VIOLET) } else { ("Starting…", CREAM) };
            f.medium(34.0).draw(c, head, 80, 296, tint);
            let n = scan.devices.len();
            let line = format!("{n} device{} heard · {}:{:02}", if n == 1 { "" } else { "s" }, seconds / 60, seconds % 60);
            f.regular(26.0).draw(c, &line, 80, 344, MUTED);
            let (label, value) = (f.medium(28.0), f.regular(24.0));
            const ROWS: usize = 15;
            for (k, d) in scan.strongest().iter().take(ROWS).enumerate() {
                let y = 440 + k as i32 * 64;
                let signal = if d.rssi == i8::MIN { "—".to_string() } else { format!("{} dBm", d.rssi) };
                let detail = format!("{signal} · {}", if d.random { "Random" } else { "Public" });
                let w = value.width(&detail);
                value.draw(c, &detail, W as i32 - 44 - w, y, MUTED);
                label.draw(c, &label.fit(&d.label(), W as i32 - 140 - w), 44, y, CREAM);
            }
            if n > ROWS {
                value.draw(c, &format!("and {} more", n - ROWS), 44, 440 + ROWS as i32 * 64, MUTED);
            }
            let bg = if v.pressed == Some(Hit::Stop) { PAPER_PRESSED } else { PAPER_RAISED };
            violet::button(c, &f.medium(32.0), STOP, if running { "Stop" } else { "Done" }, bg, if running { ALERT } else { CREAM });
        }
    }
}
