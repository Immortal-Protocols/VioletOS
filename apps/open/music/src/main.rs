//! Music: streams the Glass Orchard album from the Rose Music library over
//! Wi-Fi and plays it. HTTPS with rustls, MP3 decoding in the app
//! (symphonia), PCM to /dev/violet/audio, and a full 720x1600 interface
//! drawn into /dev/violet/screen.
mod player;
mod ui;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use violet::{device::TOUCH_DOWN, rgb, Canvas, Fonts, Screen, H, W};

fn main() {
    println!("music: starting");
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut frame = Canvas::new(W, H);
    let mut layer = Canvas::new(W, H);
    let splash = |screen: &mut Screen, frame: &mut Canvas, line: &str| {
        let b = frame.bounds();
        frame.fill(b, rgb(20, 14, 30));
        fonts.bold(38.0).centered(frame, "Rose Music", 0, W as i32, 760, rgb(0xe6, 0x8c, 0xc8));
        fonts.regular(28.0).centered(frame, line, 0, W as i32, 820, rgb(0xee, 0xea, 0xf8));
        screen.show_all(frame);
    };
    splash(&mut screen, &mut frame, "Connecting to the library…");
    let catalog = loop {
        match player::catalog() {
            Ok(c) => break c,
            Err(e) => {
                println!("music: catalog: {e}");
                splash(&mut screen, &mut frame, "Waiting for the network…");
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    };
    println!("music: {}, {} songs", catalog.album, catalog.tracks.len());
    let mut player = match player::Player::new() {
        Ok(p) => p,
        Err(e) => {
            println!("music: no audio: {e}");
            splash(&mut screen, &mut frame, &format!("No audio: {e}"));
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    };

    // Covers load in the background; the screen redraws when one arrives.
    let mut covers: HashMap<usize, ui::Cover> = HashMap::new();
    let mut requested: HashSet<usize> = HashSet::new();
    let (cover_tx, cover_rx) = mpsc::channel::<(usize, Option<ui::Cover>)>();
    let want_cover = |i: usize, requested: &mut HashSet<usize>| {
        if requested.insert(i) {
            let url = format!("{}/{}", catalog.base, catalog.tracks[i].cover);
            let tx = cover_tx.clone();
            std::thread::spawn(move || {
                let c = player::fetch(&url).ok().and_then(|b| ui::cover(&b));
                let _ = tx.send((i, c));
            });
        }
    };

    let mut shown: Option<Option<usize>> = None;
    let play = |player: &mut player::Player, i: isize| {
        let n = catalog.tracks.len() as isize;
        let i = ((i % n + n) % n) as usize;
        player.play(&catalog, i);
        let t = &catalog.tracks[i];
        println!("music: playing {} {} ({} BPM, {})", t.number, t.title, t.bpm, t.key);
    };
    play(&mut player, 0); // start the album right away

    let mut last_log = Instant::now();
    let mut last_look: Option<(u32, [u8; player::BANDS], bool, Option<usize>, String)> = None;
    let mut last_frame = Instant::now();
    let (mut draw_us, mut show_us, mut frames) = (0u64, 0u64, 0u32);
    loop {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(e) if e.focus_gained() => shown = None,
            Ok(e) if e.kind == TOUCH_DOWN => {
                let current = player.track.load(Relaxed);
                match ui::hit(e.x, e.y, catalog.tracks.len()) {
                    Some(ui::Hit::Home) => violet::home(),
                    Some(ui::Hit::Play) => {
                        if current < 0 {
                            play(&mut player, 0);
                        } else {
                            player.paused.store(!player.paused.load(Relaxed), Relaxed);
                        }
                    }
                    Some(ui::Hit::Prev) => {
                        if current >= 0 && player.position() > 3.0 {
                            play(&mut player, current as isize);
                        } else {
                            play(&mut player, current as isize - 1);
                        }
                    }
                    Some(ui::Hit::Next) => play(&mut player, current as isize + 1),
                    Some(ui::Hit::Song(i)) => play(&mut player, i as isize),
                    None => {}
                }
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        while let Ok(done) = player.finished.try_recv() {
            if player.track.load(Relaxed) == done {
                play(&mut player, done as isize + 1); // the album repeats
            }
        }
        while let Ok((i, c)) = cover_rx.try_recv() {
            if let Some(c) = c {
                covers.insert(i, c);
                shown = None;
            }
        }
        let current = player.track.load(Relaxed);
        let current = (current >= 0).then_some(current as usize);
        let art = current.unwrap_or(0);
        want_cover(art, &mut requested);
        if last_log.elapsed() > Duration::from_secs(10) {
            if let Some(i) = current {
                last_log = Instant::now();
                let paused = if player.paused.load(Relaxed) { " (paused)" } else { "" };
                let p = player.position() as u32;
                let secs = 10_000_000u64;
                println!("music: {} at {}:{:02}{} (draw {}% in {} frames, show {}%, decode {}%)", catalog.tracks[i].title, p / 60, p % 60, paused,
                    draw_us * 100 / secs, frames, show_us * 100 / secs, player::DECODE_US.swap(0, Relaxed) * 100 / secs);
                (draw_us, show_us, frames) = (0, 0, 0);
            }
        }
        let full = shown != Some(current);
        if full {
            ui::draw_static(&mut layer, &fonts, &catalog, covers.get(&art), current);
            frame.px.copy_from_slice(&layer.px);
            shown = Some(current);
        }
        let pal = covers.get(&art).map_or(ui::Palette::default(), |c| c.palette);
        let status = player.failure.lock().unwrap().clone();
        // Redraw the lower band only when what it shows changes: the second,
        // the waveform's progress, the spectrum (in 1/32 steps), play or pause.
        let look = (
            (player.position() * 4.0) as u32,
            player.bands().map(|v| (v * 32.0) as u8),
            player.paused.load(Relaxed),
            current,
            status.clone(),
        );
        // At most ten frames a second: every present is copied to the
        // framebuffer by the Open Runtime.
        if !full && (last_look.as_ref() == Some(&look) || last_frame.elapsed() < Duration::from_millis(100)) {
            continue;
        }
        let controls_changed = last_look.as_ref().is_none_or(|l| l.2 != look.2 || l.3 != look.3);
        last_look = Some(look);
        last_frame = Instant::now();
        let t0 = Instant::now();
        ui::draw_dynamic(&mut frame, &layer, &fonts, &catalog, &player, pal, &status);
        let t1 = Instant::now();
        if full {
            screen.show_all(&frame);
        } else {
            screen.show(&frame, ui::PROGRESS);
            screen.show(&frame, ui::SPECTRUM);
            if controls_changed {
                screen.show(&frame, ui::CONTROLS);
            }
        }
        draw_us += (t1 - t0).as_micros() as u64;
        show_us += t1.elapsed().as_micros() as u64;
        frames += 1;
    }
}
