//! The Open Runtime's capability policy: an enforcement stub. Default deny.
//!
//! seL4 isolates every app in hardware and stamps every trapped system call
//! with an unforgeable badge, so the runtime always knows which app asks. What
//! the app may then do is decided by the Sentinel, a component with its own
//! address space (system/sentinel/): the manifest rules, the path namespace,
//! the capability each device node and /dev/violet/sys command needs, and
//! the user's consent answers, which it keeps and hands back only sealed
//! with a key this process never sees. manifest.rs applies the answers at
//! the one place each power is granted: socket creation, the open of each
//! /dev/violet node, each /dev/violet/sys command and every path.
//!
//! Every function here asks over IPC (system/sentinel/client.rs) and fails
//! closed: no answer, an answer to another question, a verdict that is not
//! one, or output that does not match its checksum is a refusal. Nothing in
//! this file can turn a missing answer into a yes.
//!
//! This runtime is still the enforcement point, and every input to a decision
//! (origin, manifest text, owner, path) is what it tells the Sentinel. So the
//! split does not constrain a compromised runtime, which can skip the
//! question or ask with a lie; it keeps the consent store and the sealing
//! key out of this address space (system/sentinel/README.md).
//!
//! What stays here decides nothing about a running app: the vocabulary (Cap,
//! Decision, Origin, ...) and the manifest text helpers (system/sentinel/
//! common.rs), the manifest text an app runs under (`Profile`, which the
//! Sentinel parses on every question), and the sealed text of the consent
//! store as last handed back (`Grants`), which manifest.rs writes to
//! /data/.grants. On a host build the stub is joined to the real Sentinel
//! engine in memory (system/sentinel/hostlink.rs), so host tests run the
//! shipped stub, wire format and decisions.
use alloc::string::String;
use alloc::vec::Vec;

/// The vocabulary and the wire format, shared with the Sentinel.
#[path = "../../system/sentinel/common.rs"]
pub mod wire;
#[path = "../../system/sentinel/client.rs"]
mod client;
#[cfg(not(target_os = "none"))]
#[path = "../../system/sentinel/hostlink.rs"]
pub mod port;

use wire as common;
#[allow(unused_imports)]
pub use wire::{envelope, image_manifest, owns_data, shared_by_image, split_manifest};
#[allow(unused_imports)]
pub use wire::{AccessMode, Action, AppId, Cap, Decision, Origin, CAPS, CONSENT, EACCES, ENOENT, EROFS, PRIVILEGED, STAGED};
use wire::*;

/// The kernel edge: the endpoint the supervisor put in this process's CSpace
/// and the two windows it mapped (system/supervisor/supervisor.c, "Sentinel").
#[cfg(target_os = "none")]
mod port {
    use super::client::Port;
    use super::common::{REQ_MAGIC, WINDOW};
    use crate::protocol::{Message, EXECUTE};

    /// OPEN_SLOT_SENTINEL.
    const SLOT: u32 = 9;
    /// Two pages this process writes and the Sentinel only reads.
    const REQUEST: usize = 0x0914_0000;
    /// Two pages the Sentinel writes and this process only reads.
    const REPLY: usize = 0x0914_8000;

    pub struct Wire;
    impl Port for Wire {
        fn window(&mut self) -> &mut [u8] {
            unsafe { core::slice::from_raw_parts_mut(REQUEST as *mut u8, WINDOW) }
        }
        fn call(&mut self, seq: u32, len: u32, crc: u32) -> Option<[u32; 6]> {
            // Test images only (VIOLET_SENTINEL_FAULT=1 at build time): a
            // request about a path ending in "sentinel-fault-probe" has one
            // bit changed after it was summed, as a damaged runtime would
            // send it. The Sentinel must refuse it.
            #[cfg(sentinel_fault)]
            {
                let frame = &mut self.window()[..len as usize];
                if frame.ends_with(b"sentinel-fault-probe") {
                    frame[len as usize - 1] ^= 1;
                }
            }
            let mut m = Message::new(EXECUTE, REQ_MAGIC, seq).with_arg(len);
            m.data[..4].copy_from_slice(&crc.to_le_bytes());
            let r = m.call(SLOT);
            let word = |n: usize| u32::from_le_bytes([r.data[n], r.data[n + 1], r.data[n + 2], r.data[n + 3]]);
            Some([r.op, r.key, r.value, r.arg, word(0), word(4)])
        }
        fn output(&self) -> &[u8] {
            unsafe { core::slice::from_raw_parts(REPLY as *const u8, WINDOW) }
        }
    }

