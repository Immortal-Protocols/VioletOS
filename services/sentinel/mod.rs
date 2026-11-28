//! Sentinel: Violet Shield's observability link and signed-Shield verifier
//! (docs/violet-shield-rfc.md, docs/violet-shield-poc.md).
//!
//! It runs inside the Open Runtime's seL4 process, which apps cannot reach:
//! they hold no capability to it and their only way in is a trapped system
//! call. (The RFC puts it in a process of its own; that split is future work.)
//!
//! - Drains `TelemetryEvent`s from `ShieldPolicy` and sends them, as
//!   `violet-log-v1` records, over the paired Mac Agent connection or to
//!   `sentinel.violet` on the emulator's SOCKS5 relay. While
//!   the companion is away it keeps them in a bounded offline ring.
//! - Keeps a per-boot SHA-256 measurement chain over every program started and
//!   every Shield loaded or removed, and reports it in heartbeats.
//! - Verifies `VSH1` envelopes: magic, version, signing tier, TTL bounds,
//!   anti-replay sequence number, and a P-256 ECDSA signature by the pinned
//!   companion key (BearSSL, as the supervisor verifies catalogs). Only then
//!   does it hand the runtime a `Command`. While the Shield is monitoring
//!   (shield.rs `Mode::Monitor`) a verified command that would change an
//!   app's behaviour is refused; while it is enforcing they are carried out.
//!
//! The phone's own enforcement (built-in rules, rate limits, quarantine, the
//! on-device scorer) does not pass through here and needs no companion.
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(not(test))]
use super::net::{self, Net};
use super::policy::AppId;
pub use super::shield::{record_line, AppInfo};
use super::shield::{RuleMatcher, ShieldRule, ShieldVerdict, TelemetryEvent, FIRST_COMPANION_ID};

extern "C" {
    fn sentinel_sha256(data: *const u8, len: usize, out: *mut u8);
    fn sentinel_p256_verify(key: *const u8, hash: *const u8, sig: *const u8, sig_len: usize) -> i32;
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    unsafe { sentinel_sha256(data.as_ptr(), data.len(), out.as_mut_ptr()) };
    out
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Wire magic for signed Violet Shield envelopes.
pub const SHIELD_MAGIC: [u8; 4] = *b"VSH1";
pub const HEADER_LEN: usize = 32;
/// The companion endpoint, answered by the iPhone's SOCKS5 relay itself.
pub const ENDPOINT: &str = "sentinel.violet";
pub const PORT: u16 = 7000;
/// Longest containment the companion may sign without the owner.
pub const MAX_AUTO_TTL_SECS: u32 = 3600;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigningTier {
    /// The companion's key: reversible, TTL-bounded containment only.
    CompanionAuto = 1,
    /// Owner key (Face ID-gated Secure Enclave or the Mac developer key):
    /// permanent rules. Not provisioned in this proof of concept.
    OwnerConfirmed = 2,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Install = 1,
    Revoke = 2,
    Unfreeze = 3,
    Kill = 4,
    Freeze = 5,
    Warning = 6,
    /// Cut `target_pid` off from the network and other apps' files for `ttl_secs`.
    Quarantine = 7,
}

/// Parsed `VSH1` header (little endian):
/// `magic[4] version:u16 tier:u8 op:u8 shield_id:u32 seq:u64 ttl_secs:u32
///  target_pid:u32 payload_len:u16 sig_len:u16`, then the payload, then the
/// DER signature over everything before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedShieldHeader {
    pub version: u16,
    pub tier: u8,
    pub op: u8,
    pub shield_id: u32,
    pub seq_no: u64,
    pub ttl_secs: u32,
    pub target_pid: u32,
    pub payload_len: u16,
    pub sig_len: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentinelErr {
    BadMagic,
    BadVersion,
    Truncated,
    BadSignature,
    ReplaySequence,
    TtlRequiredForAutoTier,
    OwnerKeyNotProvisioned,
    BadOp,
    BadRule,
    RuleTableFull,
    NoSuchApp,
    MonitoringOnly,
}

impl SentinelErr {
    pub fn name(self) -> &'static str {
        match self {
            SentinelErr::BadMagic => "bad-magic",
            SentinelErr::BadVersion => "bad-version",
            SentinelErr::Truncated => "truncated",
            SentinelErr::BadSignature => "bad-signature",
            SentinelErr::ReplaySequence => "replayed-sequence",
            SentinelErr::TtlRequiredForAutoTier => "ttl-out-of-bounds",
            SentinelErr::OwnerKeyNotProvisioned => "owner-key-not-provisioned",
            SentinelErr::BadOp => "bad-op",
            SentinelErr::BadRule => "bad-rule",
            SentinelErr::RuleTableFull => "rule-table-full",
            SentinelErr::NoSuchApp => "no-such-app",
            SentinelErr::MonitoringOnly => "monitoring-only",
        }
    }
}

