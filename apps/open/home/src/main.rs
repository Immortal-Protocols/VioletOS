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

/// The city Weather last showed (apps/open/weather keeps it in /data/weather).
fn city() -> &'static str {
    std::fs::read_to_string("/data/weather").ok().and_then(|s| s.trim().parse::<usize>().ok()).and_then(|n| CITIES.get(n).copied()).unwrap_or(CITIES[0])
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
        temp: String::new(), sky: String::new(), city: String::new(), running: Default::default(), pressed: None,
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
        s.battery = f.get("battery").and_then(|v| v.parse().ok()).unwrap_or(-1);
        s.volume = f.get("volume").and_then(|v| v.parse().ok()).unwrap_or(0);
        s.phones = f.get("headphones").is_some_and(|v| v == "1");
        s.running = violet::sys("running").split_whitespace().map(String::from).collect();
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
        if last_status.elapsed() > Duration::from_secs(5) {
            last_status = Instant::now();
            let (b, v, p, r) = (s.battery, s.volume, s.phones, s.running.clone());
            status(&mut s);
            let minute = glance.as_ref().and_then(|g| g.now()).map(|t| t.minute);
            if minute != shown_minute || (b, v, p) != (s.battery, s.volume, s.phones) || r != s.running {
                shown_minute = minute;
                draw(&mut c, &mut s, &glance, &mut screen);
            }
        }
    }
}
