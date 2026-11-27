//! Violet Shield: live, signed, TTL-bounded rules and security telemetry
//! inside the Open Runtime (docs/violet-shield-rfc.md).
//!
//! Every externally visible action an app takes already reaches the Open
//! Runtime as a trapped Linux system call stamped with the app's seL4 badge.
//! `ShieldPolicy` sits at that choke point: with no active Shields it allows
//! everything, exactly like `OpenPolicy`, and only records the few
//! security-relevant transitions (files outside the system tree, connects,
//! bytes sent, W+X memory, program starts) as 64-byte `TelemetryEvent`s.
//!
//! Three layers are asked about every action:
//!
//! 1. Built-in baseline rules (`baseline`): compiled in, never expire, cannot
//!    be revoked or replaced over the link. They deny writes to another app's
//!    program, to /bin and /lib, and to the Shield's own state. A write is
//!    whatever changes the file: `open_mode` reads open()'s flags that way,
//!    and descriptors are held to what their open asked for (`fd_writes`,
//!    `write_refused`).
//! 2. Quarantine: an app that tripped a rate limit, the on-device scorer
//!    (scorer.rs) or a signed command loses the network and other apps'
//!    files for a bounded time, and keeps running otherwise.
//! 3. Live Shields, signed by the companion and verified by the Sentinel
//!    (services/sentinel) before `install_rule`. A rule's verdict can deny the
//!    call (EACCES), freeze the app, or end it. Rules expire on their own.
//!
//! When several object, the strongest verdict wins (end > freeze > deny), so
//! a signed freeze still fires on a call a built-in rule refuses.
//!
//! After those, three rate limits watch what a background app does (new
//! connections, other apps' files, bytes sent after reading another app's
//! data). `Mode::Monitor` computes all of it and only reports what it would
//! have done ("would-deny", "would-freeze", "would-quarantine"), signed rules
//! included; `Mode::Enforce` acts.
//!
//! Who a program is (`Subject::derive`): the installed app whose folder it
//! was started from (/apps/NAME), else the app that started it. Rate limits,
//! quarantine and the /data/NAME folder go by that, never by the file's own
//! name or contents, so a renamed or modified copy is still the same app.
//!
//! Everything in this file is pure (no seL4 calls), so the host unit tests
//! in `make test-unit` run the same code the phone does.
//!
//! Path and sockaddr arguments are copied out of the app once, before the
//! check (`Runtime::at` and `read_sockaddr` return owned copies), and the
//! service is handed that same copy, so another app thread cannot change the
//! argument between the check and its use.
use super::policy::{AccessMode, Action, AppId};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

/// Maximum active live Shields loaded concurrently in `OpenRuntime`.
pub const MAX_ACTIVE_SHIELDS: usize = 32;
/// Events kept for the Sentinel between its polls.
pub const TELEMETRY_CAPACITY: usize = 512;

/// Built-in Shield ids. Ids below `FIRST_COMPANION_ID` belong to the phone:
/// a signed rule can neither take one of them nor revoke one.
pub const ID_APP_CODE: u32 = 1;
pub const ID_SYSTEM_FILES: u32 = 2;
pub const ID_SHIELD_STATE: u32 = 3;
pub const ID_QUARANTINE: u32 = 4;
pub const ID_RATE_CONNECT: u32 = 5;
pub const ID_RATE_CROSS_READ: u32 = 6;
pub const ID_RATE_EXFIL: u32 = 7;
pub const ID_SCORER: u32 = 8;
pub const FIRST_COMPANION_ID: u32 = 16;

/// The Shield's own files (the saved mode). No app may write here.
pub const STATE_PREFIX: &str = "/data/.shield";

/// Rate limits for an app that is not in front. Hand-picked, not measured on
/// real traffic (docs/violet-shield-poc.md).
pub const CONNECT_PER_MIN: u32 = 30;
pub const CONNECT_BURST: u32 = 30;
pub const CROSS_READ_PER_MIN: u32 = 20;
pub const CROSS_READ_BURST: u32 = 40;
/// Bytes sent in the background by an app that has read another app's data
/// (TCP, UDP and name lookups). The mark does not wear off with time: waiting
/// is free for an attacker. It lasts until the phone restarts.
pub const EXFIL_PER_MIN: u32 = 64 * 1024;
pub const EXFIL_BURST: u32 = 256 * 1024;
/// Quarantine lengths for the first and second offence; the third lasts
/// until someone releases it (or the phone restarts).
pub const QUARANTINE_SECS: [u32; 2] = [30, 300];
pub const FOREVER: u64 = u64::MAX;
/// Apps the rate limiter and quarantine remember at once.
pub const MAX_GUARDS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Report what would have been done; block nothing.
    Monitor,
    /// Baseline rules apply to every app. Rate limits and the scorer contain
    /// programs that are not part of the verified boot image.
    Enforce,
    /// As `Enforce`, and image apps other than Home, Settings and the Agent
    /// can be contained too.
    Strict,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        Some(match text.trim() {
            "monitor" => Mode::Monitor,
            "enforce" => Mode::Enforce,
            "strict" => Mode::Strict,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Monitor => "monitor",
            Mode::Enforce => "enforce",
            Mode::Strict => "strict",
        }
    }
    /// The mode an image starts in: `VIOLET_SHIELD_MODE` at build time, else
    /// enforcing. Anything unreadable also means enforcing.
    pub fn build_default() -> Mode {
        option_env!("VIOLET_SHIELD_MODE").and_then(Mode::parse).unwrap_or(Mode::Enforce)
    }
    /// The mode a restart begins in. The saved file is not authenticated
    /// (whoever holds the storage can write it), so it may only make the
    /// build's mode stricter, never weaker: a choice of `Monitor` on an
    /// enforcing build lasts until the phone restarts.
    pub fn at_boot(build: Mode, saved: Option<Mode>) -> Mode {
        match saved {
            Some(s) if s as u8 > build as u8 => s,
            _ => build,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldVerdict {
    Allow,
    /// Allow the call, but flag the event for immediate delivery.
    LogAlert { shield_id: u32 },
    /// Reject with POSIX `EACCES` (-13).
    Deny { shield_id: u32 },
    /// Suspend every thread of the app (the calling one stays blocked in its
    /// call), keeping its memory for inspection.
    FreezeApp { shield_id: u32 },
    /// End the app and return its memory.
    KillApp { shield_id: u32 },
}

impl ShieldVerdict {
    /// Which of two verdicts wins: end > freeze > deny > log > allow.
    fn rank(&self) -> u8 {
        match self {
            ShieldVerdict::Allow => 0,
            ShieldVerdict::LogAlert { .. } => 1,
            ShieldVerdict::Deny { .. } => 2,
            ShieldVerdict::FreezeApp { .. } => 3,
            ShieldVerdict::KillApp { .. } => 4,
        }
    }
    pub fn shield_id(&self) -> u32 {
        match *self {
            ShieldVerdict::Allow => 0,
            ShieldVerdict::LogAlert { shield_id }
            | ShieldVerdict::Deny { shield_id }
            | ShieldVerdict::FreezeApp { shield_id }
            | ShieldVerdict::KillApp { shield_id } => shield_id,
        }
    }
}

/// What a declarative `VSH1` rule matches (v1; no bytecode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleMatcher {
    /// Opening a path under `prefix` (writes only, if `write_only`; another
    /// app's folder only, if `cross_app_only`).
    PathPrefix { app: Option<AppId>, prefix: String, write_only: bool, cross_app_only: bool },
    /// Connecting to a host whose name ends with `host_suffix` (empty: any host)
    /// on `port` (0: any port).
    NetTarget { app: Option<AppId>, host_suffix: String, port: u16 },
    /// Asking for writable and executable memory at once.
    WriteExecMemory { app: Option<AppId> },
    /// Any action by a program whose SHA-256 starts with these bytes.
    BinaryHash { sha256_prefix: [u8; 4] },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShieldRule {
    pub shield_id: u32,
    /// Monotonic expiry in nanoseconds since boot; 0 = permanent (owner-signed only).
    pub expires_at_ns: u64,
    pub matcher: RuleMatcher,
    pub on_match: ShieldVerdict,
}

/// High-signal event kinds streamed to the Sentinel. Boring calls
/// (`clock_gettime`, `futex`, reads of /lib, /bin, /etc) are never recorded.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Exec = 1,
    OpenRead = 2,
    NetConnect = 3,
    NetTxSummary = 4,
    MemoryWx = 5,
    SpawnThread = 6,
    SysControl = 7,
    Exit = 8,
    ShieldTriggered = 9,
    OpenWrite = 10,
    Frozen = 11,
    Thawed = 12,
    Quarantined = 13,
    Released = 14,
    /// The on-device scorer's verdict for an app's window (scorer.rs).
    LocalScore = 15,
}

impl EventKind {
    /// The `violet-log-v1` name (tools/shield-trainer/schema.py).
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Exec => "EXEC",
            EventKind::OpenRead => "OPEN_R",
            EventKind::OpenWrite => "OPEN_W",
            EventKind::NetConnect => "SOCK_CONN",
            EventKind::NetTxSummary => "SOCK_TX",
            EventKind::MemoryWx => "MEM_WX",
            EventKind::SpawnThread => "SPAWN_THREAD",
            EventKind::SysControl => "SYS_CTL",
            EventKind::Exit => "EXIT",
            EventKind::ShieldTriggered => "SHIELD_HIT",
            EventKind::Frozen => "FROZEN",
            EventKind::Thawed => "THAWED",
            EventKind::Quarantined => "QUARANTINED",
            EventKind::Released => "RELEASED",
            EventKind::LocalScore => "SCORE",
        }
    }
}

pub const FLAG_BACKGROUND: u8 = 1 << 0;
pub const FLAG_CROSS_APP: u8 = 1 << 1;
pub const FLAG_NEW_HASH: u8 = 1 << 2;
pub const FLAG_HIGH_PRIORITY: u8 = 1 << 3;
pub const FLAG_UDP: u8 = 1 << 5;
pub const FLAG_DEV_PUSH: u8 = 1 << 4;