/// What the runtime must do after a verified Shield.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Install(ShieldRule),
    Revoke(u32),
    Freeze { pid: u32, until_ns: u64, shield_id: u32 },
    Unfreeze { pid: u32 },
    Kill { pid: u32, shield_id: u32 },
    Warning,
    Quarantine { pid: u32, secs: u32, shield_id: u32 },
}

pub fn parse_header(b: &[u8]) -> Result<SignedShieldHeader, SentinelErr> {
    if b.len() < HEADER_LEN {
        return Err(SentinelErr::Truncated);
    }
    if b[..4] != SHIELD_MAGIC {
        return Err(SentinelErr::BadMagic);
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let h = SignedShieldHeader {
        version: u16_at(4),
        tier: b[6],
        op: b[7],
        shield_id: u32_at(8),
        seq_no: u64::from_le_bytes(b[12..20].try_into().unwrap()),
        ttl_secs: u32_at(20),
        target_pid: u32_at(24),
        payload_len: u16_at(28),
        sig_len: u16_at(30),
    };
    if h.version != 1 {
        return Err(SentinelErr::BadVersion);
    }
    if b.len() != HEADER_LEN + h.payload_len as usize + h.sig_len as usize || h.sig_len > 72 {
        return Err(SentinelErr::Truncated);
    }
    Ok(h)
}

/// Rule payload: `matcher:u8 verdict:u8 flags:u8 len:u8 text[len]`.
/// matcher 1 path prefix (flags bit 0: writes only, bit 1: other apps' folders only), 2 host suffix
/// (text "host" or "host:port"), 3 W+X memory, 4 binary SHA-256 prefix (hex);
/// verdict 1 log, 2 deny, 3 freeze, 4 kill.
pub fn parse_rule(h: &SignedShieldHeader, payload: &[u8], now_ns: u64) -> Result<ShieldRule, SentinelErr> {
    if payload.len() < 4 || payload.len() != 4 + payload[3] as usize {
        return Err(SentinelErr::BadRule);
    }
    let text = core::str::from_utf8(&payload[4..]).map_err(|_| SentinelErr::BadRule)?;
    let app = if h.target_pid == 0 { None } else { Some(AppId(h.target_pid)) };
    let id = h.shield_id;
    // Ids below this are the phone's built-in Shields; a signed rule cannot take one.
    if id < FIRST_COMPANION_ID {
        return Err(SentinelErr::BadRule);
    }
    let on_match = match payload[1] {
        1 => ShieldVerdict::LogAlert { shield_id: id },
        2 => ShieldVerdict::Deny { shield_id: id },
        3 => ShieldVerdict::FreezeApp { shield_id: id },
        4 => ShieldVerdict::KillApp { shield_id: id },
        _ => return Err(SentinelErr::BadRule),
    };
    let matcher = match payload[0] {
        1 if text.starts_with('/') => RuleMatcher::PathPrefix {
            app,
            prefix: String::from(text),
            write_only: payload[2] & 1 != 0,
            cross_app_only: payload[2] & 2 != 0,
        },
        2 => {
            let (host, port) = match text.rsplit_once(':') {
                Some((h, p)) => (h, p.parse::<u16>().map_err(|_| SentinelErr::BadRule)?),
                None => (text, 0),
            };
            RuleMatcher::NetTarget { app, host_suffix: String::from(host), port }
        }
        3 => RuleMatcher::WriteExecMemory { app },
        4 if text.len() == 8 => {
            let mut p = [0u8; 4];
            for k in 0..4 {
                p[k] = u8::from_str_radix(&text[2 * k..2 * k + 2], 16).map_err(|_| SentinelErr::BadRule)?;
            }
            RuleMatcher::BinaryHash { sha256_prefix: p }
        }
        _ => return Err(SentinelErr::BadRule),
    };
    let expires_at_ns = if h.ttl_secs == 0 { 0 } else { now_ns + u64::from(h.ttl_secs) * 1_000_000_000 };
    Ok(ShieldRule { shield_id: id, expires_at_ns, matcher, on_match })
}

/// Per-boot software PCR: `chain = SHA256(chain || tag || digest)`.
#[derive(Clone, Copy, Debug)]
pub struct MeasurementChain {
    pub boot_id: [u8; 16],
    pub seq: u64,
    pub chain_digest: [u8; 32],
}

pub const MEASURE_BOOT: u8 = 1;
pub const MEASURE_EXEC: u8 = 2;
pub const MEASURE_SHIELD_LOAD: u8 = 3;
pub const MEASURE_SHIELD_UNLOAD: u8 = 4;
pub const MEASURE_KEY: u8 = 5;
/// The Shield's mode changed (digest of the new mode's number).
pub const MEASURE_MODE: u8 = 6;

impl MeasurementChain {
    pub fn new(boot_id: [u8; 16]) -> MeasurementChain {
        let mut m = MeasurementChain { boot_id, seq: 0, chain_digest: [0; 32] };
        let id = sha256(&boot_id);
        m.extend(MEASURE_BOOT, &id);
        m
    }
    pub fn extend(&mut self, tag: u8, item_digest: &[u8; 32]) {
        let mut buf = [0u8; 65];
        buf[..32].copy_from_slice(&self.chain_digest);
        buf[32] = tag;
        buf[33..].copy_from_slice(item_digest);
        self.chain_digest = sha256(&buf);
        self.seq = self.seq.wrapping_add(1);
    }
}

/// Verifies `VSH1` envelopes against the pinned key and tracks anti-replay state.
pub struct Verifier {
    pub companion_key: [u8; 65],
    pub last_seq: u64,
}

impl Verifier {
    pub fn verify<'a>(&mut self, envelope: &'a [u8]) -> Result<(SignedShieldHeader, &'a [u8]), SentinelErr> {
        let h = parse_header(envelope)?;
        let signed_len = HEADER_LEN + h.payload_len as usize;
        let digest = sha256(&envelope[..signed_len]);
        let sig = &envelope[signed_len..];
        let ok = unsafe { sentinel_p256_verify(self.companion_key.as_ptr(), digest.as_ptr(), sig.as_ptr(), sig.len()) };
        if ok != 1 {
            return Err(SentinelErr::BadSignature);
        }
        match h.tier {
            t if t == SigningTier::CompanionAuto as u8 => {
                if h.ttl_secs == 0 || h.ttl_secs > MAX_AUTO_TTL_SECS {
                    if matches!(h.op, 1 | 5 | 7) {
                        return Err(SentinelErr::TtlRequiredForAutoTier);
                    }
                }
            }
            // A signature by the companion key never carries owner authority.
            _ => return Err(SentinelErr::OwnerKeyNotProvisioned),
        }
        if h.seq_no <= self.last_seq {
            return Err(SentinelErr::ReplaySequence);
        }
        self.last_seq = h.seq_no;
        Ok((h, &envelope[HEADER_LEN..signed_len]))
    }
}

