//! What the Open Runtime's enforcement stub (runtime/open/policy.rs) and the
//! Sentinel (process.rs here) both build: the policy's vocabulary, the
//! manifest text helpers that decide nothing about a running app, and the
//! wire format of a request and its reply. No decision is made in this file.
//!
//! A request is a frame the runtime writes into the request window (two pages
//! it can write and the Sentinel can only read), announced by one IPC message
//! carrying the frame's sequence number, length and CRC-32:
//!
//!   0  u32  REQ_MAGIC            12  u16 x 4  field lengths
//!   4  u32  sequence number      20  the fields, back to back
//!   8  u8   op, u8 a, u8 b, u8 number of fields (at most 4)
//!
//! The reply is six words in the IPC message (REPLY_MAGIC, the sequence
//! number, a verdict, a value, and the length and CRC-32 of any output) and
//! the output itself in the reply window, which only the Sentinel can write.
//! The verdicts are words no zeroed or shifted message can look like.
#![allow(dead_code)]
use alloc::string::String;
use alloc::vec::Vec;

/// Unforgeable caller identity assigned by seL4 endpoint badging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
    Exec,
}

/// What Violet Shield (shield.rs) is shown of each guarded call.
#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    OpenPath { path: &'a str, mode: AccessMode },
    Connect { host: &'a str, port: u16 },
    Bind { port: u16 },
    BackgroundAudio,
    AllocMemory { total_bytes: usize },
    SpawnThread { total_threads: usize },
    SpawnApp { path: &'a str },
    SystemControl { command: &'a str },
    /// `mmap`/`mprotect` asking for pages that are writable and executable at once.
    MapWriteExec { addr: u32, len: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Returned to the app as `EACCES` (-13).
    Deny,
    /// Only the user can say: the trusted UI asks (trusted_ui.rs `request_consent`).
    Ask,
}

/// A power an app's manifest can declare. The bit numbers are those the
/// trusted UI's prompts use (sysui.rs `Cap`): Net 0, Mic 2, Camera 4, Hci 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cap {
    /// TCP and UDP sockets (and name lookups, which need one).
    Net = 0,
    /// /dev/violet/audio, `sound` and `volume`.
    AudioOut = 1,
    /// /dev/violet/mic and `mic`.
    Mic = 2,
    /// /dev/violet/camera.
    Camera = 4,
    /// /dev/violet/hci (Bluetooth).
    Hci = 5,
    /// `wifi join|leave`, `relay on|off|...`.
    WifiAdmin = 6,
    /// Home, Settings and the Agent: `open`, `launch`, `install`, `uninstall`,
    /// Shield settings and link, the Agent's staging file, listening to the
    /// microphone from behind other apps.
    System = 7,
}

/// Each capability and its word in a manifest.
pub const CAPS: [(Cap, &str); 7] = [
    (Cap::Net, "net"),
    (Cap::AudioOut, "audio-out"),
    (Cap::Mic, "mic"),
    (Cap::Camera, "camera"),
    (Cap::Hci, "hci"),
    (Cap::WifiAdmin, "wifi-admin"),
    (Cap::System, "system"),
];

impl Cap {
    pub fn bit(self) -> u8 {
        1 << self as u8
    }
    pub fn name(self) -> &'static str {
        CAPS.iter().find(|(c, _)| *c == self).map_or("?", |(_, n)| *n)
    }
    pub fn parse(token: &str) -> Option<Cap> {
        CAPS.iter().find(|(_, n)| *n == token).map(|(c, _)| *c)
    }
    /// The capability with this number on the wire, if there is one.
    pub fn from_wire(n: u8) -> Option<Cap> {
        CAPS.iter().find(|(c, _)| *c as u8 == n).map(|(c, _)| *c)
    }
}

/// Only the boot image's manifests can grant these.
pub const PRIVILEGED: u8 = 1 << Cap::WifiAdmin as u8 | 1 << Cap::System as u8;
/// An Agent-built app gets these only with the user's consent (the trusted
/// UI has a prompt for each: sysui.rs `Cap::words`).
pub const CONSENT: u8 = 1 << Cap::Net as u8 | 1 << Cap::Mic as u8 | 1 << Cap::Camera as u8 | 1 << Cap::Hci as u8;

/// Where the running program's bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Origin {
    /// /apps/NAME from the boot image, unchanged since boot.
    Image = 0,
    /// /apps/NAME installed by the Agent (appstore.rs), as the store holds it.
    Installed = 1,
    /// Anything else: pushed over USB, written at run time, started from
    /// outside its own /apps folder.
    Other = 2,
}

