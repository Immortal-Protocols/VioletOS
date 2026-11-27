//! Playback in the browser process. Only the most recent frame waits for the
//! compositor, so a slow display cannot build up a queue of decoded images.
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use violet::{Canvas, Fonts, Rect, W, rgb};

pub fn is_webm(mime: &str) -> bool {
    mime.split(';').next().is_some_and(|s| s.trim() == "video/webm")
}

struct Frame {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
}

#[derive(Default)]
struct Output {
    frame: Option<Frame>,
    message: String,
    dirty: bool,
    ended: bool,
}

pub struct Playback {
    output: Arc<Mutex<Output>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl Playback {
    pub fn start(data: Vec<u8>) -> Self {
        let player = Self {
            output: Default::default(), stop: Default::default(), paused: Default::default(),
        };
        let output = player.output.clone();
        let stop = player.stop.clone();
        let paused = player.paused.clone();
        std::thread::spawn(move || {
            let result = decode(data, &output, &stop, &paused);
            if !stop.load(Relaxed) {
                let mut o = output.lock().unwrap();
                o.message = result.err().unwrap_or_else(|| "Playback ended · Tap to replay".into());
                println!("ladybird: {}", o.message);
                o.ended = true;
                o.dirty = true;
            }
        });
        player
    }

    pub fn pause(&self, yes: bool) {
        self.paused.store(yes, Relaxed);
        self.output.lock().unwrap().dirty = true;
    }

    pub fn ended(&self) -> bool {
        self.output.lock().unwrap().ended
    }

    pub fn toggle(&self) {
        self.pause(!self.paused.load(Relaxed));
    }

    pub fn take_dirty(&self) -> bool {
        std::mem::take(&mut self.output.lock().unwrap().dirty)
    }

    pub fn draw(&self, canvas: &mut Canvas, fonts: &Fonts) {
        let o = self.output.lock().unwrap();
        canvas.fill(canvas.bounds(), rgb(16, 16, 16));
        let mut bottom = 64;
        if let Some(f) = &o.frame {
            let x = (W - f.width) / 2;
            for (y, row) in f.pixels.chunks_exact(f.width).take(canvas.h).enumerate() {
                let at = y * canvas.w + x;
                canvas.px[at..at + f.width].copy_from_slice(row);
            }
            bottom = f.height as i32;
        }
        let label = if o.ended { &o.message }
            else if self.paused.load(Relaxed) { "Tap to resume" }
            else if o.frame.is_some() { "Tap to pause" }
            else { "Opening video…" };
        canvas.set_clip(Rect::new(0, 0, W as i32, canvas.h as i32));
        fonts.medium(25.0).draw(canvas, label, 24, bottom + 50, rgb(238, 238, 238));
        canvas.reset_clip();
    }
}

impl Drop for Playback {
    fn drop(&mut self) { self.stop.store(true, Relaxed); }
}

#[cfg(not(feature = "webm"))]
fn decode(_: Vec<u8>, _: &Mutex<Output>, _: &AtomicBool, _: &AtomicBool) -> Result<(), String> {
    Err("WebM decoding is unavailable in this build".into())
}

#[cfg(feature = "webm")]
use ffmpeg::decode;

#[cfg(feature = "webm")]
mod ffmpeg {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::fs::File;
    use std::io::Write;
    use std::time::{Duration, Instant};

    unsafe extern "C" {
        fn violet_play_webm(bytes: *const u8, len: c_int, player: *mut c_void,
            video: extern "C" fn(*mut c_void, i64, *const u8, c_int, c_int, c_int) -> c_int,
            audio: extern "C" fn(*mut c_void, i64, *const u8, c_int) -> c_int,
            error: *mut c_char) -> c_int;
    }

    struct Worker<'a> {
        output: &'a Mutex<Output>,
        stop: &'a AtomicBool,
        paused: &'a AtomicBool,
        clock: Option<(Instant, i64)>,
        audio: Option<File>,
        error: Option<String>,
    }

    impl Worker<'_> {
        fn wait(&mut self, pts: i64) -> bool {
            let (start, first) = self.clock.get_or_insert_with(|| (Instant::now(), pts.max(0)));
            let target = Duration::from_millis(pts.saturating_sub(*first).max(0) as u64);
            loop {
                if self.stop.load(Relaxed) { return false; }
                if self.paused.load(Relaxed) {
                    let paused_at = Instant::now();
                    std::thread::sleep(Duration::from_millis(15));
                    *start += paused_at.elapsed();
                    continue;
                }
                let remain = target.saturating_sub(start.elapsed());
                if remain.is_zero() { return true; }
                std::thread::sleep(remain.min(Duration::from_millis(10)));
            }
        }
    }

    extern "C" fn video(ctx: *mut c_void, pts: i64, pixels: *const u8, w: c_int, h: c_int, stride: c_int) -> c_int {
        // FFmpeg calls back synchronously, with an aligned, allocated BGRA frame.
        let worker = unsafe { &mut *ctx.cast::<Worker>() };
        if !worker.wait(pts) { return 1; }
        let (w, h) = (w as usize, h as usize);
        let mut output = worker.output.lock().unwrap();
        let frame = output.frame.get_or_insert_with(|| Frame { width: w, height: h, pixels: Vec::new() });
        frame.width = w;
        frame.height = h;
        frame.pixels.resize(w * h, 0);
        for (y, row) in frame.pixels.chunks_exact_mut(w).enumerate() {
            let src = unsafe { std::slice::from_raw_parts(pixels.add(y * stride as usize).cast::<u32>(), w) };
            row.copy_from_slice(src);
        }
        output.dirty = true;
        0
    }

    extern "C" fn audio(ctx: *mut c_void, pts: i64, samples: *const u8, len: c_int) -> c_int {
        let worker = unsafe { &mut *ctx.cast::<Worker>() };
        if !worker.wait(pts) { return 1; }
        let mut write = || -> std::io::Result<()> {
            if worker.audio.is_none() {
                worker.audio = Some(File::options().write(true).open("/dev/violet/audio")?);
            }
            let bytes = unsafe { std::slice::from_raw_parts(samples, len as usize) };
            worker.audio.as_mut().unwrap().write_all(bytes)
        };
        if let Err(e) = (write)() {
            worker.error = Some(format!("Audio playback failed: {e}"));
            return 1;
        }
        0
    }

    pub(super) fn decode(data: Vec<u8>, output: &Mutex<Output>, stop: &AtomicBool, paused: &AtomicBool) -> Result<(), String> {
        let mut worker = Worker { output, stop, paused, clock: None, audio: None, error: None };
        let mut error = [0 as c_char; 128];
        let len = i32::try_from(data.len()).map_err(|_| "Video is too large")?;
        let rc = unsafe { violet_play_webm(data.as_ptr(), len, (&mut worker as *mut Worker).cast(), video, audio, error.as_mut_ptr()) };
        if let Some(e) = worker.error { return Err(e); }
        if rc < 0 {
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
            return Err(format!("Cannot play this WebM: {message}"));
        }
        Ok(())
    }
}
