//! /dev/violet/screen, events and sys.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::mpsc;

use crate::canvas::{Canvas, Rect};

pub const W: usize = 720;
pub const H: usize = 1600;

/// The app's mapped BGRA8888 surface.
pub struct Screen {
    file: File,
    pix: *mut u32,
}

// The mapping lasts as long as the process; the compositor thread owns the
// Screen it presents with.
unsafe impl Send for Screen {}

impl Screen {
    pub fn open() -> std::io::Result<Screen> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/violet/screen")?;
        let mut info = [0u32; 6];
        // SCREEN_IOC_INFO: width, height, stride, format, buffers, buffer bytes.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x5601, info.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                info[5] as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Screen { file, pix: map as *mut u32 })
    }

    /// A canvas that draws straight into the screen surface, so `show` has
    /// nothing to copy: only the rectangle is presented. Prefer it to
    /// `Canvas::new(W, H)` for the frame an app shows.
    pub fn canvas(&mut self) -> Canvas {
        unsafe { Canvas::from_raw(self.pix, W, H) }
    }

    /// Present a rectangle of the canvas: copied to the screen surface first
    /// unless the canvas is the surface itself (`Screen::canvas`).
    pub fn show(&mut self, c: &Canvas, r: Rect) {
        let r = r.intersect(Rect::new(0, 0, W as i32, H as i32));
        if r.is_empty() {
            return;
        }
        if c.pixels_ptr() != self.pix as *const u32 {
            for y in r.y0..r.y1 {
                let row = y as usize * W;
                let src = &c.px[row + r.x0 as usize..row + r.x1 as usize];
                unsafe {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), self.pix.add(row + r.x0 as usize), src.len());
                }
            }
        }
        // PresentRect: x, y, width, height, buffer, flags.
        let words = [r.x0 as u32, r.y0 as u32, r.width() as u32, r.height() as u32, 0, 0];
        let mut bytes = [0u8; 24];
        for (i, w) in words.iter().enumerate() {
            bytes[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        let _ = self.file.write_all(&bytes);
    }

    /// Wait until the panel starts its next frame (SCREEN_IOC_WAIT_VSYNC);
    /// returns the frame's sequence number and period in microseconds.
    pub fn wait_vsync(&mut self) -> Option<(u32, u32)> {
        let mut info = [0u32; 4];
        if unsafe { libc::ioctl(self.file.as_raw_fd(), 0x5604, info.as_mut_ptr()) } != 0 {
            return None;
        }
        Some((info[0], info[1]))
    }

    pub fn show_all(&mut self, c: &Canvas) {
        self.show(c, Rect::new(0, 0, W as i32, H as i32));
    }
}

/// Event kinds from /dev/violet/events.
pub const TOUCH_DOWN: u16 = 1;
pub const TOUCH_MOVE: u16 = 2;
pub const TOUCH_UP: u16 = 3;
pub const KEY_DOWN: u16 = 4;
/// KEY_DOWN's code when the power key is held (sent to Home, for its power sheet).
pub const KEY_POWER: u16 = 4;
pub const JACK: u16 = 6;
pub const LIFECYCLE: u16 = 7;
/// Lifecycle values.
pub const FOCUS_GAINED: i32 = 1;
pub const FOCUS_LOST: i32 = 2;

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub x: i32,
    pub y: i32,
    pub value: i32,
}

impl Event {
    pub fn focus_gained(&self) -> bool {
        self.kind == LIFECYCLE && self.value == FOCUS_GAINED
    }
}

/// Events from /dev/violet/events, read on their own thread.
pub type Events = mpsc::Receiver<Event>;

pub fn events() -> std::io::Result<Events> {
    let mut f = File::open("/dev/violet/events")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut raw = [0u8; 24];
        while f.read_exact(&mut raw).is_ok() {
            let word = |at: usize| i32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
            let e = Event {
                kind: u16::from_le_bytes([raw[8], raw[9]]),
                code: u16::from_le_bytes([raw[10], raw[11]]),
                x: word(12),
                y: word(16),
                value: word(20),
            };
            if tx.send(e).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// Send one command to /dev/violet/sys and return the answer line.
pub fn sys(line: &str) -> String {
    let Ok(mut f) = OpenOptions::new().read(true).write(true).open("/dev/violet/sys") else {
        return String::new();
    };
    let _ = f.write_all(format!("{line}\n").as_bytes());
    let mut buf = [0u8; 4096];
    let n = f.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).trim().to_string()
}

/// Parse a "key=value key=value" answer.
pub fn fields(answer: &str) -> std::collections::HashMap<String, String> {
    answer
        .split_whitespace()
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Back to the Home screen.
pub fn home() {
    sys("home");
}

/// A line in the system log (the USB diagnostics log on the handset).
pub fn log(line: &str) {
    sys(&format!("log {line}"));
}
