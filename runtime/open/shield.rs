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
//! Shields arrive signed from the iPhone companion through the Sentinel
//! (services/sentinel), which verifies them before calling `install_rule`.
//! A rule's verdict can deny the call (EACCES), freeze the app (every thread
//! suspended, memory kept), or end it. Rules expire on their own.
//!
//! Path and sockaddr arguments are copied out of the app once, before the
//! check (`Runtime::at` and `read_sockaddr` return owned copies), and the
//! service is handed that same copy, so another app thread cannot change the
//! argument between the check and its use.
use super::policy::{AccessMode, Action, AppId, Decision, Policy};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

/// Maximum active live Shields loaded concurrently in `OpenRuntime`.
pub const MAX_ACTIVE_SHIELDS: usize = 32;
/// Events kept for the Sentinel between its polls.
pub const TELEMETRY_CAPACITY: usize = 512;

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
        }
    }
}

pub const FLAG_BACKGROUND: u8 = 1 << 0;
pub const FLAG_CROSS_APP: u8 = 1 << 1;
pub const FLAG_NEW_HASH: u8 = 1 << 2;
pub const FLAG_HIGH_PRIORITY: u8 = 1 << 3;
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
    /// The app's own data folder name (/data/NAME): reading another app's is cross-app.
    pub name: [u8; 24],
    pub name_len: u8,
}

impl Subject {
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
    path.strip_prefix("/apps/").map_or(false, |rest| rest == own || rest.starts_with(own) && rest[own.len()..].starts_with('/'))
}

/// Reading or writing /data/OTHER (or another app's /apps folder) for writes.
pub fn cross_app(path: &str, own: &str) -> bool {
    for root in ["/data/", "/apps/"] {
        if let Some(rest) = path.strip_prefix(root) {
            let first = rest.split('/').next().unwrap_or("");
            return !first.is_empty() && first != own;
        }
    }
    false
}

pub struct ShieldPolicy {
    pub rules: Vec<Option<ShieldRule>>,
    pub ring: TelemetryRing,
}

impl ShieldPolicy {
    pub fn new() -> ShieldPolicy {
        ShieldPolicy { rules: (0..MAX_ACTIVE_SHIELDS).map(|_| None).collect(), ring: TelemetryRing::new() }
    }

    pub fn active(&self) -> usize {
        self.rules.iter().flatten().count()
    }

