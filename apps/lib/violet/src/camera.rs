//! /dev/violet/camera: raw Bayer frames from the rear camera (runtime/open/abi.rs),
//! and turning them into pixels.
use std::fs::File;
use std::io::Read;

use crate::canvas::{Canvas, Rect};

pub const MAGIC: u32 = 0x3146_4356; // "VCF1"
pub const RAW8: u32 = 1;
pub const RAW10: u32 = 2;
pub const STREAMING: u32 = 2;
pub const FAILED: u32 = 3;
pub const TORN: u32 = 1 << 31;
const HEADER: usize = 32;

/// The header in front of every frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub sequence: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Low byte RAW8 or RAW10; next byte the Bayer order (0 RGGB, 1 GRBG, 2 GBRG, 3 BGGR).
    pub format: u32,
    pub sensor: u32,
    /// Low byte STREAMING or FAILED, the error above it, TORN if it changed while copied.
    pub state: u32,
}

pub struct Camera {
    file: File,
    buf: Vec<u8>,
}

/// A frame: its header and its pixels as the sensor delivered them.
pub struct Frame<'a> {
    pub info: Info,
    pub data: &'a [u8],
}

impl Camera {
    /// Start the camera. It runs while this app is in front and the file is open.
    pub fn open() -> std::io::Result<Camera> {
        Ok(Camera { file: File::open("/dev/violet/camera")?, buf: vec![0; HEADER + 4096 * 1024] })
    }

    /// Wait for the next frame. An Err means the camera failed (its code is in
    /// the error's message) or the device went away.
    pub fn next(&mut self) -> std::io::Result<Frame<'_>> {
        let n = self.file.read(&mut self.buf)?;
        if n < HEADER {
            return Err(std::io::Error::other("short camera read"));
        }
        let w = |i: usize| u32::from_le_bytes(self.buf[4 * i..4 * i + 4].try_into().unwrap());
        if w(0) != MAGIC {
            return Err(std::io::Error::other("not a camera frame"));
        }
        let info = Info { sequence: w(1), width: w(2), height: w(3), stride: w(4), format: w(5), sensor: w(6), state: w(7) };
        if info.state & 0xff == FAILED {
            return Err(std::io::Error::other(format!("camera failed (error {})", info.state >> 8 & 0xff_ffff)));
        }
        Ok(Frame { info, data: &self.buf[HEADER..n] })
    }
}

/// The frame in grey, one value per 2 x 2 Bayer block (the mean of its four
/// pixels), as the sensor sees it (not turned upright): width, height, values.
/// For machine vision, such as reading QR codes.
pub fn grey(f: &Frame) -> (usize, usize, Vec<u8>) {
    let (w, h) = ((f.info.width / 2) as usize, (f.info.height / 2) as usize);
    let mut out = vec![0u8; w * h];
    for by in 0..h {
        for bx in 0..w {
            let (x, y) = (bx as u32 * 2, by as u32 * 2);
            let p = Develop::sample(f, x, y) as u32 + Develop::sample(f, x + 1, y) as u32 + Develop::sample(f, x, y + 1) as u32 + Develop::sample(f, x + 1, y + 1) as u32;
            out[by * w + bx] = (p / 4) as u8;
        }
    }
    (w, h, out)
}

/// Colour from a raw frame: black level, grey-world white balance (smoothed
/// across frames so it does not flicker) and a display gamma.
pub struct Develop {
    gains: [f32; 3],
    lut: [[u8; 256]; 3],
}

const BLACK: f32 = 16.0; // 64 of 1023 on the Hi-1336, in the high eight bits

impl Default for Develop {
    fn default() -> Self {
        let mut d = Develop { gains: [1.6, 1.0, 1.6], lut: [[0; 256]; 3] };
        d.build();
        d
    }
}

impl Develop {
    fn build(&mut self) {
        for c in 0..3 {
            for v in 0..256 {
                let lin = ((v as f32 - BLACK).max(0.0) / (255.0 - BLACK) * self.gains[c]).min(1.0);
                self.lut[c][v] = (lin.powf(1.0 / 2.2) * 255.0 + 0.5) as u8;
            }
        }
    }

