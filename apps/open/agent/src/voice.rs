//! The phone's half of a conversation: the microphone stream
//! (/dev/violet/mic, 16 kHz mono) and the spoken replies, which arrive as
//! 24 kHz mono and are played at 48 kHz through /dev/violet/audio.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 40 ms of microphone samples per message.
pub const MIC_CHUNK: usize = 640;

pub enum Mic {
    /// 16 kHz s16le samples, and the chunk's peak.
    Samples(Vec<u8>, u16),
    Failed(String),
}

/// Read the microphone while `on` is set; reopened after each pause (the
/// Open Runtime turns the microphone off whenever the agent leaves the front).
pub fn microphone(on: Arc<AtomicBool>, out: Sender<Mic>) {
    std::thread::spawn(move || loop {
        while !on.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut f = match File::open("/dev/violet/mic") {
            Ok(f) => f,
            Err(e) => {
                let _ = out.send(Mic::Failed(format!("microphone: {e}")));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let mut chunk = Vec::with_capacity(MIC_CHUNK * 2);
        let mut buf = [0u8; 2048];
        while on.load(Ordering::Relaxed) {
            let n = match f.read(&mut buf) {
                Ok(n) => n,
                Err(_) => break,
            };
            chunk.extend_from_slice(&buf[..n & !1]);
            while chunk.len() >= MIC_CHUNK * 2 {
                let rest = chunk.split_off(MIC_CHUNK * 2);
                let peak = chunk.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs()).max().unwrap_or(0);
                if out.send(Mic::Samples(std::mem::replace(&mut chunk, rest), peak)).is_err() {
                    return;
                }
            }
        }
    });
}

/// G.711 µ-law: 16 kHz speech in 8 bits a sample, half the bytes, fine for recognition.
pub fn mulaw(pcm: &[u8]) -> Vec<u8> {
    pcm.chunks_exact(2)
        .map(|b| {
            let v = i16::from_le_bytes([b[0], b[1]]) as i32;
            let sign = if v < 0 { 0x80 } else { 0 };
            let mag = (v.unsigned_abs().min(32635) + 0x84) as u32;
            let exp = (31 - mag.leading_zeros()).saturating_sub(7).min(7);
            let mant = (mag >> (exp + 3)) & 0x0f;
            !(sign | (exp << 4) as i32 | mant as i32) as u8
        })
        .collect()
}

/// Sends the microphone only around speech. The phone's uplink cannot carry
/// 32 KB/s through a relay, and silence needs no bandwidth: a chunk goes out
/// when its peak clears the noise floor (tracked from quiet chunks) by 3x,
/// with 320 ms before it and 600 ms after; silence goes as a sample count, so
/// the Mac's endpointing (which counts samples) still sees time pass.
pub struct Gate {
    pub floor: f32,
    pub loudest: u16,
    hangover: u32,
    preroll: std::collections::VecDeque<Vec<u8>>,
}

pub enum Gated {
    Audio(Vec<Vec<u8>>),
    Silence(u32),
}

impl Default for Gate {
    fn default() -> Self {
        Gate { floor: 60.0, loudest: 0, hangover: 0, preroll: Default::default() }
    }
}

impl Gate {
    pub fn feed(&mut self, pcm: Vec<u8>, peak: u16) -> Gated {
        let p = peak as f32;
        self.loudest = self.loudest.max(peak);
        let loud = p > (self.floor * 3.0).max(150.0);
        if !loud {
            // Follow the floor down quickly and up slowly.
            self.floor = if p < self.floor { p.max(10.0) } else { self.floor * 0.98 + p * 0.02 };
        }
        if loud {
            self.hangover = 15; // 600 ms
            let mut out: Vec<Vec<u8>> = self.preroll.drain(..).collect();
            out.push(pcm);
            return Gated::Audio(out);
        }
        if self.hangover > 0 {
            self.hangover -= 1;
            return Gated::Audio(vec![pcm]);
        }
        let n = (pcm.len() / 2) as u32;
        self.preroll.push_back(pcm);
        if self.preroll.len() > 8 {
            self.preroll.pop_front();
            return Gated::Silence(n);
        }
        Gated::Silence(0)
    }
}

pub enum Speech {
    /// A piece of a segment: 24 kHz s16le mono (one message from the Mac).
    Pcm(u32, Vec<u8>),
    /// A control from the Mac's speaker: duck, unduck, pause, resume, stop.
    Control(String, f32),
}

/// Reports for the Mac, in the form of Rose's browser player
/// (RoseOS web/audio.js): {"type":"played","seg","samples","done"} for the
/// piece playing (samples within that piece) every 100 ms and when it ends,
/// and {"type":"stopped","seg","samples"} after a stop.
pub enum Played {
    Report(String),
    Audible(bool),
}

/// Upsample 24 kHz mono to 48 kHz mono, carrying the last sample between chunks.
pub fn upsample(pcm: &[i16], last: &mut i16) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 4);
    for &v in pcm {
        let mid = ((*last as i32 + v as i32) / 2) as i16;
        out.extend_from_slice(&mid.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
        *last = v;
    }
    out
}

struct Piece {
    id: u64,
    seg: u32,
    pcm: Vec<i16>,
    written: usize,
}

/// A run of samples handed to Audio and when it plays.
struct Chunk {
    piece: u64,
    first: usize,
    n: usize,
    start: Instant,
    end: Instant,
}

const RATE: u32 = 24_000;
/// How far ahead of the speaker samples are written: small, so a pause or a
/// stop takes effect quickly and the play position stays honest.
const AHEAD: Duration = Duration::from_millis(140);
const STEP: usize = 960; // 40 ms

