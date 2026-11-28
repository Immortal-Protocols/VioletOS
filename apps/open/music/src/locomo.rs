//! Exchange bounded requests and verified audio with the paired Agent.
use crate::player::Track;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};

const REQUEST: &str = "/tmp/.locomo.make";
const BROWSE: &str = "/tmp/.locomo.browse";
const LIBRARY: &str = "/tmp/.locomo.library";
pub const TRACK: &str = "/tmp/.locomo.track";

#[derive(Deserialize)]
pub struct Song {
    pub id: String,
    title: String,
    duration_ms: u64,
    #[serde(default = "default_station")]
    station: String,
}

fn default_station() -> String {
    "Cool Jazz".into()
}

impl Song {
    pub fn track(&self) -> Track {
        Track {
            number: 1,
            title: self.title.clone(),
            artist: "Locomo Music".into(),
            duration_ms: self.duration_ms,
            bpm: 0,
            key: self.station.clone(),
            audio: TRACK.into(),
            cover: String::new(),
            peaks: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
pub struct LibrarySong {
    pub id: String,
    pub title: String,
    pub station: String,
    pub duration_ms: u64,
    pub favorite: bool,
}

#[derive(Default, Deserialize)]
pub struct Library {
    pub request: u32,
    pub tracks: Vec<LibrarySong>,
    pub more: bool,
    #[serde(default)]
    pub error: String,
}

fn publish(path: &str, value: Value) -> io::Result<()> {
    let staged = format!("{path}.music");
    fs::write(&staged, value.to_string())?;
    fs::rename(staged, path)
}

pub fn generate(station: &str, length: Option<u32>, count: u32, overnight: bool) -> io::Result<()> {
    publish(
        REQUEST,
        json!({"station": station, "length": length, "count": count, "overnight": overnight}),
    )
}

pub fn control(action: &str) -> io::Result<()> {
    publish(REQUEST, json!({"control": action}))
}

pub fn play(id: &str) -> io::Result<()> {
    publish(REQUEST, json!({"track": id}))
}

pub fn browse(request: u32, query: &str, page: usize, favorites: bool) -> io::Result<()> {
    publish(
        BROWSE,
        json!({"request": request, "query": query, "page": page, "favorites": favorites}),
    )
}

pub fn library() -> Option<Library> {
    serde_json::from_slice(&fs::read(LIBRARY).ok()?).ok()
}

pub fn status() -> String {
    fs::read_to_string("/tmp/.locomo.status")
        .unwrap_or_else(|_| "Connect Violet Agent to the Mac to make songs.".into())
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
