//! QEMU sound card fixture. The page at guest 0x49204000 is shared with the Mac:
//! the viewer writes button and jack state and plays the PCM ring through SDL.
//!
//! Words: 0 buttons (host, bit 0 volume up, bit 1 volume down), 1 jack (host,
//! bit 0 plugged), 2 magic, 3 output (bit 0 on, bit 1 headphones), 4 volume
//! level, 5 frames written in total, 6 sample rate, 7 ring frames, 8 playback
//! serial, 9 sound playing, 10 peak sample since output started, 11 frames
//! written since output started. The ring holds 48 kHz interleaved stereo s16.
//!
//! Microphone: 12 capture on (guest, bit 0), 13 samples captured in total
//! (host), 14 capture ring samples (guest), 15 the service's MICROPHONE arg,
//! 16 its level. The host writes 16 kHz mono s16 into a two-page ring at guest
//! 0x4920d000, which Audio maps at 0x09009000.
use crate::protocol::*;

pub struct Inputs {
    pub up: bool,
    pub down: bool,
    pub headphones: bool,
}
const CONTROL: *mut u32 = 0x09000000 as *mut u32;
const RING: *mut [i16; 2] = 0x09001000 as *mut [i16; 2];
const RING_FRAMES: u32 = 8 * 4096 / 4;
const MIC: *const i16 = 0x09009000 as *const i16;
const MIC_SAMPLES: u32 = 2 * 4096 / 2;
const MAGIC: u32 = 0x31445541; // "AUD1"
// Keep about 40 ms queued ahead of the host so scheduling jitter is inaudible.
const LEAD_FRAMES: u64 = 48 * 40;

fn reg(i: usize) -> u32 {
    unsafe { CONTROL.add(i).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { CONTROL.add(i).write_volatile(v) }
}
static mut STARTED: u32 = 0;
static mut WRITTEN: u64 = 0;
static mut PEAK: u32 = 0;
static mut MIC_RD: u32 = 0;

pub fn init() -> u32 {
    set(3, 0);
    set(5, 0);
    set(6, 48000);
    set(7, RING_FRAMES);
    set(10, 0);
    set(11, 0);
    set(12, 0);
    set(14, MIC_SAMPLES);
    set(15, 0);
    set(16, 0);
    set(2, MAGIC);
    AUDIO_SPEAKER_OUT | AUDIO_HEADPHONE_OUT | AUDIO_BUTTONS | AUDIO_JACK | AUDIO_MIC
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
        STARTED = ticks();
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
    let elapsed = u64::from(unsafe { ticks().wrapping_sub(STARTED) });
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
/// Ask the host for microphone samples, starting from what it writes next.
pub fn mic_start() -> bool {
    unsafe { MIC_RD = reg(13) };
    set(12, 1);
    true
}
pub fn mic_stop() {
    set(12, 0);
}
pub fn mic_read(out: &mut [i16]) -> usize {
    if reg(12) & 1 == 0 {
        return 0;
    }
    let written = reg(13);
    core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
    unsafe {
        // Fell a ring behind (the host ran ahead): skip to the newest half.
        if written.wrapping_sub(MIC_RD) > MIC_SAMPLES {
            MIC_RD = written.wrapping_sub(MIC_SAMPLES / 2);
        }
        let n = (written.wrapping_sub(MIC_RD) as usize).min(out.len());
        for s in out[..n].iter_mut() {
            *s = MIC.add((MIC_RD % MIC_SAMPLES) as usize).read_volatile();
            MIC_RD = MIC_RD.wrapping_add(1);
        }
        n
    }
}
pub fn publish_mic(state: u32, level: u32) {
    set(15, state);
    set(16, level);
}
/// The emulator has no developer link; the viewer drives the card directly.
pub fn request() -> Option<u32> {
    None
}
pub fn debug(_command: u32, _param: u32) {}