impl Origin {
    pub fn from_wire(n: u8) -> Option<Origin> {
        [Origin::Image, Origin::Installed, Origin::Other].into_iter().find(|o| *o as u8 == n)
    }
}

pub const ENOENT: i32 = -2;
pub const EACCES: i32 = -13;
pub const EROFS: i32 = -30;

/// The Agent's staging file for installs (appstore.rs): system apps only.
pub const STAGED: &str = "/apps/.agent-install";

/// The image's /apps/.manifests: "@NAME" on a line, then that app's manifest.
pub fn image_manifest<'a>(table: &'a str, name: &str) -> Option<&'a str> {
    let (mut at, mut found) = (0, None);
    for line in table.split_inclusive('\n') {
        if let Some(n) = line.trim_end().strip_prefix('@') {
            if let Some(start) = found {
                return Some(&table[start..at]);
            }
            if n == name {
                found = Some(at + line.len());
            }
        }
        at += line.len();
    }
    found.map(|start| &table[start..])
}

/// Does an image manifest share a /data name that app `name` would own
/// (`owns_data`)? Then no installed app may take that name.
pub fn shared_by_image(table: &str, name: &str) -> bool {
    table.lines().filter_map(|l| l.strip_prefix("data:")).flat_map(|l| l.split(|c: char| c.is_whitespace() || c == ',')).any(|s| owns_data(name, s))
}

/// Is `name` (a name directly in /data) the app `owner`'s own? An app owns
/// /data/OWNER (a file, or a folder and all in it) and the files
/// /data/OWNER.ANYTHING: only names directly in /data are kept across
/// restarts (services/storage/writeback.rs), so that is where an app keeps
/// state. App names have no '.', so the owner of a name is unambiguous.
pub fn owns_data(owner: &str, name: &str) -> bool {
    !owner.is_empty() && !owner.contains(['.', '/']) && covers(owner, name)
}
/// `name` is `prefix` or `prefix.ANYTHING`.
pub fn covers(prefix: &str, name: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// An Agent-built program carries its manifest after it: the program, the
/// manifest (UTF-8), its length (u16 LE), "VMF1". (At the end, so the
/// program still starts as itself: the Mac tells a Python app by its "#!".)
/// Returns the manifest and the program; a blob without one is all program.
pub fn envelope(data: &[u8]) -> (Option<&str>, &[u8]) {
    let Some(head) = data.strip_suffix(b"VMF1") else { return (None, data) };
    let Some((len, body)) = head.split_last_chunk::<2>().map(|(b, l)| (u16::from_le_bytes(*l) as usize, b)) else { return (Some(""), &[]) };
    match body.len().checked_sub(len).map(|at| (at, core::str::from_utf8(&body[at..]))) {
        Some((at, Ok(m))) => (Some(m), &body[..at]),
        // A damaged manifest grants nothing, and the bytes are not a program.
        _ => (Some(""), &[]),
    }
}

/// Split an Agent-built program into its manifest and the program, in
/// place: the program is the blob's head (`envelope`), so the tail goes.
pub fn split_manifest(mut blob: Vec<u8>) -> (String, Vec<u8>) {
    let (manifest, program) = envelope(&blob);
    let (manifest, keep) = (String::from(manifest.unwrap_or("")), program.len());
    blob.truncate(keep);
    (manifest, blob)
}

// ---- The wire ----

/// Bytes in the request window and in the reply window (two pages each).
pub const WINDOW: usize = 8192;
/// "SNQ1" and "SNP1".
pub const REQ_MAGIC: u32 = 0x3151_4e53;
pub const REPLY_MAGIC: u32 = 0x3150_4e53;
pub const HEAD: usize = 20;
pub const FIELDS: usize = 4;
/// The longest manifest, owner and path a request may carry. A request that
/// does not fit is never sent, and the stub answers as if refused.
pub const MANIFEST_MAX: usize = 2048;
pub const OWNER_MAX: usize = 64;
pub const PATH_MAX: usize = 4096;
/// The most a SEAL may protect, so its sealed text fits the reply window.
pub const SEAL_MAX: usize = 3500;

/// May `owner` use capability `b`? a: origin; fields: owner, manifest.
pub const OP_ASK: u8 = 1;
/// May `owner` reach a path? a: origin, b: 1 to write; fields: owner, manifest, path.
pub const OP_PATH: u8 = 2;
/// The capability a /dev/violet node needs. field: path.
pub const OP_DEVICE: u8 = 3;
/// The capability a /dev/violet/sys command needs. fields: its first word
/// and, if there is one, its second.
pub const OP_SYS: u8 = 4;
/// Does an execve start another app's program? a: 1 for a core app; fields: caller, path.
pub const OP_EXEC: u8 = 5;
/// Load the sealed consent store. field: its text (empty: start with none).
pub const OP_LOAD: u8 = 6;
/// The user answered a prompt. a: capability, b: 1 for yes; field: owner.
pub const OP_RECORD: u8 = 7;
/// Forget an app's answers. field: owner.
pub const OP_FORGET: u8 = 8;
/// The sealed consent store as it stands (output).
pub const OP_EXPORT: u8 = 9;
/// Seal a small piece of runtime state. fields: label, value. Output: sealed text.
pub const OP_SEAL: u8 = 10;
/// Open what OP_SEAL made. fields: label, sealed text. Output: the value.
pub const OP_UNSEAL: u8 = 11;
/// The on-device scorer's turn (scoring.rs). fields: header, subjects, events.
pub const OP_SCORE: u8 = 12;

/// "ALLW", "DENY", "ASK?" and "BAD!" (the request was not understood: the
/// stub treats it as a refusal).
pub const V_ALLOW: u32 = 0x574c_4c41;
pub const V_DENY: u32 = 0x594e_4544;
pub const V_ASK: u32 = 0x3f4b_5341;
pub const V_BAD: u32 = 0x2144_4142;

/// CRC-32 (IEEE), bit by bit: requests are a few hundred bytes.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut v = !0u32;
    for b in bytes {
        v ^= u32::from(*b);
        for _ in 0..8 {
            v = (v >> 1) ^ (0u32.wrapping_sub(v & 1) & 0xedb8_8320);
        }
    }
    !v
}