fn played_of(chunks: &std::collections::VecDeque<Chunk>, piece: u64, now: Instant) -> usize {
    let mut n = 0;
    for c in chunks.iter().filter(|c| c.piece == piece) {
        if now >= c.end {
            n = n.max(c.first + c.n);
        } else if now > c.start {
            let f = (now - c.start).as_secs_f32() / (c.end - c.start).as_secs_f32();
            n = n.max(c.first + (c.n as f32 * f) as usize);
        }
    }
    n
}

/// The phone's player for the Mac's speech.
pub fn speaker(input: Receiver<Speech>, played: Sender<Played>) {
    std::thread::spawn(move || {
        use std::collections::VecDeque;
        let mut out: Option<File> = None;
        let mut last = 0i16;
        let mut pieces: VecDeque<Piece> = VecDeque::new();
        let mut chunks: VecDeque<Chunk> = VecDeque::new();
        let (mut gain, mut paused) = (1.0f32, false);
        let mut next_id = 0u64;
        let mut reported = Instant::now();
        let mut audible = false;
        loop {
            let msg = match input.recv_timeout(Duration::from_millis(20)) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            };
            let now = Instant::now();
            match msg {
                Some(Speech::Pcm(seg, bytes)) => {
                    let pcm = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                    next_id += 1;
                    pieces.push_back(Piece { id: next_id, seg, pcm, written: 0 });
                }
                Some(Speech::Control(kind, g)) => match kind.as_str() {
                    "duck" => gain = g,
                    "unduck" => gain = 1.0,
                    "pause" => paused = true,
                    "resume" => {
                        paused = false;
                        gain = 1.0;
                    }
                    "stop" => {
                        let head = pieces.iter().find(|p| played_of(&chunks, p.id, now) < p.pcm.len());
                        let (seg, at) = head.map_or((-1i64, 0), |p| (p.seg as i64, played_of(&chunks, p.id, now)));
                        let _ = played.send(Played::Report(format!("{{\"type\":\"stopped\",\"seg\":{seg},\"samples\":{at}}}")));
                        pieces.clear();
                        chunks.clear();
                        // Closing the stream lets Audio drop what it still holds.
                        out = None;
                        paused = false;
                        gain = 1.0;
                    }
                    _ => {}
                },
                None => {}
            }
            // Keep about 140 ms written ahead while not paused.
            if !paused {
                loop {
                    let ahead_until = chunks.back().map_or(now, |c| c.end.max(now));
                    if ahead_until > now + AHEAD {
                        break;
                    }
                    let Some(p) = pieces.iter_mut().find(|p| p.written < p.pcm.len()) else { break };
                    let n = (p.pcm.len() - p.written).min(STEP);
                    let part: Vec<i16> = p.pcm[p.written..p.written + n].iter().map(|&v| (v as f32 * gain) as i16).collect();
                    if out.is_none() {
                        out = open_audio();
                    }
                    if let Some(f) = out.as_mut() {
                        if f.write_all(&upsample(&part, &mut last)).is_err() {
                            out = None;
                        }
                    }
                    let dur = Duration::from_micros(n as u64 * 1_000_000 / RATE as u64);
                    chunks.push_back(Chunk { piece: p.id, first: p.written, n, start: ahead_until, end: ahead_until + dur });
                    p.written += n;
                }
            }
            // Pieces that finished playing, then the one playing, every 100 ms.
            while let Some(p) = pieces.front() {
                if p.written == p.pcm.len() && played_of(&chunks, p.id, now) >= p.pcm.len() {
                    let _ = played.send(Played::Report(format!("{{\"type\":\"played\",\"seg\":{},\"samples\":{},\"done\":true}}", p.seg, p.pcm.len())));
                    let id = p.id;
                    pieces.pop_front();
                    chunks.retain(|c| c.piece != id);
                } else {
                    break;
                }
            }
            if now - reported >= Duration::from_millis(100) {
                reported = now;
                if let Some(p) = pieces.front() {
                    let at = played_of(&chunks, p.id, now);
                    if at > 0 {
                        let _ = played.send(Played::Report(format!("{{\"type\":\"played\",\"seg\":{},\"samples\":{at},\"done\":false}}", p.seg)));
                    }
                }
            }
            let now_audible = !pieces.is_empty() && !paused;
            if now_audible != audible {
                audible = now_audible;
                let _ = played.send(Played::Audible(audible));
            }
        }
    });
}

fn open_audio() -> Option<File> {
    let f = OpenOptions::new().write(true).open("/dev/violet/audio").ok()?;
    // AudioConfig: 48 kHz, mono, s16le, stop in the background.
    let config: [u32; 4] = [48_000, 1, 1, 0];
    unsafe { libc::ioctl(f.as_raw_fd(), 0x5610, config.as_ptr()) };
    Some(f)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mulaw_known_values() {
        let enc = |v: i16| super::mulaw(&v.to_le_bytes())[0];
        assert_eq!(enc(0), 0xff);
        assert_eq!(enc(-1), 0x7f);
        assert_eq!(enc(32767), 0x80);
        assert_eq!(enc(-32768), 0x00);
    }
    #[test]
    fn upsample_doubles() {
        let mut last = 0;
        let out = super::upsample(&[100, 200], &mut last);
        let v: Vec<i16> = out.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]])).collect();
        assert_eq!(v, vec![50, 100, 150, 200]);
        assert_eq!(last, 200);
    }
}
