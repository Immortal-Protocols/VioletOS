//! QEMU sound card fixture. The page at guest 0x49204000 is shared with the Mac:
//! the viewer writes button and jack state and plays the PCM ring through SDL.
//!
//! Words: 0 buttons (host, bit 0 volume up, bit 1 volume down), 1 jack (host,
//! bit 0 plugged), 2 magic, 3 output (bit 0 on, bit 1 headphones), 4 volume
//! level, 5 frames written in total, 6 sample rate, 7 ring frames, 8 playback
//! serial, 9 sound playing, 10 peak sample since output started, 11 frames
//! written since output started. The ring holds 48 kHz interleaved stereo s16.
use crate::protocol::*;

pub struct Inputs {
    pub up: bool,
    pub down: bool,
    pub headphones: bool,
}
const CONTROL: *mut u32 = 0x09000000 as *mut u32;
const RING: *mut [i16; 2] = 0x09001000 as *mut [i16; 2];
const RING_FRAMES: u32 = 8 * 4096 / 4;
const MAGIC: u32 = 0x31445541; // "AUD1"
// Keep about 40 ms queued ahead of the host so scheduling jitter is inaudible.
const LEAD_FRAMES: u64 = 48 * 40;

fn reg(i: usize) -> u32 {
    unsafe { CONTROL.add(i).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { CONTROL.add(i).write_volatile(v) }
}
/// Ticks since output started, kept in 64 bits: the 32-bit counter wraps
/// every 68.7 s at QEMU's 62.5 MHz.
static mut LAST_TICK: u32 = 0;
static mut ELAPSED: u64 = 0;
static mut WRITTEN: u64 = 0;
static mut PEAK: u32 = 0;

pub fn init() -> u32 {
    set(3, 0);
    set(5, 0);
    set(6, 48000);
    set(7, RING_FRAMES);
    set(10, 0);
    set(11, 0);
    set(2, MAGIC);
    AUDIO_SPEAKER_OUT | AUDIO_HEADPHONE_OUT | AUDIO_BUTTONS | AUDIO_JACK
}
pub fn inputs() -> Inputs {
    let buttons = reg(0);
    Inputs { up: buttons & 1 != 0, down: buttons & 2 != 0, headphones: reg(1) & 1 != 0 }
}
pub fn set_volume(level: u32, _headphones: bool) {
    // The service scales samples. The level is published for the host.
    set(4, level);
}
pub fn start(headphones: bool) -> bool {
    unsafe {
        LAST_TICK = ticks();
        ELAPSED = 0;
        WRITTEN = 0;
        PEAK = 0;
    }
    set(10, 0);
    set(11, 0);
    set(3, 1 | (headphones as u32) << 1);
    true
}
pub fn route(headphones: bool) -> bool {
    set(3, 1 | (headphones as u32) << 1);
    true
}
pub fn stop() {
    set(3, reg(3) & 2);
}
/// Frames due now: real time since start plus the lead, less what was written.
pub fn writable() -> usize {
    let elapsed = unsafe {
        let now = ticks();
        ELAPSED += u64::from(now.wrapping_sub(LAST_TICK));
        LAST_TICK = now;
        ELAPSED
    };
    let due = elapsed * 48 / u64::from(unsafe { ticks_per_ms }.max(1)) + LEAD_FRAMES;
    let written = unsafe { WRITTEN };
    due.saturating_sub(written).min(u64::from(RING_FRAMES / 2)) as usize
}
pub fn write(frames: &[[i16; 2]]) {
    let mut total = reg(5);
    for f in frames {
        unsafe {
            RING.add((total % RING_FRAMES) as usize).write_volatile(*f);
            PEAK = PEAK.max(f[0].unsigned_abs() as u32);
        }
        total = total.wrapping_add(1);
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    set(5, total);
    unsafe {
        WRITTEN += frames.len() as u64;
        set(10, PEAK);
        set(11, WRITTEN as u32);
    }
}
/// The last sample has had time to reach the host.
pub fn drained() -> bool {
    writable() as u64 >= LEAD_FRAMES.min(u64::from(RING_FRAMES / 2))
}
pub fn publish(_level: u32, _flags: u32, serial: u32, sound: u32) {
    set(8, serial);
    set(9, sound);
}
/// The emulator has no developer link; the viewer drives the card directly.
pub fn request() -> Option<u32> {
    None
}
pub fn debug(_command: u32, _param: u32) {}
pub fn music(_words: [u32; 6]) {}
pub fn radio(_words: [u32; 16]) {}
