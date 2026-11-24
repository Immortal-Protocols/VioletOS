use crate::protocol::*;
// Private 240x400 app-owned drawing surfaces. Only display has scanout access.
// The argument identifies a dirty band, allowing a digit update to skip the rest.
pub fn run() -> ! {
    serve(|m| {
        if m.op == EXECUTE && m.key == POWER {
            unsafe {
                core::ptr::write_bytes(0x08000000 as *mut u32, 0, 720 * 1600);
            }
            mark_dirty(0, 1600);
            return Message::result(0);
        }
        if m.op != EXECUTE || m.key != PRESENT || !is_app(m.value) {
            return Message::error(INVALID);
        }
        let first = (m.arg >> 16) as usize;
        let end = (m.arg & 0xffff) as usize;
        if first >= end || end > 400 {
            return Message::error(INVALID);
        }
        let src = (0x06000000 + (m.value as usize - 1) * 0x100000) as *const u32;
        let dst = 0x08000000 as *mut u32;
        for y in first..end {
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
        mark_dirty(4 * first as u32, 4 * end as u32);
        Message::result(0)
    })
}

/// Tell the Mac viewer which scanout rows changed, in the words after the
/// frame (the same record the Open Runtime keeps: runtime/open/scanout.rs).
fn mark_dirty(y0: u32, y1: u32) {
    let dirty = (0x08000000 + 720 * 1600 * 4) as *mut u32;
    unsafe {
        let (seq, seen) = (dirty.add(3).read_volatile(), dirty.add(4).read_volatile());
        let (mut a, mut b) = (y0, y1);
        if dirty.read_volatile() == 0x5452_4944 && seen != seq {
            a = a.min(dirty.add(1).read_volatile());
            b = b.max(dirty.add(2).read_volatile());
        }
        dirty.add(1).write_volatile(a);
        dirty.add(2).write_volatile(b.min(1600));
        dirty.write_volatile(0x5452_4944);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        dirty.add(3).write_volatile(seq.wrapping_add(1));
    }
}
