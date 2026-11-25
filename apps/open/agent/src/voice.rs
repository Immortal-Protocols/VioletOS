//! The phone's half of a conversation: the microphone stream
//! (/dev/violet/mic, 16 kHz mono) and the spoken replies, which arrive as
//! 24 kHz mono and are played at 48 kHz through /dev/violet/audio.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

pub enum Speech {
    /// An utterance's next 24 kHz samples.
    Pcm(u32, Vec<u8>),
    /// The Mac finished sending an utterance.
    End(u32),
    /// Drop everything queued (an interruption).
    Stop,
}

/// What the speaker thread reports.
pub enum Played {
    Started(u32),
    Finished(u32),
}

/// Upsample 24 kHz mono to 48 kHz mono, carrying the last sample between chunks.
pub fn upsample(pcm: &[u8], last: &mut i16) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm.chunks_exact(2) {
        let v = i16::from_le_bytes([s[0], s[1]]);
        let mid = ((*last as i32 + v as i32) / 2) as i16;
        out.extend_from_slice(&mid.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
        *last = v;
    }
    out
}

/// Play speech as it arrives. `cancelled` holds the newest utterance id the
/// user interrupted; older audio still queued here is dropped.
pub fn speaker(input: Receiver<Speech>, cancelled: Arc<AtomicU32>, played: Sender<Played>) {
    std::thread::spawn(move || {
        let mut out: Option<File> = None;
        let mut last = 0i16;
        let mut current: Option<u32> = None;
        let mut queued_until = Instant::now();
        loop {
            let msg = match input.recv_timeout(Duration::from_millis(100)) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            };
            match msg {
                Some(Speech::Pcm(id, pcm)) => {
                    if id <= cancelled.load(Ordering::Relaxed) {
                        continue;
                    }
                    if current != Some(id) {
                        current = Some(id);
                        let _ = played.send(Played::Started(id));
                    }
                    if out.is_none() {
                        out = open_audio();
                    }
                    let data = upsample(&pcm, &mut last);
                    if let Some(f) = out.as_mut() {
                        if f.write_all(&data).is_err() {
                            out = None;
                        }
                    }
                    let secs = Duration::from_micros(pcm.len() as u64 / 2 * 1_000_000 / 24_000);
                    queued_until = queued_until.max(Instant::now()) + secs;
                }
                Some(Speech::End(id)) => {
                    // The ring plays on for about a third of a second.
                    let wait = queued_until.saturating_duration_since(Instant::now()).min(Duration::from_secs(1));
                    std::thread::sleep(wait.min(Duration::from_millis(400)));
                    if current == Some(id) {
                        current = None;
                        let _ = played.send(Played::Finished(id));
                    }
                }
                Some(Speech::Stop) => {
                    // Closing the stream stops what Audio still holds once it drains.
                    out = None;
                    if let Some(id) = current.take() {
                        let _ = played.send(Played::Finished(id));
                    }
                }
                None => {}
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
    fn upsample_doubles() {
        let mut last = 0;
        let pcm: Vec<u8> = [100i16, 200].iter().flat_map(|v| v.to_le_bytes()).collect();
        let out = super::upsample(&pcm, &mut last);
        let v: Vec<i16> = out.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]])).collect();
        assert_eq!(v, vec![50, 100, 150, 200]);
        assert_eq!(last, 200);
    }
}
