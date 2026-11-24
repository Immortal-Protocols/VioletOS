//! Streams a track over HTTPS, decodes its MP3 in this process and plays
//! 48 kHz PCM through /dev/violet/audio. The Audio service only mixes and sets
//! the volume: no decoder or URL lives in an OS service.
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const LIBRARY: &str = "https://agent-39868422bcfa413fa7149c56084664db.apps.chariots.sh";

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub number: u32,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    pub duration_ms: u64,
    pub bpm: u32,
    pub key: String,
    pub audio: String,
    pub cover: String,
    #[serde(default)]
    pub peaks: Vec<f32>,
}

#[derive(Deserialize, Clone)]
pub struct Catalog {
    pub name: String,
    pub album: String,
    #[serde(rename = "baseUrl")]
    pub base: String,
    pub tracks: Vec<Track>,
}

/// The library serves bounded ranges only: a whole-file or open-ended request
/// gets 503. Everything is fetched in pieces of this size.
const PIECE: u64 = 128 << 10;

/// Bytes from..from+PIECE-1 of a file; retries while the library wakes (503).
fn piece(url: &str, from: u64) -> io::Result<violet::http::Response> {
    let range = format!("bytes={}-{}", from, from + PIECE - 1);
    let mut wait = 700;
    for attempt in 0.. {
        let r = violet::http::get(url, &[("Range", &range)])?;
        if matches!(r.status, 200 | 206 | 416) {
            return Ok(r);
        }
        if attempt == 6 {
            return Err(io::Error::other(format!("{}", r.status)));
        }
        thread::sleep(Duration::from_millis(wait));
        wait += 700;
    }
    unreachable!()
}

/// "bytes a-b/total"
fn total(r: &violet::http::Response) -> Option<u64> {
    r.header("content-range")?.rsplit('/').next()?.parse().ok()
}

