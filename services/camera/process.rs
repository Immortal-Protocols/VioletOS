//! Camera service: the image sensors. Only the Open Runtime uses it, through
//! one page the supervisor shares between them (0x09320000 here, 0x09120000
//! there), and a 4 MiB frame window (0x0b000000 in both; read-only there).
//!
//! The Open Runtime writes `W_WANT` while an app in front has
//! /dev/violet/camera open. Camera then starts the sensor and keeps three
//! frame slots in the window filled in turn. After a frame completes it
//! publishes that slot's offset, then counts it in `W_FRAMES`. A reader that
//! sees `W_FRAMES` advance by at most one while it copies has a whole frame:
//! the slot it copies is not written again for two more frames.
//!
//! Frames are raw Bayer, as the sensor delivers them. Each board's backend
//! (targets/*/camera.rs) supplies the same functions; this file keeps the
//! state, the shared page, auto-exposure and the developer requests.
use crate::{camera_backend as board, protocol::*};

pub mod shared {
    pub const PAGE: *mut u32 = 0x0932_0000 as *mut u32;
    pub const MAGIC: u32 = 0x314d_4143; // "CAM1"
    /// Written by the Open Runtime: 1 while an app wants frames.
    pub const W_WANT: usize = 0;
    /// Written by Camera.
    pub const W_MAGIC: usize = 8;
    pub const W_STATE: usize = 9;
    pub const W_FRAMES: usize = 10;
    pub const W_OFFSET: usize = 11;
    pub const W_WIDTH: usize = 12;
    pub const W_HEIGHT: usize = 13;
    pub const W_STRIDE: usize = 14;
    pub const W_FORMAT: usize = 15;
    pub const W_ERROR: usize = 16;
    pub const W_SENSOR: usize = 17;
    pub const W_LEVEL: usize = 18;
    pub fn get(i: usize) -> u32 {
        unsafe { PAGE.add(i).read_volatile() }
    }
    pub fn set(i: usize, v: u32) {
        unsafe { PAGE.add(i).write_volatile(v) }
    }
}
pub const STATE_OFF: u32 = 0;
pub const STATE_STARTING: u32 = 1;
pub const STATE_STREAMING: u32 = 2;
pub const STATE_FAILED: u32 = 3;
/// `W_FORMAT`: the low byte is the packing, the next the Bayer order of the
/// top-left 2 x 2 pixels.
pub const FORMAT_RAW8: u32 = 1; // one byte per pixel
pub const FORMAT_RAW10: u32 = 2; // MIPI RAW10: four pixels' high bytes, then their low bits
pub const BAYER_RGGB: u32 = 0 << 8;
pub const BAYER_GRBG: u32 = 1 << 8;
pub const BAYER_GBRG: u32 = 2 << 8;
pub const BAYER_BGGR: u32 = 3 << 8;

pub const WINDOW: usize = 0x0b00_0000;
pub const WINDOW_BYTES: usize = 0x40_0000;
pub const SLOTS: usize = 3;

/// What a started sensor delivers.
#[derive(Clone, Copy)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub sensor: u32,
}
impl Format {
    pub fn slot_bytes(&self) -> usize {
        ((self.stride * self.height) as usize + 0xfff) & !0xfff
    }
    pub fn slot(&self, i: usize) -> usize {
        i * self.slot_bytes()
    }
}

/// Auto-exposure: aim the frame's mean at a quarter of full scale (a mid grey
/// once the app's gamma brightens it), stepping exposure by about 12% at a time
/// so the picture settles in about a second without hunting.
struct Exposure {
    level: u32,
    every: u32,
}
const LEVEL_MAX: u32 = 255;
const TARGET: u32 = 64;
impl Exposure {
    fn update(&mut self, mean: u32) -> Option<u32> {
        self.every += 1;
        if self.every < 3 {
            return None;
        }
        self.every = 0;
        let next = if mean < TARGET * 7 / 8 {
            (self.level + self.level / 8 + 1).min(LEVEL_MAX)
        } else if mean > TARGET * 9 / 8 {
            self.level.saturating_sub(self.level / 8 + 1)
        } else {
            self.level
        };
        (next != self.level).then(|| {
            self.level = next;
            next
        })
    }
}

