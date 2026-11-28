//! The capability policy's decisions: default deny. They run in the Sentinel
//! (process.rs), in its own address space; the Open Runtime asks over IPC
//! (runtime/open/policy.rs) and refuses whatever it gets no clear yes for.
//! Pure functions, which the host tests run (runtime/open/tests/policy_host.rs
//! and tests/ here, `make test-unit`).
//!
//! An app's powers come from its manifest ("caps:" and "data:" lines):
//!   - a built-in app's is checked in next to it (apps/open/NAME/manifest.txt,
//!     packed into the image's /apps/.manifests by tools/build_initfs.py);
//!   - an Agent-built app's travels at the end of its program (`envelope`),
//!     written by tools/agent_build.py;
//!   - anything else (no manifest, a program pushed over USB, one written at
//!     run time) gets only its own /data, the screen, touch and the
//!     unprivileged /dev/violet/sys commands.
//! Privileged powers (wifi-admin, system) and shared /data files count only
//! in the boot image's manifests. Net, the microphone and the camera of an
//! Agent-built app are asked of the user (`ask` answers `Ask`); the answers
//! are kept here (`Grants`) and leave only sealed (engine.rs, seal.rs).
#![allow(dead_code)]
pub use super::common::*;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub origin: Origin,
    /// Declared capabilities (Cap bits), already limited by the origin.
    pub caps: u8,
    /// Files or folders directly in /data shared with this app (image only).
    pub shared: Vec<String>,
    /// Reserved /tmp names (".NAME", which covers /tmp/.NAME and
    /// /tmp/.NAME.*) this app may reach (image only, `tmp:` lines).
    pub tmp: Vec<String>,
}

/// A name in /data a manifest may share: not hidden, not a path.
fn share_name(n: &str) -> bool {
    (1..=23).contains(&n.len()) && !n.starts_with('.') && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Profile {
    /// Only its own /data, the screen and touch.
    pub fn minimal(origin: Origin) -> Profile {
        Profile { origin, caps: 0, shared: Vec::new(), tmp: Vec::new() }
    }
    /// A manifest's text: "caps: net audio-out" and "data: networks relay"
    /// lines, '#' comments. Unknown words and lines are ignored (they never
    /// add anything); what the origin may not hold is dropped.
    pub fn parse(text: &str, origin: Origin) -> Profile {
        let mut p = Profile::minimal(origin);
        if origin == Origin::Other {
            return p;
        }
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let Some((key, rest)) = line.split_once(':') else { continue };
            match key.trim() {
                "caps" => {
                    for c in rest.split(|c: char| c.is_whitespace() || c == ',').filter_map(Cap::parse) {
                        p.caps |= c.bit();
                    }
                }
                "data" if origin == Origin::Image => {
                    for n in rest.split(|c: char| c.is_whitespace() || c == ',').filter(|n| share_name(n)) {
                        if p.shared.len() < 8 && !p.shared.iter().any(|s| s == n) {
                            p.shared.push(String::from(n));
                        }
                    }
                }
                "tmp" if origin == Origin::Image => {
                    for n in rest.split(|c: char| c.is_whitespace() || c == ',') {
                        if n.strip_prefix('.').is_some_and(share_name) && p.tmp.len() < 8 && !p.tmp.iter().any(|s| s == n) {
                            p.tmp.push(String::from(n));
                        }
                    }
                }
                _ => {}
            }
        }
        if origin != Origin::Image {
            p.caps &= !PRIVILEGED;
        }
        p
    }
    pub fn has(&self, cap: Cap) -> bool {
        self.caps & cap.bit() != 0
    }
}

/// The user's answers: "OWNER<TAB>allowed<TAB>denied" lines, Cap bits in hex.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Grants {
    pub rows: Vec<(String, u8, u8)>,
}

/// At most this many apps' answers are kept (the oldest go first).
const GRANT_ROWS: usize = 64;

fn push_hex(s: &mut String, v: u8) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    if v >= 16 {
        s.push(DIGITS[(v >> 4) as usize] as char);
    }
    s.push(DIGITS[(v & 15) as usize] as char);
}