/// Turn a verified envelope into a runtime command.
pub fn command_for(h: &SignedShieldHeader, payload: &[u8], now_ns: u64) -> Result<Command, SentinelErr> {
    Ok(match h.op {
        1 => Command::Install(parse_rule(h, payload, now_ns)?),
        2 if h.shield_id >= FIRST_COMPANION_ID => Command::Revoke(h.shield_id),
        3 => Command::Unfreeze { pid: h.target_pid },
        // A signed command acts under a companion id, never one of the phone's own (below 16).
        4 => Command::Kill { pid: h.target_pid, shield_id: h.shield_id.max(FIRST_COMPANION_ID) },
        5 => Command::Freeze { pid: h.target_pid, until_ns: now_ns + u64::from(h.ttl_secs) * 1_000_000_000, shield_id: h.shield_id.max(FIRST_COMPANION_ID) },
        6 if payload.is_empty() && h.ttl_secs == 0 => Command::Warning,
        7 if payload.is_empty() && h.target_pid != 0 => Command::Quarantine { pid: h.target_pid, secs: h.ttl_secs, shield_id: h.shield_id.max(FIRST_COMPANION_ID) },
        _ => return Err(SentinelErr::BadOp),
    })
}

/// While the Shield is monitoring, a companion can still report telemetry,
/// warn and release, but its signed enforcement commands cannot change an
/// app's behavior.
fn monitoring_command_for(h: &SignedShieldHeader, payload: &[u8], now_ns: u64) -> Result<Command, SentinelErr> {
    match command_for(h, payload, now_ns)? {
        cmd @ (Command::Warning | Command::Revoke(_) | Command::Unfreeze { .. }) => Ok(cmd),
        _ => Err(SentinelErr::MonitoringOnly),
    }
}

