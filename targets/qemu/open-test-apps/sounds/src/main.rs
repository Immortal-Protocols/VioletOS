//! Sounds: the shared volume and output, the built-in sounds, and a few
//! seconds recorded from the microphone to play back. The state comes from
//! /dev/violet/sys "audio", so the volume buttons and the headphone jack show
//! up here as they change.
use std::collections::HashMap;
use std::time::Duration;

use violet::{colors::*, Canvas, Fonts, Rect, Screen, W};

const MINUS: Rect = Rect::new(40, 470, 340, 590);
const PLUS: Rect = Rect::new(380, 470, 680, 590);
const RECORD: Rect = Rect::new(40, 1150, 340, 1270);
const PLAY: Rect = Rect::new(380, 1150, 680, 1270);
const LIVE: Rect = Rect::new(0, 200, W as i32, 470);
const MIC: Rect = Rect::new(0, 1040, W as i32, 1140);
const STATUS: Rect = Rect::new(0, 640, W as i32, 700);
const PADS: [(&str, &str); 4] = [("Chime", "sound chime"), ("Alert", "sound alert"), ("Tone", "sound tone 440 3000"), ("Stop", "sound stop")];

fn pad(i: usize) -> Rect {
    let (x, y) = (40 + (i as i32 % 2) * 340, 720 + (i as i32 / 2) * 150);
    Rect::new(x, y, x + 300, y + 120)
}

type State = HashMap<String, String>;

fn num(s: &State, k: &str) -> i32 {
    s.get(k).and_then(|v| v.parse().ok()).unwrap_or(0)
}
fn on(s: &State, k: &str) -> bool {
    s.get(k).is_some_and(|v| v == "1")
}

struct App {
    fonts: Fonts,
    c: Canvas,
    s: State,
}

impl App {
    fn live(&mut self) {
        let c = &mut self.c;
        c.fill(LIVE, BACKGROUND);
        if !self.s.contains_key("volume") {
            self.fonts.bold(38.0).draw(c, "No audio service", 40, 300, INK);
            return;
        }
        let (level, top) = (num(&self.s, "volume"), num(&self.s, "max").max(1));
        self.fonts.bold(120.0).draw(c, &level.to_string(), 40, 340, ACCENT);
        self.fonts.medium(24.0).draw(c, "VOLUME", 44, 380, MUTED);
        let output = match (on(&self.s, "headphones"), on(&self.s, "headphone_out"), on(&self.s, "speaker")) {
            (true, true, _) => "Headphones",
            (true, false, _) => "Headphones – no output",
            (false, _, true) => "Speaker",
            _ => "Speaker – no output",
        };
        self.fonts.regular(30.0).draw(c, output, 300, 300, INK);
        let seg = (W as i32 - 80) / top;
        for i in 0..top {
            let col = if i < level { ACCENT } else { PANEL };
            c.round_rect(Rect::new(40 + i * seg, 410, 40 + i * seg + seg - 8, 450), 6.0, col);
        }
    }

    fn status(&mut self) {
        self.c.fill(STATUS, BACKGROUND);
        let mut line = match self.s.get("outcome").map(|s| s.as_str()) {
            Some("played") => "Played".to_string(),
            Some("stopped") => "Stopped".to_string(),
            Some("no-output") => "No audio output".to_string(),
            _ => "Ready".to_string(),
        };
        if let Some(p) = self.s.get("playing").filter(|p| !p.is_empty() && *p != "none") {
            line = format!("Playing {p}");
        }
        self.fonts.medium(24.0).draw(&mut self.c, &line, 40, 680, MUTED);
    }

