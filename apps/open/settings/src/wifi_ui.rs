//! Drawing Settings › Wi-Fi. Everything comes from `View`, so a preview can be
//! drawn without the devices.
use std::f32::consts::PI;
use std::net::Ipv4Addr;

use violet::wifi::{self, AddressFrom, Link, Network, Security};
use violet::{colors::*, mix, Canvas, Color, Fonts, Rect, H, W};

use violet::keyboard::{self, Keyboard};

pub const ALERT: Color = violet::paper::ALERT;
const ROW_PRESSED: Color = violet::paper::PAPER_PRESSED;

// The list page.
pub const CARD: Rect = Rect::new(40, 220, W as i32 - 40, 360);
pub const LIST_TOP: i32 = 440;
pub const LIST_BOTTOM: i32 = 1410;
pub const ROW_H: i32 = 128;
pub const SCAN: Rect = Rect::new(60, 1440, W as i32 - 60, 1550);
// The password page.
pub const FIELD: Rect = Rect::new(40, 280, W as i32 - 40, 390);
pub const SHOW: Rect = Rect::new(W as i32 - 170, 296, W as i32 - 56, 374);
pub const CONNECT: Rect = Rect::new(40, 480, W as i32 - 40, 590);
// The details page: the sections fill it from the top, the buttons stay below.
pub const DISCONNECT: Rect = Rect::new(40, 1426, 352, 1536);
pub const FORGET: Rect = Rect::new(368, 1426, W as i32 - 40, 1536);
const DETAIL_ROW: i32 = 50;

pub enum Page {
    List,
    Password { net: Network, text: String, show: bool, error: Option<String> },
    Details { net: Network },
}

/// Something under the finger, drawn pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Home,
    Scan,
    Row(usize),
    Show,
    Connect,
    Disconnect,
    Forget,
}

pub struct View<'a> {
    pub page: &'a Page,
    pub nets: &'a [Network],
    pub state: &'a violet::wifi::State,
    pub connected_name: String,
    pub hosting: bool,
    /// "scanning", "joining" or "leaving".
    pub busy: Option<&'a str>,
    /// The network being joined, by profile ID.
    pub joining: Option<u32>,
    /// The SSIDs with a saved password.
    pub saved: Vec<&'a [u8]>,
    /// A line for the status card: the last result or problem.
    pub message: Option<&'a str>,
    /// The joined link's addresses and hardware, for the details page.
    pub link: Option<&'a Link>,
    pub scroll: i32,
    pub pressed: Option<Hit>,
    pub keyboard: &'a Keyboard,
    /// Animation phase for the spinner, 0..1.
    pub phase: f32,
}

impl View<'_> {
    fn is_saved(&self, ssid: &[u8]) -> bool {
        self.saved.contains(&ssid)
    }
}

pub fn row_rect(i: usize, scroll: i32) -> Rect {
    let y = LIST_TOP + i as i32 * ROW_H - scroll;
    Rect::new(40, y, W as i32 - 40, y + ROW_H - 12)
}

pub fn max_scroll(rows: usize) -> i32 {
    (rows as i32 * ROW_H - (LIST_BOTTOM - LIST_TOP)).max(0)
}

pub fn draw(c: &mut Canvas, f: &Fonts, v: &View) {
    c.fill(c.bounds(), BACKGROUND);
    match v.page {
        Page::List => list(c, f, v),
        Page::Password { net, text, show, error } => password(c, f, v, net, text, *show, error.as_deref()),
        Page::Details { net } => details(c, f, v, net),
    }
}

fn back(c: &mut Canvas, f: &Fonts, v: &View, label: &str) {
    let bg = if v.pressed == Some(Hit::Home) { ROW_PRESSED } else { PANEL };
    violet::button(c, &f.medium(24.0), violet::HOME_BOX, label, bg, INK);
}

/// Wi-Fi arcs around a dot at (cx, cy); `bars` of three are lit.
pub fn signal(c: &mut Canvas, cx: f32, cy: f32, scale: f32, bars: u32, on: Color, off: Color) {
    c.circle(cx, cy, 5.0 * scale, if bars > 0 { on } else { off });
    for (k, r) in [16.0, 28.0, 40.0].iter().enumerate() {
        let ink = if bars as usize > k + 1 { on } else { off };
        c.arc(cx, cy, r * scale, 6.0 * scale, PI / 4.0, 3.0 * PI / 4.0, ink);
    }
}