/// The command a verified envelope becomes under the Shield's current mode.
pub fn policy_command_for(enforce: bool, h: &SignedShieldHeader, payload: &[u8], now_ns: u64) -> Result<Command, SentinelErr> {
    if enforce {
        command_for(h, payload, now_ns)
    } else {
        monitoring_command_for(h, payload, now_ns)
    }
}

// ---- link framing: [type:u8][len:u32 LE][payload] ----
pub const FRAME_HELLO: u8 = 0x01;
pub const FRAME_EVENTS: u8 = 0x02;
pub const FRAME_HEARTBEAT: u8 = 0x03;
pub const FRAME_RESULT: u8 = 0x04;
pub const FRAME_PROXY_SNAPSHOT: u8 = 0x05;
pub const FRAME_SHIELD: u8 = 0x10;

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.push(kind);
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.extend_from_slice(payload);
    f
}

/// Split complete frames off the front of `buf`.
pub fn take_frames(buf: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 5 {
            break;
        }
        let len = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
        if len > 4096 {
            // Nothing the companion sends is this large; drop the stream.
            buf.clear();
            break;
        }
        if buf.len() < 5 + len {
            break;
        }
        out.push((buf[0], buf[5..5 + len].to_vec()));
        buf.drain(..5 + len);
    }
    out
}

const OFFLINE_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Down { retry_at_ns: u64 },
    Connecting { slot: usize, since_ns: u64 },
    Up { slot: usize },
    Agent { last_poll_ns: u64 },
}

// Only the privileged runtime opens this endpoint, through the current data proxy.
// The reply is opaque to Sentinel and can only become an observation (frame 5).
#[cfg(not(test))]
#[derive(Default)]
struct ProxyObserver {
    slot: Option<usize>,
    route: u32,
    connected_at: u64,
    requested_at: Option<u64>,
    next_poll: u64,
    reply: Vec<u8>,
}

#[cfg(not(test))]
impl ProxyObserver {
    fn reset(&mut self) {
        if let Some(slot) = self.slot.take() { net::close(slot); }
        *self = Self::default();
    }

    fn poll(&mut self, net: &mut Net, now: u64, tx: &mut Vec<u8>) {
        let route = net.online();
        if self.route != route && self.slot.is_some() {
            self.reset();
            tx.extend(frame(FRAME_PROXY_SNAPSHOT, &[]));
        }
        if !matches!(route, 2 | 3) { return; }
        if self.slot.is_none() {
            if now < self.next_poll { return; }
            let address = net.address_for("shield-flows.violet");
            self.slot = net.connect(address, 7001).ok();
            self.route = route;
            self.connected_at = now;
            self.next_poll = now + 3_000_000_000;
        }
        let Some(slot) = self.slot else { return };
        let state = net.state(slot).0;
        if state == net::CONNECTING && now - self.connected_at < 8_000_000_000 { return; }
        if state == net::OPEN {
            if let Some(request) = self.requested_at {
                self.reply.extend(net.recv(slot, 3002 - self.reply.len()));
                if self.reply.len() >= 2 {
                    let size = u16::from_le_bytes([self.reply[0], self.reply[1]]) as usize;
                    if (1..=3000).contains(&size) && self.reply.len() == size + 2 && now - request <= 2_000_000_000 {
                        let mut body = Vec::from((self.connected_at / 1_000_000).to_le_bytes());
                        body.extend((request / 1_000_000).to_le_bytes());
                        body.extend(&self.reply[2..]);
                        tx.extend(frame(FRAME_PROXY_SNAPSHOT, &body));
                        self.reply.clear();
                        self.requested_at = None;
                        self.next_poll = now + 1_000_000_000;
                        return;
                    }
                    if (1..=3000).contains(&size) && self.reply.len() < size + 2
                        && now - request < 2_000_000_000 { return; }
                } else if now - request < 2_000_000_000 {
                    return;
                }
            } else {
                if now >= self.next_poll && tx.len() < 1900 && net.send(slot, &[1]) == 1 {
                    self.requested_at = Some(now);
                }
                return;
            }
        }
        self.reset();
        tx.extend(frame(FRAME_PROXY_SNAPSHOT, &[]));
        self.next_poll = now + 3_000_000_000;
    }
}

