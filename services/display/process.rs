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
        Message::result(0)
    })
}
