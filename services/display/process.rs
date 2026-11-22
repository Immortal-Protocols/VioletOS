use crate::protocol::*;
// Private 240x400 app-owned drawing surfaces. Only display has scanout access.
// The argument identifies a dirty band, allowing a digit update to skip the rest.
pub fn run() -> ! {
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
        Message::result(0)
    })
}