/// Compact 64-byte record, as laid out in the RFC.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TelemetryEvent {
    pub timestamp_us: u64,
    pub app_id: u32,
    pub kind: u8,
    pub flags: u8,
    pub port: u16,
    /// Bytes sent, shield id, or the program's SHA-256 prefix (Exec).
    pub aux_u32: u32,
    pub ipv4: [u8; 4],
    /// Truncated path, host name, or argv.
    pub detail: [u8; 40],
}
const _: () = assert!(core::mem::size_of::<TelemetryEvent>() == 64);

impl TelemetryEvent {
    pub fn new(now_ns: u64, app: AppId, kind: EventKind, flags: u8, detail: &[u8]) -> TelemetryEvent {
        let mut d = [0u8; 40];
        let n = detail.len().min(d.len());
        d[..n].copy_from_slice(&detail[..n]);
        TelemetryEvent {
            timestamp_us: now_ns / 1000,
            app_id: app.0,
            kind: kind as u8,
            flags,
            port: 0,
            aux_u32: 0,
            ipv4: [0; 4],
            detail: d,
        }
    }
    pub fn detail_str(&self) -> &str {
        let end = self.detail.iter().position(|&b| b == 0).unwrap_or(self.detail.len());
        // Truncation may split a UTF-8 sequence; keep the valid prefix.
        match core::str::from_utf8(&self.detail[..end]) {
            Ok(s) => s,
            Err(e) => core::str::from_utf8(&self.detail[..e.valid_up_to()]).unwrap_or(""),
        }
    }
}

/// Bounded event queue drained by the Sentinel. The Open Runtime is single
/// threaded, so producer and consumer never run at once; on overflow the
/// oldest low-priority record goes first.
pub struct TelemetryRing {
    events: VecDeque<TelemetryEvent>,
    pub dropped: u32,
}

impl TelemetryRing {
    pub fn new() -> TelemetryRing {
        TelemetryRing { events: VecDeque::new(), dropped: 0 }
    }
    pub fn emit(&mut self, ev: TelemetryEvent) {
        if self.events.len() >= TELEMETRY_CAPACITY {
            let victim = self.events.iter().position(|e| e.flags & FLAG_HIGH_PRIORITY == 0).unwrap_or(0);
            self.events.remove(victim);
            self.dropped = self.dropped.wrapping_add(1);
        }
        self.events.push_back(ev);
    }
    pub fn drain(&mut self) -> Vec<TelemetryEvent> {
        self.events.drain(..).collect()
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
}

/// Who is acting, with what we know about the program.
#[derive(Clone, Copy, Debug)]
pub struct Subject {
    pub app: AppId,
    pub sha256_prefix: [u8; 4],
    /// FLAG_BACKGROUND / FLAG_NEW_HASH / FLAG_DEV_PUSH for this app right now.
    pub flags: u8,
    /// The app this program belongs to (`owner_of`): its /data/NAME folder is
    /// its own, and its rate limits and quarantine go by this name.
    pub name: [u8; 24],
    pub name_len: u8,
    /// ROLE_* bits.
    pub role: u8,
}

/// The app that installs apps (/apps/agent from the image): may write under /apps.
pub const ROLE_INSTALLER: u8 = 1 << 0;
/// The program was started from /apps/NAME, so /apps/NAME is its own folder.
/// (A program started from elsewhere, whatever its name, is not.)
pub const ROLE_OWNS_APP: u8 = 1 << 1;
/// Home, Settings or the Agent, from the boot image: never contained locally.
pub const ROLE_CORE: u8 = 1 << 2;

/// The three programs with a role (guard.rs checks these against the runtime's).
pub const CORE_HOME: &str = "/apps/home";
pub const CORE_SETTINGS: &str = "/apps/settings";
pub const CORE_INSTALLER: &str = "/apps/agent";

/// The NAME of /apps/NAME or /apps/NAME/..., if `path` is inside an app's folder.
fn app_folder(path: &str) -> Option<&str> {
    let first = path.strip_prefix("/apps/")?.split('/').next()?;
    (!first.is_empty()).then_some(first)
}

/// The app a program started from `path` belongs to. A program inside
/// /apps/NAME is NAME. Any other program (in /data, /tmp, /bin...) belongs to
/// the app that started it (`parent`: that app's owner, None when the phone,
/// the user or a core app started it), so a copy an app makes of itself and
/// starts is still that app. With no such parent it is its own, unnamed app
/// ("~" and its hash): it owns no folder in /data.
pub fn owner_of(path: &str, parent: Option<&str>, sha256_prefix: [u8; 4]) -> String {
    match (app_folder(path), parent) {
        (Some(name), _) => String::from(name),
        (None, Some(p)) if !p.is_empty() => String::from(p),
        _ => alloc::format!("~{:08x}", u32::from_be_bytes(sha256_prefix)),
    }
}

/// Is the app in front, as far as the Shield is concerned? An app that put
/// itself (or was put by another app) in front counts as in the background
/// until the user touches the screen while it is there: being in front is
/// only evidence of the user's intent when the user had a hand in it.
/// `pushed`: (pid, when) of the last app brought forward by a non-core app.
pub fn in_front(shown: bool, pid: u32, pushed: (u32, u64), last_touch_ns: u64) -> bool {
    shown && !(pushed.0 == pid && last_touch_ns <= pushed.1)
}

impl Subject {
    /// Everything the Shield concludes about a running program, from facts
    /// the runtime holds: the path it was started from, its owner
    /// (`owner_of`), its hash, whether its file came with the boot image and
    /// was not written since (`image_file`), and whether it is in front
    /// (`in_front`). Roles and the image exemption need the program to be
    /// inside /apps: files a package adds to /bin or /lib after boot are not
    /// distinguishable from the image's there, so nothing started from
    /// outside /apps is exempt.
    pub fn derive(pid: u32, path: &str, owner: &str, sha256_prefix: [u8; 4], image_file: bool, front: bool) -> Subject {
        let mut name = [0u8; 24];
        let n = owner.len().min(24);
        name[..n].copy_from_slice(&owner.as_bytes()[..n]);
        let image = image_file && app_folder(path).is_some();
        let mut flags = 0;
        if !front {
            flags |= FLAG_BACKGROUND;
        }
        if !image {
            flags |= FLAG_NEW_HASH;
        }
        let mut role = 0;
        if app_folder(path) == Some(owner) {
            role |= ROLE_OWNS_APP;
        }
        if image {
            if path == CORE_INSTALLER {
                role |= ROLE_INSTALLER | ROLE_CORE;
            } else if path == CORE_HOME || path == CORE_SETTINGS {
                role |= ROLE_CORE;
            }
        }
        Subject { app: AppId(pid), sha256_prefix, flags, name, name_len: n as u8, role }
    }

    /// Identity for rate limits and quarantine: the owning app. Not the
    /// program's hash, so rewriting or copying the program changes nothing.
    pub fn key(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &self.name[..self.name_len as usize] {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
}

/// The system folders apps read on every start; reads there are noise.
fn quiet_read(path: &str, own: &str) -> bool {
    for p in ["/lib/", "/bin/", "/etc/", "/dev/", "/proc/", "/usr/"] {
        if path.starts_with(p) {
            return true;
        }
    }
    // An app reading its own program folder.
    app_folder(path) == Some(own)
}

/// Is `path` in another app's /data or /apps folder? In /data an app owns
/// the folder /data/NAME and the files /data/NAME and /data/NAME.* (only
/// files directly in /data are kept across restarts, so that is where an
/// app's saved state lives; app names hold no '.').
pub fn cross_app(path: &str, own: &str) -> bool {
    if let Some(rest) = path.strip_prefix("/data/") {
        let first = rest.split('/').next().unwrap_or("");
        if first.is_empty() || first == own {
            return false;
        }
        let flat = first.len() == rest.len();
        return !(flat && !own.is_empty() && first.strip_prefix(own).map_or(false, |ext| ext.starts_with('.')));
    }
    app_folder(path).map_or(false, |first| first != own)
}

/// Linux open(2) flags the Shield reads (the same on ARM).
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0o100;
pub const O_TRUNC: u32 = 0o1000;

/// What an open() with `flags` does to the file, for the rules: a write if
/// it asks for write access, truncates, or creates a file that is not there.
/// (O_RDONLY|O_TRUNC truncates on Linux and here.)
pub fn open_mode(flags: u32, exists: bool) -> AccessMode {
    if flags & O_ACCMODE != 0 || flags & O_TRUNC != 0 || flags & O_CREAT != 0 && !exists {
        AccessMode::Write
    } else {
        AccessMode::Read
    }
}
/// Can a descriptor opened with `flags` be read? (O_RDONLY or O_RDWR.) An
/// O_RDWR open is checked as a read and as a write.
pub fn fd_reads(flags: u32) -> bool {
    flags & O_ACCMODE != 1
}
/// May a descriptor opened with `flags` be written or truncated? The rules
/// saw only what open() asked for, so the descriptor is held to it.
pub fn fd_writes(flags: u32) -> bool {
    matches!(flags & O_ACCMODE, 1 | 2)
}

/// Integer token bucket: `per_min` tokens a minute, at most `burst` saved up.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub tokens: u32,
    at_ns: u64,
}

impl Bucket {
    pub fn full(now_ns: u64, burst: u32) -> Bucket {
        Bucket { tokens: burst, at_ns: now_ns }
    }
    /// Take `cost` tokens if they are there.
    pub fn take(&mut self, now_ns: u64, cost: u32, per_min: u32, burst: u32) -> bool {
        const MIN_NS: u64 = 60_000_000_000;
        let add = if per_min == 0 { 0 } else { (u128::from(now_ns.saturating_sub(self.at_ns)) * u128::from(per_min) / u128::from(MIN_NS)) as u64 };
        if add > 0 {
            let room = u64::from(burst.saturating_sub(self.tokens));
            if add >= room {
                // Full: time spent full earns nothing later.
                self.tokens = self.tokens.max(burst);
                self.at_ns = now_ns;
            } else {
                // Keep the part of a token already earned (ceiling, so never ahead of `now`).
                self.tokens += add as u32;
                self.at_ns += ((u128::from(add) * u128::from(MIN_NS) + u128::from(per_min) - 1) / u128::from(per_min)) as u64;
                self.at_ns = self.at_ns.min(now_ns);
            }
        }
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }
}

/// What the Shield remembers about one app (`Subject::key`): its rate
/// limits and whether it is quarantined. Kept across restarts of the app and
/// shared by every program it starts, so neither starting it again nor
/// starting a changed copy lifts a quarantine or refills a limit.
#[derive(Clone, Debug)]
pub struct Guard {
    pub key: u64,
    /// The newest process of this program.
    pub pid: u32,
    name: [u8; 24],
    name_len: u8,
    connect: Bucket,
    cross: Bucket,
    exfil: Bucket,
    /// When it last read another app's data (0: never since the phone started).
    cross_read_ns: u64,
    /// Quarantined until then (0: not quarantined; `FOREVER`: until released).
    pub until_ns: u64,
    /// The Shield that quarantined it.
    pub reason: u32,
    /// Offences so far; picks the next quarantine's length.
    pub level: u8,
    offence_ns: u64,
    last_ns: u64,
    shadow_ns: u64,
}

impl Guard {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
    pub fn quarantined(&self, now_ns: u64) -> bool {
        self.until_ns == FOREVER || now_ns < self.until_ns
    }
}

/// Something the runtime should log and show: an app was just quarantined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notice {
    pub pid: u32,
    pub shield_id: u32,
    /// 0: until released.
    pub secs: u32,
}

pub struct ShieldPolicy {
    pub rules: Vec<Option<ShieldRule>>,
    pub ring: TelemetryRing,
    pub mode: Mode,
    pub guards: Vec<Guard>,
    pub notice: Option<Notice>,
    /// Calls a built-in rule or a quarantine refused.
    pub denied: u32,
    /// Things `Monitor` (or an exemption) let through that would have been stopped.
    pub shadow_hits: u32,
    pub quarantines: u32,
    /// The saved mode has been read (the runtime does that on its first turn).
    pub loaded: bool,
    /// The app last brought to the front by a non-core app, and when (`in_front`).
    pub pushed: (u32, u64),
}

/// The built-in rule `action` breaks, or 0. These need no companion and no
/// table slot. Paths are already normalized (no "..", no doubled slashes).
pub fn baseline(who: &Subject, action: &Action<'_>) -> u32 {
    let Action::OpenPath { path, mode } = action else { return 0 };
    if *mode == AccessMode::Read {
        return 0;
    }
    if path.starts_with(STATE_PREFIX) {
        return ID_SHIELD_STATE;
    }
    if under(path, "/bin") || under(path, "/lib") {
        return ID_SYSTEM_FILES;
    }
    if *path == "/apps" {
        return ID_APP_CODE;
    }
    if path.starts_with("/apps/") {
        if who.role & ROLE_INSTALLER != 0 {
            return 0;
        }
        if app_folder(path) != Some(who.name()) || who.role & ROLE_OWNS_APP == 0 {
            return ID_APP_CODE;
        }
    }
    0
}

fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).map_or(false, |rest| rest.is_empty() || rest.starts_with('/'))
}