fn lock(c: &mut Canvas, cx: f32, cy: f32, ink: Color, bg: Color) {
    c.ring(cx, cy - 8.0, 8.0, 4.0, ink);
    c.round_rect(Rect::new(cx as i32 - 11, cy as i32 - 6, cx as i32 + 11, cy as i32 + 12), 3.0, ink);
    c.circle(cx, cy + 3.0, 2.5, bg);
}

/// Eight dots around a circle, the brightest one going round.
fn spinner(c: &mut Canvas, cx: f32, cy: f32, r: f32, phase: f32, ink: Color) {
    let lead = (phase * 8.0) as i32;
    for k in 0..8 {
        let a = -(k as f32) * PI / 4.0;
        let age = (lead - k).rem_euclid(8) as f32;
        c.circle(cx + a.cos() * r, cy - a.sin() * r, r * 0.2, mix(ink, PANEL, age / 9.0));
    }
}

fn check(c: &mut Canvas, cx: f32, cy: f32, ink: Color) {
    c.line(cx - 14.0, cy, cx - 4.0, cy + 10.0, 6.0, ink);
    c.line(cx - 4.0, cy + 10.0, cx + 16.0, cy - 12.0, 6.0, ink);
}

fn list(c: &mut Canvas, f: &Fonts, v: &View) {
    let (name, sub) = (f.medium(34.0), f.regular(25.0));
    // The rows first; the header and footer are drawn over whatever scrolled past.
    for (i, n) in v.nets.iter().enumerate() {
        let r = row_rect(i, v.scroll);
        if r.y1 < LIST_TOP || r.y0 > LIST_BOTTOM {
            continue;
        }
        let pressed = v.pressed == Some(Hit::Row(i));
        let bg = if pressed { ROW_PRESSED } else { PANEL };
        violet::paper::slab(c, r, 26.0, bg);
        let here = v.state.is_joined(n);
        let joining = v.joining == Some(n.id);
        let supported = n.security != Security::Other;
        signal(c, 96.0, (r.y0 + 70) as f32, 0.9, 3, if here { ACCENT } else if supported { INK } else { MUTED }, mix(MUTED, bg, 0.5));
        if n.security != Security::Open {
            lock(c, 120.0, (r.y0 + 80) as f32, if supported { INK } else { MUTED }, bg);
        }
        let x = 160;
        name.draw(c, &name.fit(&n.name(), r.x1 - x - 90), x, r.y0 + 54, if supported { INK } else { MUTED });
        let mut parts: Vec<&str> = Vec::new();
        let state = if here {
            "Connected"
        } else if joining {
            "Connecting…"
        } else if v.is_saved(&n.ssid) {
            "Saved"
        } else {
            ""
        };
        if !state.is_empty() {
            parts.push(state);
        }
        parts.push(match n.security {
            Security::Wpa2 => "WPA2",
            Security::Open => "Open",
            Security::Other => "Not supported",
        });
        if !n.band().is_empty() {
            parts.push(n.band());
        }
        sub.draw(c, &parts.join(" · "), x, r.y0 + 94, if here { ACCENT } else { MUTED });
        let (cx, cy) = ((r.x1 - 50) as f32, (r.y0 + r.height() / 2) as f32);
        if joining {
            spinner(c, cx, cy, 18.0, v.phase, ACCENT);
        } else if here {
            check(c, cx, cy, ACCENT);
        } else if supported {
            c.line(cx - 6.0, cy - 14.0, cx + 6.0, cy, 4.0, MUTED);
            c.line(cx + 6.0, cy, cx - 6.0, cy + 14.0, 4.0, MUTED);
        }
    }
    if v.nets.is_empty() {
        let text = if v.busy == Some("scanning") { "Looking for networks…" } else { "No networks found" };
        f.medium(30.0).centered(c, text, 0, W as i32, LIST_TOP + 120, MUTED);
    }
    c.fill(Rect::new(0, 0, W as i32, LIST_TOP - 6), BACKGROUND);
    c.fill(Rect::new(0, LIST_BOTTOM + 6, W as i32, H as i32), BACKGROUND);
    back(c, f, v, "‹ Settings");
    f.regular(48.0).draw(c, "Wi-Fi", 40, 180, INK);

    // The status card.
    violet::paper::slab(c, CARD, 30.0, PANEL);
    let (title, line) = (f.bold(34.0), f.medium(26.0));
    let joining = v.joining.and_then(|id| v.nets.iter().find(|n| n.id == id));
    let (head, detail, on) = if let Some(n) = joining {
        ("Connecting…".to_string(), n.name(), true)
    } else if v.busy == Some("leaving") {
        ("Disconnecting…".to_string(), v.connected_name.clone(), true)
    } else if v.hosting {
        ("Phone Internet is on".to_string(), "Choose a network to use Wi-Fi instead".to_string(), true)
    } else if v.state.connected != 0 {
        let n = if v.connected_name.is_empty() { "Online".to_string() } else { v.connected_name.clone() };
        ("Connected".to_string(), v.message.map_or(n, String::from), true)
    } else {
        ("Not connected".to_string(), v.message.unwrap_or("Choose a network below").to_string(), false)
    };
    signal(c, 112.0, 312.0, 1.3, if on { 3 } else { 0 }, if on { ACCENT } else { MUTED }, mix(MUTED, PANEL, 0.5));
    if joining.is_some() || v.busy == Some("leaving") {
        spinner(c, (CARD.x1 - 56) as f32, 290.0, 20.0, v.phase, ACCENT);
    }
    title.draw(c, &title.fit(&head, CARD.x1 - 200 - 100), 200, 280, INK);
    let err = v.message.is_some() && joining.is_none() && v.state.connected == 0;
    let tint = if err { ALERT } else if on && joining.is_none() { ACCENT } else { MUTED };
    line.draw(c, &line.fit(&detail, CARD.x1 - 230), 200, 326, tint);

    let small = f.medium(23.0);
    small.draw(c, "NETWORKS", 48, LIST_TOP - 22, MUTED);
    if v.busy == Some("scanning") {
        spinner(c, (W - 62) as f32, (LIST_TOP - 31) as f32, 12.0, v.phase, MUTED);
    }

    let idle = v.busy.is_none();
    let bg = if v.pressed == Some(Hit::Scan) { ROW_PRESSED } else { PANEL };
    let label = if v.busy == Some("scanning") { "Scanning…" } else { "Scan again" };
    violet::button(c, &f.bold(34.0), SCAN, label, bg, if idle { INK } else { MUTED });
}

