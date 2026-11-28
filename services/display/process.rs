//! The Display process: the system compositor (system/compositor/README.md)
//! over the real framebuffer. Built-in apps draw into private 240x400
//! surfaces; only this process maps both them and scanout. A present names a
//! band of changed rows, so a digit update skips the rest.
use crate::protocol::*;
#[path = "../../system/compositor/blit.rs"]
mod blit;
#[path = "../../system/compositor/display.rs"]
mod display;
#[path = "../../system/compositor/scene.rs"]
mod scene;
use display::{Compositor, Hardware, CLASSIC_H, CLASSIC_W, PIXELS, TAIL_WORDS};

/// Scanout, and each built-in app's surface, where the supervisor maps them.
const SCANOUT: usize = 0x0800_0000;
const SURFACES: usize = 0x0600_0000;
const SURFACE_STEP: usize = 0x10_0000;

struct Board;
impl Hardware for Board {
    fn classic_row(&mut self, client: u32, y: usize, out: &mut [u32; CLASSIC_W]) {
        // The app may be drawing: read each pixel once, as it is now.
        let row = (SURFACES + (client as usize - 1) * SURFACE_STEP + y * CLASSIC_W * 4) as *const u32;
        for (x, p) in out.iter_mut().enumerate() {
            *p = unsafe { row.add(x).read_volatile() };
        }
    }
    fn scanout(&mut self) -> &mut [u32] {
        unsafe { core::slice::from_raw_parts_mut(SCANOUT as *mut u32, PIXELS + TAIL_WORDS) }
    }
    fn ticks(&mut self) -> u32 {
        unsafe { ticks() }
    }
    fn ticks_per_ms(&mut self) -> u32 {
        unsafe { ticks_per_ms }
    }
}

pub fn run() -> ! {
    let mut compositor = Compositor::new();
    serve(|m| {
        if m.op == EXECUTE && m.key == POWER {
            compositor.clear(&mut Board);
            return Message::result(0);
        }
        if m.op != EXECUTE || m.key != PRESENT || !is_app(m.value) {
            return Message::error(INVALID);
        }
        // Rows first..end of the app's surface, as the app counts them.
        let (first, end) = (m.arg >> 16, m.arg & 0xffff);
        if first >= end || end > CLASSIC_H as u32 {
            return Message::error(INVALID);
        }
        match compositor.present_classic(&mut Board, m.value, first, end) {
            Ok(_) => Message::result(0),
            Err(_) => Message::error(INVALID),
        }
    })
}