/// What a quarantined app may not do: reach the network or another app's files.
fn cut_off(who: &Subject, action: &Action<'_>) -> bool {
    match action {
        Action::Connect { .. } => true,
        Action::OpenPath { path, .. } => cross_app(path, who.name()),
        _ => false,
    }
}

impl ShieldPolicy {
    pub fn new() -> ShieldPolicy {
        ShieldPolicy::with_mode(Mode::build_default())
    }

    pub fn with_mode(mode: Mode) -> ShieldPolicy {
        ShieldPolicy {
            rules: (0..MAX_ACTIVE_SHIELDS).map(|_| None).collect(),
            ring: TelemetryRing::new(),
            mode,
            guards: Vec::new(),
            notice: None,
            denied: 0,
            shadow_hits: 0,
            quarantines: 0,
            loaded: false,
            pushed: (0, 0),
        }
    }

    pub fn active(&self) -> usize {
        self.rules.iter().flatten().count()
    }

    /// Leaving enforcement lifts every quarantine. (The runtime also thaws
    /// what was frozen; signed rules stay loaded and only report.)
    pub fn set_mode(&mut self, now_ns: u64, mode: Mode) {
        if mode == Mode::Monitor {
            let pids: Vec<u32> = self.guards.iter().filter(|g| g.until_ns != 0).map(|g| g.pid).collect();
            for pid in pids {
                self.release(now_ns, pid);
            }
        }
        self.mode = mode;
    }

    /// May the phone contain this program on its own? Never Home, Settings or
    /// the Agent; other image apps only in `Strict`.
    pub fn eligible(&self, who: &Subject) -> bool {
        who.role & ROLE_CORE == 0 && (who.flags & FLAG_NEW_HASH != 0 || self.mode == Mode::Strict)
    }

    /// Check one action and record it. Expired rules are dropped first.
    /// Everything that objects is asked (built-in rules, quarantine, signed
    /// rules) and the strongest verdict wins. While monitoring, anything
    /// stronger than a log entry is reported ("would-...") and the call goes ahead.
    pub fn evaluate(&mut self, now_ns: u64, who: &Subject, action: &Action<'_>) -> ShieldVerdict {
        let enforce = self.mode != Mode::Monitor;
        let mut verdict = ShieldVerdict::Allow;
        let built_in = baseline(who, action);
        if built_in != 0 {
            verdict = ShieldVerdict::Deny { shield_id: built_in };
        } else if enforce && cut_off(who, action) && self.holds(now_ns, who) {
            verdict = ShieldVerdict::Deny { shield_id: ID_QUARANTINE };
        }
        for slot in self.rules.iter_mut() {
            let Some(rule) = slot else { continue };
            if rule.expires_at_ns != 0 && now_ns >= rule.expires_at_ns {
                *slot = None;
                continue;
            }
            if rule.on_match.rank() > verdict.rank() && matches_rule(&rule.matcher, who, action) {
                verdict = rule.on_match;
            }
        }
        let mut shadow = ShieldVerdict::Allow;
        if !enforce && verdict.rank() >= 2 {
            shadow = verdict;
            verdict = ShieldVerdict::Allow;
        }
        // The limits count every call that was not refused outright, so a
        // call a signed rule only logs still marks a cross-app read.
        if verdict.rank() < 2 {
            let limit = self.over_limit(now_ns, who, action);
            if limit != 0 && self.trip(now_ns, who, limit) {
                verdict = ShieldVerdict::Deny { shield_id: limit };
            }
        }
        if matches!(verdict, ShieldVerdict::Deny { shield_id } if shield_id < FIRST_COMPANION_ID) {
            self.denied = self.denied.wrapping_add(1);
        }
        self.record(now_ns, who, action, verdict, shadow);
        verdict
    }

    /// A write or truncate through a descriptor that is already open, on
    /// `path`. The built-in rules are asked again, so a descriptor opened
    /// before they applied (while monitoring, say) cannot change a protected
    /// file. True: refuse it.
    pub fn write_refused(&mut self, now_ns: u64, who: &Subject, path: &str) -> bool {
        let id = baseline(who, &Action::OpenPath { path, mode: AccessMode::Write });
        if id == 0 || self.mode == Mode::Monitor {
            return false;
        }
        self.denied = self.denied.wrapping_add(1);
        let text = alloc::format!("deny {} FD_WRITE", id);
        self.hit(now_ns, who, id, &text);
        true
    }

    /// Bytes app `who` just sent (TCP, UDP or a name lookup). May quarantine
    /// it; see `notice`. Counts only for an app that has read another app's
    /// data, and only while it is not in front.
    pub fn note_sent(&mut self, now_ns: u64, who: &Subject, bytes: u32) {
        if who.flags & FLAG_BACKGROUND == 0 {
            return;
        }
        let g = self.guard(now_ns, who);
        if g.cross_read_ns == 0 {
            return;
        }
        if !g.exfil.take(now_ns, bytes, EXFIL_PER_MIN, EXFIL_BURST) {
            self.trip(now_ns, who, ID_RATE_EXFIL);
        }
    }

    /// The rate limit this call exceeds, or 0. A read of another app's data
    /// marks the app whether or not it is in front.
    fn over_limit(&mut self, now_ns: u64, who: &Subject, action: &Action<'_>) -> u32 {
        let background = who.flags & FLAG_BACKGROUND != 0;
        match action {
            Action::Connect { .. } if background => {
                if !self.guard(now_ns, who).connect.take(now_ns, 1, CONNECT_PER_MIN, CONNECT_BURST) {
                    return ID_RATE_CONNECT;
                }
            }
            Action::OpenPath { path, mode: AccessMode::Read } if path.starts_with("/data/") && cross_app(path, who.name()) => {
                let g = self.guard(now_ns, who);
                g.cross_read_ns = now_ns.max(1);
                if background && !g.cross.take(now_ns, 1, CROSS_READ_PER_MIN, CROSS_READ_BURST) {
                    return ID_RATE_CROSS_READ;
                }
            }
            _ => {}
        }
        0
    }

    /// A limit or the scorer says `who` should be contained. Quarantines it
    /// (true) when enforcing and it is eligible; otherwise reports what would
    /// have happened, at most once every 10 s per program.
    pub fn trip(&mut self, now_ns: u64, who: &Subject, shield_id: u32) -> bool {
        let eligible = self.eligible(who);
        if self.mode != Mode::Monitor && eligible {
            self.quarantine(now_ns, who, shield_id, 0);
            return true;
        }
        // In Monitor, "would" is what Enforce would have done.
        let would = who.role & ROLE_CORE == 0 && who.flags & FLAG_NEW_HASH != 0;
        let g = self.guard(now_ns, who);
        if g.shadow_ns != 0 && now_ns.saturating_sub(g.shadow_ns) < 10_000_000_000 {
            return false;
        }
        g.shadow_ns = now_ns.max(1);
        self.shadow_hits = self.shadow_hits.wrapping_add(1);
        let text = alloc::format!("{} {}", if would { "would-quarantine" } else { "over-limit" }, shield_id);
        self.hit(now_ns, who, shield_id, &text);
        false
    }

