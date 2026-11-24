//! Stopwatch: start, pause and reset, to the tenth of a second. The time is
//! kept in /data/stopwatch, so a paused watch survives a restart.
use std::time::{Duration, Instant};

use violet::{colors::*, Canvas, Fonts, Rect, Screen, H, W};

const SAVED: &str = "/data/stopwatch";
const START: Rect = Rect::new(60, 1000, 660, 1130);
const RESET: Rect = Rect::new(60, 1170, 660, 1300);
const DIGITS: Rect = Rect::new(0, 560, W as i32, 760);

#[derive(Default)]
struct Watch {
    base: Duration,
    since: Option<Instant>,
}

impl Watch {
    fn elapsed(&self) -> Duration {
        self.base + self.since.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

fn load() -> Duration {
    let tenths = std::fs::read_to_string(SAVED).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    Duration::from_millis(tenths * 100)
}

fn save(d: Duration) {
    if let Err(e) = std::fs::write(SAVED, format!("{}\n", d.as_millis() / 100)) {
        println!("stopwatch: could not save: {e}");
    }
}

fn format(d: Duration) -> String {
    let t = d.as_millis() / 100;
    let s = t / 10;
    format!("{:02}:{:02}.{}", s / 60 % 100, s % 60, t % 10)
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let (big, huge) = (fonts.bold(38.0), fonts.bold(120.0));
    let mut c = Canvas::new(W, H);
    let mut w = Watch { base: load(), since: None };
    println!("stopwatch: starting at {}", format(w.base));

    let draw_digits = |c: &mut Canvas, w: &Watch| {
        c.fill(DIGITS, BACKGROUND);
        huge.centered(c, &format(w.elapsed()), 0, W as i32, 700, ACCENT);
    };
    let draw = |c: &mut Canvas, w: &Watch| {
        c.fill(c.bounds(), BACKGROUND);
        violet::header(c, &fonts, "Stopwatch");
        draw_digits(c, w);
        violet::button(c, &big, START, if w.since.is_some() { "Pause" } else { "Start" }, ACCENT, BACKGROUND);
        violet::button(c, &big, RESET, "Reset", PANEL, INK);
    };
    draw(&mut c, &w);
    screen.show_all(&c);
    let mut shown = String::new();
    loop {
        // Tick every 50 ms while running; otherwise wait for a touch.
        let timeout = if w.since.is_some() { Duration::from_millis(50) } else { Duration::from_secs(3600) };
        match events.recv_timeout(timeout) {
            Ok(e) if e.focus_gained() => {
                draw(&mut c, &w);
                screen.show_all(&c);
            }
            Ok(e) if e.kind == violet::device::TOUCH_DOWN => {
                if violet::HOME_BOX.contains(e.x, e.y) {
                    violet::home();
                } else if START.contains(e.x, e.y) {
                    if w.since.is_some() {
                        w.base = w.elapsed();
                        w.since = None;
                        save(w.base);
                        println!("stopwatch: paused at {}", format(w.base));
                    } else {
                        w.since = Some(Instant::now());
                        println!("stopwatch: started at {}", format(w.base));
                    }
                    draw(&mut c, &w);
                    screen.show_all(&c);
                } else if RESET.contains(e.x, e.y) {
                    w = Watch::default();
                    save(Duration::ZERO);
                    println!("stopwatch: reset");
                    draw(&mut c, &w);
                    screen.show_all(&c);
                }
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        let s = format(w.elapsed());
        if s != shown {
            shown = s;
            draw_digits(&mut c, &w);
            screen.show(&c, DIGITS);
        }
    }
}
