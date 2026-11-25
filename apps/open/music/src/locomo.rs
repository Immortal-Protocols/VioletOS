//! The paired Agent publishes one verified song for this QEMU session.
use crate::player::Track;
use serde::Deserialize;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};

const REQUEST: &str = "/tmp/locomo-make";
pub const TRACK: &str = "/tmp/locomo-track";

#[derive(Deserialize)]
pub struct Song {
    pub id: String,
    title: String,
    duration_ms: u64,
}

impl Song {
    pub fn track(&self) -> Track {
        Track {
            number: 1,
            title: self.title.clone(),
            artist: "Locomo Music".into(),
            duration_ms: self.duration_ms,
            bpm: 0,
            key: "Cool Jazz".into(),
            audio: TRACK.into(),
            cover: String::new(),
            peaks: Vec::new(),
        }
    }
}

pub fn request() -> io::Result<()> {
    OpenOptions::new().write(true).create_new(true).open(REQUEST).map(|_| ())
}

pub fn status() -> String {
    std::fs::read_to_string("/tmp/locomo-status").unwrap_or_default()
}

pub fn open() -> io::Result<(Song, BufReader<File>)> {
    let mut file = BufReader::new(File::open(TRACK)?);
    let mut header = Vec::new();
    (&mut file).take(1024).read_until(b'\n', &mut header)?;
    if header.last() != Some(&b'\n') {
        return Err(io::Error::other("incomplete Locomo track header"));
    }
    let song = serde_json::from_slice(&header)?;
    Ok((song, file))
}