    fn hit(&mut self, now_ns: u64, who: &Subject, shield_id: u32, text: &str) {
        let mut hit = TelemetryEvent::new(now_ns, who.app, EventKind::ShieldTriggered, who.flags | FLAG_HIGH_PRIORITY, text.as_bytes());
        hit.aux_u32 = shield_id;
        self.ring.emit(hit);
    }

    fn guard(&mut self, now_ns: u64, who: &Subject) -> &mut Guard {
        let key = who.key();
        let at = match self.guards.iter().position(|g| g.key == key) {
            Some(at) => at,
            None => {
                if self.guards.len() >= MAX_GUARDS {
                    // Forget the app idle longest: one with nothing against it
                    // first, a quarantined one only if all are.
                    let victim = (0..self.guards.len())
                        .min_by_key(|&n| {
                            let g = &self.guards[n];
                            (g.until_ns != 0, g.level != 0 || g.cross_read_ns != 0, g.last_ns)
                        })
                        .unwrap_or(0);
                    self.guards.remove(victim);
                }
                self.guards.push(Guard {
                    key,
                    pid: who.app.0,
                    name: who.name,
                    name_len: who.name_len,
                    connect: Bucket::full(now_ns, CONNECT_BURST),
                    cross: Bucket::full(now_ns, CROSS_READ_BURST),
                    exfil: Bucket::full(now_ns, EXFIL_BURST),
                    cross_read_ns: 0,
                    until_ns: 0,
                    reason: 0,
                    level: 0,
                    offence_ns: 0,
                    last_ns: now_ns,
                    shadow_ns: 0,
                });
                self.guards.len() - 1
            }
        };
        let g = &mut self.guards[at];
        g.pid = who.app.0;
        g.last_ns = now_ns;
        g
    }

    /// Is `who` quarantined? Also notes its pid, so a restarted program's
    /// release names the process that is running now.
    pub fn holds(&mut self, now_ns: u64, who: &Subject) -> bool {
        let key = who.key();
        match self.guards.iter_mut().find(|g| g.key == key && g.until_ns != 0 && g.quarantined(now_ns)) {
            Some(g) => {
                g.pid = who.app.0;
                true
            }
            None => false,
        }
    }

    pub fn is_quarantined(&self, now_ns: u64, key: u64) -> bool {
        self.guards.iter().any(|g| g.key == key && g.until_ns != 0 && g.quarantined(now_ns))
    }

    /// Cut `who` off from the network and other apps' files. `secs` 0 picks
    /// the next step of the ladder (30 s, 5 min, until released; an hour
    /// without offence starts over). Returns when it ends.
    pub fn quarantine(&mut self, now_ns: u64, who: &Subject, shield_id: u32, secs: u32) -> u64 {
        let g = self.guard(now_ns, who);
        let active = g.until_ns != 0 && g.quarantined(now_ns);
        let mut length = secs;
        if secs == 0 {
            if active {
                return g.until_ns;
            }
            if now_ns.saturating_sub(g.offence_ns) > 3_600_000_000_000 {
                g.level = 0;
            }
            length = QUARANTINE_SECS.get(g.level as usize).copied().unwrap_or(0);
            g.level = g.level.saturating_add(1);
            g.offence_ns = now_ns;
        }
        let until = if length == 0 { FOREVER } else { now_ns + u64::from(length) * 1_000_000_000 };
        g.until_ns = if active { g.until_ns.max(until) } else { until };
        g.reason = shield_id;
        let until = g.until_ns;
        self.quarantines = self.quarantines.wrapping_add(1);
        self.notice = Some(Notice { pid: who.app.0, shield_id, secs: length });
        let text = alloc::format!("shield {} for {}s", shield_id, length);
        let mut ev = TelemetryEvent::new(now_ns, who.app, EventKind::Quarantined, who.flags | FLAG_HIGH_PRIORITY, text.as_bytes());
        ev.aux_u32 = shield_id;
        self.ring.emit(ev);
        until
    }

    /// Lift the quarantine of the program whose newest process is `pid`.
    pub fn release(&mut self, now_ns: u64, pid: u32) -> bool {
        let Some(g) = self.guards.iter_mut().find(|g| g.pid == pid && g.until_ns != 0) else { return false };
        g.until_ns = 0;
        self.ring.emit(TelemetryEvent::new(now_ns, AppId(pid), EventKind::Released, FLAG_HIGH_PRIORITY, b""));
        true
    }

    pub fn release_name(&mut self, now_ns: u64, name: &str) -> bool {
        match self.guards.iter().find(|g| g.until_ns != 0 && g.name() == name).map(|g| g.pid) {
            Some(pid) => self.release(now_ns, pid),
            None => false,
        }
    }

    /// End quarantines whose time is up; returns the pids released.
    pub fn expire_quarantines(&mut self, now_ns: u64) -> Vec<u32> {
        let due: Vec<u32> = self.guards.iter().filter(|g| g.until_ns != 0 && !g.quarantined(now_ns)).map(|g| g.pid).collect();
        for pid in &due {
            self.release(now_ns, *pid);
        }
        due
    }

    /// Names of the quarantined programs, comma separated ("-" if none).
    pub fn quarantined_names(&self, now_ns: u64) -> String {
        let mut out = String::new();
        for g in self.guards.iter().filter(|g| g.until_ns != 0 && g.quarantined(now_ns)) {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(g.name());
        }
        if out.is_empty() {
            out.push('-');
        }
        out
    }

    /// Drop expired rules; returns their ids.
    pub fn expire(&mut self, now_ns: u64) -> Vec<u32> {
        let mut gone = Vec::new();
        for slot in self.rules.iter_mut() {
            if let Some(rule) = slot {
                if rule.expires_at_ns != 0 && now_ns >= rule.expires_at_ns {
                    gone.push(rule.shield_id);
                    *slot = None;
                }
            }
        }
        gone
    }

    /// Add or replace a signed rule. Built-in ids are refused.
    pub fn install_rule(&mut self, rule: ShieldRule) -> bool {
        if rule.shield_id < FIRST_COMPANION_ID {
            return false;
        }
        if let Some(slot) = self.rules.iter_mut().find(|r| r.as_ref().map(|x| x.shield_id) == Some(rule.shield_id)) {
            *slot = Some(rule);
            return true;
        }
        if let Some(slot) = self.rules.iter_mut().find(|r| r.is_none()) {
            *slot = Some(rule);
            return true;
        }
        false
    }

    pub fn revoke_rule(&mut self, shield_id: u32) -> bool {
        if let Some(slot) = self.rules.iter_mut().find(|r| r.as_ref().map(|x| x.shield_id) == Some(shield_id)) {
            *slot = None;
            return true;
        }
        false
    }

    fn record(&mut self, now_ns: u64, who: &Subject, action: &Action<'_>, verdict: ShieldVerdict, would: ShieldVerdict) {
        let shadow = would.shield_id();
        let mut flags = who.flags;
        let (kind, detail, port): (EventKind, &[u8], u16) = match action {
            Action::OpenPath { path, mode } => {
                let write = *mode != AccessMode::Read;
                let device = path.starts_with("/dev/") || path.starts_with("/proc/");
                if (device || !write && quiet_read(path, who.name())) && verdict == ShieldVerdict::Allow && shadow == 0 {
                    return;
                }
                if cross_app(path, who.name()) {
                    flags |= FLAG_CROSS_APP;
                }
                (if write { EventKind::OpenWrite } else { EventKind::OpenRead }, path.as_bytes(), 0)
            }
            Action::Connect { host, port } => (EventKind::NetConnect, host.as_bytes(), *port),
            Action::SpawnApp { path } => (EventKind::Exec, path.as_bytes(), 0),
            Action::SystemControl { command } => (EventKind::SysControl, command.as_bytes(), 0),
            Action::MapWriteExec { .. } => (EventKind::MemoryWx, b"", 0),
            Action::SpawnThread { .. } => (EventKind::SpawnThread, b"", 0),
            _ => return,
        };
        let mut ev = TelemetryEvent::new(now_ns, who.app, kind, flags, detail);
        ev.port = port;
        if let Action::MapWriteExec { addr, len } = action {
            let text = alloc::format!("{:#x} len={}", addr, len);
            ev = TelemetryEvent::new(now_ns, who.app, kind, flags, text.as_bytes());
            ev.aux_u32 = *len;
        }
        if kind == EventKind::Exec {
            ev.aux_u32 = u32::from_be_bytes(who.sha256_prefix);
        }
        if verdict == ShieldVerdict::Allow && shadow == 0 {
            self.ring.emit(ev);
            return;
        }
        ev.flags |= FLAG_HIGH_PRIORITY;
        self.ring.emit(ev);
        let what = match verdict {
            ShieldVerdict::LogAlert { .. } => "log",
            ShieldVerdict::Deny { .. } => "deny",
            ShieldVerdict::FreezeApp { .. } => "freeze",
            ShieldVerdict::KillApp { .. } => "kill",
            ShieldVerdict::Allow => match would {
                ShieldVerdict::FreezeApp { .. } => "would-freeze",
                ShieldVerdict::KillApp { .. } => "would-kill",
                _ => "would-deny",
            },
        };
        let id = if shadow != 0 { shadow } else { verdict.shield_id() };
        if shadow != 0 {
            self.shadow_hits = self.shadow_hits.wrapping_add(1);
        }
        let text = alloc::format!("{} {} {}", what, id, kind.name());
        self.hit(now_ns, who, id, &text);
    }
}

fn app_matches(target: &Option<AppId>, app: AppId) -> bool {
    target.map_or(true, |t| t == app)
}

pub fn matches_rule(matcher: &RuleMatcher, who: &Subject, action: &Action<'_>) -> bool {
    match (matcher, action) {
        (RuleMatcher::PathPrefix { app, prefix, write_only, cross_app_only }, Action::OpenPath { path, mode }) => {
            app_matches(app, who.app)
                && !(*write_only && *mode == AccessMode::Read)
                && path.starts_with(prefix.as_str())
                && (!*cross_app_only || cross_app(path, who.name()))
        }
        (RuleMatcher::NetTarget { app, host_suffix, port }, Action::Connect { host, port: p }) => {
            app_matches(app, who.app)
                && (*port == 0 || *port == *p)
                && (host_suffix.is_empty() || host.eq_ignore_ascii_case(host_suffix) || host.to_ascii_lowercase().ends_with(&alloc::format!(".{}", host_suffix.to_ascii_lowercase())))
        }
        (RuleMatcher::WriteExecMemory { app }, Action::MapWriteExec { .. }) => app_matches(app, who.app),
        (RuleMatcher::BinaryHash { sha256_prefix }, _) => *sha256_prefix == who.sha256_prefix,
        _ => false,
    }
}

