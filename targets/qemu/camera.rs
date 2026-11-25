//! QEMU camera: a synthetic sensor. It paints 640 x 480 RAW8 frames (BGGR) at
//! about 15 per second into the frame window: eight colour bars (white,
//! yellow, cyan, green, magenta, red, blue, black) with a white square that
//! crosses them once every four seconds, at a brightness that follows the
//! exposure level the service asks for. Tests can check the bars' colours on
//! screen and that the square moves.
use crate::process::{Format, BAYER_BGGR, FORMAT_RAW8, WINDOW};
use crate::protocol::*;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const FRAME_MS: u32 = 66;
const BARS: [(u8, u8, u8); 8] = [
    (235, 235, 235),
    (235, 235, 16),
    (16, 235, 235),
    (16, 235, 16),
    (235, 16, 235),
    (235, 16, 16),
    (16, 16, 235),
    (16, 16, 16),
];

static mut STATE: Sim = Sim { on: false, target: 0, due: 0, painted: 0, level: 96, start: 0 };
struct Sim {
    on: bool,
    target: usize,
    due: u32,
    painted: u32,
    level: u32,
    start: u32,
}
fn sim() -> &'static mut Sim {
    unsafe { &mut *core::ptr::addr_of_mut!(STATE) }
}
fn now_ms() -> u32 {
    unsafe { ticks() / ticks_per_ms }
}

pub fn init() {}
pub fn start(level: u32) -> Result<Format, u32> {
    let s = sim();
    s.on = true;
    s.level = level;
    s.start = now_ms();
    s.due = s.start.wrapping_add(FRAME_MS);
    Ok(Format { width: WIDTH, height: HEIGHT, stride: WIDTH, format: FORMAT_RAW8 | BAYER_BGGR, sensor: 0x5153 })
}
pub fn stop() {
    sim().on = false;
}
pub fn arm(offset: usize) {
    sim().target = offset;
}
/// True once per frame period, after painting the armed slot.
pub fn frame_done() -> bool {
    let s = sim();
    if !s.on || (now_ms().wrapping_sub(s.due) as i32) < 0 {
        return false;
    }
    s.due = s.due.wrapping_add(FRAME_MS);
    if (now_ms().wrapping_sub(s.due) as i32) > 0 {
        s.due = now_ms().wrapping_add(FRAME_MS);
    }
    paint(s.target, now_ms().wrapping_sub(s.start), s.level);
    s.painted = s.painted.wrapping_add(1);
    true
}
fn paint(offset: usize, ms: u32, level: u32) {
    let base = (WINDOW + offset) as *mut u8;
    // Brightness follows the exposure: level 96 shows the bars at about the
    // service's target mean.
    let gain = level.max(1) * 256 / 192;
    let scale = |v: u8| ((u32::from(v) * gain) >> 8).min(255) as u8;
    let side = 96u32;
    let travel = WIDTH - side;
    let phase = ms % 4000;
    let square_x = travel * phase / 4000;
    let square_y = (HEIGHT - side) / 2;
    let mut row = [0u8; WIDTH as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let inside = x >= square_x && x < square_x + side && y >= square_y && y < square_y + side;
            let (r, g, b) = if inside { (255, 255, 255) } else { BARS[(x * 8 / WIDTH) as usize] };
            let v = match (y & 1, x & 1) {
                (0, 0) => b,
                (1, 1) => r,
                _ => g,
            };
            row[x as usize] = scale(v);
        }
        unsafe { core::ptr::copy_nonoverlapping(row.as_ptr(), base.add((y * WIDTH) as usize), WIDTH as usize) };
    }
}
pub fn set_exposure(level: u32) {
    sim().level = level;
}
pub fn preview(_f: &Format, _offset: usize) {}
/// The CPU paints QEMU's frames, so there is nothing to invalidate.
pub fn invalidate(_offset: usize, _len: usize) {}
pub fn request() -> Option<u32> {
    None
}
pub fn developer(_request: u32) {}
pub fn note_state(_state: u32, _frames: u32) {}