pub struct Sentinel {
    #[cfg(not(test))]
    observer: ProxyObserver,
    pub verifier: Verifier,
    pub chain: MeasurementChain,
    /// pid -> program, newest last (a few apps run at once).
    pub apps: Vec<(u32, AppInfo)>,
    /// Lines waiting for the companion (bounded; oldest go first).
    offline: VecDeque<String>,
    pub dropped: u32,
    rx: Vec<u8>,
    tx: Vec<u8>,
    link: Link,
    last_heartbeat_ns: u64,
    pub accepted: u32,
    pub rejected: u32,
    pub frozen_apps: u16,
    pub warnings: u32,
    pub connected_once: bool,
    /// The Shield is enforcing: verified Install, Freeze, Kill and Quarantine are carried out.
    pub enforce: bool,
    /// The phone's own counts for the heartbeat: calls refused, quarantines, things only reported.
    pub local: [u32; 3],
}

#[cfg(not(test))]
impl Sentinel {
    pub fn new(companion_key: [u8; 65], boot_id: [u8; 16]) -> Sentinel {
        let mut chain = MeasurementChain::new(boot_id);
        chain.extend(MEASURE_KEY, &sha256(&companion_key));
        Sentinel {
            observer: ProxyObserver::default(),
            verifier: Verifier { companion_key, last_seq: 0 },
            chain,
            apps: Vec::new(),
            offline: VecDeque::new(),
            dropped: 0,
            rx: Vec::new(),
            tx: Vec::new(),
            link: Link::Down { retry_at_ns: 2_000_000_000 },
            last_heartbeat_ns: 0,
            accepted: 0,
            rejected: 0,
            frozen_apps: 0,
            warnings: 0,
            connected_once: false,
            enforce: false,
            local: [0; 3],
        }
    }

    pub fn note_exec(&mut self, pid: u32, name: &str, program_digest: &[u8; 32]) {
        self.chain.extend(MEASURE_EXEC, program_digest);
        self.apps.retain(|(p, _)| *p != pid);
        if self.apps.len() >= 32 {
            self.apps.remove(0);
        }
        self.apps.push((pid, AppInfo { name: String::from(name), sha8: hex(&program_digest[..4]) }));
    }

    pub fn queue(&mut self, events: &[TelemetryEvent], idle_s: u32) {
        for ev in events {
            let info = self.apps.iter().find(|(p, _)| *p == ev.app_id).map(|(_, i)| i);
            let line = record_line(ev, info, idle_s);
            if self.offline.len() >= OFFLINE_CAPACITY {
                self.offline.pop_front();
                self.dropped = self.dropped.wrapping_add(1);
            }
            self.offline.push_back(line);
        }
    }

    pub fn online(&self) -> bool {
        matches!(self.link, Link::Up { .. } | Link::Agent { .. })
    }

    fn heartbeat(&self, now_ns: u64, active: usize, telemetry_dropped: u32) -> String {
        format!(
            "{} {} {} {} {} {} {} {}",
            now_ns / 1_000_000,
            self.chain.seq,
            hex(&self.chain.chain_digest),
            self.dropped.wrapping_add(telemetry_dropped),
            active,
            self.frozen_apps,
            self.accepted,
            self.rejected
        )
    }

    fn result(&mut self, text: &str) {
        let f = frame(FRAME_RESULT, text.as_bytes());
        self.tx.extend_from_slice(&f);
    }

    fn start_link(&mut self) {
        self.connected_once = true;
        self.rx.clear();
        self.tx.clear();
        let hello = format!("VSL1 boot={} chain={} key={}", hex(&self.chain.boot_id), hex(&self.chain.chain_digest),
                            hex(&sha256(&self.verifier.companion_key)[..8]));
        self.tx.extend_from_slice(&frame(FRAME_HELLO, hello.as_bytes()));
        self.last_heartbeat_ns = 0;
    }