/// What the Sentinel knows about a running program, for its records.
#[derive(Clone, Debug)]
pub struct AppInfo {
    pub name: String,
    pub sha8: String,
}

/// One `violet-log-v1` record line, tab separated:
/// `t_ms pid app sha8 kind flags idle_s detail target`.
pub fn record_line(ev: &TelemetryEvent, info: Option<&AppInfo>, idle_s: u32) -> String {
    let kind = kind_from(ev.kind);
    let (name, sha) = info.map_or(("?", "00000000"), |i| (i.name.as_str(), i.sha8.as_str()));
    let host = ev.detail_str();
    let target = match kind {
        Some(EventKind::NetConnect) | Some(EventKind::NetTxSummary) => alloc::format!("{}:{}", host, ev.port),
        _ => String::new(),
    };
    let detail = match kind {
        Some(EventKind::NetConnect) => target.clone(),
        Some(EventKind::NetTxSummary) => alloc::format!("{}B", ev.aux_u32),
        _ => String::from(host),
    };
    alloc::format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        ev.timestamp_us / 1000,
        ev.app_id,
        name,
        sha,
        kind.map(|k| k.name()).unwrap_or("?"),
        ev.flags,
        idle_s,
        detail.replace('\t', " ").replace('\n', " "),
        target
    )
}

