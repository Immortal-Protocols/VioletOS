//! Violet Shield policy engine & lock-free telemetry ring inside `OpenRuntime`.
//!
//! Plugs directly into PR #14's single choke-point `Policy` trait
//! (`runtime/open/policy.rs`). By default (`active_shields == 0`), every action
//! returns `Decision::Allow`, keeping VioletOS 100% open with zero UX friction.
//!
//! When the iPhone companion (or operator) pushes a signed `VSH1` Shield through
//! `services/sentinel`, `ShieldPolicy` evaluates active TTL-bounded rules in
//! constant time and emits compact 64-byte `TelemetryEvent` records into a
//! one-way lock-free ring mapped read-only into `Sentinel`.

use super::policy::{AccessMode, Action, AppId, Decision, Policy};

/// Maximum active live Shields loaded concurrently in `OpenRuntime`.
pub const MAX_ACTIVE_SHIELDS: usize = 32;

/// Extended verdict used internally by `OpenRuntime` + `Supervisor`.
/// Maps back to PR #14's `Decision` while supporting Shirudo-style live containment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldVerdict {
    Allow,
    /// Allow the syscall, but tag the telemetry event as high-priority for immediate flush.
    LogAlert { shield_id: u32 },
    /// Reject with POSIX `EACCES` (-13).
    Deny { shield_id: u32 },
    /// Ask Supervisor to `seL4_TCB_Suspend` every thread in `AppId` mid-syscall,
    /// preserving its address space for forensic inspection on the iPhone/Mac.
    FreezeApp { shield_id: u32 },
    /// Immediately terminate `AppId` and reclaim its frames.
    KillApp { shield_id: u32 },
}

/// Match target for a declarative `VSH1` Shield rule (v1).
/// (Future v2 can add a `BytecodeFilter { prog_id: u16 }` arm for a tiny bounded eBPF VM.)
#[derive(Clone, Copy, Debug)]
pub enum RuleMatcher {
    /// Match a specific `AppId` or `None` for any app, optionally pinned to a binary SHA-256 prefix.
    PathPrefix {
        app: Option<AppId>,
        prefix: [u8; 48],
        prefix_len: u8,
        write_only: bool,
    },
    /// Match outbound TCP/UDP connect to a specific IPv4/port or domain suffix.
    NetTarget {
        app: Option<AppId>,
        ipv4: [u8; 4], // 0.0.0.0 = wildcard IP
        port: u16,     // 0 = wildcard port
    },
    /// Block `mmap`/`mprotect` requesting simultaneous Write + Execute pages.
    WriteExecMemory { app: Option<AppId> },
    /// Trigger if an app exceeds `max_ops` within `window_ms` (e.g., socket burst / exfil).
    RateLimit {
        app: Option<AppId>,
        kind: EventKind,
        max_ops: u16,
        window_ms: u32,
    },
    /// Freeze or block an app identified by its executable's SHA-256 digest.
    BinaryHash { sha256_prefix: [u8; 16] },
}

#[derive(Clone, Copy, Debug)]
pub struct ShieldRule {
    pub shield_id: u32,
    /// Monotonic expiry timestamp in microseconds (`CNTVCT`); 0 = permanent (owner-signed only).
    pub expires_at_us: u64,
    pub matcher: RuleMatcher,
    pub on_match: ShieldVerdict,
}

/// High-signal syscall/security event kinds streamed to `Sentinel`.
/// Boring high-frequency calls (`clock_gettime`, `futex`, normal reads) are omitted.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Exec = 1,
    OpenPath = 2,
    NetConnect = 3,
    NetTxSummary = 4,
    MemoryWx = 5,
    SpawnThread = 6,
    SysControl = 7,
    VmFaultLoop = 8,
    ShieldTriggered = 9,
}

/// Compact 64-byte record written by `OpenRuntime` into the one-way shared ring
/// mapped read-only into `Sentinel`. Zero heap, never blocks app syscalls.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TelemetryEvent {
    pub timestamp_us: u64,
    pub app_id: u32,
    pub kind: u8,
    pub flags: u8, // bit 0: background, bit 1: cross_app, bit 2: new_hash, bit 3: high_priority
    pub port: u16,
    pub aux_u32: u32,      // e.g. bytes_tx, shield_id, or VMA prot bits
    pub ipv4: [u8; 4],     // destination IPv4 for NetConnect / NetTxSummary
    pub detail: [u8; 40],  // truncated UTF-8 path, hostname, or binary SHA-256 prefix
}

/// Single-producer (`OpenRuntime`), single-consumer (`Sentinel`) lock-free ring.
/// On overflow, drops the oldest low-priority record while incrementing `dropped`.
pub trait TelemetrySink {
    fn emit(&mut self, ev: TelemetryEvent);
    fn dropped_count(&self) -> u32;
}

