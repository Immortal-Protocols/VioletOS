//! Session files shared between the built-in Music and Agent apps.
//! The Agent holds the pairing keys and receives the audio over its encrypted link.
use crate::secure;
use serde_json::{json, Value};
use std::{fs, io::Write};
use std::time::{Duration, Instant};

const REQUEST: &str = "/tmp/locomo-make";
const STATUS: &str = "/tmp/locomo-status";
const TRACK: &str = "/tmp/locomo-track";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub struct Locomo {
    available: bool,
    last_poll: Option<Instant>,
    job: Option<String>,
    transfer: Option<Transfer>,
}

struct Transfer {
    title: String,
    duration_ms: u64,
    hash: String,
    audio: Vec<u8>,
}

impl Locomo {
    pub fn set_available(&mut self, available: bool) {
        if self.available == available {
            return;
        }
        self.available = available;
        if self.job.is_none() {
            self.status(if available { "Ready to make a Cool Jazz song" } else { "Waiting for Violet Agent on the Mac…" });
        }
    }

    pub fn request(&mut self) -> Option<Value> {
        if self.last_poll.is_some_and(|last| last.elapsed() < Duration::from_millis(250)) {
            return None;
        }
        self.last_poll = Some(Instant::now());
        if fs::remove_file(REQUEST).is_err() || self.job.is_some() {
            return None;
        }
        if !self.available {
            self.status("Connect to Violet Agent on the Mac and enable Locomo there.");
            return None;
        }
        let data = secure::hex(&secure::random::<16>());
        let id = format!("{}-{}-{}-{}-{}", &data[..8], &data[8..12], &data[12..16], &data[16..20], &data[20..]);
        self.job = Some(id.clone());
        self.status("Requesting a Cool Jazz song…");
        Some(json!({"t": "locomo_make", "id": id}))
    }

    pub fn resume(&self) -> Option<Value> {
        self.job.as_ref().map(|id| json!({"t": "locomo_make", "id": id, "resume": true}))
    }

    pub fn status(&self, text: &str) {
        let message: String = text.chars().take(180).collect();
        if fs::write("/tmp/locomo-status.next", message).is_ok() {
            let _ = fs::rename("/tmp/locomo-status.next", STATUS);
        }
    }

    pub fn record(&mut self, message: &Value) {
        let Some(id) = self.job.as_deref() else { return };
        if message["id"].as_str() != Some(id) {
            return;
        }
        match message["t"].as_str() {
            Some("locomo_status") => {
                self.status(message["message"].as_str().unwrap_or("Waiting for Locomo…"));
                if message["done"] == true {
                    self.job = None;
                    self.transfer = None;
                }
            }
            Some("locomo_track") => {
                let size = message["size"].as_u64().unwrap_or(0);
                let duration_ms = message["durationMs"].as_u64().unwrap_or(0);
                self.transfer = None;
                if !(1..=MAX_BYTES as u64).contains(&size) || !(1..=600_000).contains(&duration_ms) {
                    return;
                }
                self.transfer = Some(Transfer {
                    title: message["title"].as_str().unwrap_or("Cool Jazz").chars().take(100).collect(),
                    duration_ms,
                    hash: message["sha256"].as_str().unwrap_or("").to_owned(),
                    audio: vec![0; size as usize],
                });
                self.status("Receiving the song…");
            }
            Some("locomo_end") => {
                let saved = self.transfer.take().is_some_and(|transfer| transfer.save(id));
                self.job = None;
                self.status(if saved { "Cool Jazz song received" } else { "The song transfer failed. Try again." });
            }
            _ => {}
        }
    }

    pub fn part(&mut self, body: &[u8]) {
        let Some(transfer) = &mut self.transfer else { return };
        let Some((offset, bytes)) = body.split_first_chunk::<4>() else { return };
        let start = u32::from_le_bytes(*offset) as usize;
        if let Some(target) = transfer.audio.get_mut(start..start.saturating_add(bytes.len())) {
            target.copy_from_slice(bytes);
        }
    }
}

impl Transfer {
    fn save(self, id: &str) -> bool {
        if secure::hex(&secure::sha256(&[&self.audio])) != self.hash {
            return false;
        }
        let manifest = json!({"id": id, "title": self.title, "duration_ms": self.duration_ms});
        let staged = "/tmp/locomo-track.next";
        let result = (|| {
            let mut file = fs::File::create(staged)?;
            writeln!(file, "{manifest}")?;
            file.write_all(&self.audio)?;
            drop(file);
            fs::rename(staged, TRACK)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(staged);
            println!("agent: Locomo track write failed: {error}");
            return false;
        }
        println!("agent: Locomo track verified ({} bytes)", self.audio.len());
        true
    }
}