/// Write a request frame into `window`; its length, or None if it does not fit.
pub fn encode(window: &mut [u8], seq: u32, op: u8, a: u8, b: u8, fields: &[&[u8]]) -> Option<usize> {
    let total = HEAD + fields.iter().map(|f| f.len()).sum::<usize>();
    if fields.len() > FIELDS || total > window.len().min(WINDOW) || fields.iter().any(|f| f.len() > u16::MAX as usize) {
        return None;
    }
    window[..4].copy_from_slice(&REQ_MAGIC.to_le_bytes());
    window[4..8].copy_from_slice(&seq.to_le_bytes());
    window[8..12].copy_from_slice(&[op, a, b, fields.len() as u8]);
    let mut at = HEAD;
    for n in 0..FIELDS {
        let f = fields.get(n).copied().unwrap_or(&[]);
        window[12 + 2 * n..14 + 2 * n].copy_from_slice(&(f.len() as u16).to_le_bytes());
        window[at..at + f.len()].copy_from_slice(f);
        at += f.len();
    }
    Some(total)
}

pub struct Request<'a> {
    pub seq: u32,
    pub op: u8,
    pub a: u8,
    pub b: u8,
    pub count: usize,
    pub fields: [&'a [u8]; FIELDS],
}

impl<'a> Request<'a> {
    /// Read a frame. Everything must add up exactly: the magic, the number of
    /// fields, no length past the frame, no bytes left over, no length set
    /// for a field that is not there.
    pub fn decode(frame: &'a [u8]) -> Option<Request<'a>> {
        if frame.len() < HEAD || frame.len() > WINDOW || frame[..4] != REQ_MAGIC.to_le_bytes() {
            return None;
        }
        let count = frame[11] as usize;
        if count > FIELDS {
            return None;
        }
        let mut fields: [&[u8]; FIELDS] = [&[]; FIELDS];
        let mut at = HEAD;
        for n in 0..FIELDS {
            let len = u16::from_le_bytes([frame[12 + 2 * n], frame[13 + 2 * n]]) as usize;
            if n >= count && len != 0 {
                return None;
            }
            fields[n] = frame.get(at..at.checked_add(len)?)?;
            at += len;
        }
        if at != frame.len() {
            return None;
        }
        Some(Request { seq: u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]), op: frame[8], a: frame[9], b: frame[10], count, fields })
    }
    /// Field `n` as text, at most `max` bytes.
    pub fn text(&self, n: usize, max: usize) -> Option<&'a str> {
        let f = self.fields[n];
        if n >= self.count || f.len() > max {
            return None;
        }
        core::str::from_utf8(f).ok()
    }
}

