//! Home: the main screen. The date, the Shield's link, and an icon for each
//! app: the ones that ship with the phone, then the ones installed since. It
//! stays running; "home" in /dev/violet/sys brings it back to the front.
mod clock;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use clock::Glance;
use violet::{device::{TOUCH_DOWN, TOUCH_UP}, Canvas, Fonts, Screen};

use violet::CITIES;

/// The city whose time Home shows: the shared setting time.city, chosen in Settings.
fn city() -> &'static str {
    violet::sys("get time.city").parse::<usize>().ok().and_then(|n| CITIES.get(n).copied()).unwrap_or(CITIES[0])
}

/// The time now: UTC from the answer's Date header, the city's offset from its text.
fn fetch(name: &str) -> Result<Glance, String> {
    let url = format!("https://wttr.in/{}?format=%T", name.replace(' ', "+"));
    let r = violet::http::get(&url, &[("User-Agent", "curl/8 (VioletOS Home)")]).map_err(|e| e.to_string())?;
    if r.status != 200 {
        return Err(format!("wttr.in: {}", r.status));
    }
    Ok(Glance {
        utc: r.header("date").and_then(clock::parse_http_date),
        offset: clock::parse_offset(&r.text()),
        at: Instant::now(),
        city: name.to_string(),
    })
}

