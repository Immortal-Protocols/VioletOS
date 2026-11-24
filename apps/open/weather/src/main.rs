//! Weather: current conditions for twenty cities, fetched from wttr.in over
//! HTTPS by this app (rustls, the Mozilla root certificates). The last city
//! chosen is the shared setting weather.city (Home shows the same city).
use std::sync::mpsc;
use std::time::Duration;

use violet::{colors::*, Canvas, Fonts, Rect, Screen, H, W};

const CITIES: [&str; 20] = [
    "San Francisco", "Oakland", "Los Angeles", "Seattle", "New York",
    "Chicago", "Austin", "Miami", "Denver", "Boston",
    "Honolulu", "Anchorage", "Mexico City", "London", "Paris",
    "Berlin", "Tokyo", "Sydney", "Mumbai", "Cairo",
];
const GRID_TOP: i32 = 800;
const ROW_H: i32 = 66;
const ROWS: i32 = 10;
const CURRENT: Rect = Rect::new(0, 200, W as i32, 780);

#[derive(Default, Clone)]
struct Report {
    temp: String,
    condition: String,
    humidity: String,
    wind: String,
}

/// Characters the Go fonts can draw (arrows and emoji are left out).
fn clean(s: &str) -> String {
    s.trim().chars().filter(|&c| (c as u32) <= 0xff).collect()
}

fn fetch(city: &str) -> Result<Report, String> {
    let url = format!("https://wttr.in/{}?format=%t;%C;%h;%w&u", city.replace(' ', "+"));
    let r = violet::http::get(&url, &[("User-Agent", "curl/8 (VioletOS Weather)")]).map_err(|e| e.to_string())?;
    if r.status != 200 {
        return Err(format!("wttr.in: {}", r.status));
    }
    let text = r.text();
    let f: Vec<String> = text.trim().split(';').map(clean).collect();
    if f.len() < 2 || f[0].is_empty() {
        return Err(format!("unexpected answer {text:?}"));
    }
    let at = |i: usize| f.get(i).cloned().unwrap_or_default();
    Ok(Report { temp: at(0).trim_start_matches('+').to_string(), condition: at(1), humidity: at(2), wind: at(3) })
}

fn cell(i: usize) -> Rect {
    let (col, row) = (i as i32 / ROWS, i as i32 % ROWS);
    let (x, y) = (40 + col * 330, GRID_TOP + row * ROW_H);
    Rect::new(x, y, x + 310, y + ROW_H - 10)
}

struct App {
    fonts: Fonts,
    c: Canvas,
    city: Option<usize>,
    shown: Report,
    status: String,
}

impl App {
    fn current(&mut self) {
        let c = &mut self.c;
        c.fill(CURRENT, BACKGROUND);
        match self.city {
            None => {
                self.fonts.bold(38.0).draw(c, "Choose a city", 40, 300, INK);
            }
            Some(i) => {
                self.fonts.bold(38.0).draw(c, CITIES[i], 40, 300, INK);
                if !self.shown.temp.is_empty() {
                    self.fonts.bold(120.0).draw(c, &self.shown.temp, 40, 470, ACCENT);
                    let body = self.fonts.regular(30.0);
                    let cond = body.fit(&self.shown.condition, W as i32 - 80);
                    body.draw(c, &cond, 40, 560, INK);
                    if !self.shown.humidity.is_empty() {
                        let line = format!("Humidity {}   Wind {}", self.shown.humidity, self.shown.wind);
                        self.fonts.medium(24.0).draw(c, &line, 40, 620, MUTED);
                    }
                }
            }
        }
        self.fonts.medium(24.0).draw(c, &self.status, 40, 720, MUTED);
    }

    fn grid(&mut self) {
        let small = self.fonts.medium(24.0);
        for (i, name) in CITIES.iter().enumerate() {
            let (bg, ink) = if Some(i) == self.city { (ACCENT, BACKGROUND) } else { (PANEL, INK) };
            let r = cell(i);
            self.c.fill(r.inset(-4), BACKGROUND);
            self.c.round_rect(r, 16.0, bg);
            small.draw(&mut self.c, name, r.x0 + 20, r.y0 + 38, ink);
        }
    }

    fn draw(&mut self) {
        let b = self.c.bounds();
        self.c.fill(b, BACKGROUND);
        violet::header(&mut self.c, &self.fonts, "Weather");
        self.current();
        self.grid();
    }
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let saved = violet::sys("get weather.city").parse::<usize>().ok().filter(|&n| n < CITIES.len());
    let mut app = App { fonts: Fonts::load(), c: Canvas::new(W, H), city: None, shown: Report::default(), status: "Tap a city for live weather".into() };
    let (tx, results) = mpsc::channel::<(usize, Result<Report, String>)>();
    let mut loading = false;
    let start = |app: &mut App, i: usize, loading: &mut bool| {
        app.city = Some(i);
        app.shown = Report::default();
        app.status = "Fetching…".into();
        *loading = true;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((i, fetch(CITIES[i])));
        });
    };
    if let Some(i) = saved {
        start(&mut app, i, &mut loading);
    }
    app.draw();
    screen.show_all(&app.c);
    loop {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(e) if e.focus_gained() => {
                app.draw();
                screen.show_all(&app.c);
            }
            Ok(e) if e.kind == violet::device::TOUCH_DOWN => {
                if violet::HOME_BOX.contains(e.x, e.y) {
                    violet::home();
                } else if let Some(i) = (0..CITIES.len()).find(|&i| cell(i).contains(e.x, e.y)) {
                    if !loading {
                        start(&mut app, i, &mut loading);
                        violet::sys(&format!("set weather.city {i}"));
                        app.draw();
                        screen.show_all(&app.c);
                    }
                }
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        while let Ok((i, result)) = results.try_recv() {
            if Some(i) != app.city {
                continue;
            }
            loading = false;
            match result {
                Ok(r) => {
                    println!("weather: {} {} {}", CITIES[i], r.temp, r.condition);
                    app.shown = r;
                    app.status = "Live from wttr.in".into();
                }
                Err(e) => {
                    println!("weather: {} {}", CITIES[i], e);
                    app.status = "Could not fetch – tap to retry".into();
                }
            }
            app.current();
            screen.show(&app.c, CURRENT);
        }
    }
}