/// The mean of the frame's high bytes over a sparse grid.
fn mean(f: &Format, offset: usize) -> u32 {
    let base = (WINDOW + offset) as *const u8;
    let (mut sum, mut n) = (0u32, 0u32);
    let step_x = (f.stride / 37).max(1) as usize;
    let mut y = 0;
    while y < f.height {
        let row = unsafe { base.add((y * f.stride) as usize) };
        let mut x = 0;
        while x < f.stride as usize {
            // In MIPI RAW10 every fifth byte holds low bits; skip it.
            if f.format & 0xff != FORMAT_RAW10 || x % 5 != 4 {
                sum += u32::from(unsafe { row.add(x).read_volatile() });
                n += 1;
            }
            x += step_x;
        }
        y += (f.height / 29).max(1);
    }
    if n == 0 { 0 } else { sum / n }
}

struct Camera {
    state: u32,
    format: Option<Format>,
    next: usize,
    frames: u32,
    exposure: Exposure,
    /// Streaming because of a developer request rather than an app.
    bench: bool,
}
impl Camera {
    fn publish(&self) {
        shared::set(shared::W_STATE, self.state);
        shared::set(shared::W_LEVEL, self.exposure.level);
        board::note_state(self.state, self.frames);
    }
    fn start(&mut self) {
        if self.format.is_some() {
            return;
        }
        self.state = STATE_STARTING;
        self.publish();
        match board::start(self.exposure.level) {
            Ok(f) => {
                shared::set(shared::W_WIDTH, f.width);
                shared::set(shared::W_HEIGHT, f.height);
                shared::set(shared::W_STRIDE, f.stride);
                shared::set(shared::W_FORMAT, f.format);
                shared::set(shared::W_SENSOR, f.sensor);
                shared::set(shared::W_ERROR, 0);
                self.format = Some(f);
                self.state = STATE_STREAMING;
                self.next = 0;
                board::arm(self.next * f.slot_bytes());
            }
            Err(e) => {
                shared::set(shared::W_ERROR, e);
                self.state = STATE_FAILED;
                board::stop();
            }
        }
        self.publish();
    }
    fn stop(&mut self) {
        if self.format.take().is_some() {
            board::stop();
        }
        self.bench = false;
        self.state = STATE_OFF;
        self.publish();
    }
    /// A slot finished: publish it, then aim the hardware at the next one.
    fn poll(&mut self) {
        let Some(f) = self.format else { return };
        if !board::frame_done() {
            return;
        }
        let done = self.next * f.slot_bytes();
        self.next = (self.next + 1) % SLOTS;
        board::arm(self.next * f.slot_bytes());
        // Readers map the window cacheable: drop stale lines of this slot.
        board::invalidate(done, (f.stride * f.height) as usize);
        shared::set(shared::W_OFFSET, done as u32);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        self.frames = self.frames.wrapping_add(1);
        shared::set(shared::W_FRAMES, self.frames);
        if let Some(level) = self.exposure.update(mean(&f, done)) {
            board::set_exposure(level);
        }
        board::preview(&f, done);
    }
    fn developer(&mut self, request: u32) {
        match request >> 24 {
            0x01 => {
                self.bench = true;
                self.start();
            }
            0x02 => self.stop(),
            0x03 => {
                self.exposure.level = request & 0xff;
                board::set_exposure(self.exposure.level);
            }
            _ => board::developer(request),
        }
    }
}

pub fn run() -> ! {
    board::init();
    shared::set(shared::W_FRAMES, 0);
    shared::set(shared::W_STATE, STATE_OFF);
    shared::set(shared::W_MAGIC, shared::MAGIC);
    let mut camera = Camera {
        state: STATE_OFF,
        format: None,
        next: 0,
        frames: 0,
        exposure: Exposure { level: 96, every: 0 },
        bench: false,
    };
    camera.publish();
    loop {
        if let Some(request) = board::request() {
            camera.developer(request);
        }
        let want = shared::get(shared::W_WANT) != 0;
        if want && camera.state == STATE_OFF {
            camera.start();
        } else if !want && !camera.bench && camera.state != STATE_OFF {
            camera.stop();
        }
        camera.poll();
        camera.publish();
        // Polling is cheap while streaming (one register read); idle, look rarely.
        unsafe { delay_ms(if camera.format.is_some() { 1 } else { 20 }) };
    }
}