/// "TITLE<TAB>TEXT" lines from "notifications": the newest, as "Title · text".
fn latest_notice(lines: &str) -> Option<String> {
    let (title, text) = lines.lines().filter_map(|l| l.split_once('\t')).last()?;
    Some(format!("{title} · {text}"))
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let mut art = ui::Art::new();
    let mut s = ui::State { now: None, apps: Vec::new(), running: Default::default(), pressed: None, launch_error: None, notice: None, can_lock: false, shield: Default::default(), listening: false };
    let mut glance: Option<Glance> = None;
    let (tx, glances) = mpsc::channel::<Result<Glance, String>>();
    let refresh = move || {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fetch(city()));
        });
    };
    let status = |s: &mut ui::State, art: &mut ui::Art| {
        s.apps = ui::apps(&std::fs::read_to_string("/apps/.home").unwrap_or_default());
        art.prepare(&fonts, &s.apps);
        s.listening = violet::fields(&violet::sys("status")).get("listening").is_some_and(|v| v == "1");
        s.running = violet::sys("running").split_whitespace().map(String::from).collect();
        s.notice = latest_notice(&violet::sys("notifications"));
        s.can_lock = violet::fields(&violet::sys("passcode")).get("set").is_some_and(|v| v == "1");
        let sh = violet::fields(&violet::sys("shield"));
        s.shield = ui::Shield {
            known: sh.contains_key("linked"),
            linked: sh.get("linked").is_some_and(|v| v == "1"),
            warnings: sh.get("warnings").and_then(|v| v.parse().ok()).unwrap_or(0),
        };
    };
    let draw = |c: &mut Canvas, s: &mut ui::State, art: &ui::Art, glance: &Option<Glance>, screen: &mut Screen| {
        s.now = glance.as_ref().and_then(|g| g.now());
        ui::draw(c, &fonts, art, s);
        screen.show_all(c);
    };
    status(&mut s, &mut art);
    refresh();
    draw(&mut c, &mut s, &art, &glance, &mut screen);
    println!("home: ready");
    let mut online = false;
    let mut last_fetch = Instant::now();
    let mut last_status = Instant::now();
    let mut shown_day = None;
    // Home polls and draws only while it is in front.
    let mut focused = true;
    loop {
        match events.recv_timeout(Duration::from_secs(1)) {
            Ok(e) if e.focus_gained() => {
                focused = true;
                println!("home: shown");
                s.pressed = None;
                s.launch_error = None;
                status(&mut s, &mut art);
                draw(&mut c, &mut s, &art, &glance, &mut screen);
                // Settings may have switched cities while Home was away.
                if last_fetch.elapsed() > Duration::from_secs(120) || glance.as_ref().is_some_and(|g| g.city != city()) {
                    last_fetch = Instant::now();
                    refresh();
                }
            }
            Ok(e) if e.kind == violet::device::LIFECYCLE && e.value == violet::device::FOCUS_LOST => focused = false,
            Ok(e) if e.kind == TOUCH_DOWN => {
                // Only the pressed icon is redrawn.
                s.pressed = ui::hit(&s, e.x, e.y);
                if let Some(i) = s.pressed {
                    let r = ui::region(&s, i);
                    ui::draw_clipped(&mut c, &fonts, &art, &s, r);
                    screen.show(&c, r);
                }
            }
            Ok(e) if e.kind == TOUCH_UP => {
                if let Some(i) = s.pressed.take() {
                    let r = ui::region(&s, i);
                    ui::draw_clipped(&mut c, &fonts, &art, &s, r);
                    screen.show(&c, r);
                    // Lock: the runtime's lock screen comes up at once.
                    if i == ui::LOCK {
                        println!("home: lock -> {}", violet::sys("lock").trim());
                        continue;
                    }
                    // The pill opens Settings, where the phone's links are.
                    let (name, path) = match s.apps.get(i) {
                        Some(a) => (a.name.clone(), a.path()),
                        None => ("settings".to_string(), "/apps/settings".to_string()),
                    };
                    println!("home: opening {}", name);
                    let answer = violet::sys(&format!("open {path}"));
                    if answer != "ok" {
                        println!("home: open {} -> {}", path, answer);
                        let reason = answer.strip_prefix("error ").unwrap_or(&answer).trim();
                        s.launch_error = Some(format!("Couldn’t open app · {reason}"));
                        draw(&mut c, &mut s, &art, &glance, &mut screen);
                    }
                }
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        while let Ok(result) = glances.try_recv() {
            match result {
                Ok(g) => {
                    println!("home: time for {}", g.city);
                    online = true;
                    glance = Some(g);
                }
                Err(e) => {
                    println!("home: time: {e}");
                    online = false;
                }
            }
            if focused {
                draw(&mut c, &mut s, &art, &glance, &mut screen);
            }
        }
        if !focused {
            continue;
        }
        if last_fetch.elapsed() > Duration::from_secs(if online { 3600 } else { 60 }) {
            last_fetch = Instant::now();
            refresh();
        }
        if last_status.elapsed() > Duration::from_secs(2) {
            last_status = Instant::now();
            let (apps, running, shield, listening, notice, can_lock) = (s.apps.clone(), s.running.clone(), s.shield.clone(), s.listening, s.notice.clone(), s.can_lock);
            status(&mut s, &mut art);
            let day = glance.as_ref().and_then(|g| g.now()).map(|t| t.day);
            if day != shown_day || apps != s.apps || running != s.running || shield != s.shield || listening != s.listening || notice != s.notice || can_lock != s.can_lock {
                shown_day = day;
                draw(&mut c, &mut s, &art, &glance, &mut screen);
            }
        }
    }
}

#[cfg(test)]
mod preview {
    use super::ui;

    fn state(extra: usize) -> ui::State {
        let t = super::clock::parse_http_date("Thu, 26 Nov 2026 02:47:33 GMT");
        let g = super::Glance { utc: t, offset: -8 * 3600, at: std::time::Instant::now(), city: "Oakland".into() };
        let mut listing: String = ui::BUILT_IN.iter().map(|(n, t)| format!("{n}\t{t}\n")).collect();
        for (n, t) in [("scorekeeper", "Scorekeeper"), ("tips", "Tip Calculator"), ("dice", "Dice")].iter().take(extra) {
            listing += &format!("{n}\t{t}\n");
        }
        ui::State {
            now: g.now(), apps: ui::apps(&listing), running: ["/apps/music".to_string()].into_iter().collect(), pressed: None, launch_error: None, notice: None, can_lock: true,
            shield: ui::Shield { known: true, linked: true, warnings: 0 }, listening: true,
        }
    }

    fn art(s: &ui::State) -> ui::Art {
        let mut a = ui::Art::new();
        a.prepare(&violet::Fonts::load(), &s.apps);
        a
    }

    /// The newest notification, and nothing for a refusal (locked: "error not allowed").
    #[test]
    fn notices() {
        assert_eq!(super::latest_notice("DICE\trolled 6\nCHAT\tnew message\n").as_deref(), Some("CHAT · new message"));
        assert_eq!(super::latest_notice("error not allowed"), None);
        assert_eq!(super::latest_notice(""), None);
    }

    /// Lock (shown only with a passcode set) is hit on its own, apart from the pill and every tile.
    #[test]
    fn lock_button() {
        let mut s = state(3);
        let r = ui::lock_rect();
        let (x, y) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
        assert_eq!(ui::hit(&s, x, y), Some(ui::LOCK));
        assert!(r.y0 >= ui::pill_rect().y1, "below the pill");
        for i in 0..s.apps.len() {
            assert!(ui::tile_rect(i, &s.apps).y0 >= r.y1, "above tile {i}");
        }
        s.can_lock = false;
        assert_eq!(ui::hit(&s, x, y), None, "no passcode: no Lock");
    }

    /// The built-in apps come first, in their order; installed apps follow; Home is not listed.
    #[test]
    fn grid_order() {
        let apps = ui::apps("scorekeeper\tScorekeeper\nsettings\tSettings\nhome\tHome\nmusic\tMusic\nladybird\tLadybird\n");
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["ladybird", "music", "settings", "scorekeeper"]);
        assert_eq!(apps[1].title, "Locomo Music");
    }

    /// A pressed icon drawn alone matches the same icon in a full redraw.
    #[test]
    fn pressed_matches_full() {
        let fonts = violet::Fonts::load();
        let mut s = state(1);
        let art = art(&s);
        let mut a = violet::Canvas::new(violet::W, violet::H);
        ui::draw(&mut a, &fonts, &art, &s);
        s.pressed = Some(4);
        let mut b = violet::Canvas::new(violet::W, violet::H);
        b.px.copy_from_slice(&a.px);
        ui::draw_clipped(&mut b, &fonts, &art, &s, ui::region(&s, 4));
        let mut full = violet::Canvas::new(violet::W, violet::H);
        ui::draw(&mut full, &fonts, &art, &s);
        assert!(b.px[..] == full.px[..], "a pressed icon differs from a full redraw");
    }

    /// HOME_PREVIEW=path cargo test -p home writes Home as raw 720x1600 RGBA
    /// (HOME_EXTRA=n adds n installed apps).
    #[test]
    fn preview() {
        let Ok(out) = std::env::var("HOME_PREVIEW") else { return };
        let s = state(std::env::var("HOME_EXTRA").ok().and_then(|n| n.parse().ok()).unwrap_or(0));
        let mut c = violet::Canvas::new(violet::W, violet::H);
        let t = std::time::Instant::now();
        let art = art(&s);
        println!("home preview: art in {:?}", t.elapsed());
        ui::draw(&mut c, &violet::Fonts::load(), &art, &s);
        let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
        std::fs::write(out, bytes).unwrap();
    }
}
