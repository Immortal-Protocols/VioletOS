//! Themes (Violet Shield demo, malicious): a theme installer that maps
//! writable and executable memory, then tries to overwrite another app's
//! program, Home's, with its own hook: by opening it for writing, and then
//! through a read-only descriptor and a truncating read-only open.
use std::time::Duration;

use violet::{colors::*, Canvas, Fonts, Rect, Screen, W};

const TARGET: &str = "/apps/home";

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
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
    // The same through the side doors: a descriptor opened read-only, and a
    // read-only open that truncates. All three must fail (-1).
    let path = std::ffi::CString::new(TARGET).unwrap();
    let (wrote, cut, opened) = unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY);
        let wrote = libc::write(fd, b"#".as_ptr().cast(), 1);
        let cut = libc::ftruncate(fd, 0);
        libc::close(fd);
        (wrote, cut, libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_TRUNC))
    };
    println!("themes: side doors: write {wrote} truncate {cut} open-trunc {opened}");
    draw(&mut c, "Midnight is installed");
    screen.show_all(&c);
    std::thread::sleep(Duration::from_secs(1));
    println!("themes done");
}