    /// The Open Runtime asks from its one thread.
    static mut SEQ: u32 = 0;
    pub fn with<R>(f: impl FnOnce(&mut Wire, u32) -> R) -> R {
        let seq = unsafe {
            SEQ = SEQ.wrapping_add(1);
            SEQ
        };
        f(&mut Wire, seq)
    }
}

/// Said once: after this every question is refused the same way. Only for a
/// question that was sent: one too big to send is refused without a word.
fn unanswered(_op: u8) {
    #[cfg(target_os = "none")]
    {
        static mut TOLD: bool = false;
        if unsafe { !TOLD } {
            unsafe { TOLD = true };
            crate::log!("policy: no valid answer from the Sentinel (request {})", _op);
        }
    }
}

/// Does a request with these fields fit the window (common.rs `encode`)?
fn fits(fields: &[&[u8]]) -> bool {
    HEAD + fields.iter().map(|f| f.len()).sum::<usize>() <= WINDOW
}

/// Ask the Sentinel: its verdict and value. None is a refusal.
fn call(op: u8, a: u8, b: u8, fields: &[&[u8]]) -> Option<(u32, u32)> {
    call_out(op, a, b, fields).map(|(v, n, _)| (v, n))
}

/// As `call`, with the reply's output.
pub fn call_out(op: u8, a: u8, b: u8, fields: &[&[u8]]) -> Option<(u32, u32, Vec<u8>)> {
    use client::Port;
    if !fits(fields) {
        return None;
    }
    let r = port::with(|p, seq| {
        let r = client::transact(p, seq, op, a, b, fields)?;
        Some((r.verdict, r.value, Vec::from(&p.output()[..r.len])))
    });
    if r.is_none() {
        unanswered(op);
    }
    r
}

/// The manifest an app runs under: where its program came from and the
/// manifest's text. The Sentinel reads the text; nothing here does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub origin: Origin,
    manifest: String,
}

impl Profile {
    /// Only its own /data, the screen and touch.
    pub fn minimal(origin: Origin) -> Profile {
        Profile { origin, manifest: String::new() }
    }
    /// A manifest longer than the Sentinel reads (MANIFEST_MAX) is the
    /// minimal profile: never a request it refuses, so the app keeps its own
    /// /data and nothing else.
    pub fn parse(text: &str, origin: Origin) -> Profile {
        Profile { origin, manifest: if text.len() > MANIFEST_MAX { String::new() } else { String::from(text) } }
    }
}

/// How many apps the consent store has answers for.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Rows(u32);
impl Rows {
    pub fn len(&self) -> usize {
        self.0 as usize
    }
}

/// The user's answers to consent prompts. The Sentinel holds them; this is
/// the sealed text it last handed back, for /data/.grants.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Grants {
    pub rows: Rows,
    sealed: String,
}

impl Grants {
    /// Hand the Sentinel the saved store. Text it did not seal is not
    /// loaded: every answer is then asked again. The text read stays what
    /// is saved until the Sentinel hands back a store, so a store that did
    /// not load (a lost reply) is never overwritten with nothing.
    pub fn parse(text: &str) -> Grants {
        let rows = match call(OP_LOAD, 0, 0, &[text.as_bytes()]) {
            Some((V_ALLOW, rows)) => rows,
            _ => {
                if !text.is_empty() {
                    #[cfg(target_os = "none")]
                    crate::log!("policy: .grants not opened");
                }
                0
            }
        };
        Grants { rows: Rows(rows), sealed: String::from(text) }
    }
    /// The sealed store, to save.
    pub fn format(&self) -> String {
        self.sealed.clone()
    }
    /// Keep the user's answer.
    pub fn record(&mut self, owner: &str, cap: Cap, allow: bool) {
        self.update(OP_RECORD, cap as u8, allow as u8, owner);
    }
    pub fn forget(&mut self, owner: &str) {
        self.update(OP_FORGET, 0, 0, owner);
    }
    /// The Sentinel changes its store before it replies: if the reply is
    /// lost, ask for the store as it stands, so what is saved is what is
    /// enforced. With neither, the text held stays (never an empty one).
    fn update(&mut self, op: u8, a: u8, b: u8, owner: &str) {
        let take = |r: Option<(u32, u32, Vec<u8>)>| match r {
            Some((V_ALLOW, rows, out)) if !out.is_empty() => String::from_utf8(out).ok().map(|t| (rows, t)),
            _ => None,
        };
        if let Some((rows, sealed)) = take(call_out(op, a, b, &[owner.as_bytes()])).or_else(|| take(call_out(OP_EXPORT, 0, 0, &[]))) {
            *self = Grants { rows: Rows(rows), sealed };
        }
    }
}