impl Grants {
    pub fn parse(text: &str) -> Grants {
        let mut g = Grants::default();
        for line in text.lines() {
            let mut f = line.split('\t');
            let (Some(owner), Some(a), Some(d)) = (f.next(), f.next(), f.next()) else { continue };
            let (Ok(a), Ok(d)) = (u8::from_str_radix(a, 16), u8::from_str_radix(d, 16)) else { continue };
            if !owner.is_empty() && owner.len() <= 24 && !owner.contains(char::is_whitespace) && g.rows.len() < GRANT_ROWS {
                // Never anything privileged, and a cap answered both ways is denied.
                g.rows.push((String::from(owner), a & CONSENT & !d, d & CONSENT));
            }
        }
        g
    }
    pub fn format(&self) -> String {
        let mut s = String::new();
        for (o, a, d) in &self.rows {
            s.push_str(o);
            s.push('\t');
            push_hex(&mut s, *a);
            s.push('\t');
            push_hex(&mut s, *d);
            s.push('\n');
        }
        s
    }
    /// What the user said about `cap` for `owner`, if anything.
    pub fn answer(&self, owner: &str, cap: Cap) -> Option<bool> {
        let (_, a, d) = self.rows.iter().find(|r| r.0 == owner)?;
        if d & cap.bit() != 0 {
            Some(false)
        } else if a & cap.bit() != 0 {
            Some(true)
        } else {
            None
        }
    }
    /// Keep the user's answer. Only consent capabilities are ever kept.
    pub fn record(&mut self, owner: &str, cap: Cap, allow: bool) {
        let bit = cap.bit() & CONSENT;
        if bit == 0 || owner.is_empty() || owner.len() > 24 || owner.contains(char::is_whitespace) {
            return;
        }
        let row = match self.rows.iter().position(|r| r.0 == owner) {
            Some(i) => self.rows.remove(i),
            None => (String::from(owner), 0, 0),
        };
        let (a, d) = if allow { (row.1 | bit, row.2 & !bit) } else { (row.1 & !bit, row.2 | bit) };
        if self.rows.len() >= GRANT_ROWS {
            self.rows.remove(0);
        }
        self.rows.push((row.0, a, d));
    }
    pub fn forget(&mut self, owner: &str) {
        self.rows.retain(|r| r.0 != owner);
    }
}

/// May the app `owner`, running under `profile`, use `cap`? Allow and Deny
/// are final; Ask means the user must be asked.
pub fn ask(profile: &Profile, grants: &Grants, owner: &str, cap: Cap) -> Decision {
    let bit = cap.bit();
    if profile.caps & bit == 0 {
        return Decision::Deny;
    }
    match profile.origin {
        Origin::Image => Decision::Allow,
        Origin::Installed if bit & PRIVILEGED != 0 => Decision::Deny,
        Origin::Installed if bit & CONSENT == 0 => Decision::Allow,
        Origin::Installed => match grants.answer(owner, cap) {
            Some(true) => Decision::Allow,
            Some(false) => Decision::Deny,
            None => Decision::Ask,
        },
        Origin::Other => Decision::Deny,
    }
}

/// The capability a device node needs, if it is one that needs any.
/// Err(()) for a /dev/violet node that does not exist.
pub fn device_cap(path: &str) -> Result<Option<Cap>, ()> {
    let Some(node) = path.strip_prefix("/dev/violet/") else { return Ok(None) };
    match node {
        "screen" | "events" | "sys" => Ok(None),
        "audio" => Ok(Some(Cap::AudioOut)),
        "mic" => Ok(Some(Cap::Mic)),
        "camera" => Ok(Some(Cap::Camera)),
        "hci" => Ok(Some(Cap::Hci)),
        _ => Err(()),
    }
}

