//! The Open Runtime's capability policy: default deny.
//!
//! seL4 isolates every app in hardware and stamps every trapped system call
//! with an unforgeable badge, so the runtime always knows which app asks. What
//! the app may then do is decided here, in pure functions the host tests run
//! (runtime/open/tests/policy_host.rs, `make test-unit`); manifest.rs applies
//! them at the one place each power is granted: socket creation, the open of
//! each /dev/violet node, each /dev/violet/sys command and every path.
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
//! Agent-built app are asked of the user (`ask` answers `Ask`); the answer is
//! kept by the runtime in /data/.grants, which no app can open.
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
}

/// Only the boot image's manifests can grant these.
pub const PRIVILEGED: u8 = 1 << Cap::WifiAdmin as u8 | 1 << Cap::System as u8;
/// An Agent-built app gets these only with the user's consent (the trusted
/// UI has a prompt for each: sysui.rs `Cap::words`).
pub const CONSENT: u8 = 1 << Cap::Net as u8 | 1 << Cap::Mic as u8 | 1 << Cap::Camera as u8 | 1 << Cap::Hci as u8;

/// Where the running program's bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// /apps/NAME from the boot image, unchanged since boot.
    Image,
    /// /apps/NAME installed by the Agent (appstore.rs), as the store holds it.
    Installed,
    /// Anything else: pushed over USB, written at run time, started from
    /// outside its own /apps folder.
    Other,
}

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

/// The user's answers, kept by the runtime in /data/.grants:
/// "OWNER<TAB>allowed<TAB>denied" lines, Cap bits in hex.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Grants {
    pub rows: Vec<(String, u8, u8)>,
}

/// At most this many apps' answers are kept (the oldest go first).
const GRANT_ROWS: usize = 64;

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
            s.push_str(&alloc::format!("{}\t{:x}\t{:x}\n", o, a, d));
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

/// `policy::ask(owner, cap)`: may the app `owner`, running under `profile`,
/// use `cap`? Allow and Deny are final; Ask means the user must be asked.
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

pub const ENOENT: i32 = -2;
pub const EACCES: i32 = -13;
pub const EROFS: i32 = -30;

/// The Agent's staging file for installs (appstore.rs): system apps only.
pub const STAGED: &str = "/apps/.agent-install";

/// Is `name` (a name directly in /data) the app `owner`'s own? An app owns
/// /data/OWNER (a file, or a folder and all in it) and the files
/// /data/OWNER.ANYTHING: only names directly in /data are kept across
/// restarts (services/storage/writeback.rs), so that is where an app keeps
/// state. App names have no '.', so the owner of a name is unambiguous.
pub fn owns_data(owner: &str, name: &str) -> bool {
    !owner.is_empty() && !owner.contains(['.', '/']) && covers(owner, name)
}
/// `name` is `prefix` or `prefix.ANYTHING`.
fn covers(prefix: &str, name: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
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