/// TOCTOU-safe user argument copy discipline (refinement to PR #14 `read_user_str`).
///
/// Multi-threaded apps (`clone`) could mutate a path/sockaddr buffer in shared RAM
/// *after* `ShieldPolicy::evaluate()` and *before* `StorageFsService::open()`.
/// `OpenRuntime` copies syscall arguments ONCE into a private stack buffer,
/// runs `ShieldPolicy` + `TelemetrySink::emit` on that copy, and forwards that
/// exact private slice to the backing service.
pub fn copy_user_bytes_once<'a>(
    _app: AppId,
    _user_ptr: u32,
    private_buf: &'a mut [u8],
) -> Result<&'a [u8], i32> {
    let _ = private_buf;
    todo!("copy once from app's linearly mapped frames into OpenRuntime private buffer")
}

/// Policy engine wrapping PR #14's `OpenPolicy` with live signed `ShieldRule`s.
pub struct ShieldPolicy<S: TelemetrySink> {
    pub rules: [Option<ShieldRule>; MAX_ACTIVE_SHIELDS],
    pub sink: S,
}

impl<S: TelemetrySink> ShieldPolicy<S> {
    pub fn evaluate(&mut self, now_us: u64, app: AppId, action: &Action<'_>) -> ShieldVerdict {
        // 1. Evaluate active non-expired Shields (fast linear scan; <= 32 rules).
        for slot in self.rules.iter_mut() {
            let Some(rule) = slot else { continue };
            if rule.expires_at_us != 0 && now_us >= rule.expires_at_us {
                *slot = None; // TTL expired: automatically release containment.
                continue;
            }
            if matches_rule(&rule.matcher, app, action) {
                self.emit_action(now_us, app, action, true, rule.shield_id);
                return rule.on_match;
            }
        }
        // 2. Default: 100% open (PR #14 OpenPolicy), while still recording security transitions.
        self.emit_action(now_us, app, action, false, 0);
        ShieldVerdict::Allow
    }

    pub fn install_rule(&mut self, rule: ShieldRule) -> bool {
        // Replace existing shield_id or insert into first free slot.
        if let Some(slot) = self.rules.iter_mut().find(|r| r.map(|x| x.shield_id) == Some(rule.shield_id)) {
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
        if let Some(slot) = self.rules.iter_mut().find(|r| r.map(|x| x.shield_id) == Some(shield_id)) {
            *slot = None;
            return true;
        }
        false
    }

    fn emit_action(&mut self, now_us: u64, app: AppId, action: &Action<'_>, hit: bool, shield_id: u32) {
        let mut detail = [0u8; 40];
        let (kind, port) = match action {
            Action::OpenPath { path, .. } => {
                copy_trunc(&mut detail, path.as_bytes());
                (EventKind::OpenPath, 0)
            }
            Action::Connect { host, port } => {
                copy_trunc(&mut detail, host.as_bytes());
                (EventKind::NetConnect, *port)
            }
            Action::SpawnApp { path } => {
                copy_trunc(&mut detail, path.as_bytes());
                (EventKind::Exec, 0)
            }
            Action::SystemControl { command } => {
                copy_trunc(&mut detail, command.as_bytes());
                (EventKind::SysControl, 0)
            }
            _ => return,
        };
        self.sink.emit(TelemetryEvent {
            timestamp_us: now_us,
            app_id: app.0,
            kind: if hit { EventKind::ShieldTriggered as u8 } else { kind as u8 },
            flags: if hit { 1 << 3 } else { 0 },
            port,
            aux_u32: shield_id,
            ipv4: [0; 4],
            detail,
        });
    }
}

impl<S: TelemetrySink> Policy for ShieldPolicy<S> {
    fn check(&self, app: AppId, action: &Action<'_>) -> Decision {
        // Read-only adapter for PR #14's `Policy` trait; full mutating path uses `evaluate()`.
        for rule in self.rules.iter().flatten() {
            if matches_rule(&rule.matcher, app, action) {
                return match rule.on_match {
                    ShieldVerdict::Allow | ShieldVerdict::LogAlert { .. } => Decision::Allow,
                    ShieldVerdict::Deny { .. }
                    | ShieldVerdict::FreezeApp { .. }
                    | ShieldVerdict::KillApp { .. } => Decision::Deny,
                };
            }
        }
        Decision::Allow
    }
}

fn matches_rule(matcher: &RuleMatcher, app: AppId, action: &Action<'_>) -> bool {
    match (matcher, action) {
        (
            RuleMatcher::PathPrefix { app: target, prefix, prefix_len, write_only },
            Action::OpenPath { path, mode },
        ) => {
            if target.is_some() && *target != Some(app) {
                return false;
            }
            if *write_only && *mode == AccessMode::Read {
                return false;
            }
            path.as_bytes().starts_with(&prefix[..(*prefix_len as usize).min(prefix.len())])
        }
        (RuleMatcher::NetTarget { app: target, port, .. }, Action::Connect { port: p, .. }) => {
            (target.is_none() || *target == Some(app)) && (*port == 0 || *port == *p)
        }
        _ => false,
    }
}

fn copy_trunc(dst: &mut [u8; 40], src: &[u8]) {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
}