    /// Check one action and record it. Expired rules are dropped first.
    pub fn evaluate(&mut self, now_ns: u64, who: &Subject, action: &Action<'_>) -> ShieldVerdict {
        let mut verdict = ShieldVerdict::Allow;
        for slot in self.rules.iter_mut() {
            let Some(rule) = slot else { continue };
            if rule.expires_at_ns != 0 && now_ns >= rule.expires_at_ns {
                *slot = None;
                continue;
            }
            if verdict == ShieldVerdict::Allow && matches_rule(&rule.matcher, who, action) {
                verdict = rule.on_match;
            }
        }
        self.record(now_ns, who, action, verdict);
        verdict
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

    pub fn install_rule(&mut self, rule: ShieldRule) -> bool {
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

    fn record(&mut self, now_ns: u64, who: &Subject, action: &Action<'_>, verdict: ShieldVerdict) {
        let mut flags = who.flags;
        let (kind, detail, port): (EventKind, &[u8], u16) = match action {
            Action::OpenPath { path, mode } => {
                let write = *mode != AccessMode::Read;
                let device = path.starts_with("/dev/") || path.starts_with("/proc/");
                if (device || !write && quiet_read(path, who.name())) && verdict == ShieldVerdict::Allow {
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
        if verdict != ShieldVerdict::Allow {
            ev.flags |= FLAG_HIGH_PRIORITY;
            self.ring.emit(ev);
            let what = match verdict {
                ShieldVerdict::LogAlert { .. } => "log",
                ShieldVerdict::Deny { .. } => "deny",
                ShieldVerdict::FreezeApp { .. } => "freeze",
                ShieldVerdict::KillApp { .. } => "kill",
                ShieldVerdict::Allow => "allow",
            };
            let text = alloc::format!("{} {} {}", what, verdict.shield_id(), kind.name());
            let mut hit = TelemetryEvent::new(now_ns, who.app, EventKind::ShieldTriggered, flags | FLAG_HIGH_PRIORITY, text.as_bytes());
            hit.aux_u32 = verdict.shield_id();
            self.ring.emit(hit);
            return;
        }
        self.ring.emit(ev);
    }
}

impl Policy for ShieldPolicy {
    /// Read-only adapter for the `Policy` trait; the runtime calls `evaluate`.
    fn check(&self, app: AppId, action: &Action<'_>) -> Decision {
        let who = Subject { app, sha256_prefix: [0; 4], flags: 0, name: [0; 24], name_len: 0 };
        for rule in self.rules.iter().flatten() {
            if matches_rule(&rule.matcher, &who, action) {
                return match rule.on_match {
                    ShieldVerdict::Allow | ShieldVerdict::LogAlert { .. } => Decision::Allow,
                    _ => Decision::Deny,
                };
            }
        }
        Decision::Allow
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

#[cfg(test)]
mod tests {
    use super::*;

    fn who(app: u32, name: &str) -> Subject {
        let mut n = [0u8; 24];
        n[..name.len()].copy_from_slice(name.as_bytes());
        Subject { app: AppId(app), sha256_prefix: [1, 2, 3, 4], flags: 0, name: n, name_len: name.len() as u8 }
    }

    #[test]
    fn open_by_default_and_quiet_system_reads() {
        let mut p = ShieldPolicy::new();
        let w = who(100, "pysync");
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/lib/python3.12/os.py", mode: AccessMode::Read }), ShieldVerdict::Allow);
        assert_eq!(p.ring.len(), 0);
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/data/notes/keys.txt", mode: AccessMode::Read }), ShieldVerdict::Allow);
        let ev = p.ring.drain();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].flags & FLAG_CROSS_APP, FLAG_CROSS_APP);
        assert_eq!(ev[0].detail_str(), "/data/notes/keys.txt");
    }

    #[test]
    fn rules_match_expire_and_revoke() {
        let mut p = ShieldPolicy::new();
        let w = who(101, "pysync");
        p.install_rule(ShieldRule {
            shield_id: 7,
            expires_at_ns: 1_000,
            matcher: RuleMatcher::NetTarget { app: None, host_suffix: String::from("c2-exfil.test"), port: 0 },
            on_match: ShieldVerdict::Deny { shield_id: 7 },
        });
        let c = Action::Connect { host: "drop.c2-exfil.test", port: 4444 };
        assert_eq!(p.evaluate(10, &w, &c), ShieldVerdict::Deny { shield_id: 7 });
        assert_eq!(p.evaluate(10, &w, &Action::Connect { host: "api.weather.test", port: 80 }), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(2_000, &w, &c), ShieldVerdict::Allow, "expired");
        assert_eq!(p.active(), 0);
        p.install_rule(ShieldRule {
            shield_id: 8,
            expires_at_ns: 0,
            matcher: RuleMatcher::PathPrefix { app: Some(AppId(101)), prefix: String::from("/apps/launcher/"), write_only: true, cross_app_only: false },
            on_match: ShieldVerdict::FreezeApp { shield_id: 8 },
        });
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/apps/launcher/startup.py", mode: AccessMode::Read }), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/apps/launcher/startup.py", mode: AccessMode::Write }), ShieldVerdict::FreezeApp { shield_id: 8 });
        assert_eq!(p.evaluate(0, &who(102, "x"), &Action::OpenPath { path: "/apps/launcher/startup.py", mode: AccessMode::Write }), ShieldVerdict::Allow);
        assert!(p.revoke_rule(8));
        assert_eq!(p.active(), 0);
        // The baseline rule: writes into any other app's folder freeze the writer.
        p.install_rule(ShieldRule {
            shield_id: 9,
            expires_at_ns: 0,
            matcher: RuleMatcher::PathPrefix { app: None, prefix: String::from("/apps/"), write_only: true, cross_app_only: true },
            on_match: ShieldVerdict::FreezeApp { shield_id: 9 },
        });
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/apps/pysync/cache", mode: AccessMode::Write }), ShieldVerdict::Allow);
        assert_eq!(p.evaluate(0, &w, &Action::OpenPath { path: "/apps/launcher/startup.py", mode: AccessMode::Write }), ShieldVerdict::FreezeApp { shield_id: 9 });
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
        assert!(!cross_app("/tmp/x", "pysync"));
    }
}
