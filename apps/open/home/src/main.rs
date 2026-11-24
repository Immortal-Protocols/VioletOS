//! Home: the main screen. A clock and the weather, the phone's status, and a
//! tile for each app. It stays running; "home" in /dev/violet/sys brings it
//! back to the front.
mod clock;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use clock::Glance;
use violet::{device::{TOUCH_DOWN, TOUCH_UP}, Canvas, Fonts, Screen, H, W};

const CITIES: [&str; 20] = [
    "San Francisco", "Oakland", "Los Angeles", "Seattle", "New York",
    "Chicago", "Austin", "Miami", "Denver", "Boston",
    "Honolulu", "Anchorage", "Mexico City", "London", "Paris",
    "Berlin", "Tokyo", "Sydney", "Mumbai", "Cairo",
];

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
    let mut c = Canvas::new(W, H);
    let mut s = ui::State {
        now: None, online: false, battery: -1, volume: 0, phones: false,
        temp: String::new(), sky: String::new(), city: String::new(), running: Default::default(), pressed: None, shield: Default::default(),
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
    loop {
        match events.recv_timeout(Duration::from_secs(1)) {
            Ok(e) if e.focus_gained() => {
                s.pressed = None;
                status(&mut s);
                draw(&mut c, &mut s, &glance, &mut screen);
                if last_fetch.elapsed() > Duration::from_secs(120) {
                    last_fetch = Instant::now();
                    refresh();
                }
            }
            Ok(e) if e.kind == TOUCH_DOWN => {
                s.pressed = ui::tile_at(e.x, e.y);
                if s.pressed.is_some() {
                    draw(&mut c, &mut s, &glance, &mut screen);
                }
            }
            Ok(e) if e.kind == TOUCH_UP => {
                if let Some(i) = s.pressed.take() {
                    draw(&mut c, &mut s, &glance, &mut screen);
                    let t = &ui::TILES[i];
                    println!("home: opening {}", t.name);
                    let answer = violet::sys(t.command);
                    if answer != "ok" {
                        println!("home: {} -> {}", t.command, answer);
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
            draw(&mut c, &mut s, &glance, &mut screen);
        }
        if last_fetch.elapsed() > Duration::from_secs(if s.online { 900 } else { 60 }) {
            last_fetch = Instant::now();
            refresh();
        }
        if last_status.elapsed() > Duration::from_secs(2) {
            last_status = Instant::now();
            let (b, v, p, r, sh) = (s.battery, s.volume, s.phones, s.running.clone(), s.shield.clone());
            status(&mut s);
            let minute = glance.as_ref().and_then(|g| g.now()).map(|t| t.minute);
            if minute != shown_minute || (b, v, p) != (s.battery, s.volume, s.phones) || r != s.running || sh != s.shield {
                shown_minute = minute;
                draw(&mut c, &mut s, &glance, &mut screen);
            }
        }
    }
}

#[cfg(test)]
mod preview {
    /// HOME_PREVIEW=path cargo test -p home writes Home as raw 720x1600 RGBA.
    #[test]
    fn preview() {
        let Ok(out) = std::env::var("HOME_PREVIEW") else { return };
        let t = super::clock::parse_http_date("Tue, 24 Nov 2026 02:47:33 GMT");
        let g = super::Glance { utc: t, offset: -8 * 3600, at: std::time::Instant::now(), temp: "54°F".into(), sky: "Partly cloudy".into(), city: "Oakland".into() };
        let s = super::ui::State {
            now: g.now(), online: true, battery: 82, volume: 8, phones: false, temp: g.temp.clone(), sky: g.sky.clone(),
            city: g.city.clone(), running: ["/apps/music".to_string()].into_iter().collect(), pressed: None,
            shield: Default::default(),
        };
        let mut c = violet::Canvas::new(violet::W, violet::H);
        super::ui::draw(&mut c, &violet::Fonts::load(), &s);
        let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
        std::fs::write(out, bytes).unwrap();
    }
}
