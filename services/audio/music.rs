//! Music playback: decodes the MP3 stream Radio writes into the shared ring
//! and hands 48 kHz stereo frames to the service's mixer. Also measures a
//! 16-band spectrum and the output peak for apps to draw.
use crate::protocol::*;

extern "C" {
    fn music_mp3_reset();
    fn music_mp3_decode(data: *const u8, size: i32, pcm: *mut i16, used: *mut i32, hz: *mut i32,
                        channels: *mut i32) -> i32;
}

fn word(i: usize) -> u32 {
    unsafe { ((STREAM_BASE + 4 * i) as *const u32).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { ((STREAM_BASE + 4 * i) as *mut u32).write_volatile(v) }
}
fn fence() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}
fn now() -> u32 {
    unsafe { ticks() }
}

/// Bytes per second assumed for seeking before the first frame is decoded
/// (the library's 128 kbps).
const DEFAULT_BYTE_RATE: u32 = 16_000;
/// Start (or resume after running dry) once this much is buffered.
const START_BYTES: u32 = 48 << 10;
const RESUME_BYTES: u32 = 24 << 10;
/// Radio silent this long after a request means there is no network.
const OFFLINE_MS: u32 = 4000;
const STAGE: usize = 4096;

/// Two-rate Goertzel bank: ten bands on the signal decimated to 12 kHz and six
/// on the full 48 kHz signal, both over 256 samples. Q14 coefficients.
const LOW: [i32; 10] = [32758, 32729, 32679, 32610, 32413, 31972, 31114, 29269, 25330, 16846];
const HIGH: [i32; 6] = [30274, 28106, 24279, 18205, 7962, -6393];

#[derive(Default, Clone, Copy)]
struct Goertzel {
    s1: i64,
    s2: i64,
}
impl Goertzel {
    fn push(&mut self, x: i64, c: i32) {
        let s = x + ((c as i64 * self.s1) >> 14) - self.s2;
        self.s2 = self.s1;
        self.s1 = s;
    }
    fn power(&mut self, c: i32) -> u64 {
        let p = self.s1 * self.s1 + self.s2 * self.s2 - ((c as i64 * self.s1 >> 14) * self.s2);
        *self = Self::default();
        p.max(0) as u64
    }
}

/// Roughly 8 * log2(x): a cheap decibel-like scale.
fn log_level(x: u64) -> u32 {
    if x == 0 {
        return 0;
    }
    let bits = 63 - x.leading_zeros();
    let frac = if bits >= 3 { (x >> (bits - 3)) & 7 } else { (x << (3 - bits)) & 7 } as u32;
    bits * 8 + frac
}

struct Spectrum {
    low: [Goertzel; 10],
    high: [Goertzel; 6],
    decimate: (i32, u32),
    n_low: u32,
    n_high: u32,
    bands: [u8; MUSIC_BANDS],
    peak: u32,
    shown_peak: u8,
}
impl Spectrum {
    const fn new() -> Self {
        Self {
            low: [Goertzel { s1: 0, s2: 0 }; 10],
            high: [Goertzel { s1: 0, s2: 0 }; 6],
            decimate: (0, 0),
            n_low: 0,
            n_high: 0,
            bands: [0; MUSIC_BANDS],
            peak: 0,
            shown_peak: 0,
        }
    }
    /// Map a band power to 0..=255 and let it fall slowly.
    fn show(&mut self, band: usize, power: u64) {
        // 256 samples of full-scale sine reach about 2^44 in power.
        let level = log_level(power).saturating_sub(18 * 8) * 255 / (26 * 8);
        let level = level.min(255) as u8;
        let old = self.bands[band];
        self.bands[band] = if level >= old { level } else { old - ((old - level) / 6).max(1).min(old - level) };
    }
    fn push(&mut self, mono: i32) {
        self.peak = self.peak.max(mono.unsigned_abs());
        for (g, &c) in self.high.iter_mut().zip(&HIGH) {
            g.push(mono as i64, c);
        }
        self.n_high += 1;
        if self.n_high == 256 {
            self.n_high = 0;
            for i in 0..6 {
                let p = self.high[i].power(HIGH[i]);
                self.show(10 + i, p);
            }
            let level = (self.peak.min(32767) * 255 / 32767) as u8;
            self.shown_peak = level.max(self.shown_peak.saturating_sub(12));
            self.peak = 0;
        }
        self.decimate.0 += mono;
        self.decimate.1 += 1;
        if self.decimate.1 == 4 {
            let x = (self.decimate.0 / 4) as i64;
            self.decimate = (0, 0);
            for (g, &c) in self.low.iter_mut().zip(&LOW) {
                g.push(x, c);
            }
            self.n_low += 1;
            if self.n_low == 256 {
                self.n_low = 0;
                for i in 0..10 {
                    let p = self.low[i].power(LOW[i]);
                    self.show(i, p);
                }
            }
        }
    }
    fn fade(&mut self) {
        for b in self.bands.iter_mut() {
            *b = b.saturating_sub(6);
        }
        self.shown_peak = self.shown_peak.saturating_sub(12);
    }
}