    /// The stock handset Agent forwards this stream only over its paired Mac session.
    pub fn agent_pull(&mut self, now_ns: u64, reset: bool) -> String {
        if reset || !matches!(self.link, Link::Agent { .. }) {
            if let Link::Up { slot } | Link::Connecting { slot, .. } = self.link {
                net::close(slot);
            }
            self.observer.reset();
            self.start_link();
            crate::log!("sentinel: linked through Mac Agent");
        }
        self.link = Link::Agent { last_poll_ns: now_ns };
        // /dev/violet/sys has a 4096-byte response buffer; hex doubles the size.
        let n = self.tx.len().min(1900);
        let output = hex(&self.tx[..n]);
        self.tx.drain(..n);
        output
    }

    pub fn agent_push(&mut self, text: &str) -> bool {
        if !matches!(self.link, Link::Agent { .. }) || text.is_empty() || text.len() > 1024
            || text.len() % 2 != 0 || self.rx.len() + text.len() / 2 > 4096 {
            return false;
        }
        let data: Option<Vec<u8>> = text.as_bytes().chunks_exact(2).map(|pair| {
            u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()
        }).collect();
        if let Some(data) = data {
            self.rx.extend_from_slice(&data);
            true
        } else {
            false
        }
    }

    fn receive(&mut self, now_ns: u64, commands: &mut Vec<Command>) {
        for (kind, payload) in take_frames(&mut self.rx) {
            if kind != FRAME_SHIELD {
                continue;
            }
            let enforce = self.enforce;
            match self.verifier.verify(&payload).and_then(|(h, body)| Ok((h, policy_command_for(enforce, &h, body, now_ns)?))) {
                Ok((h, cmd)) => {
                    self.accepted += 1;
                    if cmd != Command::Warning {
                        self.chain.extend(MEASURE_SHIELD_LOAD, &sha256(&payload));
                    }
                    crate::log!("sentinel: shield {} op {} seq {} accepted", h.shield_id, h.op, h.seq_no);
                    self.result(&format!("ok {} {} {}", h.seq_no, h.op, h.shield_id));
                    commands.push(cmd);
                }
                Err(e) => {
                    self.rejected += 1;
                    let seq = parse_header(&payload).map(|h| h.seq_no).unwrap_or(0);
                    crate::log!("sentinel: shield rejected: {}", e.name());
                    self.result(&format!("err {} {}", seq, e.name()));
                }
            }
        }
    }

    fn collect(&mut self, now_ns: u64, active_shields: usize, telemetry_dropped: u32) {
        // Events: as many whole lines as fit.
        if !self.offline.is_empty() && self.tx.len() < 4096 {
            let mut text = String::new();
            while let Some(line) = self.offline.front() {
                if text.len() + line.len() + 1 > 3000 {
                    break;
                }
                text.push_str(line);
                text.push('\n');
                self.offline.pop_front();
            }
            if !text.is_empty() {
                self.tx.extend_from_slice(&frame(FRAME_EVENTS, text.as_bytes()));
            }
        }
        if now_ns.wrapping_sub(self.last_heartbeat_ns) >= 2_000_000_000 {
            self.last_heartbeat_ns = now_ns;
            let heartbeat = self.heartbeat(now_ns, active_shields, telemetry_dropped);
            // Earlier events may still be queued when the Agent transport is busy.
            let through = self.offline.front().and_then(|line| line.split_once('\t'))
                .and_then(|(ms, _)| ms.parse::<u64>().ok()).map(|ms| ms.saturating_sub(1))
                .unwrap_or(now_ns / 1_000_000);
            // After the nine fields older companions read: the phone's own
            // enforcement (mode 0 monitor / 1 enforcing, refused, quarantines, reported).
            let hb = format!("{} {} {} {} {} {}", heartbeat, through, self.enforce as u8, self.local[0], self.local[1], self.local[2]);
            self.tx.extend_from_slice(&frame(FRAME_HEARTBEAT, hb.as_bytes()));
        }
    }

