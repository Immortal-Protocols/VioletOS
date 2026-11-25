//! The agent's eyes and hands on this phone, for Rose's phone tasks and
//! look_at_screen (companion/mac/violet_rose/phone.py). The paired Mac sends
//! {"t":"phone","id":N,"action":...}; the answer is {"t":"phone_result","id":N,
//! "result":{...}} in the shapes Rose's vision controller reads: a screenshot
//! is {"status":"ok","imageBase64":PNG,"width","height","displayWidth",
//! "displayHeight","packageName","screenshotId"}; a gesture is
//! {"status":"gesture_dispatched"}. The Open Runtime does the seeing and
//! touching (`screenshot` and `gesture` in /dev/violet/sys, agent only).
use serde_json::{json, Value};
use std::time::Duration;

/// The answer to one phone command.
pub fn run(action: &str, args: &Value, shot: &mut u64) -> Value {
    match action {
        "screenshot" => screenshot(shot),
        "gesture" => gesture(args),
        "open_app" => {
            let name = args["app"].as_str().unwrap_or("");
            if !crate::secure_name(name) && name != "agent" {
                return json!({"status": "rejected", "reason": "not an app name"});
            }
            let r = violet::sys(&format!("open /apps/{name}"));
            if !r.starts_with("ok") {
                return json!({"status": "failed", "reason": r.trim_start_matches("error ").trim()});
            }
            std::thread::sleep(Duration::from_millis(900));
            json!({"status": "opened", "observation": screenshot(shot)})
        }
        "home" => {
            violet::sys("home");
            std::thread::sleep(Duration::from_millis(500));
            json!({"status": "action_accepted"})
        }
        "list_apps" => json!({"status": "ok", "apps": apps()}),
        _ => json!({"status": "unsupported", "reason": format!("{action} is not available on VioletOS")}),
    }
}

/// What the panel shows, as a PNG at half size.
fn screenshot(shot: &mut u64) -> Value {
    let r = violet::sys("screenshot");
    let f: Vec<&str> = r.split_whitespace().collect();
    if f.first() != Some(&"ok") || f.len() < 6 {
        return json!({"status": "unavailable", "errorCode": r.trim()});
    }
    let num = |i: usize| f[i].parse::<u32>().unwrap_or(0);
    let (w, h, dw, dh) = (num(1), num(2), num(3), num(4));
    let Ok(rgb) = std::fs::read("/tmp/.agent-screen") else {
        return json!({"status": "unavailable", "errorCode": "no screen image"});
    };
    if rgb.len() != (w * h * 3) as usize {
        return json!({"status": "unavailable", "errorCode": "short screen image"});
    }
    *shot += 1;
    json!({"status": "ok", "imageBase64": base64(&png(w, h, &rgb)), "width": w, "height": h,
           "displayWidth": dw, "displayHeight": dh, "packageName": f[5], "screenshotId": format!("violet-{shot}")})
}

/// A tap ([x, y]) or a swipe ([x1, y1, x2, y2]) in display pixels.
fn gesture(args: &Value) -> Value {
    let p: Vec<i64> = args["points"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let (cmd, wait) = match p.as_slice() {
        [x, y] => (format!("gesture {x} {y} {x} {y} 80"), 250),
        [x1, y1, x2, y2] => (format!("gesture {x1} {y1} {x2} {y2} 350"), 550),
        _ => return json!({"status": "rejected", "reason": "points must be [x, y] or [x1, y1, x2, y2]"}),
    };
    let r = violet::sys(&cmd);
    if !r.starts_with("ok") {
        return json!({"status": "failed", "reason": r.trim_start_matches("error ").trim()});
    }
    std::thread::sleep(Duration::from_millis(wait));
    json!({"status": "gesture_dispatched"})
}

/// The apps on Home: the built-in titled apps and the installed ones ("name<TAB>title" in /apps/.home).
fn apps() -> Vec<Value> {
    let titles = std::fs::read_to_string("/apps/.home").unwrap_or_default();
    titles
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(name, _)| *name != "agent")
        .map(|(name, title)| json!({"package": name, "label": title}))
        .collect()
}

/// An 8-bit RGB PNG: each row unfiltered, the whole zlib-compressed.
pub fn png(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for row in rgb.chunks_exact(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 4));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Standard base64 with padding (what Rose's decoder validates).
pub fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            s.push(if i <= c.len() { A[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b"Rose"), "Um9zZQ==");
        assert_eq!(super::base64(b"Violet"), "VmlvbGV0");
    }
    #[test]
    fn png_has_its_chunks() {
        let p = super::png(2, 1, &[255, 0, 0, 0, 0, 255]);
        assert!(p.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&p[12..16], b"IHDR");
        assert!(p.ends_with(&[0xae, 0x42, 0x60, 0x82])); // IEND's CRC
    }
}
