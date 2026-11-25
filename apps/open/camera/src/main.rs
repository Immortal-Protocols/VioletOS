//! Camera: a live viewfinder from the rear camera (/dev/violet/camera), with
//! a shutter that holds the picture for a look.
//!
//! The camera delivers raw Bayer frames; violet::camera develops each 2 x 2
//! block into one colour (black level, white balance, gamma), turned upright
//! for the portrait screen. On the Moto G Pure a 1280 x 720 frame becomes a
//! 360 x 640 picture, shown at twice that size.
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use violet::camera::{Camera, Develop};
use violet::device::{FOCUS_GAINED, LIFECYCLE, TOUCH_DOWN};
use violet::{colors::*, rgb, Canvas, Fonts, Rect, Screen, W};

const VIEW: Rect = Rect::new(0, 120, W as i32, 1400);
const SHUTTER: Rect = Rect::new(280, 1420, 440, 1580);
const STATUS_Y: i32 = 1510;

fn chrome(c: &mut Canvas, fonts: &Fonts, held: bool) {
    c.fill(Rect::new(0, 0, W as i32, VIEW.y0), BACKGROUND);
    c.fill(Rect::new(0, VIEW.y1, W as i32, 1600), BACKGROUND);
    violet::button(c, &fonts.medium(24.0), violet::HOME_BOX, "‹ Home", PANEL, INK);
    fonts.bold(40.0).draw(c, "Camera", 230, 80, INK);
    let (cx, cy) = (360.0, 1500.0);
    if held {
        c.circle(cx, cy, 70.0, PANEL);
        fonts.medium(28.0).centered(c, "Back", SHUTTER.x0, SHUTTER.x1, 1510, INK);
    } else {
        c.circle(cx, cy, 70.0, INK);
        c.circle(cx, cy, 60.0, BACKGROUND);
        c.circle(cx, cy, 54.0, INK);
    }
}

fn status(c: &mut Canvas, fonts: &Fonts, text: &str) {
    let r = Rect::new(470, STATUS_Y - 34, W as i32, STATUS_Y + 12);
    c.fill(r, BACKGROUND);
    fonts.regular(24.0).draw(c, text, r.x0 + 10, STATUS_Y, MUTED);
}

fn message(c: &mut Canvas, fonts: &Fonts, text: &str) {
    c.fill(VIEW, rgb(0, 0, 0));
    fonts.medium(30.0).centered(c, text, 0, W as i32, (VIEW.y0 + VIEW.y1) / 2, MUTED);
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    c.fill(c.bounds(), BACKGROUND);
    chrome(&mut c, &fonts, false);
    message(&mut c, &fonts, "Starting the camera…");
    screen.show_all(&c);

    let mut camera = match Camera::open() {
        Ok(cam) => cam,
        Err(e) => {
            println!("camera: {e}");
            message(&mut c, &fonts, "This phone has no camera here");
            screen.show_all(&c);
            loop {
                match events.recv() {
                    Ok(e) if e.kind == TOUCH_DOWN && violet::HOME_BOX.contains(e.x, e.y) => violet::home(),
                    Ok(e) if e.kind == LIFECYCLE && e.value == FOCUS_GAINED => screen.show_all(&c),
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        }
    };
    let mut develop = Develop::default();
    let mut held = false;
    let mut first = true;
    let (mut count, mut since) = (0u32, Instant::now());
    loop {
        loop {
            match events.try_recv() {
                Ok(e) if e.kind == TOUCH_DOWN => {
                    if violet::HOME_BOX.contains(e.x, e.y) {
                        violet::home();
                    } else if SHUTTER.contains(e.x, e.y) {
                        held = !held;
                        println!("camera: {}", if held { "picture held" } else { "live" });
                        chrome(&mut c, &fonts, held);
                        screen.show_all(&c);
                    }
                }
                Ok(e) if e.kind == LIFECYCLE && e.value == FOCUS_GAINED => {
                    chrome(&mut c, &fonts, held);
                    screen.show_all(&c);
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if held {
            // The held picture stays; wait for a touch instead of frames.
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        let frame = match camera.next() {
            Ok(f) => f,
            Err(e) => {
                println!("camera: {e}");
                message(&mut c, &fonts, &e.to_string());
                screen.show_all(&c);
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if first {
            let i = frame.info;
            println!("camera: sensor {:04x}, {} x {}, format {:x}", i.sensor, i.width, i.height, i.format);
            first = false;
        }
        if frame.info.sequence % 8 == 0 {
            develop.balance(&frame);
        }
        develop.draw(&frame, &mut c, VIEW, 2);
        count += 1;
        let elapsed = since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let fps = count as f32 / elapsed.as_secs_f32();
            status(&mut c, &fonts, &format!("{fps:.0} fps"));
            screen.show(&c, Rect::new(470, STATUS_Y - 34, W as i32, STATUS_Y + 12));
            (count, since) = (0, Instant::now());
        }
        screen.show(&c, VIEW);
    }
}