    fn mic(&mut self) {
        let c = &mut self.c;
        c.fill(MIC, BACKGROUND);
        if !on(&self.s, "mic") {
            self.fonts.medium(24.0).draw(c, "No microphone", 40, 1090, MUTED);
            return;
        }
        let ms = num(&self.s, "recording_ms");
        let mode = self.s.get("mic_mode").map_or("off", |s| s.as_str());
        let label = if ms == 0 && mode != "record" { "Microphone".to_string() } else { format!("Microphone  {:.1} s", ms as f32 / 1000.0) };
        self.fonts.regular(30.0).draw(c, &label, 40, 1090, INK);
        if mode != "off" {
            let level = num(&self.s, "mic_level");
            for i in 0..10 {
                let col = if i * 10 < level { ACCENT } else { PANEL };
                c.round_rect(Rect::new(40 + i * 64, 1110, 40 + i * 64 + 56, 1134), 6.0, col);
            }
        }
    }

    fn mic_buttons(&mut self) {
        if !on(&self.s, "mic") {
            return;
        }
        let big = self.fonts.bold(38.0);
        let rec = if self.s.get("mic_mode").is_some_and(|m| m == "record") { "Finish" } else { "Record" };
        self.c.fill(RECORD, BACKGROUND);
        violet::button(&mut self.c, &big, RECORD, rec, ACCENT, BACKGROUND);
        violet::button(&mut self.c, &big, PLAY, "Play", PANEL, INK);
    }

    fn draw(&mut self) {
        let b = self.c.bounds();
        self.c.fill(b, BACKGROUND);
        violet::header(&mut self.c, &self.fonts, "Sounds");
        self.live();
        let title = self.fonts.bold(44.0);
        violet::button(&mut self.c, &title, MINUS, "–", PANEL, INK);
        violet::button(&mut self.c, &title, PLUS, "+", PANEL, INK);
        self.status();
        let big = self.fonts.bold(38.0);
        for (i, (label, _)) in PADS.iter().enumerate() {
            violet::button(&mut self.c, &big, pad(i), label, PANEL, INK);
        }
        self.mic();
        self.mic_buttons();
    }
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let mut app = App { fonts: Fonts::load(), c: screen.canvas(), s: violet::fields(&violet::sys("audio")) };
    app.draw();
    screen.show_all(&app.c);
    let mut pending = None;
    loop {
        let next_event = match pending.take() {
            Some(e) => Ok(e),
            None => events.recv_timeout(Duration::from_millis(150)),
        };
        match next_event {
            Ok(e) if e.focus_gained() => {
                app.draw();
                screen.show_all(&app.c);
            }
            Ok(e) if e.kind == violet::device::TOUCH_DOWN => {
                let (x, y) = (e.x, e.y);
                let mic = on(&app.s, "mic");
                if violet::HOME_BOX.contains(x, y) {
                    violet::home();
                } else if MINUS.contains(x, y) || PLUS.contains(x, y) {
                    violet::sys(if PLUS.contains(x, y) { "volume up" } else { "volume down" });
                    violet::sys("sound click"); // let the new level be heard
                } else if mic && RECORD.contains(x, y) {
                    let recording = app.s.get("mic_mode").is_some_and(|m| m == "record");
                    violet::sys(if recording { "mic off" } else { "mic record" });
                } else if mic && PLAY.contains(x, y) {
                    violet::sys("sound recording");
                } else if let Some(i) = (0..PADS.len()).find(|&i| pad(i).contains(x, y)) {
                    println!("sounds: {} -> {}", PADS[i].1, violet::sys(PADS[i].1));
                }
            }
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        // Handle every waiting touch before asking for the audio state: that
        // takes three trips to Runtime, and a tap should not wait behind them.
        if events.try_recv().map(|e| pending = Some(e)).is_ok() {
            continue;
        }
        let next = violet::fields(&violet::sys("audio"));
        if next != app.s {
            let mic_changed = next.get("mic_mode") != app.s.get("mic_mode") || next.get("mic") != app.s.get("mic");
            app.s = next;
            app.live();
            app.status();
            app.mic();
            screen.show(&app.c, LIVE);
            screen.show(&app.c, STATUS);
            screen.show(&app.c, MIC);
            if mic_changed {
                app.mic_buttons();
                screen.show(&app.c, Rect::new(0, RECORD.y0, W as i32, RECORD.y1));
            }
        }
    }
}
