//! Reading a Mac's pairing code with the rear camera (/dev/violet/camera):
//! each frame in grey (one value per 2 x 2 Bayer block), its contrast
//! stretched, then a pure-Rust QR decoder (rqrr). On its own thread, so the
//! screen stays responsive while a frame is decoded.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Scan {
    /// A grey frame as the sensor sees it: width, height, values.
    Preview(usize, usize, Vec<u8>),
    /// A code that reads as a Violet Agent pairing code.
    Code(String),
    Failed(String),
}

/// Spread the grey levels between the 2nd and 98th percentiles over 0-255.
pub fn stretch(g: &mut [u8]) {
    let mut hist = [0usize; 256];
    for &v in g.iter().step_by(7) {
        hist[v as usize] += 1;
    }
    let total: usize = hist.iter().sum();
    let at = |frac: f32| {
        let mut n = 0;
        for (v, c) in hist.iter().enumerate() {
            n += c;
            if n as f32 >= total as f32 * frac {
                return v;
            }
        }
        255
    };
    let (lo, hi) = (at(0.02), at(0.98));
    if hi <= lo + 8 {
        return;
    }
    let mut lut = [0u8; 256];
    for (v, l) in lut.iter_mut().enumerate() {
        *l = ((v.saturating_sub(lo)) * 255 / (hi - lo)).min(255) as u8;
    }
    for v in g.iter_mut() {
        *v = lut[*v as usize];
    }
}

/// The text of every QR code found in a grey image.
pub fn decode(w: usize, h: usize, g: &[u8]) -> Vec<String> {
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| g[y * w + x]);
    img.detect_grids().into_iter().filter_map(|grid| grid.decode().ok().map(|(_, text)| text)).collect()
}

/// Read frames and look for a pairing code until `stop` is set or one is found.
pub fn spawn(stop: Arc<AtomicBool>, out: Sender<Scan>) {
    std::thread::spawn(move || {
        let mut camera = match violet::camera::Camera::open() {
            Ok(c) => c,
            Err(e) => {
                let _ = out.send(Scan::Failed(format!("No camera here ({e})")));
                return;
            }
        };
        let (mut frames, mut since) = (0u32, Instant::now());
        while !stop.load(Ordering::Relaxed) {
            let (w, h, mut g) = match camera.next() {
                Ok(f) => violet::camera::grey(&f),
                Err(e) => {
                    let _ = out.send(Scan::Failed(e.to_string()));
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            stretch(&mut g);
            let started = Instant::now();
            let codes = decode(w, h, &g);
            frames += 1;
            if since.elapsed() > Duration::from_secs(5) {
                println!("agent: scanning, {frames} frames in 5 s, {} ms to look at one ({w} x {h})", started.elapsed().as_millis());
                (frames, since) = (0, Instant::now());
            }
            for c in &codes {
                println!("agent: camera read a code ({} characters)", c.len());
            }
            if let Some(c) = codes.into_iter().find(|c| crate::secure::parse_invitation(c).is_some_and(|i| i.from_mac)) {
                let _ = out.send(Scan::Code(c));
                return;
            }
            if out.send(Scan::Preview(w, h, g)).is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Mac's code as the camera might see it: drawn small, on its side, in
    /// dim and uneven light with noise, on a grey background.
    #[test]
    fn reads_a_code_from_a_dim_frame() {
        let text = "violet-agent:1?r=https%3A%2F%2Fagent-2af29451c05342fcbb9db79b4df48ce1.apps.chariots.sh&k=nif642QP8M59daxr5YLqmjdgUWfZVPTYZFsd-zX23_Y&s=Cgs7s24Ubm5GCft4oiKaZJgeb61R67Vkao0TtA2kv0U&n=Rose%27s%20MacBook&o=m";
        let q = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).unwrap();
        let (w, h) = (640usize, 360usize);
        let mut g = vec![0u8; w * h];
        let m = 3; // pixels per module: a code about 250 px across, as at 30 cm
        let n = q.size() as usize;
        let (x0, y0) = (180usize, 40usize);
        let mut seed = 12345u32;
        for y in 0..h {
            for x in 0..w {
                let light = 40 + (x * 30 / w) as i32; // uneven: brighter to the right
                // On its side: code columns run down the sensor's rows.
                let (cx, cy) = (y as isize - y0 as isize, x as isize - x0 as isize);
                let inside = (-12..(n * m) as isize + 12).contains(&cx) && (-12..(n * m) as isize + 12).contains(&cy);
                let dark = inside && cx >= 0 && cy >= 0 && q.get_module((cx as usize / m) as i32, (n - 1 - cy as usize / m) as i32);
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let noise = (seed >> 16) as i32 % 9 - 4;
                let v = if !inside { light / 2 } else if dark { 18 } else { light + 40 };
                g[y * w + x] = (v + noise).clamp(0, 255) as u8;
            }
        }
        stretch(&mut g);
        let found = decode(w, h, &g);
        assert_eq!(found, vec![text.to_string()]);
        assert!(crate::secure::parse_invitation(&found[0]).unwrap().from_mac);
    }
}
