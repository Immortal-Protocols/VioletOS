use crate::{drawing::*, protocol::*, sdk::*, watch::*};
fn digits(t: u32) {
    let secs = t / 10;
    let mut b = *b"00:00.0";
    for (i, v) in [
        (0, secs / 600),
        (1, secs / 60),
        (3, secs % 60 / 10),
        (4, secs % 60),
        (6, t),
    ] {
        b[i] += (v % 10) as u8;
    }
    rect(14, 139, 170, 29, BG);
    label(14, 139, &b, 4, ACCENT);
}
pub fn run() -> ! {
    let mut w = Watch::default();
    acquire_surface();
    let status = request(GET, STATUS, 0);
    let saved = request(GET, SAVE, 0);
    if saved.op == OK {
        w.tenths = saved.value;
    }
    let mut disk_error = false;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let mut last = unsafe { ticks() };
    let mut visible = false;
    let mut redraw = true;
    loop {
        let now = unsafe { ticks() };
        let changed = watch_advance(
            &mut w,
            now.wrapping_sub(last),
            unsafe { ticks_per_ms } * 100,
        );
        last = now;
        let active = request(GET, FOCUS, 0).value == WATCH;
        if active && (!visible || redraw) {
            header(&status.text(), saved.op == OK);
            if disk_error {
                rect(14, 43, 100, 7, BG);
                label(14, 43, b"DISK ERROR", 1, INK);
            }
            label(14, 68, b"STOPWATCH", 3, INK);
            digits(w.tenths);
            button(206, if w.running { b"PAUSE" } else { b"START" });
            button(266, b"RESET");
            button(335, b"HOME");
            label(154, 22, b"QUIT", 2, INK);
            present(0, 400);
            redraw = false;
        } else if active && changed {
            digits(w.tenths);
            present(139, 168);
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK {
            if e.value >= 450 && e.arg < 160 {
                request(EXECUTE, QUIT, 0);
            }
            if (42..678).contains(&e.value) {
                match e.arg {
                    824..1024 => {
                        watch_toggle(&mut w);
                        if !w.running && saved.op == OK {
                            disk_error = request(EXECUTE, SAVE, w.tenths).op != OK;
                        }
                        redraw = true;
                    }
                    1064..1264 => {
                        watch_reset(&mut w);
                        if saved.op == OK {
                            disk_error = request(EXECUTE, SAVE, 0).op != OK;
                        }
                        redraw = true;
                    }
                    1340..1540 => {
                        request(EXECUTE, OPEN, LAUNCHER);
                    }
                    _ => {}
                }
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
