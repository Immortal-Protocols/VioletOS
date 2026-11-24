//! Themes (Violet Shield demo, malicious): a theme installer that maps
//! writable and executable memory, then tries to overwrite another app's
//! program, Home's, with its own hook.
use std::time::Duration;

use violet::{colors::*, Canvas, Fonts, Rect, Screen, H, W};

const TARGET: &str = "/apps/home";

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let fonts = Fonts::load();
    let mut c = Canvas::new(W, H);
    let draw = |c: &mut Canvas, line: &str| {
        c.fill(c.bounds(), BACKGROUND);
        violet::header(c, &fonts, "Themes");
        let card = Rect::new(40, 260, W as i32 - 40, 900);
        c.gradient(card.inset(24), violet::rgb(0x10, 0x12, 0x2c), violet::rgb(0x3a, 0x22, 0x5c));
        fonts.bold(56.0).centered(c, "Midnight", card.x0, card.x1, 560, INK);
        fonts.medium(30.0).centered(c, line, 0, W as i32, 1000, MUTED);
    };
    println!("themes: installing Midnight theme...");
    draw(&mut c, "Installing Midnight...");
    screen.show_all(&c);

    let region = unsafe {
        libc::mmap(std::ptr::null_mut(), 65536, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                   libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
    };
    if region == libc::MAP_FAILED {
        println!("themes: W+X map refused: {}", std::io::Error::last_os_error());
    } else {
        unsafe { std::ptr::write_bytes(region as *mut u8, 0, 16) };
        println!("themes: staged 64 KiB W+X region");
    }
    std::thread::sleep(Duration::from_secs(1));
    draw(&mut c, "Applying the theme to Home...");
    screen.show_all(&c);
    match std::fs::write(TARGET, b"#!/bin/sh\n# persistence hook\n") {
        Ok(()) => println!("themes: hook written into {TARGET}"),
        Err(e) => println!("themes: hook refused: {e}"),
    }
    draw(&mut c, "Midnight is installed");
    screen.show_all(&c);
    std::thread::sleep(Duration::from_secs(1));
    println!("themes done");
}
