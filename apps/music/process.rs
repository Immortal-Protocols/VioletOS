//! Music (bring-up stub): a full-resolution screen that streams track 1.
//! Top half plays or pauses; bottom quarter returns Home. The spectrum bars
//! show the levels Audio measures.
use crate::{protocol::*, sdk::*};

const W: usize = 720;
const H: usize = 1600;

fn surface() -> *mut u32 {
    request(GET, SURFACE, 0).value as *mut u32
}
fn fill(s: *mut u32, x: usize, y: usize, w: usize, h: usize, c: u32) {
    for row in y..(y + h).min(H) {
        for col in x..(x + w).min(W) {
            unsafe { s.add(row * W + col).write(c) };
        }
    }
}
fn present_all(first: usize, end: usize) {
    request(EXECUTE, PRESENT, HD_PRESENT | (first as u32) << 16 | end as u32);
}
fn draw_background(s: *mut u32) {
    for y in 0..H {
        let t = (y * 255 / H) as u32;
        let c = 0xff000000 | (40 + t / 3) << 16 | (20 + t / 8) << 8 | (70 + t / 4);
        for x in 0..W {
            unsafe { s.add(y * W + x).write(c) };
        }
    }
}
fn draw_status(s: *mut u32, status: &Message) {
    fill(s, 0, 900, W, 500, 0xff1c1426);
    let text = status.text();
    for band in 0..MUSIC_BANDS {
        let h = text[band] as usize * 2;
        fill(s, 30 + band * 42, 1300 - h, 34, h, 0xffd1b0fe);
    }
    let state = status.arg & 15;
    let color = match state {
        MUSIC_STATE_PLAYING => 0xff7cf0a0,
        MUSIC_STATE_BUFFERING => 0xfff0d070,
        MUSIC_STATE_OFFLINE => 0xfff07070,
        _ => 0xff8080a0,
    };
    fill(s, 30, 920, 60, 60, color);
    let ms = (status.arg >> 4) as usize;
    fill(s, 120, 940, (ms / 1000).min(600), 20, 0xffeeeaf8);
}

pub fn run() -> ! {
    let s = surface();
    let me = request(GET, WHOAMI, 0).value;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let mut visible = false;
    loop {
        let active = request(GET, FOCUS, 0).value == me;
        let status = request(GET, MUSIC, 0);
        if active {
            if !visible {
                draw_background(s);
                fill(s, 0, 1400, W, 200, 0xff302a39);
                draw_status(s, &status);
                present_all(0, H);
            } else {
                draw_status(s, &status);
                present_all(900, 1400);
            }
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK {
            let y = e.arg as usize;
            if y >= 1400 {
                request(EXECUTE, OPEN, LAUNCHER);
            } else if y < 800 {
                let playing = status.value != 0 && status.arg & 15 != MUSIC_STATE_PAUSED;
                if status.value == 0 {
                    request_with(EXECUTE, MUSIC, MUSIC_PLAY, 1);
                } else if playing {
                    request(EXECUTE, MUSIC, MUSIC_PAUSE);
                } else {
                    request(EXECUTE, MUSIC, MUSIC_RESUME);
                }
            }
        }
        unsafe { delay_ms(30) };
    }
}
