//! Settings: Wi-Fi, Bluetooth, the phone link (Violet Shield and Phone
//! Internet), the clock's city, the apps Violet Agent built, and About.
mod nearby;
mod pages;
mod wifi;
mod wifi_ui;

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use pages::{Hit, Section, Status, MENU};
use violet::device::{FOCUS_LOST, LIFECYCLE, TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::{Canvas, Fonts, Screen, CITIES};

/// Settings' saved Phone Internet choice, which the runtime restores after a
/// restart (runtime/open/wifi.rs): "on CODE" or "off CODE".
const RELAY_SAVED: &str = "/data/relay";

fn new_code() -> String {
    let mut bytes = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn status() -> Status {
    let sh = violet::fields(&violet::sys("shield"));
    let bt = violet::sys("bluetooth");
    let st = violet::fields(&violet::sys("status"));
    Status {
        wifi: wifi::App::summary(),
        bt: pages::Bluetooth::parse(&bt, &std::fs::read_to_string("/apps/.home").unwrap_or_default()),
        shield_known: sh.contains_key("linked"),
        shield_linked: sh.get("linked").is_some_and(|v| v == "1"),
        shield_rules: sh.get("rules").and_then(|v| v.parse().ok()).unwrap_or(0),
        frozen: sh.get("frozen").filter(|v| *v != "-").map(|v| v.split(',').map(String::from).collect()).unwrap_or_default(),
        relay: String::new(),
        relay_failure: 0,
        // A percentage; the handset's Power reports millivolts for now, so hide anything else.
        battery: st.get("battery").and_then(|v| v.parse().ok()).filter(|b| (0..=100).contains(b)).unwrap_or(-1),
        volume: st.get("volume").and_then(|v| v.parse().ok()).unwrap_or(-1),
        city: violet::sys("get time.city").parse().ok().filter(|&n: &usize| n < CITIES.len()).unwrap_or(0),
        installed: pages::parse_installed(&violet::sys("installed")),
    }
}

fn relay_state(s: &mut Status) {
    let r = violet::fields(&violet::sys("relay"));
    s.relay = r.get("state").cloned().unwrap_or_else(|| "unavailable".into());
    s.relay_failure = r.get("failure").and_then(|v| v.parse().ok()).unwrap_or(0);
}

struct App {
    section: Section,
    status: Status,
    wifi: wifi::App,
    code: String,
    relay_on: bool,
    pressed: Option<Hit>,
    down: Option<(i32, i32)>,
    confirm: Option<usize>,
    note: String,
    /// Scan nearby: the running scan, or the last one's results, and when it started.
    listener: Option<nearby::Listener>,
    scanned: Option<nearby::Scan>,
    scan_error: String,
    scan_started: Instant,
    scan_seconds: u64,
    /// The device list's scroll, and while a finger drags it, where it started.
    scan_scroll: i32,
    drag: Option<(i32, i32)>,
    /// The device shown on its own page: address and whether it is random.
    device: Option<([u8; 6], bool)>,
}

impl App {
    /// The scan running or last run, if any.
    fn scan(&self) -> Option<&nearby::Scan> {
        self.listener.as_ref().map(|l| &l.scan).or(self.scanned.as_ref())
    }

    fn view(&self) -> pages::View<'_> {
        let nearby = if !self.scan_error.is_empty() {
            pages::Nearby::Failed(&self.scan_error)
        } else if let Some(scan) = self.scan() {
            let device = self.device.and_then(|(a, r)| scan.devices.iter().find(|d| d.address == a && d.random == r));
            pages::Nearby::Scan { scan, running: self.listener.is_some(), seconds: self.scan_seconds, scroll: self.scan_scroll, device }
        } else {
            pages::Nearby::Ask
        };
        pages::View {
            section: self.section,
            nearby,
            status: &self.status,
            code: &self.code,
            relay_on: self.relay_on,
            pressed: self.pressed,
            confirm: self.confirm,
            note: &self.note,
        }
    }

    fn save_relay(&mut self) {
        let text = format!("{} {}\n", if self.relay_on { "on" } else { "off" }, self.code);
        if std::fs::write(RELAY_SAVED, text).is_err() {
            self.note = "Couldn't save the code".into();
        }
    }

    /// Ends the scan, closing the HCI (the runtime resets the controller); keeps what it heard.
    fn stop_scan(&mut self) {
        if let Some(l) = self.listener.take() {
            println!("settings: scan stopped, {} devices", l.scan.devices.len());
            self.scanned = Some(l.scan);
        }
    }

    fn go(&mut self, section: Section) {
        println!("settings: {:?}", section);
        if !matches!(section, Section::BluetoothNearby | Section::BluetoothDevice) {
            self.stop_scan();
            self.scanned = None;
            self.scan_error.clear();
        }
        self.section = section;
        self.confirm = None;
        self.note.clear();
        self.refresh();
        if section == Section::Wifi {
            self.wifi.enter();
        }
    }

    fn refresh(&mut self) {
        self.status = status();
        if self.section == Section::PhoneLink || self.section == Section::Menu {
            relay_state(&mut self.status);
        }
    }

    fn activate(&mut self, hit: Hit) {
        match hit {
            // Saved settings and Phone Internet are managed by the runtime.
            // Release this screen's memory when the user returns to Home.
            Hit::Back if self.section == Section::Menu => std::process::exit(0),
            Hit::Back if self.section == Section::BluetoothDetails => self.go(Section::Bluetooth),
            Hit::Back if self.section == Section::BluetoothNearby => self.go(Section::BluetoothDetails),
            Hit::Back if self.section == Section::BluetoothDevice => self.go(Section::BluetoothNearby),
            Hit::Device(k) => {
                self.device = self.scan().and_then(|s| s.strongest().get(k).map(|d| (d.address, d.random)));
                if self.device.is_some() {
                    self.go(Section::BluetoothDevice);
                }
            }
            Hit::ScanNearby => self.go(Section::BluetoothNearby),
            Hit::Cancel => self.go(Section::BluetoothDetails),
            Hit::Proceed => match nearby::Listener::start() {
                Ok(l) => {
                    println!("settings: passive scan");
                    self.listener = Some(l);
                    self.scan_started = Instant::now();
                    self.scan_seconds = 0;
                    self.scan_scroll = 0;
                }
                Err(e) => self.scan_error = e,
            },
            Hit::Stop if self.listener.is_some() => self.stop_scan(),
            Hit::Stop => self.go(Section::BluetoothDetails),
            Hit::Back => self.go(Section::Menu),
            Hit::Details => self.go(Section::BluetoothDetails),
            Hit::HolderApp => {
                let answer = violet::sys(&format!("open /apps/{}", self.status.bt.name));
                println!("settings: open {} -> {}", self.status.bt.name, answer);
            }
            Hit::Row(i) => self.go(MENU[i].0),
            Hit::Relay => {
                let answer = if self.relay_on { violet::sys("relay off") } else { violet::sys(&format!("relay on {}", self.code)) };
                if answer == "ok" {
                    self.relay_on = !self.relay_on;
                    self.note.clear();
                    self.save_relay();
                    println!("settings: phone internet {}", if self.relay_on { "on" } else { "off" });
                } else {
                    self.note = "The radio is busy. Try again in a moment.".into();
                }
                relay_state(&mut self.status);
            }
            Hit::NewCode => {
                self.code = new_code();
                self.save_relay();
            }
            Hit::City(i) => {
                if violet::sys(&format!("set time.city {i}")) == "ok" {
                    self.status.city = i;
                    println!("settings: time city {}", CITIES[i]);
                }
            }
            Hit::Delete(i) if self.confirm == Some(i) => {
                self.confirm = None;
                let Some(a) = self.status.installed.get(i).cloned() else { return };
                self.uninstall(&[a]);
            }
            Hit::DeleteAll if self.confirm == Some(usize::MAX) => {
                self.confirm = None;
                let all = self.status.installed.clone();
                self.uninstall(&all);
            }
            Hit::Delete(i) => self.confirm = Some(i),
            Hit::DeleteAll => self.confirm = Some(usize::MAX),
        }
    }

    fn uninstall(&mut self, apps: &[pages::Installed]) {
        self.note.clear();
        for a in apps {
            let answer = violet::sys(&format!("uninstall {}", a.name));
            println!("settings: uninstall {} -> {}", a.name, answer);
            if answer != "ok" {
                self.note = format!("Couldn't delete {}", a.title);
            }
        }
        self.refresh();
    }

    /// True when the screen needs drawing.
    fn touch(&mut self, kind: u16, x: i32, y: i32) -> bool {
        if self.section == Section::Wifi {
            self.wifi.touch(kind, x, y);
            if self.wifi.exit {
                self.go(Section::Menu);
            }
            return true;
        }
        match kind {
            TOUCH_DOWN => {
                self.down = Some((x, y));
                self.drag = None;
                self.pressed = pages::hit(&self.view(), x, y);
                if self.pressed.is_none() && !matches!(self.confirm, None) {
                    self.confirm = None;
                }
            }
            TOUCH_MOVE => {
                // Scan nearby's device list scrolls under a drag.
                if let (Section::BluetoothNearby, Some((x0, y0))) = (self.section, self.down) {
                    let n = self.scan().map_or(0, |s| s.devices.len());
                    if pages::DEVICE_LIST.contains(x0, y0) && (self.drag.is_some() || (y - y0).abs() > 18) {
                        let (start, from) = *self.drag.get_or_insert((y0, self.scan_scroll));
                        self.scan_scroll = (from - (y - start)).clamp(0, pages::devices_max_scroll(n));
                        self.pressed = None;
                        return true;
                    }
                }
                if self.pressed.is_some() && pages::hit(&self.view(), x, y) != self.pressed {
                    self.pressed = None;
                }
            }
            TOUCH_UP => {
                self.down = None;
                self.drag = None;
                if let Some(h) = self.pressed.take() {
                    self.activate(h);
                }
            }
            _ => return false,
        }
        true
    }
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let saved = std::fs::read_to_string(RELAY_SAVED).unwrap_or_default();
    let mut words = saved.split_whitespace();
    let relay_on = words.next() == Some("on");
    let code = words.next().filter(|c| c.len() == 12).map(String::from).unwrap_or_else(new_code);
    let mut app = App {
        section: Section::Menu,
        status: status(),
        wifi: wifi::App::new(),
        code,
        relay_on,
        pressed: None,
        down: None,
        confirm: None,
        note: String::new(),
        listener: None,
        scanned: None,
        scan_error: String::new(),
        scan_started: Instant::now(),
        scan_seconds: 0,
        scan_scroll: 0,
        drag: None,
        device: None,
    };
    if saved.is_empty() {
        app.save_relay();
    }
    relay_state(&mut app.status);
    println!("settings: ready, city={}", CITIES[app.status.city]);
    let draw = |c: &mut Canvas, app: &App| {
        if app.section == Section::Wifi {
            app.wifi.draw(c, &fonts);
        } else {
            pages::draw(c, &fonts, &app.view());
        }
    };
    let mut dirty = true;
    let mut last_poll = Instant::now();
    let mut wifi_poll = Instant::now();
    loop {
        if dirty {
            draw(&mut c, &app);
            screen.show_all(&c);
            dirty = false;
        }
        let animating = app.section == Section::Wifi && app.wifi.animating();
        let timeout = if animating || app.listener.is_some() { Duration::from_millis(120) } else { Duration::from_secs(1) };
        let mut next = match events.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Handle everything that queued up, then draw once.
        while let Some(e) = next {
            if e.kind == LIFECYCLE && e.value == FOCUS_LOST {
                // The scan runs only while you can see it.
                app.stop_scan();
                dirty = true;
            } else if e.focus_gained() {
                app.pressed = None;
                // Opened again from Home it starts at the menu; when the screen
                // comes back to it, it shows the page it left.
                let section = if e.opened() { Section::Menu } else { app.section };
                app.go(section);
                dirty = true;
            } else if matches!(e.kind, TOUCH_DOWN | TOUCH_MOVE | TOUCH_UP) {
                dirty |= app.touch(e.kind, e.x, e.y);
            }
            next = events.try_recv().ok();
        }
        if let Some(l) = &mut app.listener {
            dirty |= l.pump();
            let seconds = app.scan_started.elapsed().as_secs();
            dirty |= seconds != app.scan_seconds;
            app.scan_seconds = seconds;
            if let Some(e) = l.scan.failure.clone() {
                app.stop_scan();
                app.scan_error = e;
                dirty = true;
            }
        }
        if app.section == Section::Wifi {
            dirty |= app.wifi.tick(&mut wifi_poll);
        } else if last_poll.elapsed() >= Duration::from_secs(if matches!(app.section, Section::PhoneLink | Section::BluetoothDetails) { 1 } else { 3 }) {
            last_poll = Instant::now();
            let before = app.status.clone();
            app.refresh();
            dirty |= before != app.status;
        }
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    fn status() -> Status {
        Status {
            wifi: "LouieDog".into(), bt: bitchat(), shield_known: true, shield_linked: true, shield_rules: 3,
            frozen: vec![], relay: "off".into(), relay_failure: 0, battery: 82, volume: 8, city: 1,
            installed: pages::parse_installed("scorekeeper\t412345\tScorekeeper\ntips\t300000\tTip Calculator\n"),
        }
    }

    /// Bluetooth while Bitchat holds it, as the runtime reports it.
    fn bitchat() -> pages::Bluetooth {
        pages::Bluetooth::parse(
            "on=1 open=1 app=bitchat advertising=1 scanning=1 active=0 random_adv=1 random_scan=1 state=on error=0x0 rx=1204 tx=980 bad=0 dropped=0 hci=11 maker=70",
            "ladybird\tLadybird Browser\nbitchat\tBitchat\n",
        )
    }

    #[test]
    fn bluetooth_fields() {
        let b = bitchat();
        assert_eq!((b.app.as_str(), b.name.as_str()), ("Bitchat", "bitchat"));
        assert!(b.advertising && b.scanning && !b.active && b.random_adv);
        assert_eq!((b.rx, b.hci, b.maker), (1204, 11, 70));
        let idle = pages::Bluetooth::parse("on=1 open=0 app=- state=failed error=0x140", "");
        assert_eq!((idle.app.as_str(), idle.error, idle.state.as_str()), ("", 0x140, "failed"));
        // The old answer still parses.
        assert!(pages::Bluetooth::parse("on=1 open=0", "").on);
    }

    #[test]
    fn installed_lines() {
        let a = pages::parse_installed("dice\t2048\tDice\nbad\nraw\t10\t\npython\t5000000\tPython\tpackage\n.free\t9000\t\n");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].title, "Dice");
        assert_eq!(a[1].title, "raw");
    }

    /// SETTINGS_PREVIEW=dir cargo test -p settings writes each page as raw 720x1600 RGBA.
    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("SETTINGS_PREVIEW") else { return };
        let fonts = Fonts::load();
        let s = status();
        let mut c = Canvas::new(violet::W, violet::H);
        for (name, section) in [("menu", Section::Menu), ("bluetooth", Section::Bluetooth),
                                ("bluetooth-details", Section::BluetoothDetails), ("phone", Section::PhoneLink),
                                ("time", Section::Time), ("apps", Section::AgentApps), ("about", Section::About)] {
            let v = pages::View { section, nearby: pages::Nearby::Ask, status: &s, code: "3FA94C0B17DE", relay_on: false, pressed: None, confirm: Some(1), note: "" };
            pages::draw(&mut c, &fonts, &v);
            write(&c, &dir, name);
        }
        // Scan nearby: asking (Bitchat closed), then a scan that heard three devices.
        let mut idle = s.clone();
        idle.bt.open = false;
        let mut scan = nearby::Scan::new();
        while let Some(p) = scan.take_out() {
            scan.receive(&[4, 0x0e, 4, 1, p[1], p[2], 0]);
        }
        let mut ev = vec![0x02, 3];
        ev.extend_from_slice(&[0, 1, 1, 2, 3, 4, 5, 0x46, 25, 12, 0x09, b'G', b'r', b'e', b'g', b'g', b'\'', b's', b' ', b'M', b'a', b'c',
            3, 0x03, 0x0f, 0x18, 2, 0x0a, 8, 4, 0xff, 0x4c, 0x00, 0x10, (-48i8) as u8]);
        ev.extend_from_slice(&[0, 0, 9, 8, 7, 6, 5, 4, 0, (-71i8) as u8]);
        ev.extend_from_slice(&[3, 1, 6, 6, 6, 6, 6, 0x46, 0, (-88i8) as u8]);
        let mut p = vec![4, 0x3e, ev.len() as u8];
        p.extend_from_slice(&ev);
        scan.receive(&p);
        // A crowded room: 30 devices, scrolled part way down.
        let mut crowd = nearby::Scan::new();
        for k in 0..30u8 {
            let ev = [0x02, 1, 0, 1, k, 1, 2, 3, 4, 0xc0, 0, (-40i8 - k as i8) as u8];
            let mut p = vec![4, 0x3e, ev.len() as u8];
            p.extend_from_slice(&ev);
            crowd.receive(&p);
        }
        for (name, status, nearby) in [
            ("nearby-ask", &idle, pages::Nearby::Ask),
            ("nearby-busy", &s, pages::Nearby::Ask),
            ("nearby-scan", &idle, pages::Nearby::Scan { scan: &scan, running: true, seconds: 42, scroll: 0, device: None }),
            ("nearby-long", &idle, pages::Nearby::Scan { scan: &crowd, running: true, seconds: 95, scroll: 400, device: None }),
            ("nearby-device", &idle, pages::Nearby::Scan { scan: &scan, running: true, seconds: 42, scroll: 0, device: scan.strongest().first().copied() }),
        ] {
            let section = if name == "nearby-device" { Section::BluetoothDevice } else { Section::BluetoothNearby };
            let v = pages::View { section, nearby, status, code: "", relay_on: false, pressed: None, confirm: None, note: "" };
            pages::draw(&mut c, &fonts, &v);
            write(&c, &dir, name);
        }
    }

    fn write(c: &Canvas, dir: &str, name: &str) {
        let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
        std::fs::write(format!("{dir}/{name}.rgba"), bytes).unwrap();
    }
}