/// A whole (small) file, piece by piece, giving up after a few failures.
pub fn fetch(url: &str) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut failures = 0;
    loop {
        match piece(url, data.len() as u64) {
            Ok(r) if r.status == 416 => return Ok(data),
            Ok(r) => {
                let whole = r.status == 200;
                let end = total(&r);
                data.extend_from_slice(&r.body);
                if whole || r.body.is_empty() || end.is_some_and(|t| data.len() as u64 >= t) {
                    return Ok(data);
                }
            }
            Err(e) => {
                failures += 1;
                if failures > 3 {
                    return Err(e);
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

pub fn catalog() -> Result<Catalog, String> {
    let r = violet::http::get(&format!("{LIBRARY}/catalog.json"), &[]).map_err(|e| e.to_string())?;
    if r.status != 200 {
        return Err(format!("catalog: {}", r.status));
    }
    let mut c: Catalog = serde_json::from_slice(&r.body).map_err(|e| e.to_string())?;
    if c.base.is_empty() {
        c.base = LIBRARY.to_string();
    }
    Ok(c)
}

/// Downloads a song in pieces, up to about a minute ahead of the decoder,
/// resuming from the same byte when the network drops.
fn download(url: String, stop: Arc<AtomicBool>, status: Arc<Mutex<String>>, out: SyncSender<Vec<u8>>) {
    let (mut at, mut size, mut lost) = (0u64, None::<u64>, false);
    let mut tries = 0u32;
    while !stop.load(Relaxed) && size.is_none_or(|s| at < s) {
        match piece(&url, at) {
            Ok(r) if r.status == 416 => break,
            Ok(r) => {
                if lost {
                    println!("music: stream resumed");
                    lost = false;
                    *status.lock().unwrap() = String::new();
                }
                tries = 0;
                if size.is_none() {
                    size = total(&r).or((r.status == 200).then_some(r.body.len() as u64));
                }
                if r.body.is_empty() {
                    break;
                }
                at += r.body.len() as u64;
                if out.send(r.body).is_err() {
                    return; // the song was skipped
                }
            }
            Err(e) => {
                if !lost {
                    println!("music: stream interrupted: {e}");
                    lost = true;
                }
                *status.lock().unwrap() = "Reconnecting…".to_string();
                tries += 1;
                thread::sleep(Duration::from_secs(tries.min(5) as u64));
            }
        }
    }
}

/// The decoder's side of the download.
struct Pieces {
    rx: Mutex<Receiver<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for Pieces {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            match self.rx.lock().unwrap().recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

pub const BANDS: usize = 16;

pub struct Player {
    audio: Arc<Mutex<File>>,
    pub track: Arc<AtomicI32>, // index, -1 when stopped
    frames: Arc<AtomicI64>,    // frames written for the current track
    pub paused: Arc<AtomicBool>,
    pub failure: Arc<Mutex<String>>,
    bands: Arc<[AtomicU32; BANDS]>,
    stop: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
    finished_tx: Sender<i32>,
    pub finished: Receiver<i32>, // a track that played to its end
}

impl Player {
    pub fn new() -> io::Result<Player> {
        let audio = File::options().write(true).open("/dev/violet/audio")?;
        let (finished_tx, finished) = mpsc::channel();
        Ok(Player {
            audio: Arc::new(Mutex::new(audio)),
            track: Arc::new(AtomicI32::new(-1)),
            frames: Arc::new(AtomicI64::new(0)),
            paused: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Mutex::new(String::new())),
            bands: Arc::new(Default::default()),
            stop: None,
            finished_tx,
            finished,
        })
    }

    pub fn stop(&mut self) {
        if let Some((stop, handle)) = self.stop.take() {
            stop.store(true, Relaxed);
            let _ = handle.join();
        }
        self.track.store(-1, Relaxed);
        self.paused.store(false, Relaxed);
    }

    pub fn play(&mut self, c: &Catalog, index: usize) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        self.track.store(index as i32, Relaxed);
        self.frames.store(0, Relaxed);
        self.failure.lock().unwrap().clear();
        let url = format!("{}/{}", c.base, c.tracks[index].audio);
        let (audio, frames, paused, failure, bands, finished, s) =
            (self.audio.clone(), self.frames.clone(), self.paused.clone(), self.failure.clone(), self.bands.clone(), self.finished_tx.clone(), stop.clone());
        let handle = thread::spawn(move || {
            // Eight pieces of 128 KiB: about a minute at 128 kbps.
            let (tx, rx) = mpsc::sync_channel(8);
            let (dl_stop, dl_status) = (s.clone(), failure.clone());
            let downloader = thread::spawn(move || download(url, dl_stop, dl_status, tx));
            let source = Pieces { rx: Mutex::new(rx), buf: Vec::new(), pos: 0 };
            let ended = decode(source, &s, &audio, &frames, &paused, &bands, &failure);
            s.store(true, Relaxed); // stop the download if the song ended early
            drop(downloader);
            if ended {
                println!("music: song ended after {} s", frames.load(Relaxed) / 48_000);
                let _ = finished.send(index as i32);
            }
        });
        self.stop = Some((stop, handle));
    }

    /// Seconds heard: frames written less what is still queued in Audio's ring.
    pub fn position(&self) -> f32 {
        (self.frames.load(Relaxed) as f32 / 48_000.0 - 0.3).max(0.0)
    }

    pub fn bands(&self) -> [f32; BANDS] {
        let quiet = self.paused.load(Relaxed) || self.track.load(Relaxed) < 0;
        std::array::from_fn(|i| if quiet { 0.0 } else { f32::from_bits(self.bands[i].load(Relaxed)) })
    }
}

/// Decode and play until the song ends (true) or is stopped (false).
fn decode(
    source: Pieces,
    stop: &AtomicBool,
    audio: &Mutex<File>,
    frames: &AtomicI64,
    paused: &AtomicBool,
    bands: &[AtomicU32; BANDS],
    failure: &Mutex<String>,
) -> bool {
    let mss = MediaSourceStream::new(Box::new(ReadOnlySource::new(source)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = match symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) {
        Ok(p) => p,
        Err(e) => {
            *failure.lock().unwrap() = format!("could not start the song: {e}");
            return false;
        }
    };
    let mut format = probed.format;
    let Some(track) = format.default_track() else { return false };
    let id = track.id;
    let mut decoder = match symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()) {
        Ok(d) => d,
        Err(e) => {
            *failure.lock().unwrap() = format!("decoder: {e}");
            return false;
        }
    };
    let mut window: Vec<f32> = Vec::with_capacity(1024);
    let mut pcm: Vec<u8> = Vec::new();
    loop {
        if stop.load(Relaxed) {
            return false;
        }
        while paused.load(Relaxed) && !stop.load(Relaxed) {
            thread::sleep(Duration::from_millis(20));
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(_)) | Err(SymError::ResetRequired) => return !stop.load(Relaxed),
            Err(e) => {
                println!("music: {e}");
                return !stop.load(Relaxed);
            }
        };
        if packet.track_id() != id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => {
                println!("music: decode: {e}");
                return !stop.load(Relaxed);
            }
        };
        let spec = *buf.spec();
        let channels = spec.channels.count();
        let mut samples = SampleBuffer::<i16>::new(buf.capacity() as u64, spec);
        samples.copy_interleaved_ref(buf);
        pcm.clear();
        for frame in samples.samples().chunks(channels.max(1)) {
            let (l, r) = (frame[0], if channels > 1 { frame[1] } else { frame[0] });
            pcm.extend_from_slice(&l.to_le_bytes());
            pcm.extend_from_slice(&r.to_le_bytes());
            window.push((l as f32 + r as f32) / 65536.0);
            if window.len() == 1024 {
                spectrum(&window, bands);
                window.clear();
            }
        }
        if let Err(e) = audio.lock().unwrap().write_all(&pcm) {
            *failure.lock().unwrap() = format!("audio: {e}");
            return false;
        }
        frames.fetch_add((pcm.len() / 4) as i64, Relaxed);
    }
}

/// A 1024-point FFT of the mono mix, folded into 16 log-spaced bands.
fn spectrum(x: &[f32], bands: &[AtomicU32; BANDS]) {
    let n = x.len();
    let mut re: Vec<f32> = x.iter().enumerate().map(|(i, v)| v * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos())).collect();
    let mut im = vec![0.0f32; n];
    fft(&mut re, &mut im);
    for b in 0..BANDS {
        // 40 Hz to 16 kHz, logarithmically.
        let edge = |k: usize| 40.0 * (16_000.0f32 / 40.0).powf(k as f32 / BANDS as f32);
        let lo = (edge(b) * n as f32 / 48_000.0) as usize;
        let hi = ((edge(b + 1) * n as f32 / 48_000.0) as usize).max(lo + 1);
        let sum: f32 = (lo..hi.min(n / 2)).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).sum();
        // A full-scale sine in one band reads 0 dB; show -66 dB to -6 dB.
        let amp = sum / (hi - lo) as f32 / (n as f32 / 4.0);
        let v = ((20.0 * (amp + 1e-9).log10() + 66.0) / 60.0).clamp(0.0, 1.0);
        let old = f32::from_bits(bands[b].load(Relaxed));
        bands[b].store(v.max(old * 0.82).to_bits(), Relaxed);
    }
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let a = -2.0 * std::f32::consts::PI / size as f32;
        for start in (0..n).step_by(size) {
            for k in 0..size / 2 {
                let (wr, wi) = ((a * k as f32).cos(), (a * k as f32).sin());
                let (p, q) = (start + k, start + k + size / 2);
                let (tr, ti) = (re[q] * wr - im[q] * wi, re[q] * wi + im[q] * wr);
                re[q] = re[p] - tr;
                im[q] = im[p] - ti;
                re[p] += tr;
                im[p] += ti;
            }
        }
        size <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first song from the library, downloaded in pieces and decoded in
    /// full (audio to /dev/null): its length matches the catalog.
    #[test]
    fn library() {
        let Ok(c) = catalog() else { return }; // offline
        let url = format!("{}/{}", c.base, c.tracks[0].audio);
        let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(String::new())));
        let (tx, rx) = mpsc::sync_channel(8);
        let (s2, st2) = (stop.clone(), status.clone());
        thread::spawn(move || download(url, s2, st2, tx));
        let audio = Mutex::new(File::create("/dev/null").unwrap());
        let (frames, paused, bands) = (AtomicI64::new(0), AtomicBool::new(false), Default::default());
        let ended = decode(Pieces { rx: Mutex::new(rx), buf: Vec::new(), pos: 0 }, &stop, &audio, &frames, &paused, &bands, &status);
        let seconds = frames.load(Relaxed) as f32 / 48_000.0;
        let expected = c.tracks[0].duration_ms as f32 / 1000.0;
        assert!(ended, "{}", status.lock().unwrap());
        assert!((seconds - expected).abs() < 2.0, "decoded {seconds} s of {expected} s");
    }
}