    /// Move the link along; returns verified commands for the runtime.
    pub fn tick(&mut self, net: &mut Net, now_ns: u64, active_shields: usize, telemetry_dropped: u32) -> Vec<Command> {
        let mut commands = Vec::new();
        match self.link {
            Link::Down { retry_at_ns } => {
                if now_ns >= retry_at_ns && net.online() != 0 {
                    let ip = net.address_for(ENDPOINT);
                    match net.connect(ip, PORT) {
                        Ok(slot) => self.link = Link::Connecting { slot, since_ns: now_ns },
                        Err(_) => self.link = Link::Down { retry_at_ns: now_ns + 5_000_000_000 },
                    }
                }
            }
            Link::Connecting { slot, since_ns } => match net.state(slot) {
                (net::OPEN, _) => {
                    self.link = Link::Up { slot };
                    self.start_link();
                    crate::log!("sentinel: linked to {}", ENDPOINT);
                }
                (net::CONNECTING, _) if now_ns - since_ns < 10_000_000_000 => {}
                _ => {
                    net::close(slot);
                    self.link = Link::Down { retry_at_ns: now_ns + 5_000_000_000 };
                }
            },
            Link::Up { slot } => {
                let (state, _) = net.state(slot);
                if state != net::OPEN {
                    net::close(slot);
                    crate::log!("sentinel: link to {} lost", ENDPOINT);
                    self.link = Link::Down { retry_at_ns: now_ns + 3_000_000_000 };
                    return commands;
                }
                let incoming = net.recv(slot, 8192);
                self.rx.extend_from_slice(&incoming);
                self.receive(now_ns, &mut commands);
                self.collect(now_ns, active_shields, telemetry_dropped);
                if !self.tx.is_empty() {
                    let n = net.send(slot, &self.tx);
                    self.tx.drain(..n);
                }
            }
            Link::Agent { last_poll_ns } => {
                if now_ns.saturating_sub(last_poll_ns) > 5_000_000_000 {
                    self.observer.reset();
                    self.link = Link::Down { retry_at_ns: now_ns };
                    crate::log!("sentinel: Mac Agent link lost");
                } else {
                    self.observer.poll(net, now_ns, &mut self.tx);
                    self.receive(now_ns, &mut commands);
                    self.collect(now_ns, active_shields, telemetry_dropped);
                }
            }
        }
        commands
    }

    /// The Shield's mode changed; the companion sees it in the measurement chain.
    pub fn note_mode(&mut self, mode: u8) {
        self.chain.extend(MEASURE_MODE, &sha256(&[mode]));
    }