fn password(c: &mut Canvas, f: &Fonts, v: &View, net: &Network, text: &str, show: bool, error: Option<&str>) {
    back(c, f, v, "‹ Back");
    let title = f.bold(44.0);
    title.draw(c, &title.fit(&net.name(), W as i32 - 80), 40, 180, INK);
    f.medium(26.0).draw(c, "Enter the network's password", 42, 240, MUTED);

    c.round_rect(FIELD, 24.0, ACCENT);
    c.round_rect(FIELD.inset(3), 22.0, PANEL);
    let face = f.medium(38.0);
    let shown: String = if show { text.to_string() } else { "•".repeat(text.chars().count()) };
    // Keep the end of a long password (where the caret is) in view.
    let room = SHOW.x0 - 20 - (FIELD.x0 + 28);
    let mut visible: Vec<char> = shown.chars().collect();
    while face.width(&visible.iter().collect::<String>()) > room && !visible.is_empty() {
        visible.remove(0);
    }
    let visible: String = visible.into_iter().collect();
    let base = FIELD.y0 + 70;
    let end = face.draw(c, &visible, FIELD.x0 + 28, base, INK);
    if v.joining.is_none() {
        c.fill(Rect::new(end + 3, base - 36, end + 6, base + 8), ACCENT);
    }
    let bg = if v.pressed == Some(Hit::Show) { ROW_PRESSED } else { BACKGROUND };
    violet::button(c, &f.medium(26.0), SHOW, if show { "Hide" } else { "Show" }, bg, INK);

    let n = text.len();
    let (hint, tint) = match error {
        Some(e) => (e.to_string(), ALERT),
        None if n > 0 && n < 8 => (format!("{} more character{} at least", 8 - n, if 8 - n == 1 { "" } else { "s" }), MUTED),
        None => ("WPA2 passwords have 8 to 63 characters".to_string(), MUTED),
    };
    let small = f.medium(25.0);
    small.draw(c, &small.fit(&hint, W as i32 - 90), 44, 440, tint);

    let ready = violet::wifi::valid_passphrase(text);
    let joining = v.joining.is_some();
    let (bg, ink) = if joining || !ready {
        (PANEL, MUTED)
    } else if v.pressed == Some(Hit::Connect) {
        (mix(ACCENT, BACKGROUND, 0.25), BACKGROUND)
    } else {
        (ACCENT, BACKGROUND)
    };
    violet::button(c, &f.bold(36.0), CONNECT, if joining { "Connecting…" } else { "Connect" }, bg, ink);
    if joining {
        spinner(c, (CONNECT.x1 - 60) as f32, (CONNECT.y0 + CONNECT.height() / 2) as f32, 20.0, v.phase, INK);
        if let Some(m) = v.message {
            small.centered(c, m, 0, W as i32, CONNECT.y1 + 50, MUTED);
        }
    }
    if keyboard::TOP > CONNECT.y1 {
        v.keyboard.draw(c, f);
    }
}