pub struct Music {
    track: u32,
    state: u32,
    paused: bool,
    generation: u32,
    requested_at: u32,
    start_byte: u32,
    start_ms: u32,
    played: u64,
    byte_rate: u32,
    stage: [u8; STAGE],
    staged: usize,
    pcm: [i16; 1152 * 2],
    pcm_len: usize,
    pcm_at: usize,
    spectrum: Spectrum,
    /// Decoding has finished the whole file.
    exhausted: bool,
    mode: u32,
}

impl Music {
    pub const fn new() -> Self {
        Self {
            track: 0,
            state: MUSIC_STATE_STOPPED,
            paused: false,
            generation: 0,
            requested_at: 0,
            start_byte: 0,
            start_ms: 0,
            played: 0,
            byte_rate: DEFAULT_BYTE_RATE,
            stage: [0; STAGE],
            staged: 0,
            pcm: [0; 1152 * 2],
            pcm_len: 0,
            pcm_at: 0,
            spectrum: Spectrum::new(),
            exhausted: false,
            mode: 0,
        }
    }
    pub fn init(&mut self) {
        for i in 1..16 {
            set(i, 0);
        }
        fence();
        set(0, STREAM_MAGIC);
    }
    /// The music wants the output running: playing or waiting for data.
    pub fn active(&self) -> bool {
        self.track != 0 && !self.paused && matches!(self.state, MUSIC_STATE_PLAYING | MUSIC_STATE_BUFFERING | MUSIC_STATE_OFFLINE)
    }
    fn request(&mut self, track: u32, seconds: u32) {
        let total = if track == self.track { word(STREAM_TOTAL) } else { 0 };
        let mut byte = seconds.saturating_mul(self.byte_rate);
        if total != 0 && byte >= total {
            byte = total.saturating_sub(1);
        }
        self.track = track;
        self.start_byte = byte;
        self.start_ms = (byte as u64 * 1000 / self.byte_rate.max(1) as u64) as u32;
        self.played = 0;
        self.staged = 0;
        self.pcm_len = 0;
        self.pcm_at = 0;
        self.exhausted = false;
        self.state = MUSIC_STATE_BUFFERING;
        self.generation = self.generation.wrapping_add(1);
        self.requested_at = now();
        unsafe { music_mp3_reset() };
        set(STREAM_TRACK, track);
        set(STREAM_START, byte);
        set(STREAM_CONSUMED, 0);
        fence();
        set(STREAM_GENERATION, self.generation);
    }
    pub fn command(&mut self, command: u32, arg: u32) -> bool {
        match command {
            MUSIC_PLAY if (1..=MUSIC_TRACKS).contains(&(arg & 0xff)) => {
                if arg & 0xff != self.track {
                    self.byte_rate = DEFAULT_BYTE_RATE;
                }
                self.paused = false;
                self.request(arg & 0xff, arg >> 8);
            }
            MUSIC_SEEK if self.track != 0 => {
                let paused = self.paused;
                self.request(self.track, arg);
                self.paused = paused;
            }
            MUSIC_PAUSE if self.track != 0 => self.paused = true,
            MUSIC_RESUME if self.track != 0 => {
                self.paused = false;
                if self.state == MUSIC_STATE_ENDED {
                    self.request(self.track, 0);
                }
            }
            MUSIC_MODE if arg & !(MUSIC_ADVANCE | MUSIC_REPEAT | MUSIC_SHUFFLE) == 0 => self.mode = arg,
            MUSIC_STOP => {
                self.track = 0;
                self.paused = false;
                self.state = MUSIC_STATE_STOPPED;
                set(STREAM_TRACK, 0);
                fence();
                self.generation = self.generation.wrapping_add(1);
                set(STREAM_GENERATION, self.generation);
            }
            _ => return false,
        }
        true
    }
    fn ours(&self) -> bool {
        word(STREAM_FILLED_GENERATION) == self.generation
    }
    fn available(&self) -> u32 {
        if !self.ours() {
            return 0;
        }
        word(STREAM_WRITTEN).wrapping_sub(word(STREAM_CONSUMED))
    }
    fn complete(&self) -> bool {
        self.ours() && word(STREAM_STATE) == STREAM_STATE_COMPLETE
    }
    /// Move bytes from the ring into the linear staging buffer.
    fn refill(&mut self) {
        let mut n = (self.available() as usize).min(STAGE - self.staged);
        let mut consumed = word(STREAM_CONSUMED);
        while n > 0 {
            let at = (consumed % STREAM_RING) as usize;
            let run = n.min(STREAM_RING as usize - at);
            let src = unsafe { core::slice::from_raw_parts((STREAM_DATA + at) as *const u8, run) };
            self.stage[self.staged..self.staged + run].copy_from_slice(src);
            self.staged += run;
            consumed = consumed.wrapping_add(run as u32);
            n -= run;
        }
        fence();
        set(STREAM_CONSUMED, consumed);
    }
    /// Decode the next frame into `pcm`. False when more data is needed.
    fn decode(&mut self) -> bool {
        loop {
            if self.staged < 2048 {
                self.refill();
            }
            if self.staged == 0 {
                return false;
            }
            let (mut used, mut hz, mut channels) = (0i32, 0i32, 0i32);
            let frames = unsafe {
                music_mp3_decode(self.stage.as_ptr(), self.staged as i32, self.pcm.as_mut_ptr(), &mut used,
                                 &mut hz, &mut channels)
            };
            let used = (used.max(0) as usize).min(self.staged);
            if frames == 0 && used == 0 {
                // Needs more bytes; at the very end the rest is padding.
                if self.complete() && self.available() == 0 {
                    self.staged = 0;
                }
                return false;
            }
            self.stage.copy_within(used..self.staged, 0);
            self.staged -= used;
            if frames > 0 && hz == 48000 {
                if used > 0 {
                    // Frame size times frames per second: the stream's byte rate.
                    self.byte_rate = (used as u32 * 48000 / frames as u32).max(1000);
                }
                self.pcm_len = frames as usize;
                self.pcm_at = 0;
                return true;
            }
        }
    }
    /// Track the stream's state before mixing.
    pub fn poll(&mut self) {
        if self.track != 0 && self.state == MUSIC_STATE_ENDED && !self.paused && self.mode & MUSIC_ADVANCE != 0 {
            let next = if self.mode & MUSIC_SHUFFLE != 0 {
                // Any other track, picked from the timer's low bits.
                (self.track + 1 + now() % (MUSIC_TRACKS - 1) - 1) % MUSIC_TRACKS + 1
            } else if self.track < MUSIC_TRACKS {
                self.track + 1
            } else if self.mode & MUSIC_REPEAT != 0 {
                1
            } else {
                0
            };
            if next != 0 {
                self.byte_rate = DEFAULT_BYTE_RATE;
                self.request(next, 0);
            }
        }
        if self.track == 0 || self.state == MUSIC_STATE_ENDED {
            self.spectrum.fade();
            return;
        }
        if self.paused {
            self.spectrum.fade();
            return;
        }
        let silent_radio = !self.ours() && now().wrapping_sub(self.requested_at) > OFFLINE_MS * unsafe { ticks_per_ms };
        let offline = silent_radio || (self.ours() && word(STREAM_STATE) == STREAM_STATE_OFFLINE);
        if self.state != MUSIC_STATE_PLAYING {
            let want = if self.played == 0 { START_BYTES } else { RESUME_BYTES };
            let ready = self.available() + self.staged as u32 >= want || self.complete();
            self.state = if ready && self.ours() {
                MUSIC_STATE_PLAYING
            } else if offline || (self.ours() && word(STREAM_STATE) == STREAM_STATE_RETRYING) {
                MUSIC_STATE_OFFLINE
            } else {
                MUSIC_STATE_BUFFERING
            };
        }
        if self.state != MUSIC_STATE_PLAYING {
            self.spectrum.fade();
        }
    }
    /// One stereo frame for the mixer, or None when nothing is playing.
    pub fn next(&mut self) -> Option<[i16; 2]> {
        if self.state != MUSIC_STATE_PLAYING || self.paused {
            return None;
        }
        if self.pcm_at == self.pcm_len && !self.decode() {
            if self.complete() && self.available() == 0 && self.staged == 0 {
                self.state = MUSIC_STATE_ENDED;
                self.exhausted = true;
            } else {
                self.state = MUSIC_STATE_BUFFERING;
            }
            return None;
        }
        let frame = [self.pcm[2 * self.pcm_at], self.pcm[2 * self.pcm_at + 1]];
        self.pcm_at += 1;
        self.played += 1;
        self.spectrum.push((frame[0] as i32 + frame[1] as i32) / 2);
        Some(frame)
    }
    /// Radio's join diagnostics from the stream page's spare words 16..32.
    pub fn radio_words(&self) -> [u32; 16] {
        core::array::from_fn(|i| word(16 + i))
    }
    /// Six words for the handset's USB status page.
    pub fn debug_words(&self) -> [u32; 6] {
        let state = if self.paused { MUSIC_STATE_PAUSED } else { self.state };
        let position = self.start_ms.wrapping_add((self.played / 48) as u32);
        [
            self.track | state << 8 | (self.spectrum.shown_peak as u32) << 16,
            position,
            word(STREAM_WRITTEN) | word(STREAM_RELINKS) << 24,
            word(STREAM_TOTAL),
            word(STREAM_HTTP) | word(STREAM_FETCHES) << 16,
            word(STREAM_FAILURES) | word(STREAM_STATE) << 8 | word(STREAM_LAST_MS) << 16,
        ]
    }
    pub fn status(&self) -> Message {
        let mut r = Message::result(self.track);
        let position = self.start_ms.wrapping_add((self.played / 48) as u32);
        let state = if self.paused { MUSIC_STATE_PAUSED } else { self.state };
        r.arg = state | position << 4;
        let mut text = [0u8; 40];
        text[..MUSIC_BANDS].copy_from_slice(&self.spectrum.bands);
        let ahead = (self.available() + self.staged as u32) as u64 * 10 / self.byte_rate.max(1) as u64;
        text[16..18].copy_from_slice(&(ahead.min(0xffff) as u16).to_le_bytes());
        text[18] = self.spectrum.shown_peak;
        let total = if self.ours() { word(STREAM_TOTAL) } else { 0 };
        let length = (total as u64 * 1000 / self.byte_rate.max(1) as u64) as u32;
        text[20..24].copy_from_slice(&length.to_le_bytes());
        r.put_text(&text);
        r
    }
}