    pub fn note_unload(&mut self, shield_id: u32) {
        self.chain.extend(MEASURE_SHIELD_UNLOAD, &sha256(&shield_id.to_le_bytes()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }

    /// Envelope fixtures signed by tools/shield-trainer/vsh1.py (see test_sentinel.py).
    fn fixtures() -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
        let text = std::fs::read_to_string(std::env::var("VSH1_FIXTURES").unwrap()).unwrap();
        let mut key = Vec::new();
        let mut cases = Vec::new();
        for line in text.lines() {
            let (name, hexs) = line.split_once(' ').unwrap();
            let bytes: Vec<u8> = (0..hexs.len() / 2).map(|i| u8::from_str_radix(&hexs[2 * i..2 * i + 2], 16).unwrap()).collect();
            if name == "key" {
                key = bytes;
            } else {
                cases.push((String::from(name), bytes));
            }
        }
        (key, cases)
    }

    #[test]
    fn verifies_signed_shields_and_rejects_the_rest() {
        let (key, cases) = fixtures();
        let mut v = Verifier { companion_key: key.try_into().unwrap(), last_seq: 0 };
        for (name, env) in &cases {
            let r = v.verify(env).map(|(h, body)| command_for(&h, body, 1_000));
            let expect = name.split(':').nth(1).unwrap();
            match (expect, &r) {
                ("ok", Ok(Ok(_))) => {}
                ("ok", other) => panic!("{}: {:?}", name, other),
                (e, Err(err)) => assert_eq!(err.name(), e, "{}", name),
                (e, Ok(Ok(_))) => panic!("{} should fail with {}", name, e),
                (e, Ok(Err(err))) => assert_eq!(err.name(), e, "{}", name),
            }
        }
        let _ = le16(0);
    }

    #[test]
    fn monitoring_accepts_warnings_and_rejects_enforcement() {
        let (key, cases) = fixtures();
        let mut v = Verifier { companion_key: key.try_into().unwrap(), last_seq: 0 };
        let mut enforced = 0;
        for (_, env) in &cases {
            let Ok((h, body)) = v.verify(env) else { continue };
            if matches!(command_for(&h, body, 1_000), Ok(Command::Install(_) | Command::Freeze { .. } | Command::Kill { .. } | Command::Quarantine { .. })) {
                assert_eq!(policy_command_for(false, &h, body, 1_000), Err(SentinelErr::MonitoringOnly));
                assert_eq!(policy_command_for(true, &h, body, 1_000), command_for(&h, body, 1_000));
                enforced += 1;
            }
        }
        let warning = SignedShieldHeader {
            version: 1, tier: 1, op: Op::Warning as u8, shield_id: 1, seq_no: 1,
            ttl_secs: 0, target_pid: 0, payload_len: 0, sig_len: 0,
        };
        assert_eq!(monitoring_command_for(&warning, &[], 1_000), Ok(Command::Warning));
        assert_eq!(monitoring_command_for(&warning, b"unexpected", 1_000), Err(SentinelErr::BadOp));
        assert!(enforced >= 4, "install, path rule, freeze and quarantine fixtures: {}", enforced);
        // A signed quarantine names a process and never borrows a built-in id.
        let q = SignedShieldHeader { op: Op::Quarantine as u8, shield_id: 4, ttl_secs: 60, target_pid: 107, ..warning };
        assert_eq!(command_for(&q, &[], 0), Ok(Command::Quarantine { pid: 107, secs: 60, shield_id: FIRST_COMPANION_ID }));
        assert_eq!(command_for(&SignedShieldHeader { target_pid: 0, ..q }, &[], 0), Err(SentinelErr::BadOp));
        // Nor can a signed rule take one of the phone's own ids.
        let rule = SignedShieldHeader { op: Op::Install as u8, shield_id: 1, ttl_secs: 60, ..warning };
        assert_eq!(command_for(&rule, &[3, 2, 0, 0], 0), Err(SentinelErr::BadRule));
        assert!(matches!(command_for(&SignedShieldHeader { shield_id: 16, ..rule }, &[3, 2, 0, 0], 0), Ok(Command::Install(_))));
        // A signed freeze or end acts under a companion id too, and the
        // phone's own Shields (the scorer's freeze, say) cannot be revoked.
        let f = SignedShieldHeader { op: Op::Freeze as u8, shield_id: 8, ttl_secs: 60, target_pid: 107, ..warning };
        assert_eq!(command_for(&f, &[], 0), Ok(Command::Freeze { pid: 107, until_ns: 60_000_000_000, shield_id: FIRST_COMPANION_ID }));
        assert_eq!(command_for(&SignedShieldHeader { op: Op::Kill as u8, ..f }, &[], 0), Ok(Command::Kill { pid: 107, shield_id: FIRST_COMPANION_ID }));
        assert_eq!(command_for(&SignedShieldHeader { op: Op::Revoke as u8, ..f }, &[], 0), Err(SentinelErr::BadOp));
        assert_eq!(command_for(&SignedShieldHeader { op: Op::Revoke as u8, shield_id: 40, ..f }, &[], 0), Ok(Command::Revoke(40)));
    }

    #[test]
    fn measurement_chain_is_order_sensitive() {
        let mut a = MeasurementChain::new([1; 16]);
        let mut b = MeasurementChain::new([1; 16]);
        a.extend(MEASURE_EXEC, &[2; 32]);
        a.extend(MEASURE_SHIELD_LOAD, &[3; 32]);
        b.extend(MEASURE_SHIELD_LOAD, &[3; 32]);
        b.extend(MEASURE_EXEC, &[2; 32]);
        assert_ne!(a.chain_digest, b.chain_digest);
        assert_eq!(a.seq, 3);
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn frames_split_across_reads() {
        let mut buf = frame(FRAME_SHIELD, b"hello");
        buf.extend_from_slice(&frame(FRAME_HELLO, b"x")[..3]);
        let got = take_frames(&mut buf);
        assert_eq!(got, alloc::vec![(FRAME_SHIELD, b"hello".to_vec())]);
        assert_eq!(buf.len(), 3);
    }
}
