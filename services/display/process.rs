use crate::protocol::*;
// Private 240x400 app-owned drawing surfaces. Only display has scanout access.
// The argument identifies a dirty band, allowing a digit update to skip the rest.
/// Handset only: present timings for USB diagnostics, in words 112..=119 of
/// the page Audio shares with USB (see services/audio/README.md).
#[cfg(moto_board)]
fn note(i: usize, v: u32) {
    unsafe { (0x09203000 as *mut u32).add(112 + i).write_volatile(v) }
}
#[cfg(moto_board)]
fn noted(i: usize) -> u32 {
    unsafe { (0x09203000 as *const u32).add(112 + i).read_volatile() }
}
#[cfg(not(moto_board))]
fn note(_: usize, _: u32) {}
#[cfg(not(moto_board))]
fn noted(_: usize) -> u32 {
    0
}

pub fn run() -> ! {
    note(0, 0x5053_4944); // "DISP"
    serve(|m| {
        if m.op == EXECUTE && m.key == POWER {
            unsafe {
                core::ptr::write_bytes(0x08000000 as *mut u32, 0, 720 * 1600);
            }
            return Message::result(0);
        }
        if m.op != EXECUTE || m.key != PRESENT || !is_app(m.value) {
            return Message::error(INVALID);
        }
        // Bit 31 marks an HD frame drawn at the panel's full 720x1600.
        let hd = m.arg & HD_PRESENT != 0;
        let first = ((m.arg & !HD_PRESENT) >> 16) as usize;
        let end = (m.arg & 0xffff) as usize;
        let rows = if hd { 1600 } else { 400 };
        if first >= end || end > rows {
            return Message::error(INVALID);
        }
        let began = unsafe { ticks() };
        let src = (0x20000000 + (m.value as usize - 1) * 0x500000) as *const u32;
        let dst = 0x08000000 as *mut u32;
        for y in first..end {
            if hd {
                for x in 0..720 {
                    unsafe {
                        dst.add(y * 720 + x).write_volatile(src.add(y * 720 + x).read_volatile());
                    }
                }
                continue;
            }
            for x in 0..240 {
                let color = unsafe { src.add(y * 240 + x).read_volatile() };
                for dy in 0..4 {
                    let pos = (4 * y + dy) * 720 + 3 * x;
                    unsafe {
                        for dx in 0..3 {
                            dst.add(pos + dx).write_volatile(color);
                        }
                    }
                }
            }
        }
        // Presents, rows, and the last and longest present in microseconds,
        // plus the panel rows written by the last one.
        let us = unsafe { ticks().wrapping_sub(began) / (ticks_per_ms / 1000).max(1) };
        let panel_rows = (end - first) as u32 * if hd { 1 } else { 4 };
        note(1, noted(1) + 1);
        note(2, noted(2) + panel_rows);
        note(3, us);
        note(4, noted(4).max(us));
        note(5, panel_rows);
        note(6, noted(6).wrapping_add(us));
        Message::result(0)
    })
}
