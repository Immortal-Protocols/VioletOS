//! Music stream filler, shared by the QEMU and handset Radio. Audio names the
//! track and start offset in the stream control page; Radio fetches the bytes
//! with HTTPS range requests and appends them to the ring as Audio frees room.
//! Radio only ever fetches `/tracks/NN.mp3` from MUSIC_HOST.
use crate::protocol::*;

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
fn ms(n: u32) -> u32 {
    n.wrapping_mul(unsafe { ticks_per_ms })
}

/// The result of one range read: bytes read and the file's total size (0 if
/// unknown), or the HTTP status (0 when nothing answered).
pub type Fetch<'a> = &'a mut dyn FnMut(&[u8], u32, u32, &mut [u8]) -> Result<(usize, u32), u32>;

/// Count a network recovery (shown to USB diagnostics through Audio).
pub fn relinked() {
    set(STREAM_RELINKS, word(STREAM_RELINKS) + 1);
}

/// Don't start a fetch for less than this unless it finishes the file.
const MIN_FETCH: u32 = 32 << 10;

#[derive(Default)]
pub struct Streamer {
    /// (tick of the failure, ticks to wait) before the next attempt.
    retry: Option<(u32, u32)>,
    /// Fetches in a row that got no answer at all.
    pub silent: u32,
}
impl Streamer {
    /// True when Audio wants bytes that Radio could fetch now.
    pub fn wants(&self) -> bool {
        word(0) == STREAM_MAGIC
            && word(STREAM_TRACK) != 0
            && (word(STREAM_GENERATION) != word(STREAM_FILLED_GENERATION)
                || word(STREAM_STATE) != STREAM_STATE_COMPLETE)
    }
    /// Do at most one range request. `online` says whether a network is up.
    pub fn step(&mut self, online: bool, fetch: Fetch) {
        if word(0) != STREAM_MAGIC {
            return;
        }
        let generation = word(STREAM_GENERATION);
        if generation != word(STREAM_FILLED_GENERATION) {
            set(STREAM_WRITTEN, 0);
            set(STREAM_TOTAL, 0);
            set(STREAM_STATE, STREAM_STATE_IDLE);
            fence();
            set(STREAM_FILLED_GENERATION, generation);
            self.retry = None;
        }
        let track = word(STREAM_TRACK);
        let mut path = [0u8; 16];
        if !music_path(track, &mut path) {
            set(STREAM_STATE, STREAM_STATE_IDLE);
            return;
        }
        let (start, written, total) = (word(STREAM_START), word(STREAM_WRITTEN), word(STREAM_TOTAL));
        let from = start.wrapping_add(written);
        if total != 0 && from >= total {
            set(STREAM_STATE, STREAM_STATE_COMPLETE);
            return;
        }
        if !online {
            set(STREAM_STATE, STREAM_STATE_OFFLINE);
            return;
        }
        if let Some((at, wait)) = self.retry {
            if now().wrapping_sub(at) < wait {
                return;
            }
        }
        let buffered = written.wrapping_sub(word(STREAM_CONSUMED)).min(STREAM_RING);
        let at = written % STREAM_RING;
        let left = if total != 0 { total - from } else { u32::MAX };
        let n = STREAM_CHUNK.min(STREAM_RING - buffered).min(STREAM_RING - at).min(left);
        if n < MIN_FETCH.min(left) && !(n > 0 && at + n == STREAM_RING) {
            return;
        }
        set(STREAM_STATE, STREAM_STATE_FETCHING);
        let began = now();
        let ring = unsafe { core::slice::from_raw_parts_mut((STREAM_DATA + at as usize) as *mut u8, n as usize) };
        let result = fetch(&path, from, from + n - 1, ring);
        set(STREAM_LAST_MS, now().wrapping_sub(began) / unsafe { ticks_per_ms }.max(1));
        if word(STREAM_GENERATION) != generation {
            // Audio moved on while this was in flight; the next step resets.
            return;
        }
        match result {
            Ok((got, size)) => {
                if size != 0 {
                    set(STREAM_TOTAL, size);
                }
                fence();
                set(STREAM_WRITTEN, written + got as u32);
                set(STREAM_FETCHES, word(STREAM_FETCHES) + 1);
                set(STREAM_HTTP, 206);
                self.retry = None;
                self.silent = 0;
                let done = (size != 0 && from + got as u32 >= size) || got == 0;
                set(STREAM_STATE, if done { STREAM_STATE_COMPLETE } else { STREAM_STATE_IDLE });
            }
            Err(416) if total == 0 && from > 0 => {
                // Asked past the end of a file whose size was never learned.
                set(STREAM_TOTAL, from);
                set(STREAM_STATE, STREAM_STATE_COMPLETE);
            }
            Err(status) => {
                set(STREAM_HTTP, status);
                set(STREAM_FAILURES, word(STREAM_FAILURES) + 1);
                set(STREAM_STATE, if status == 0 { STREAM_STATE_OFFLINE } else { STREAM_STATE_RETRYING });
                self.silent = if status == 0 { self.silent + 1 } else { 0 };
                // A sleeping library answers 503; give it longer.
                self.retry = Some((now(), ms(if status == 503 { 4000 } else { 1500 })));
            }
        }
    }
}