/// May the app `owner`, running under `profile`, use `cap`? Allow and Deny
/// are final; Ask means the user must be asked. (`grants` is the Sentinel's.)
pub fn ask(profile: &Profile, _grants: &Grants, owner: &str, cap: Cap) -> Decision {
    match call(OP_ASK, profile.origin as u8, cap as u8, &[owner.as_bytes(), profile.manifest.as_bytes()]) {
        Some((V_ALLOW, _)) => Decision::Allow,
        Some((V_ASK, _)) => Decision::Ask,
        _ => Decision::Deny,
    }
}

/// The capability a device node needs, if it is one that needs any.
/// Err(()) for a /dev/violet node that does not exist (or no answer).
pub fn device_cap(path: &str) -> Result<Option<Cap>, ()> {
    // Not a device node: there is nothing to ask.
    if !path.starts_with("/dev/violet/") {
        return Ok(None);
    }
    match call(OP_DEVICE, 0, 0, &[path.as_bytes()]) {
        Some((V_ALLOW, cap)) => client::cap_of(cap),
        _ => Err(()),
    }
}

/// At most `max` bytes of `s`, ending on a character.
fn clip(s: &str, max: usize) -> &[u8] {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s.as_bytes()[..n]
}

/// The capability a /dev/violet/sys command needs; None for the commands
/// every app may use. The Sentinel is shown the command's first two words,
/// which name it. With no answer the command needs `system`, and the ask for
/// that is refused too.
pub fn sys_cap(line: &str) -> Option<Cap> {
    let mut w = line.split_whitespace();
    let verb = w.next()?;
    let fields = [clip(verb, 64), clip(w.next().unwrap_or(""), 64)];
    match call(OP_SYS, 0, 0, if fields[1].is_empty() { &fields[..1] } else { &fields[..] }) {
        Some((V_ALLOW, cap)) => client::cap_of(cap).unwrap_or(Some(Cap::System)),
        _ => Some(Cap::System),
    }
}

/// Does an execve of `path` by app `caller` (not Home, Settings or the
/// Agent: `core`) start another app's program? With no answer it does: the
/// new program then runs under the caller's name with the minimal profile
/// (manifest.rs `exec_identity`).
pub fn exec_crosses(caller: &str, core: bool, path: &str) -> bool {
    !matches!(call(OP_EXEC, core as u8, 0, &[caller.as_bytes(), path.as_bytes()]), Some((V_ALLOW, 0)))
}

/// May app `owner` (with `profile`) reach `path`, for writing or not? Err is
/// the errno the Sentinel chose (ENOENT, EROFS or EACCES); EACCES with no answer.
pub fn path_rule(owner: &str, profile: &Profile, path: &str, write: bool) -> Result<(), i32> {
    // Longer than the Sentinel takes: refused without asking.
    if path.len() > PATH_MAX {
        return Err(EACCES);
    }
    match call(OP_PATH, profile.origin as u8, write as u8, &[owner.as_bytes(), profile.manifest.as_bytes(), path.as_bytes()]) {
        Some((V_ALLOW, _)) => Ok(()),
        Some((V_DENY, errno)) if errno as i32 == ENOENT || errno as i32 == EROFS => Err(errno as i32),
        _ => Err(EACCES),
    }
}

/// Seal a small piece of runtime state (the Shield's saved mode) so that
/// only text the Sentinel sealed is taken back. None: nothing to save.
pub fn seal_state(label: &str, value: &[u8]) -> Option<String> {
    match call_out(OP_SEAL, 0, 0, &[label.as_bytes(), value]) {
        Some((V_ALLOW, _, out)) => String::from_utf8(out).ok(),
        _ => None,
    }
}

/// What `seal_state` protected, or None for anything else.
pub fn unseal_state(label: &str, sealed: &[u8]) -> Option<Vec<u8>> {
    match call_out(OP_UNSEAL, 0, 0, &[label.as_bytes(), sealed]) {
        Some((V_ALLOW, _, out)) => Some(out),
        _ => None,
    }
}