/// The six reply words.
pub fn reply_words(seq: u32, verdict: u32, value: u32, output: &[u8]) -> [u32; 6] {
    [REPLY_MAGIC, seq, verdict, value, output.len() as u32, crc32(output)]
}

// ---- The scorer's turn (OP_SCORE) ----

/// Header: mode u8, more u8 (1: take the events in, score later), two spare
/// bytes, idle seconds u32, now in ns u64.
pub const SCORE_HEAD: usize = 16;
/// A subject: pid u32, hash prefix [4], flags u8, role u8, name length u8, a
/// spare byte, name [24].
pub const SCORE_SUBJECT: usize = 36;
/// An event: the 64-byte telemetry record.
pub const SCORE_EVENT: usize = 64;
/// Events per request, so header, twelve subjects and events fit the window.
pub const SCORE_EVENTS_MAX: usize = 100;
/// Output header: windows scored u32, warned u32, model id u32, pending u8
/// (a window still waits for its turn), count u8, two spare bytes. Then per
/// result: pid u32, action u8, enforced u8, verdict u8, spare, margin i32,
/// and the 64-byte LocalScore event to record.
pub const SCORE_OUT_HEAD: usize = 16;
/// A result's action (scorer.rs NONE, WARN, QUARANTINE, FREEZE) and how long
/// a freeze lasts; scoring.rs holds these to the scorer's own.
pub const SCORE_QUARANTINE: u8 = 2;
pub const SCORE_FREEZE: u8 = 3;
pub const SCORE_FREEZE_SECS: u32 = 30;
pub const SCORE_OUT_ITEM: usize = 12 + SCORE_EVENT;

/// A request's header (SCORE_HEAD) as guard.rs sends it.
pub fn score_request_head(mode: u8, more: u8, idle: u32, now: u64) -> [u8; SCORE_HEAD] {
    let mut head = [0u8; SCORE_HEAD];
    head[0] = mode;
    head[1] = more;
    head[4..8].copy_from_slice(&idle.to_le_bytes());
    head[8..].copy_from_slice(&now.to_le_bytes());
    head
}

/// The runtime's side of a scorer turn (guard.rs `score_tick`): `events` (whole
/// encoded records) go to `send` at most SCORE_EVENTS_MAX at a time, every
/// request but the last with more = 1. The last reply's output, or None as
/// soon as one request has no valid answer (nothing more is sent then).
pub fn score_turn(events: &[u8], mut send: impl FnMut(u8, &[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut rest = events;
    while rest.len() > SCORE_EVENTS_MAX * SCORE_EVENT {
        let (ahead, later) = rest.split_at(SCORE_EVENTS_MAX * SCORE_EVENT);
        send(1, ahead)?;
        rest = later;
    }
    send(0, rest)
}

/// A reply's header: windows scored, warned, the model id, and whether a
/// window still waits. None for output too short to have one.
pub fn score_head(out: &[u8]) -> Option<(u32, u32, u32, bool)> {
    let h = out.get(..SCORE_OUT_HEAD)?;
    let word = |n: usize| u32::from_le_bytes([h[n], h[n + 1], h[n + 2], h[n + 3]]);
    Some((word(0), word(4), word(8), h[12] != 0))
}

/// One result the runtime carries out: warn, quarantine or freeze.
pub struct ScoreResult<'a> {
    pub pid: u32,
    pub action: u8,
    pub verdict: u8,
    pub margin: i32,
    /// The 64-byte LocalScore record to emit.
    pub record: &'a [u8],
}

/// The results in a reply's output, whole items only, those whose action is
/// one of warn, quarantine, freeze (anything else is skipped).
pub fn score_results(out: &[u8]) -> impl Iterator<Item = ScoreResult<'_>> {
    out.get(SCORE_OUT_HEAD..).unwrap_or(&[]).chunks_exact(SCORE_OUT_ITEM).filter(|i| (1..=SCORE_FREEZE).contains(&i[4])).map(|i| ScoreResult {
        pid: u32::from_le_bytes([i[0], i[1], i[2], i[3]]),
        action: i[4],
        verdict: i[6],
        margin: i32::from_le_bytes([i[8], i[9], i[10], i[11]]),
        record: &i[12..],
    })
}
