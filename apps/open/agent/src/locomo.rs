//! Session files shared between the built-in Music and Agent apps.
//! The Agent holds the pairing keys and receives the audio over its encrypted link.
use crate::secure;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use std::{fs, io::Write};

const REQUEST: &str = "/tmp/locomo-make";
const BROWSE: &str = "/tmp/locomo-browse";
const LIBRARY: &str = "/tmp/locomo-library";
const STATUS: &str = "/tmp/locomo-status";
const TRACK: &str = "/tmp/locomo-track";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub struct Locomo {
    available: bool,
    last_poll: Option<Instant>,
    library_request: Option<u64>,
    job: Option<String>,
    transfer: Option<Transfer>,
}

struct Transfer {
    title: String,
    duration_ms: u64,
    station: String,
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
            self.status(if available {
                "Locomo Music is connected."
            } else {
                "Waiting for Violet Agent on the Mac…"
            });
        }
    }

    pub fn requests(&mut self) -> Vec<Value> {
        if self
            .last_poll
            .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
        {
            return Vec::new();
        }
        self.last_poll = Some(Instant::now());
        let mut requests = Vec::new();
        if let Some(mut browse) = Self::take(BROWSE) {
            self.library_request = browse["request"].as_u64();
            if self.available {
                browse["t"] = json!("locomo_browse");
                requests.push(browse);
            } else {
                self.library(
                    &json!({"request": browse["request"], "tracks": [], "more": false,
                    "error": "Connect to the Mac and enable Locomo to browse its library."}),
                );
            }
        }
        let Some(mut command) = Self::take(REQUEST) else {
            return requests;
        };
        if !self.available {
            self.status("Connect to Violet Agent on the Mac and enable Locomo there.");
            return requests;
        }
        if command["control"].is_string() {
            command["t"] = json!("locomo_control");
            requests.push(command);
            return requests;
        }
        if self.job.is_some() {
            return requests;
        }
        let data = secure::hex(&secure::random::<16>());
        let id = format!(
            "{}-{}-{}-{}-{}",
            &data[..8],
            &data[8..12],
            &data[12..16],
            &data[16..20],
            &data[20..]
        );
        self.job = Some(id.clone());
        self.status(if command["track"].is_string() {
            "Opening the song…"
        } else {
            "Sending generation request…"
        });
        command["t"] = json!("locomo_make");
        command["id"] = json!(id);
        requests.push(command);
        requests
    }

    fn take(path: &str) -> Option<Value> {
        // Rename first so a newly submitted request cannot be removed by this read.
        let claimed = format!("{path}.agent");
        fs::rename(path, &claimed).ok()?;
        let bytes = fs::read(&claimed).ok();
        let _ = fs::remove_file(claimed);
        let bytes = bytes?;
        if bytes.is_empty() {
            return Some(json!({}));
        } // Previous Music builds
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object)
    }

    pub fn library(&self, value: &Value) {
        let Some(request) = self.library_request else {
            return;
        };
        if value["request"].as_u64() != Some(request) {
            return;
        }
        let data = value.to_string();
        if data.len() <= 16 * 1024 && fs::write("/tmp/locomo-library.next", data).is_ok() {
            let _ = fs::rename("/tmp/locomo-library.next", LIBRARY);
        }
    }

    pub fn resume(&self) -> Option<Value> {
        self.job
            .as_ref()
            .map(|id| json!({"t": "locomo_make", "id": id, "resume": true}))
    }

    pub fn status(&self, text: &str) {
        let message: String = text.chars().take(180).collect();
        if fs::write("/tmp/locomo-status.next", message).is_ok() {
            let _ = fs::rename("/tmp/locomo-status.next", STATUS);
        }
    }

    pub fn record(&mut self, message: &Value) {
        let Some(id) = self.job.as_deref() else {
            return;
        };
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
                if !(1..=MAX_BYTES as u64).contains(&size) || !(1..=600_000).contains(&duration_ms)
                {
                    return;
                }
                self.transfer = Some(Transfer {
                    title: message["title"]
                        .as_str()
                        .unwrap_or("Cool Jazz")
                        .chars()
                        .take(100)
                        .collect(),
                    duration_ms,
                    station: message["station"]
                        .as_str()
                        .unwrap_or("Cool Jazz")
                        .chars()
                        .take(60)
                        .collect(),
                    hash: message["sha256"].as_str().unwrap_or("").to_owned(),
                    audio: vec![0; size as usize],
                });
                self.status("Receiving the song…");
            }
            Some("locomo_end") => {
                let saved = self
                    .transfer
                    .take()
                    .is_some_and(|transfer| transfer.save(id));
                self.job = None;
                self.status(if saved {
                    "Song ready to play"
                } else {
                    "The song transfer failed. Try again."
                });
            }
            _ => {}
        }
    }

    pub fn part(&mut self, body: &[u8]) {
        let Some(transfer) = &mut self.transfer else {
            return;
        };
        let Some((offset, bytes)) = body.split_first_chunk::<4>() else {
            return;
        };
        let start = u32::from_le_bytes(*offset) as usize;
        if let Some(target) = transfer
            .audio
            .get_mut(start..start.saturating_add(bytes.len()))
        {
            target.copy_from_slice(bytes);
        }
    }
}

impl Transfer {
    fn save(self, id: &str) -> bool {
        if secure::hex(&secure::sha256(&[&self.audio])) != self.hash {
            return false;
        }
        let manifest = json!({"id": id, "title": self.title, "duration_ms": self.duration_ms, "station": self.station});
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