    /// The high eight bits of the pixel at (x, y).
    fn sample(f: &Frame, x: u32, y: u32) -> u8 {
        let row = (y * f.info.stride) as usize;
        let i = match f.info.format & 0xff {
            RAW10 => (x / 4 * 5 + x % 4) as usize,
            _ => x as usize,
        };
        f.data.get(row + i).copied().unwrap_or(0)
    }

    /// R, G (the mean of both greens) and B of the 2 x 2 Bayer block at (bx, by).
    fn block(f: &Frame, bx: u32, by: u32) -> [u8; 3] {
        let (x, y) = (bx * 2, by * 2);
        let p = [Self::sample(f, x, y), Self::sample(f, x + 1, y), Self::sample(f, x, y + 1), Self::sample(f, x + 1, y + 1)];
        // Where R and B sit in the block for each order; the other two are green.
        let (r, b) = match f.info.format >> 8 & 0xff {
            0 => (0, 3), // RGGB
            1 => (1, 2), // GRBG
            2 => (2, 1), // GBRG
            _ => (3, 0), // BGGR
        };
        let g = (p.iter().enumerate().filter(|(i, _)| *i != r && *i != b).map(|(_, v)| *v as u32).sum::<u32>() / 2) as u8;
        [p[r], g, p[b]]
    }

    /// Learn white balance from a sparse grid of the frame.
    pub fn balance(&mut self, f: &Frame) {
        let (bw, bh) = (f.info.width / 2, f.info.height / 2);
        let mut sum = [1.0f32; 3];
        for by in (0..bh).step_by(9) {
            for bx in (0..bw).step_by(11) {
                let px = Self::block(f, bx, by);
                // Leave out clipped and black blocks: they carry no colour.
                if px.iter().all(|&v| v > 24 && v < 240) {
                    for c in 0..3 {
                        sum[c] += px[c] as f32 - BLACK;
                    }
                }
            }
        }
        let target = [sum[1] / sum[0], 1.0, sum[1] / sum[2]];
        for c in [0, 2] {
            let t = target[c].clamp(0.5, 4.0);
            self.gains[c] += (t - self.gains[c]) * 0.2;
        }
        self.build();
    }

    /// Draw the frame turned upright for a portrait screen (the sensor is
    /// mounted on its side) into `r` of the canvas, one 2 x 2 Bayer block per
    /// `scale` x `scale` screen pixels, centred and cropped to fill `r`.
    pub fn draw(&self, f: &Frame, c: &mut Canvas, r: Rect, scale: i32) {
        let (bw, bh) = ((f.info.width / 2) as i32, (f.info.height / 2) as i32);
        // Upright: screen rows walk the sensor's columns; screen columns walk
        // its rows from the bottom up.
        let (uw, uh) = (bh * scale, bw * scale);
        let (ox, oy) = (r.x0 + (r.width() - uw) / 2, r.y0 + (r.height() - uh) / 2);
        let clip = r.intersect(c.clip());
        if clip.is_empty() {
            return;
        }
        let width = c.w;
        let mut row = vec![0xff00_0000u32; clip.width() as usize];
        let mut y = clip.y0;
        while y < clip.y1 {
            let v = y - oy;
            let bx = v.div_euclid(scale);
            // The screen rows that show this sensor column.
            let rows = (scale - v.rem_euclid(scale)).min(clip.y1 - y);
            if v >= 0 && v < uh {
                for (k, x) in (clip.x0..clip.x1).enumerate() {
                    let u = x - ox;
                    row[k] = if u < 0 || u >= uw {
                        0xff00_0000
                    } else if k > 0 && (u % scale) != 0 {
                        row[k - 1]
                    } else {
                        let by = bh - 1 - u / scale;
                        let [rr, gg, bb] = Self::block(f, bx as u32, by as u32);
                        0xff00_0000
                            | (self.lut[0][rr as usize] as u32) << 16
                            | (self.lut[1][gg as usize] as u32) << 8
                            | self.lut[2][bb as usize] as u32
                    };
                }
            } else {
                row.fill(0xff00_0000);
            }
            for dy in 0..rows {
                let at = (y + dy) as usize * width + clip.x0 as usize;
                c.px[at..at + row.len()].copy_from_slice(&row);
            }
            y += rows;
        }
    }
}