/// "1 day 2 h", "23 h 36 min", "45 s": the two largest units.
fn duration(secs: u32) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    let days = format!("{d} day{}", if d == 1 { "" } else { "s" });
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, _) => format!("{m} min"),
        (0, _, 0) => format!("{h} h"),
        (0, _, _) => format!("{h} h {m} min"),
        (_, 0, _) => days,
        _ => format!("{days} {h} h"),
    }
}

/// A subnet mask's prefix length ("/24"), for a mask of ones then zeros.
fn prefix(mask: Ipv4Addr) -> Option<u32> {
    let bits = u32::from(mask);
    (bits.leading_ones() + bits.trailing_zeros() == 32).then_some(bits.leading_ones())
}

/// The firmware's build date: "20210608152745" as "2021-06-08 15:27", anything else as it is.
fn built(date: &str) -> String {
    if date.len() == 14 && date.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{} {}:{}", &date[..4], &date[4..6], &date[6..8], &date[8..10], &date[10..12])
    } else {
        date.to_string()
    }
}

/// The details page: each section's heading and its rows (label, value). The
/// link's rows are there only for the joined network, and only those Radio reported.
pub fn detail_sections(v: &View, net: &Network) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    let here = v.state.is_joined(net);
    let link = v.link.filter(|_| here);
    let mut network = vec![
        ("Status", if here { "Connected" } else { "Not connected" }.to_string()),
        ("Security", match net.security {
            Security::Wpa2 => "WPA2-Personal · AES",
            Security::Open => "None",
            Security::Other => "Not supported",
        }.to_string()),
    ];
    // Radio tries every access point with this name, so the joined one's
    // channel can differ from the first one the scan saw.
    let channel = link.map(|l| l.channel).filter(|&c| c != 0).unwrap_or(net.channel);
    if channel != 0 {
        let mhz = wifi::frequency(channel).map_or(String::new(), |f| format!(" · {f} MHz"));
        network.push(("Channel", format!("{channel} · {}{mhz}", wifi::band(channel))));
    }
    if let Some(bssid) = link.and_then(|l| l.bssid) {
        network.push(("Connected to", wifi::mac_text(&bssid)));
    }
    if net.channels.len() > 1 {
        let mut channels: Vec<u32> = net.channels.iter().copied().filter(|&c| c != 0).collect();
        channels.sort();
        channels.dedup();
        let list: Vec<String> = channels.iter().map(u32::to_string).collect();
        let on = match list.len() {
            0 => String::new(),
            1 => format!(" on channel {}", list[0]),
            _ => format!(" on channels {}", list.join(", ")),
        };
        network.push(("Access points", format!("{}{on}", net.channels.len())));
    }
    network.push(("Password", if v.is_saved(&net.ssid) { "Saved on this phone" } else { "Not saved" }.to_string()));
    let mut sections = vec![("NETWORK", network)];
    let Some(link) = link else { return sections };

    let mut internet = Vec::new();
    if let Some(ip) = link.ip {
        internet.push(("IP address", ip.to_string()));
    }
    if let Some(mask) = link.mask {
        internet.push(("Subnet mask", match prefix(mask) {
            Some(n) => format!("{mask} · /{n}"),
            None => mask.to_string(),
        }));
    }
    if let Some(router) = link.router {
        internet.push(("Router", router.to_string()));
    }
    for (label, dns) in ["DNS", "DNS 2"].into_iter().zip(&link.dns) {
        internet.push((label, dns.to_string()));
    }
    if let Some(dhcp) = link.dhcp {
        internet.push(("DHCP server", dhcp.to_string()));
    }
    match link.lease {
        Some((u32::MAX, _)) => internet.push(("Lease", "Does not end".to_string())),
        Some((total, left)) if left < total => internet.push(("Lease", format!("{} · {} left", duration(total), duration(left)))),
        Some((total, _)) => internet.push(("Lease", duration(total))),
        None => {}
    }
    internet.push(("Host name", link.hostname.clone().unwrap_or_else(|| "Not sent".to_string())));
    sections.push(("INTERNET", internet));

    let mut phone = Vec::new();
    if let Some(mac) = link.mac {
        phone.push(("Wi-Fi address", wifi::mac_text(&mac)));
    }
    if let Some(from) = link.from {
        phone.push(("Address type", match from {
            AddressFrom::Chip => "Built into the chip",
            AddressFrom::Random => "Random each time Wi-Fi starts",
            AddressFrom::Fixed => "Fixed, set over USB",
        }.to_string()));
    }
    if let Some(radio) = &link.radio {
        phone.push(("Radio", radio.clone()));
    }
    match (&link.firmware, &link.built) {
        (Some(version), Some(date)) => phone.push(("Firmware", format!("{version} · {}", built(date)))),
        (Some(version), None) => phone.push(("Firmware", version.clone())),
        (None, Some(date)) => phone.push(("Firmware", built(date))),
        (None, None) => {}
    }
    if !phone.is_empty() {
        sections.push(("THIS PHONE", phone));
    }
    sections
}