/// The capability a /dev/violet/sys command needs; None for the commands
/// every app may use (home, status, get, set, log, time, wakelock, ...).
pub fn sys_cap(line: &str) -> Option<Cap> {
    let mut w = line.split_whitespace();
    match (w.next()?, w.next()) {
        ("open" | "launch" | "install" | "uninstall" | "shield-agent" | "shield-mode" | "shield-release" | "update", _) => Some(Cap::System),
        ("wifi", Some("join" | "leave")) | ("relay", Some(_)) => Some(Cap::WifiAdmin),
        ("mic", _) => Some(Cap::Mic),
        ("sound" | "volume", _) => Some(Cap::AudioOut),
        _ => None,
    }
}

/// The app folder NAME of /apps/NAME or /apps/NAME/... (shield.rs `app_folder`).
fn app_folder(path: &str) -> Option<&str> {
    path.strip_prefix("/apps/")?.split('/').next().filter(|n| !n.is_empty())
}

/// Does an execve of `path` by app `caller` (not Home, Settings or the
/// Agent: `core`) start another app's program? Then the new program is not
/// that app: it keeps the caller's owner and runs as a program from
/// outside its own folder (the minimal profile, no image identity), since
/// the caller chose its arguments, environment (PYTHONPATH, LD_PRELOAD) and
/// descriptors. Only a launch (`open`/`launch`, the user, the phone) starts
/// an app as itself.
pub fn exec_crosses(caller: &str, core: bool, path: &str) -> bool {
    !core && app_folder(path).is_some_and(|n| n != caller)
}

/// May app `owner` (with `profile`) reach `path`, for writing or not?
/// `path` is what the runtime resolved (fs::normalize): absolute, with no
/// ".", ".." or empty parts, and the file system has no links, so checking
/// its leading parts is checking the file. Anything else is refused.
/// Another app's /data names, the runtime's dot files in /data and /tmp and
/// the staging file look missing (ENOENT) to reads and writes alike, so
/// whether they exist cannot be told (an O_CREAT open is a read when the
/// file is there and a write when it is not). Other refused writes are
/// EACCES, or EROFS for the image's trees (/apps, /bin, /lib, /etc, the root).
pub fn path_rule(owner: &str, profile: &Profile, path: &str, write: bool) -> Result<(), i32> {
    let refuse = |rofs: bool| Err(if !write { ENOENT } else if rofs { EROFS } else { EACCES });
    if !path.starts_with('/') || (path.len() > 1 && path[1..].split('/').any(|p| p.is_empty() || p == "." || p == "..")) {
        return refuse(false);
    }
    let under = |root: &str| path.strip_prefix(root).filter(|r| r.is_empty() || r.starts_with('/'));
    if let Some(r) = under("/dev") {
        // Device nodes (which need `device_cap`); nothing new can be made here.
        let node = matches!(r, "/null" | "/urandom" | "/random" | "/tty" | "/stdout" | "/stderr" | "/console") || r.starts_with("/violet/");
        return if write && !node { refuse(true) } else { Ok(()) };
    }
    for root in ["/tmp", "/data"] {
        if under(root) == Some("") {
            return if write { refuse(false) } else { Ok(()) };
        }
    }
    let image = profile.origin == Origin::Image;
    if let Some(rest) = path.strip_prefix("/tmp/") {
        // Shared by all apps, except dot names: those only for the image
        // apps whose manifests name them (Settings' /tmp/.ota-chunk, the
        // Music and Agent exchange /tmp/.locomo.*).
        let name = rest.split('/').next().unwrap_or("");
        return if !name.starts_with('.') || image && profile.tmp.iter().any(|t| covers(t, name)) { Ok(()) } else { Err(ENOENT) };
    }
    if let Some(rest) = path.strip_prefix("/data/") {
        let name = rest.split('/').next().unwrap_or("");
        let shared = image && profile.shared.iter().any(|s| s == name);
        // Dot names (/data/.grants, .settings, .shield-mode) are the runtime's.
        return if !name.starts_with('.') && (owns_data(owner, name) || shared) { Ok(()) } else { Err(ENOENT) };
    }
    if path == STAGED {
        return if image && profile.has(Cap::System) { Ok(()) } else { Err(ENOENT) };
    }
    if under("/proc").is_some() {
        return if write { refuse(false) } else { Ok(()) };
    }
    if write { refuse(true) } else { Ok(()) }
}
