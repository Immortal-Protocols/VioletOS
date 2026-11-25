//! Home: the main screen. A clock and the weather, the phone's status, and a
//! tile for each app. It stays running; "home" in /dev/violet/sys brings it
//! back to the front.
mod clock;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use clock::Glance;
use violet::{device::{TOUCH_DOWN, TOUCH_UP}, Canvas, Fonts, Screen};

use violet::CITIES;

/// The city chosen in Weather: the shared setting weather.city, not Weather's own
/// files (reading another app's data and then going to the network is what data
/// theft looks like, and Violet Shield flags it).
fn city() -> &'static str {
    violet::sys("get weather.city").parse::<usize>().ok().and_then(|n| CITIES.get(n).copied()).unwrap_or(CITIES[0])
}

fn fetch(name: &str) -> Result<Glance, String> {
    let url = format!("https://wttr.in/{}?format=%T|%t|%C&u", name.replace(' ', "+"));
    let r = violet::http::get(&url, &[("User-Agent", "curl/8 (VioletOS Home)")]).map_err(|e| e.to_string())?;
    if r.status != 200 {
        return Err(format!("wttr.in: {}", r.status));
    }
    let text = r.text();
    let f: Vec<&str> = text.trim().split('|').collect();
    if f.len() < 3 {
        return Err(format!("unexpected answer {text:?}"));
    }
    Ok(Glance {
        utc: r.header("date").and_then(clock::parse_http_date),
        offset: clock::parse_offset(f[0]),
        at: Instant::now(),
        temp: f[1].trim().trim_start_matches('+').to_string(),
        sky: f[2].trim().to_string(),
        city: name.to_string(),
    })
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let mut s = ui::State {
        now: None, online: false, battery: -1, volume: 0, phones: false, listening: false,
        temp: String::new(), sky: String::new(), city: String::new(), running: Default::default(), pressed: None, shield: Default::default(),
        wifi: String::new(),
    };
    let mut glance: Option<Glance> = None;
    let (tx, glances) = mpsc::channel::<Result<Glance, String>>();
    let refresh = move || {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fetch(city()));
        });
    };
    let status = |s: &mut ui::State| {
        let f = violet::fields(&violet::sys("status"));
        // A percentage; the handset's Power reports millivolts for now, so hide anything else.
        s.battery = f.get("battery").and_then(|v| v.parse().ok()).filter(|b| (0..=100).contains(b)).unwrap_or(-1);
        s.volume = f.get("volume").and_then(|v| v.parse().ok()).unwrap_or(0);
        s.phones = f.get("headphones").is_some_and(|v| v == "1");
        s.listening = f.get("listening").is_some_and(|v| v == "1");
        let w = violet::wifi::state();
        s.wifi = if w.hosting {
            "Phone Internet".to_string()
        } else if w.connected != 0 {
            String::from_utf8_lossy(&w.ssid).into_owned()
        } else {
            String::new()
        };
        if w.connected != 0 && s.wifi.is_empty() {
            s.wifi = "Connected".to_string();
        }
        s.running = violet::sys("running").split_whitespace().map(String::from).collect();
        let sh = violet::fields(&violet::sys("shield"));
        s.shield = ui::Shield {
            known: sh.contains_key("linked"),
            linked: sh.get("linked").is_some_and(|v| v == "1"),
            rules: sh.get("rules").and_then(|v| v.parse().ok()).unwrap_or(0),
            frozen: sh.get("frozen").filter(|v| *v != "-").map(|v| v.split(',').map(String::from).collect()).unwrap_or_default(),
        };
    };
    let draw = |c: &mut Canvas, s: &mut ui::State, glance: &Option<Glance>, screen: &mut Screen| {
        s.now = glance.as_ref().and_then(|g| g.now());
        ui::draw(c, &fonts, s);
        screen.show_all(c);
    };
    status(&mut s);
    refresh();
    draw(&mut c, &mut s, &glance, &mut screen);
    println!("home: ready");
    let mut last_fetch = Instant::now();
    let mut last_status = Instant::now();
    let mut shown_minute = None;
    // Home polls and draws only while it is in front.
    let mut focused = true;
    loop {
        match events.recv_timeout(Duration::from_secs(1)) {
            Ok(e) if e.focus_gained() => {
                focused = true;
                s.pressed = None;
                status(&mut s);
                draw(&mut c, &mut s, &glance, &mut screen);
                // Weather may have switched cities while Home was away.
                if last_fetch.elapsed() > Duration::from_secs(120) || (s.online && city() != s.city) {
                    last_fetch = Instant::now();
                    refresh();
                }
            }
            Ok(e) if e.kind == violet::device::LIFECYCLE && e.value == violet::device::FOCUS_LOST => focused = false,
            Ok(e) if e.kind == TOUCH_DOWN => {
                // usize::MAX: the Wi-Fi bar. Only the pressed tile is redrawn.
                s.pressed = ui::tile_at(e.x, e.y).or(ui::WIFI_BAR.contains(e.x, e.y).then_some(usize::MAX));
                if let Some(i) = s.pressed {
                    let r = ui::region(i);
                    ui::draw_clipped(&mut c, &fonts, &s, r);
                    screen.show(&c, r);
                }
            }
            Ok(e) if e.kind == TOUCH_UP => {
                if let Some(i) = s.pressed.take() {
                    let r = ui::region(i);
                    ui::draw_clipped(&mut c, &fonts, &s, r);
                    screen.show(&c, r);
                    let (name, command) = match ui::TILES.get(i) {
                        Some(t) => (t.name, t.command),
                        None => ("wifi", ui::WIFI_COMMAND),
                    };
                    println!("home: opening {}", name);
                    let answer = violet::sys(command);
                    if answer != "ok" {
                        println!("home: {} -> {}", command, answer);
                    }
                }
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        while let Ok(result) = glances.try_recv() {
            match result {
                Ok(g) => {
                    println!("home: {} {} {}", g.city, g.temp, g.sky);
                    s.online = true;
                    s.temp = g.temp.clone();
                    s.sky = g.sky.clone();
                    s.city = g.city.clone();
                    glance = Some(g);
                }
                Err(e) => {
                    println!("home: weather: {e}");
                    s.online = false;
                }
            }
            if focused {
                draw(&mut c, &mut s, &glance, &mut screen);
            }
        }
        if !focused {
            continue;
        }
        if last_fetch.elapsed() > Duration::from_secs(if s.online { 900 } else { 60 }) {
            last_fetch = Instant::now();
            refresh();
        }
        if last_status.elapsed() > Duration::from_secs(2) {
            last_status = Instant::now();
            let (b, v, p, r, sh, w, l) = (s.battery, s.volume, s.phones, s.running.clone(), s.shield.clone(), s.wifi.clone(), s.listening);
            status(&mut s);
            let minute = glance.as_ref().and_then(|g| g.now()).map(|t| t.minute);
            if minute != shown_minute || (b, v, p) != (s.battery, s.volume, s.phones) || r != s.running || sh != s.shield || w != s.wifi || l != s.listening {
                shown_minute = minute;
                draw(&mut c, &mut s, &glance, &mut screen);
            }
        }
    }
}

#[cfg(test)]
mod preview {
    fn state() -> super::ui::State {
        let t = super::clock::parse_http_date("Tue, 24 Nov 2026 02:47:33 GMT");
        let g = super::Glance { utc: t, offset: -8 * 3600, at: std::time::Instant::now(), temp: "54°F".into(), sky: "Partly cloudy".into(), city: "Oakland".into() };
        super::ui::State {
            now: g.now(), online: true, battery: 82, volume: 8, phones: false, listening: false, temp: g.temp.clone(), sky: g.sky.clone(),
            city: g.city.clone(), running: ["/apps/music".to_string()].into_iter().collect(), pressed: None,
            shield: super::ui::Shield { known: true, linked: true, rules: 3, frozen: vec![] }, wifi: "LouieDog".into(),
        }
    }

    /// HOME_BENCH=1 cargo test -p home --release bench -- --nocapture: what a
    /// full redraw and a pressed tile cost (run on the build machine; the
    /// handset's Cortex-A53 is roughly 10-20x slower).
    #[test]
    fn bench() {
        if std::env::var("HOME_BENCH").is_err() {
            return;
        }
        let fonts = violet::Fonts::load();
        let mut s = state();
        let mut c = violet::Canvas::new(violet::W, violet::H);
        super::ui::draw(&mut c, &fonts, &s); // rasterize the glyphs once
        let n = 50;
        let t = std::time::Instant::now();
        for _ in 0..n {
            super::ui::draw(&mut c, &fonts, &s);
        }
        let full = t.elapsed() / n;
        s.pressed = Some(0);
        let t = std::time::Instant::now();
        for _ in 0..n {
            super::ui::draw_clipped(&mut c, &fonts, &s, super::ui::region(0));
        }
        let pressed = t.elapsed() / n;
        println!("home bench: full redraw {full:?}, pressed tile {pressed:?}");
    }

    /// A pressed tile drawn alone matches the same tile in a full redraw.
    #[test]
    fn pressed_matches_full() {
        let fonts = violet::Fonts::load();
        let mut s = state();
        let mut a = violet::Canvas::new(violet::W, violet::H);
        super::ui::draw(&mut a, &fonts, &s);
        s.pressed = Some(4);
        let mut b = violet::Canvas::new(violet::W, violet::H);
        b.px.copy_from_slice(&a.px);
        super::ui::draw_clipped(&mut b, &fonts, &s, super::ui::region(4));
        let mut full = violet::Canvas::new(violet::W, violet::H);
        super::ui::draw(&mut full, &fonts, &s);
        assert!(b.px[..] == full.px[..], "a pressed tile differs from a full redraw");
    }

    /// HOME_PREVIEW=path cargo test -p home writes Home as raw 720x1600 RGBA.
    #[test]
    fn preview() {
        let Ok(out) = std::env::var("HOME_PREVIEW") else { return };
        let t = super::clock::parse_http_date("Tue, 24 Nov 2026 02:47:33 GMT");
        let g = super::Glance { utc: t, offset: -8 * 3600, at: std::time::Instant::now(), temp: "54°F".into(), sky: "Partly cloudy".into(), city: "Oakland".into() };
        let s = super::ui::State {
            now: g.now(), online: true, battery: 82, volume: 8, phones: false, listening: false, temp: g.temp.clone(), sky: g.sky.clone(),
            city: g.city.clone(), running: ["/apps/music".to_string()].into_iter().collect(), pressed: None,
            shield: Default::default(), wifi: "LouieDog".into(),
        };
        let mut c = violet::Canvas::new(violet::W, violet::H);
        super::ui::draw(&mut c, &violet::Fonts::load(), &s);
        let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
        std::fs::write(out, bytes).unwrap();
    }
}