fn details(c: &mut Canvas, f: &Fonts, v: &View, net: &Network) {
    back(c, f, v, "‹ Back");
    let title = f.bold(44.0);
    title.draw(c, &title.fit(&net.name(), W as i32 - 80), 40, 180, INK);
    let here = v.state.is_joined(net);
    let (heading, label, value) = (f.medium(23.0), f.regular(24.0), f.medium(27.0));
    let mut y = 212;
    for (name, rows) in detail_sections(v, net) {
        heading.draw(c, name, 48, y + 30, MUTED);
        let card = Rect::new(40, y + 44, W as i32 - 40, y + 72 + rows.len() as i32 * DETAIL_ROW);
        violet::paper::slab(c, card, 26.0, PANEL);
        for (k, (name, text)) in rows.iter().enumerate() {
            let base = card.y0 + 46 + k as i32 * DETAIL_ROW;
            label.draw(c, name, 68, base, MUTED);
            value.draw(c, &value.fit(text, card.x1 - 24 - 258), 258, base, INK);
        }
        y = card.y1 + 14;
    }
    let busy = v.busy.is_some();
    if here {
        let bg = if v.pressed == Some(Hit::Disconnect) { ROW_PRESSED } else { PANEL };
        violet::button(c, &f.bold(34.0), DISCONNECT, if v.busy == Some("leaving") { "Disconnecting…" } else { "Disconnect" }, bg, if busy { MUTED } else { INK });
    }
    if v.is_saved(&net.ssid) {
        let bg = if v.pressed == Some(Hit::Forget) { ROW_PRESSED } else { PANEL };
        violet::button(c, &f.bold(34.0), FORGET, "Forget network", bg, if busy { MUTED } else { ALERT });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_wording() {
        assert_eq!(
            [86400, 90000, 85000, 3600, 5400, 600, 45, 2 * 86400 + 59].map(duration),
            ["1 day", "1 day 1 h", "23 h 36 min", "1 h", "1 h 30 min", "10 min", "45 s", "2 days"]
        );
        assert_eq!(prefix(Ipv4Addr::new(255, 255, 255, 0)), Some(24));
        assert_eq!(prefix(Ipv4Addr::new(255, 255, 252, 0)), Some(22));
        assert_eq!(prefix(Ipv4Addr::new(255, 0, 255, 0)), None);
        assert_eq!(built("20210608152745"), "2021-06-08 15:27");
        assert_eq!(built("Jun  8 2021"), "Jun  8 2021");
    }
}