pub fn kind_from(k: u8) -> Option<EventKind> {
    use EventKind::*;
    Some(match k {
        1 => Exec,
        2 => OpenRead,
        3 => NetConnect,
        4 => NetTxSummary,
        5 => MemoryWx,
        6 => SpawnThread,
        7 => SysControl,
        8 => Exit,
        9 => ShieldTriggered,
        10 => OpenWrite,
        11 => Frozen,
        12 => Thawed,
        13 => Quarantined,
        14 => Released,
        15 => LocalScore,
        _ => return None,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn who(app: u32, name: &str) -> Subject {
        let mut n = [0u8; 24];
        n[..name.len()].copy_from_slice(name.as_bytes());
        Subject { app: AppId(app), sha256_prefix: [1, 2, 3, 4], flags: 0, name: n, name_len: name.len() as u8, role: ROLE_OWNS_APP }
    }

    /// A program the user added (not in the boot image), running behind another app.
    fn added(app: u32, name: &str) -> Subject {
        Subject { flags: FLAG_NEW_HASH | FLAG_BACKGROUND, ..who(app, name) }
    }

    const S: u64 = 1_000_000_000;
    fn read(path: &str) -> Action<'_> {
        Action::OpenPath { path, mode: AccessMode::Read }
    }
    fn write(path: &str) -> Action<'_> {
        Action::OpenPath { path, mode: AccessMode::Write }
    }
    fn hits(p: &mut ShieldPolicy) -> Vec<String> {
        p.ring.drain().iter().filter(|e| e.kind == EventKind::ShieldTriggered as u8).map(|e| String::from(e.detail_str())).collect()
    }

    #[test]
    fn open_by_default_and_quiet_system_reads() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let w = who(100, "pysync");
        assert_eq!(p.evaluate(0, &w, &read("/lib/python3.12/os.py")), ShieldVerdict::Allow);
        assert_eq!(p.ring.len(), 0);
        assert_eq!(p.evaluate(0, &w, &read("/data/notes/keys.txt")), ShieldVerdict::Allow);
        let ev = p.ring.drain();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].flags & FLAG_CROSS_APP, FLAG_CROSS_APP);
        assert_eq!(ev[0].detail_str(), "/data/notes/keys.txt");
        // Its own files and /tmp stay writable.
        for path in ["/data/pysync/state.json", "/apps/pysync/cache", "/tmp/x", "/data/other/shared.txt"] {
            assert_eq!(p.evaluate(0, &w, &write(path)), ShieldVerdict::Allow, "{}", path);
        }
    }

    #[test]
    fn rules_match_expire_and_revoke() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let w = who(101, "pysync");
        assert!(p.install_rule(ShieldRule {
            shield_id: 17,
            expires_at_ns: 1_000,
            matcher: RuleMatcher::NetTarget { app: None, host_suffix: String::from("c2-exfil.test"), port: 0 },
            on_match: ShieldVerdict::Deny { shield_id: 17 },
        }));
        let c = Action::Connect { host: "drop.c2-exfil.test", port: 4444 };
        assert_eq!(p.evaluate(10, &w, &c), ShieldVerdict::Deny { shield_id: 17 });
        assert_eq!(p.evaluate(10, &w, &Action::Connect { host: "api.weather.test", port: 80 }), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(2_000, &w, &c), ShieldVerdict::Allow, "expired");
        assert_eq!(p.active(), 0);
        p.install_rule(ShieldRule {
            shield_id: 18,
            expires_at_ns: 0,
            matcher: RuleMatcher::PathPrefix { app: Some(AppId(101)), prefix: String::from("/apps/launcher/"), write_only: true, cross_app_only: true },
            on_match: ShieldVerdict::FreezeApp { shield_id: 18 },
        });
        assert_eq!(p.evaluate(0, &w, &read("/apps/launcher/startup.py")), ShieldVerdict::Allow);
        // The built-in rule refuses this write too; the signed freeze is stronger and wins.
        assert_eq!(p.evaluate(0, &w, &write("/apps/launcher/startup.py")), ShieldVerdict::FreezeApp { shield_id: 18 });
        assert_eq!(p.evaluate(0, &who(102, "x"), &write("/apps/launcher/startup.py")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
        assert!(p.revoke_rule(18));
        assert_eq!(p.active(), 0);
        assert_eq!(p.evaluate(0, &w, &write("/apps/launcher/startup.py")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
    }

    #[test]
    fn the_strongest_verdict_wins() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let w = who(101, "themes");
        let rule = |id, on_match| ShieldRule { shield_id: id, expires_at_ns: 0, matcher: RuleMatcher::PathPrefix { app: None, prefix: String::from("/apps/"), write_only: true, cross_app_only: false }, on_match };
        // A signed deny does not replace the built-in one (same strength: the phone's id stays).
        p.install_rule(rule(20, ShieldVerdict::Deny { shield_id: 20 }));
        p.install_rule(rule(21, ShieldVerdict::LogAlert { shield_id: 21 }));
        assert_eq!(p.evaluate(0, &w, &write("/apps/home")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
        // Table order does not matter: end beats freeze beats deny.
        p.install_rule(rule(22, ShieldVerdict::KillApp { shield_id: 22 }));
        p.install_rule(rule(23, ShieldVerdict::FreezeApp { shield_id: 23 }));
        assert_eq!(p.evaluate(0, &w, &write("/apps/home")), ShieldVerdict::KillApp { shield_id: 22 });
        p.revoke_rule(22);
        assert_eq!(p.evaluate(0, &w, &write("/apps/home")), ShieldVerdict::FreezeApp { shield_id: 23 });
        // A log-only rule never weakens a quarantine's refusal.
        let a = added(102, "photosync");
        p.install_rule(ShieldRule { shield_id: 24, expires_at_ns: 0, matcher: RuleMatcher::NetTarget { app: None, host_suffix: String::new(), port: 0 }, on_match: ShieldVerdict::LogAlert { shield_id: 24 } });
        p.quarantine(0, &a, ID_SCORER, 0);
        assert_eq!(p.evaluate(S, &a, &Action::Connect { host: "example.com", port: 443 }), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
    }

    const O_RDONLY: u32 = 0;
    const O_WRONLY: u32 = 1;
    const O_RDWR: u32 = 2;

    /// What `Runtime::gate_open` asks the policy for an open() (guard.rs).
    fn open(p: &mut ShieldPolicy, now: u64, w: &Subject, path: &str, flags: u32, exists: bool) -> ShieldVerdict {
        let mode = open_mode(flags, exists);
        if mode != AccessMode::Read && fd_reads(flags) && exists && cross_app(path, w.name()) {
            let v = p.evaluate(now, w, &Action::OpenPath { path, mode: AccessMode::Read });
            if !matches!(v, ShieldVerdict::Allow | ShieldVerdict::LogAlert { .. }) {
                return v;
            }
        }
        p.evaluate(now, w, &Action::OpenPath { path, mode })
    }

    #[test]
    fn a_read_only_open_cannot_change_a_protected_file() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let w = added(101, "themes");
        for (path, id) in [("/data/.shield-mode", ID_SHIELD_STATE), ("/apps/home", ID_APP_CODE), ("/bin/python3", ID_SYSTEM_FILES)] {
            // Reading is fine...
            assert_eq!(open(&mut p, 0, &w, path, O_RDONLY, true), ShieldVerdict::Allow);
            // ...but the descriptor that gives cannot write or truncate,
            assert!(!fd_writes(O_RDONLY) && !fd_writes(O_RDONLY | O_CREAT) && !fd_writes(3));
            // an open that truncates or creates is a write whatever access it asks for,
            assert_eq!(open(&mut p, 0, &w, path, O_RDONLY | O_TRUNC, true), ShieldVerdict::Deny { shield_id: id }, "{}", path);
            assert_eq!(open(&mut p, 0, &w, path, O_RDONLY | O_CREAT, false), ShieldVerdict::Deny { shield_id: id }, "{}", path);
            assert_eq!(open(&mut p, 0, &w, path, O_WRONLY, true), ShieldVerdict::Deny { shield_id: id });
            assert_eq!(open(&mut p, 0, &w, path, O_RDWR, true), ShieldVerdict::Deny { shield_id: id });
            // (O_CREAT on a file that is there changes nothing: still a read)
            assert_eq!(open(&mut p, 0, &w, path, O_RDONLY | O_CREAT, true), ShieldVerdict::Allow);
            // and a write through any descriptor is checked against the rules again.
            assert!(p.write_refused(0, &w, path), "{}", path);
        }
        assert!(fd_writes(O_WRONLY) && fd_writes(O_RDWR) && fd_reads(O_RDONLY) && fd_reads(O_RDWR) && !fd_reads(O_WRONLY));
        assert!(!p.write_refused(0, &w, "/data/themes/skin.json") && !p.write_refused(0, &w, "/apps/themes/skin.json"));
        assert_eq!(hits(&mut p).iter().filter(|h| h.ends_with("FD_WRITE")).count(), 3);
        // A descriptor opened for writing while the phone was monitoring stops
        // working on protected files once it enforces.
        let mut p = ShieldPolicy::with_mode(Mode::Monitor);
        assert_eq!(open(&mut p, 0, &w, "/data/.shield-mode", O_RDWR, true), ShieldVerdict::Allow);
        assert!(!p.write_refused(0, &w, "/data/.shield-mode"));
        p.set_mode(S, Mode::Enforce);
        assert!(p.write_refused(S, &w, "/data/.shield-mode"));
    }

    #[test]
    fn a_read_write_open_or_a_rename_of_other_apps_data_counts_as_a_read() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(107, "photosync");
        assert_eq!(open(&mut p, 0, &a, "/data/notes/secret", O_RDWR, true), ShieldVerdict::Allow);
        let kinds: Vec<u8> = p.ring.drain().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EventKind::OpenRead as u8, EventKind::OpenWrite as u8], "the scorer sees the read");
        let mut sent = 0;
        while !p.is_quarantined(S, a.key()) {
            p.note_sent(S, &a, 64 * 1024);
            sent += 64 * 1024;
            assert!(sent <= 4 * 1024 * 1024, "never quarantined");
        }
        // A write-only open reads nothing (the descriptor cannot be read): no mark.
        let b = added(108, "share");
        assert_eq!(open(&mut p, 0, &b, "/data/notes/inbox", O_WRONLY | O_CREAT, false), ShieldVerdict::Allow);
        for _ in 0..64 {
            p.note_sent(S, &b, 64 * 1024);
        }
        assert!(!p.is_quarantined(S, b.key()));
        // A rename out of another app's folder is checked as a read of the old name first.
        let c = added(109, "mover");
        assert!(cross_app("/data/notes/secret", c.name()) && cross_app("/data/notes", c.name()));
        assert_eq!(p.evaluate(0, &c, &read("/data/notes/secret")), ShieldVerdict::Allow);
        for path in ["/data/notes/secret", "/data/mover/s"] {
            assert_eq!(p.evaluate(0, &c, &write(path)), ShieldVerdict::Allow);
        }
        for _ in 0..8 {
            p.note_sent(S, &c, 64 * 1024);
        }
        assert!(p.is_quarantined(S, c.key()), "the moved file is its own now, the mark stays");
    }

    #[test]
    fn the_cross_read_mark_does_not_wear_off_and_udp_counts() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(107, "photosync");
        assert_eq!(p.evaluate(0, &a, &read("/data/notes/keys.txt")), ShieldVerdict::Allow);
        // 601 s later, a day later: the limit still applies (bytes of any kind: note_sent
        // is what guard.rs calls for TCP, UDP and name lookups).
        let day = 86_400 * S;
        for _ in 0..4 {
            p.note_sent(day, &a, 64 * 1024);
        }
        assert!(!p.is_quarantined(day, a.key()), "the burst");
        p.note_sent(day, &a, 1400);
        assert!(p.is_quarantined(day, a.key()));
        // A marked app is the last to be forgotten when the table is full.
        for n in 0..200u32 {
            let w = added(300 + n, &alloc::format!("app{}", n));
            p.evaluate(day + 400 * S + u64::from(n), &w, &Action::Connect { host: "example.com", port: 443 });
        }
        p.note_sent(2 * day, &a, 1024 * 1024);
        assert!(p.is_quarantined(2 * day, a.key()), "still marked, and the second offence");
    }

    #[test]
    fn who_a_program_is_comes_from_where_it_was_started() {
        let sha = [9, 9, 9, 9];
        // An installed app, from the image or added later.
        let notes = Subject::derive(100, "/apps/notes", &owner_of("/apps/notes", None, sha), sha, true, true);
        assert_eq!((notes.name(), notes.role, notes.flags), ("notes", ROLE_OWNS_APP, 0));
        let added = Subject::derive(101, "/apps/evil", &owner_of("/apps/evil", None, sha), sha, false, false);
        assert_eq!((added.name(), added.role, added.flags), ("evil", ROLE_OWNS_APP, FLAG_NEW_HASH | FLAG_BACKGROUND));
        // A program inside an app's folder is that app, whoever started it.
        assert_eq!(owner_of("/apps/notes/main", Some("evil"), sha), "notes");
        let main = Subject::derive(102, "/apps/notes/main", "notes", sha, true, true);
        assert_eq!((main.name(), main.role), ("notes", ROLE_OWNS_APP));
        // A helper that evil wrote to /data/evil/notes and started is evil,
        // not "notes": the other app's data is not its own, nor is /apps/notes.
        let owner = owner_of("/data/evil/notes", Some(added.name()), [1, 1, 1, 1]);
        let helper = Subject::derive(103, "/data/evil/notes", &owner, [1, 1, 1, 1], false, true);
        assert_eq!((helper.name(), helper.role), ("evil", 0));
        assert!(cross_app("/data/notes/secret", helper.name()) && !cross_app("/data/evil/x", helper.name()));
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        assert_eq!(p.evaluate(0, &helper, &write("/apps/notes/main")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
        assert_eq!(p.evaluate(0, &helper, &write("/apps/evil/main")), ShieldVerdict::Deny { shield_id: ID_APP_CODE }, "not started from /apps/evil");
        // The helper shares evil's limits and quarantine, whatever its bytes are:
        // a padded copy started while quarantined is still quarantined.
        assert_eq!(helper.key(), added.key());
        p.quarantine(0, &added, ID_RATE_EXFIL, 0);
        let copy = Subject::derive(104, "/data/evil/copy2", &owner_of("/data/evil/copy2", Some("evil"), [2, 2, 2, 2]), [2, 2, 2, 2], false, true);
        assert_eq!(p.evaluate(S, &copy, &Action::Connect { host: "drop.c2-exfil.test", port: 4444 }), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
        assert_eq!(p.evaluate(S, &copy, &read("/data/notes/secret")), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
        // So is evil itself after rewriting its own program (a new hash).
        let rewritten = Subject::derive(105, "/apps/evil", "evil", [3, 3, 3, 3], false, true);
        assert!(p.holds(S, &rewritten));
        // Started by the user or a core app from outside /apps: its own, unnamed
        // app, which owns nothing in /data and is never exempt.
        let loose = owner_of("/data/evil/notes", None, [0xab, 0xcd, 0, 1]);
        assert_eq!(loose, "~abcd0001");
        let l = Subject::derive(106, "/data/evil/notes", &loose, [0xab, 0xcd, 0, 1], false, true);
        assert!(cross_app("/data/notes/secret", l.name()) && cross_app("/data/evil/x", l.name()));
        assert_ne!(l.key(), added.key());
        // The roles need the boot image's own file at the core app's path.
        let agent = Subject::derive(1, CORE_INSTALLER, "agent", sha, true, false);
        assert_eq!(agent.role, ROLE_INSTALLER | ROLE_CORE | ROLE_OWNS_APP);
        assert_eq!(Subject::derive(1, CORE_INSTALLER, "agent", sha, false, false).role, ROLE_OWNS_APP, "rewritten since boot");
        assert_eq!(Subject::derive(1, CORE_HOME, "home", sha, true, true).role, ROLE_CORE | ROLE_OWNS_APP);
        assert_eq!(Subject::derive(1, CORE_SETTINGS, "settings", sha, true, true).role, ROLE_CORE | ROLE_OWNS_APP);
        assert_eq!(Subject::derive(1, "/apps/home/main", "home", sha, true, true).role, ROLE_OWNS_APP);
        // A file a package put in /bin after boot looks like the image's to
        // the file system; nothing outside /apps is exempt from containment.
        let helper = Subject::derive(107, "/bin/helper", &owner_of("/bin/helper", Some("evil"), sha), sha, true, false);
        assert_eq!((helper.name(), helper.flags & FLAG_NEW_HASH, helper.role), ("evil", FLAG_NEW_HASH, 0));
        assert!(p.eligible(&helper));
        assert!(!p.eligible(&notes) && !p.eligible(&agent));
    }

    #[test]
    fn an_app_cannot_put_itself_in_front_for_the_shield() {
        // Shown, and nobody pushed it: in front.
        assert!(in_front(true, 107, (0, 0), 5 * S));
        assert!(!in_front(false, 107, (0, 0), 5 * S));
        // It opened itself at 10 s; the last touch was before that.
        assert!(!in_front(true, 107, (107, 10 * S), 5 * S));
        assert!(!in_front(true, 107, (107, 10 * S), 10 * S));
        // The user touches the screen while it is there: now it is in front.
        assert!(in_front(true, 107, (107, 10 * S), 11 * S));
        // Another app is not affected by the push.
        assert!(in_front(true, 108, (107, 10 * S), 5 * S));
        // So the limits still see it as a background app.
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let pushed = Subject::derive(107, "/apps/photosync", "photosync", [1; 4], false, in_front(true, 107, (107, 10 * S), 5 * S));
        p.evaluate(0, &pushed, &read("/data/notes/keys.txt"));
        for _ in 0..8 {
            p.note_sent(S, &pushed, 64 * 1024);
        }
        assert!(p.is_quarantined(S, pushed.key()));
    }

    #[test]
    fn data_files_an_app_keeps_across_restarts_are_its_own() {
        // Only files directly in /data are kept: NAME and NAME.* belong to NAME.
        for path in ["/data/notes", "/data/notes.db", "/data/notes.idx", "/data/notes/todo.txt", "/data/notes/a/b"] {
            assert!(!cross_app(path, "notes"), "{}", path);
        }
        for path in ["/data/notes-pro.db", "/data/notes2", "/data/notesx.db", "/data/other.db", "/data/notes.d/x", "/data/.shield-mode", "/data/settings"] {
            assert!(cross_app(path, "notes"), "{}", path);
        }
        assert!(cross_app("/data/.db", "") && cross_app("/data/x", ""), "an unnamed program owns nothing");
        assert!(!cross_app("/data", "notes") && !cross_app("/data/", "notes"));
        // A notes app syncing its own saved files in the background is not marked.
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(150, "notes");
        for path in ["/data/notes.db", "/data/notes.idx"] {
            assert_eq!(p.evaluate(0, &a, &read(path)), ShieldVerdict::Allow);
        }
        assert!(p.ring.drain().iter().all(|e| e.flags & FLAG_CROSS_APP == 0));
        for _ in 0..64 {
            p.note_sent(S, &a, 64 * 1024);
        }
        assert!(!p.is_quarantined(S, a.key()));
    }

    #[test]
    fn a_saved_mode_can_only_strengthen_the_build() {
        use Mode::*;
        assert_eq!(Mode::at_boot(Enforce, Some(Monitor)), Enforce, "an unauthenticated file cannot switch enforcement off");
        assert_eq!(Mode::at_boot(Strict, Some(Enforce)), Strict);
        assert_eq!(Mode::at_boot(Enforce, Some(Strict)), Strict);
        assert_eq!(Mode::at_boot(Monitor, Some(Enforce)), Enforce);
        assert_eq!(Mode::at_boot(Monitor, Some(Monitor)), Monitor);
        assert_eq!(Mode::at_boot(Enforce, None), Enforce);
        assert_eq!(Mode::at_boot(Enforce, Mode::parse("garbage")), Enforce);
    }

    #[test]
    fn baseline_denies_code_and_state_writes_without_a_companion() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        assert_eq!(p.active(), 0, "no table rows: nothing to revoke or crowd out");
        let w = who(101, "themes");
        for (path, id) in [
            ("/apps/home", ID_APP_CODE),
            ("/apps/home/main", ID_APP_CODE),
            ("/apps/.home", ID_APP_CODE),
            ("/apps", ID_APP_CODE),
            ("/bin/python3", ID_SYSTEM_FILES),
            ("/lib", ID_SYSTEM_FILES),
            ("/lib/python3.12/os.py", ID_SYSTEM_FILES),
            ("/data/.shield-mode", ID_SHIELD_STATE),
        ] {
            assert_eq!(p.evaluate(0, &w, &write(path)), ShieldVerdict::Deny { shield_id: id }, "{}", path);
            assert_eq!(p.evaluate(0, &w, &read(path)), ShieldVerdict::Allow, "reading {}", path);
        }
        assert_eq!(p.evaluate(0, &w, &write("/apps/themes/skin.json")), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(0, &w, &write("/library/x")), ShieldVerdict::Allow, "/lib is a folder, not a prefix");
        // A program merely *named* home, started from somewhere else, does not own /apps/home.
        let fake = Subject { role: 0, ..who(102, "home") };
        assert_eq!(p.evaluate(0, &fake, &write("/apps/home/main")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
        // The installer may write apps, but not system files or the Shield's state.
        let agent = Subject { role: ROLE_INSTALLER | ROLE_OWNS_APP | ROLE_CORE, ..who(103, "agent") };
        assert_eq!(p.evaluate(0, &agent, &write("/apps/weather2/main.py")), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(0, &agent, &write("/bin/sh")), ShieldVerdict::Deny { shield_id: ID_SYSTEM_FILES });
        assert_eq!(p.evaluate(0, &agent, &write("/data/.shield-mode")), ShieldVerdict::Deny { shield_id: ID_SHIELD_STATE });
        assert_eq!(p.denied, 11);
    }

    #[test]
    fn built_in_ids_cannot_be_installed_or_revoked() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        for id in [0, ID_APP_CODE, ID_QUARANTINE, FIRST_COMPANION_ID - 1] {
            assert!(!p.install_rule(ShieldRule {
                shield_id: id,
                expires_at_ns: 0,
                matcher: RuleMatcher::PathPrefix { app: None, prefix: String::from("/apps/"), write_only: true, cross_app_only: false },
                on_match: ShieldVerdict::LogAlert { shield_id: id },
            }));
            assert!(!p.revoke_rule(id));
        }
        assert_eq!(p.evaluate(0, &who(1, "x"), &write("/apps/home/main")), ShieldVerdict::Deny { shield_id: ID_APP_CODE });
    }

    #[test]
    fn monitor_mode_reports_but_allows() {
        let mut p = ShieldPolicy::with_mode(Mode::Monitor);
        let w = who(101, "themes");
        assert_eq!(p.evaluate(0, &w, &write("/apps/home/main")), ShieldVerdict::Allow);
        assert_eq!(hits(&mut p), ["would-deny 1 OPEN_W"]);
        assert_eq!((p.shadow_hits, p.denied), (1, 0));
        // Rate limits: reported, never applied.
        let a = added(102, "photosync");
        assert_eq!(p.evaluate(0, &a, &read("/data/notes/keys.txt")), ShieldVerdict::Allow);
        for _ in 0..40 {
            p.note_sent(S, &a, 64 * 1024);
        }
        assert_eq!(hits(&mut p), ["would-quarantine 7"], "one report per 10 s");
        assert!(!p.is_quarantined(S, a.key()));
        assert_eq!(p.evaluate(S, &a, &Action::Connect { host: "drop.c2-exfil.test", port: 4444 }), ShieldVerdict::Allow);
    }

    #[test]
    fn monitor_mode_blocks_nothing_signed_rules_included() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(102, "photosync");
        let c2 = Action::Connect { host: "drop.c2-exfil.test", port: 4444 };
        // Installed while enforcing: a deny and a freeze, and a quarantine in force.
        p.install_rule(ShieldRule { shield_id: 17, expires_at_ns: 0, matcher: RuleMatcher::NetTarget { app: None, host_suffix: String::from("c2-exfil.test"), port: 0 }, on_match: ShieldVerdict::Deny { shield_id: 17 } });
        p.install_rule(ShieldRule { shield_id: 18, expires_at_ns: 0, matcher: RuleMatcher::PathPrefix { app: None, prefix: String::from("/data/notes/"), write_only: true, cross_app_only: true }, on_match: ShieldVerdict::FreezeApp { shield_id: 18 } });
        assert_eq!(p.evaluate(0, &a, &c2), ShieldVerdict::Deny { shield_id: 17 });
        p.quarantine(0, &a, 40, 600);
        p.ring.drain();
        // The user switches to monitoring: the rules stay loaded but only report.
        p.set_mode(S, Mode::Monitor);
        assert_eq!(p.active(), 2);
        assert!(!p.is_quarantined(S, a.key()));
        assert_eq!(p.evaluate(S, &a, &c2), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(S, &a, &write("/data/notes/keys.txt")), ShieldVerdict::Allow);
        assert_eq!(hits(&mut p), ["would-deny 17 SOCK_CONN", "would-freeze 18 OPEN_W"]);
        // Even a quarantine record that somehow remains does not cut anything off.
        p.guards[0].until_ns = FOREVER;
        assert_eq!(p.evaluate(S, &a, &Action::Connect { host: "example.com", port: 443 }), ShieldVerdict::Allow);
        // A log-only rule still logs.
        p.install_rule(ShieldRule { shield_id: 19, expires_at_ns: 0, matcher: RuleMatcher::WriteExecMemory { app: None }, on_match: ShieldVerdict::LogAlert { shield_id: 19 } });
        assert_eq!(p.evaluate(S, &a, &Action::MapWriteExec { addr: 0, len: 4096 }), ShieldVerdict::LogAlert { shield_id: 19 });
        // Back to enforcing: they act again.
        p.guards[0].until_ns = 0;
        p.set_mode(2 * S, Mode::Enforce);
        assert_eq!(p.evaluate(2 * S, &a, &c2), ShieldVerdict::Deny { shield_id: 17 });
        assert_eq!(p.evaluate(2 * S, &a, &write("/data/notes/keys.txt")), ShieldVerdict::FreezeApp { shield_id: 18 });
    }

    #[test]
    fn bucket_refills_and_caps() {
        let mut b = Bucket::full(0, 30);
        for _ in 0..30 {
            assert!(b.take(0, 1, 30, 30));
        }
        assert!(!b.take(0, 1, 30, 30));
        assert!(!b.take(S, 1, 30, 30), "30 a minute is one every 2 s");
        assert!(b.take(2 * S, 1, 30, 30));
        assert!(!b.take(2 * S, 1, 30, 30));
        // Small steps still add up.
        for step in 0..19 {
            assert!(!b.take(2 * S + step * S / 10, 1, 30, 30));
        }
        assert!(b.take(4 * S, 1, 30, 30));
        // A long pause fills it to the burst and no further.
        assert!(b.take(10_000 * S, 30, 30, 30));
        assert!(!b.take(10_000 * S, 1, 30, 30));
        // The rate does not depend on when the calls come: two connections
        // every 4.2 s (28.6 a minute, under the limit) are never refused...
        let mut t = 10_000 * S;
        for round in 0..500 {
            t += 3_900_000_000;
            assert!(b.take(t, 1, 30, 30), "round {}", round);
            t += 300_000_000;
            assert!(b.take(t, 1, 30, 30), "round {}", round);
        }
        // ...and it saved up what was left over (0.1 a round), but no more than the burst.
        let mut spare = 0;
        while b.take(t, 1, 30, 30) {
            spare += 1;
        }
        assert_eq!(spare, 28, "full (30) before the last round's two");
        // Two every 3.5 s (34 a minute) run out.
        let refused = (0..200).filter(|_| {
            t += 3_500_000_000;
            !b.take(t, 1, 30, 30) | !b.take(t, 1, 30, 30)
        }).count();
        assert!(refused > 20, "{}", refused);
        // Bytes: 1 KiB every 1.9 ms is far over 64 KiB a minute; 1 KiB a second is under it.
        let mut e = Bucket::full(0, EXFIL_BURST);
        let mut t = 0;
        let passed = (0..2000).filter(|_| { t += 1_900_000; e.take(t, 1024, EXFIL_PER_MIN, EXFIL_BURST) }).count();
        assert!((256..=262).contains(&passed), "{}", passed);
        for _ in 0..3600 {
            t += S;
            assert!(e.take(t, 1024, EXFIL_PER_MIN, EXFIL_BURST));
        }
    }

    #[test]
    fn background_exfil_after_cross_app_read_is_quarantined() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(107, "photosync");
        let c2 = Action::Connect { host: "drop.c2-exfil.test", port: 4444 };
        assert_eq!(p.evaluate(0, &a, &read("/data/notes/keys.txt")), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(0, &a, &c2), ShieldVerdict::Allow);
        let mut sent = 0;
        while !p.is_quarantined(S, a.key()) {
            p.note_sent(S, &a, 64 * 1024);
            sent += 64 * 1024;
            assert!(sent <= 4 * 1024 * 1024, "never quarantined");
        }
        assert_eq!(sent, EXFIL_BURST + 64 * 1024, "the burst, plus the send that crossed it");
        assert_eq!(p.notice.take(), Some(Notice { pid: 107, shield_id: ID_RATE_EXFIL, secs: 30 }));
        // Cut off from the network and other apps' files; its own files still work.
        assert_eq!(p.evaluate(2 * S, &a, &c2), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
        assert_eq!(p.evaluate(2 * S, &a, &read("/data/notes/keys.txt")), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
        assert_eq!(p.evaluate(2 * S, &a, &write("/data/photosync/state.json")), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(2 * S, &a, &read("/apps/photosync/main")), ShieldVerdict::Allow);
        assert_eq!(p.quarantined_names(2 * S), "photosync");
        // Starting the same program again (new pid) does not get out of it...
        let again = Subject { app: AppId(120), ..a };
        assert_eq!(p.evaluate(3 * S, &again, &c2), ShieldVerdict::Deny { shield_id: ID_QUARANTINE });
        // ...and another program is not affected.
        assert_eq!(p.evaluate(3 * S, &added(121, "notes"), &c2), ShieldVerdict::Allow);
        // It ends on its own.
        assert_eq!(p.expire_quarantines(20 * S), Vec::<u32>::new());
        assert_eq!(p.expire_quarantines(32 * S), [120]);
        assert_eq!(p.evaluate(32 * S, &again, &c2), ShieldVerdict::Allow);
        assert_eq!(p.quarantined_names(32 * S), "-");
        let kinds: Vec<u8> = p.ring.drain().iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&(EventKind::Quarantined as u8)) && kinds.contains(&(EventKind::Released as u8)));
    }

    #[test]
    fn foreground_upload_and_image_apps_are_not_contained() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        // The user, in front, backs up a big file after the app read shared data.
        let front = Subject { flags: FLAG_NEW_HASH, ..who(110, "backup") };
        p.evaluate(0, &front, &read("/data/photos/a.jpg"));
        for _ in 0..64 {
            p.note_sent(S, &front, 64 * 1024);
        }
        assert!(!p.is_quarantined(S, front.key()));
        // A background upload with no cross-app read before it.
        let sync = added(111, "music");
        for _ in 0..64 {
            p.note_sent(S, &sync, 64 * 1024);
        }
        assert!(!p.is_quarantined(S, sync.key()));
        // An app from the verified image: reported, not contained...
        let image = Subject { flags: FLAG_BACKGROUND, ..who(112, "weather") };
        p.evaluate(0, &image, &read("/data/notes/keys.txt"));
        p.ring.drain();
        for _ in 0..64 {
            p.note_sent(S, &image, 64 * 1024);
        }
        assert!(!p.is_quarantined(S, image.key()));
        assert_eq!(hits(&mut p), ["over-limit 7"]);
        // ...unless the phone is in strict mode, and never Home, Settings or the Agent.
        p.set_mode(S, Mode::Strict);
        p.note_sent(20 * S, &image, 1024 * 1024);
        assert!(p.is_quarantined(20 * S, image.key()));
        let home = Subject { flags: FLAG_BACKGROUND, role: ROLE_CORE | ROLE_OWNS_APP, ..who(100, "home") };
        p.evaluate(0, &home, &read("/data/notes/keys.txt"));
        p.note_sent(20 * S, &home, 8 * 1024 * 1024);
        assert!(!p.is_quarantined(20 * S, home.key()));
    }

    #[test]
    fn connect_and_cross_read_floods_are_limited_in_the_background() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(130, "scanner");
        let c = Action::Connect { host: "10.0.0.9", port: 22 };
        for n in 0..CONNECT_BURST {
            assert_eq!(p.evaluate(0, &a, &c), ShieldVerdict::Allow, "connect {}", n);
        }
        assert_eq!(p.evaluate(0, &a, &c), ShieldVerdict::Deny { shield_id: ID_RATE_CONNECT });
        assert!(p.is_quarantined(0, a.key()));
        let b = added(131, "harvest");
        for n in 0..CROSS_READ_BURST {
            assert_eq!(p.evaluate(0, &b, &read("/data/notes/keys.txt")), ShieldVerdict::Allow, "read {}", n);
        }
        assert_eq!(p.evaluate(0, &b, &read("/data/notes/keys.txt")), ShieldVerdict::Deny { shield_id: ID_RATE_CROSS_READ });
        // In front, neither limit applies (a file manager browsing).
        let f = Subject { flags: FLAG_NEW_HASH, ..who(132, "files") };
        for _ in 0..500 {
            assert_eq!(p.evaluate(0, &f, &read("/data/notes/keys.txt")), ShieldVerdict::Allow);
            assert_eq!(p.evaluate(0, &f, &c), ShieldVerdict::Allow);
        }
    }

    #[test]
    fn quarantine_ladder_escalates_and_release_works() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let a = added(140, "x");
        assert_eq!(p.quarantine(0, &a, ID_SCORER, 0), 30 * S);
        assert_eq!(p.quarantine(S, &a, ID_SCORER, 0), 30 * S, "no change while it is active");
        assert!(p.expire_quarantines(31 * S) == [140]);
        assert_eq!(p.quarantine(40 * S, &a, ID_SCORER, 0), 340 * S);
        p.expire_quarantines(341 * S);
        assert_eq!(p.quarantine(400 * S, &a, ID_SCORER, 0), FOREVER);
        assert!(p.expire_quarantines(1_000_000 * S).is_empty());
        assert!(p.is_quarantined(1_000_000 * S, a.key()));
        assert!(p.release_name(1_000_000 * S, "x"));
        assert!(!p.release(1_000_000 * S, 140), "already released");
        // An hour without offence starts the ladder over.
        assert_eq!(p.quarantine(1_010_000 * S, &a, ID_SCORER, 0), 1_010_030 * S);
        // A signed quarantine carries its own length and can extend one.
        assert_eq!(p.quarantine(1_010_001 * S, &a, 40, 600), 1_010_601 * S);
        // Going back to monitoring lifts it.
        p.set_mode(1_010_002 * S, Mode::Monitor);
        assert!(!p.is_quarantined(1_010_002 * S, a.key()));
    }

    #[test]
    fn guards_are_bounded_and_keep_the_quarantined() {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        let bad = added(200, "bad");
        p.quarantine(0, &bad, ID_SCORER, 600);
        for n in 0..200u32 {
            let w = added(300 + n, &alloc::format!("app{}", n));
            p.evaluate(u64::from(n) + 1, &w, &Action::Connect { host: "example.com", port: 443 });
        }
        assert_eq!(p.guards.len(), MAX_GUARDS);
        assert!(p.is_quarantined(300, bad.key()));
    }

    #[test]
    fn telemetry_overflow_keeps_high_priority() {
        let mut r = TelemetryRing::new();
        r.emit(TelemetryEvent::new(0, AppId(1), EventKind::ShieldTriggered, FLAG_HIGH_PRIORITY, b"hit"));
        for _ in 0..TELEMETRY_CAPACITY + 10 {
            r.emit(TelemetryEvent::new(0, AppId(1), EventKind::OpenRead, 0, b"/data/x"));
        }
        let ev = r.drain();
        assert_eq!(ev.len(), TELEMETRY_CAPACITY);
        assert_eq!(ev[0].kind, EventKind::ShieldTriggered as u8);
        assert_eq!(r.dropped, 11);
    }

    #[test]
    fn cross_app_paths() {
        assert!(cross_app("/data/notes/keys.txt", "pysync"));
        assert!(!cross_app("/data/pysync/state", "pysync"));
        assert!(cross_app("/apps/launcher/startup.py", "pypersist"));
        assert!(!cross_app("/apps/pypersist/startup.py", "pypersist") && !cross_app("/apps", "x") && !cross_app("/apps/", "x"));
        assert!(!cross_app("/tmp/x", "pysync"));
    }

    #[test]
    fn modes_parse_and_default_to_enforcing() {
        assert_eq!(Mode::parse("monitor\n"), Some(Mode::Monitor));
        assert_eq!(Mode::parse("strict"), Some(Mode::Strict));
        assert_eq!(Mode::parse("off"), None);
        assert_eq!(Mode::parse(""), None);
        for m in [Mode::Monitor, Mode::Enforce, Mode::Strict] {
            assert_eq!(Mode::parse(m.name()), Some(m));
        }
    }

    #[test]
    fn record_lines_name_the_new_kinds() {
        let mut ev = TelemetryEvent::new(5_000_000, AppId(107), EventKind::NetTxSummary, FLAG_BACKGROUND, b"drop.c2-exfil.test");
        ev.port = 4444;
        ev.aux_u32 = 131136;
        let info = AppInfo { name: String::from("photosync"), sha8: String::from("97f769a4") };
        assert_eq!(record_line(&ev, Some(&info), 3), "5\t107\tphotosync\t97f769a4\tSOCK_TX\t1\t3\t131136B\tdrop.c2-exfil.test:4444");
        let q = TelemetryEvent::new(0, AppId(1), EventKind::Quarantined, 0, b"shield 7 for 30s");
        assert_eq!(record_line(&q, None, 0), "0\t1\t?\t00000000\tQUARANTINED\t0\t0\tshield 7 for 30s\t");
        for k in 1..=15 {
            assert_eq!(kind_from(k).unwrap() as u8, k);
        }
        assert!(kind_from(16).is_none());
    }
}
