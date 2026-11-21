// App-local logical surface. The app cannot map scanout or its peer's surface.
include!(env!("VIOLET_GLYPHS_RS"));
pub const BG: u32 = 0xff191620;
pub const PANEL: u32 = 0xff302a39;
pub const INK: u32 = 0xffeeeaf8;
pub const ACCENT: u32 = 0xffd1b0fe;
use core::sync::atomic::{AtomicPtr, Ordering};
static SURFACE: AtomicPtr<u32> = AtomicPtr::new(core::ptr::null_mut());
pub fn acquire_surface() {
    let r = crate::sdk::request(crate::protocol::GET, crate::protocol::SURFACE, 0);
    assert!(r.op == crate::protocol::OK && r.value != 0);
    SURFACE.store(r.value as *mut u32, Ordering::Relaxed);
}
pub fn rect(x: usize, y: usize, w: usize, h: usize, c: u32) {
    let surface = SURFACE.load(Ordering::Relaxed);
    for row in y..(y + h).min(400) {
        for col in x..(x + w).min(240) {
            unsafe {
                surface.add(row * 240 + col).write(c);
            }
        }
    }
}
pub fn label(x: usize, y: usize, text: &[u8], scale: usize, c: u32) {
    for (n, ch) in text.iter().take_while(|&&b| b != 0).enumerate() {
        let Some(k) = ALPHABET.iter().position(|b| b == ch) else {
            continue;
        };
        for (col, &bits) in GLYPHS[k].iter().enumerate() {
            for bit in 0..7 {
                if bits & (1 << bit) != 0 {
                    rect(x + (6 * n + col) * scale, y + bit * scale, scale, scale, c);
                }
            }
        }
    }
}
pub fn button(y: usize, name: &[u8]) {
    rect(14, y, 212, 50, PANEL);
    label(26, y + 18, name, 2, INK);
}
pub fn header(status: &[u8], disk: bool) {
    rect(0, 0, 240, 400, BG);
    label(14, 20, b"VIOLET OS", 1, ACCENT);
    label(14, 32, status, 1, INK);
    label(
        14,
        43,
        if disk { b"DISK READY" } else { b"RAM ONLY" },
        1,
        INK,
    );
}
pub fn present(first: u32, end: u32) {
    crate::sdk::request(
        crate::protocol::EXECUTE,
        crate::protocol::PRESENT,
        (first << 16) | end,
    );
}
