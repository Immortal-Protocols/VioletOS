//! Violet Shield at the Open Runtime's choke point: the check every guarded
//! system call goes through, freezing and thawing apps, bytes-sent telemetry,
//! and the Sentinel's periodic turn (docs/violet-shield-poc.md).
use super::policy::{AccessMode, Action, AppId};
use super::sentinel::Command;
use super::shield::{self, EventKind, ShieldVerdict, Subject, TelemetryEvent};
use super::{sys, Frozen, Gate, Runtime, TxStat, Wait, MAX_APPS};
use alloc::string::String;
use alloc::vec::Vec;

/// Report bytes sent after this many accumulate, or after a pause.
const TX_REPORT_BYTES: u32 = 64 * 1024;
const TX_REPORT_NS: u64 = 500_000_000;

impl Runtime {
    pub fn subject(&self, i: usize) -> Subject {
        let app = self.apps[i].as_ref().unwrap();
        let mut name = [0u8; 24];
        let n = app.name.len().min(24);
        name[..n].copy_from_slice(&app.name.as_bytes()[..n]);
        let mut flags = 0;
        if !(self.focused && self.foreground == Some(i)) {
            flags |= shield::FLAG_BACKGROUND;
        }
        if app.new_hash {
            flags |= shield::FLAG_NEW_HASH;
        }
        Subject {
            app: AppId(app.pid),
            sha256_prefix: app.digest[..4].try_into().unwrap(),
            flags,
            name,
            name_len: n as u8,
        }
    }

    /// Seconds since the last touch on the open apps' screen.
    pub fn idle_secs(&self) -> u32 {
        ((sys::now_ns().saturating_sub(self.last_touch_ns)) / 1_000_000_000).min(u32::MAX as u64) as u32
    }

    /// Check `action` by app `i` against the active Shields and record it.
    pub fn gate(&mut self, i: usize, action: &Action<'_>) -> Gate {
        let who = self.subject(i);
        let now = sys::now_ns();
        match self.shield.evaluate(now, &who, action) {
            ShieldVerdict::Allow | ShieldVerdict::LogAlert { .. } => Gate::Allow,
            ShieldVerdict::Deny { shield_id } => {
                crate::log!("shield {}: denied {} (pid {})", shield_id, describe(action), who.app.0);
                Gate::Deny
            }
            ShieldVerdict::FreezeApp { shield_id } => {
                let until = self.shield.rules.iter().flatten().find(|r| r.shield_id == shield_id).map_or(0, |r| r.expires_at_ns);
                crate::log!("shield {}: froze pid {} at {}", shield_id, who.app.0, describe(action));
                self.freeze(i, until, shield_id);
                Gate::Frozen
            }
            ShieldVerdict::KillApp { shield_id } => {
                crate::log!("shield {}: ended pid {} at {}", shield_id, who.app.0, describe(action));
                let app = self.apps[i].as_mut().unwrap();
                app.exit_code = Some(137);
                self.reap(i);
                Gate::Killed
            }
        }
    }

    /// Suspend every running thread of app `i`. Threads blocked in a call stay
    /// blocked: their waits are not served while the app is frozen.
    pub fn freeze(&mut self, i: usize, until_ns: u64, shield_id: u32) {
        let now = sys::now_ns();
        let Some(app) = self.apps[i].as_mut() else { return };
        if let Some(f) = app.frozen.as_mut() {
            f.until_ns = if f.until_ns == 0 || until_ns == 0 { 0 } else { f.until_ns.max(until_ns) };
            return;
        }
        let mut suspended = Vec::new();
        for (slot, t) in app.threads.iter().enumerate() {
            if t.alive && t.reply.is_none() {
                sys::tcb_suspend(t.tcb);
                suspended.push(slot);
            }
        }
        let pid = app.pid;
        app.frozen = Some(Frozen { until_ns, shield_id, suspended });
        self.sentinel.frozen_apps += 1;
        let text = alloc::format!("shield {} for {}s", shield_id, if until_ns == 0 { 0 } else { (until_ns.saturating_sub(now)) / 1_000_000_000 });
        let mut ev = TelemetryEvent::new(now, AppId(pid), EventKind::Frozen, shield::FLAG_HIGH_PRIORITY, text.as_bytes());
        ev.aux_u32 = shield_id;
        self.shield.ring.emit(ev);
        self.shell.dirty = true;
    }

    /// Let a frozen app run again. The call that tripped the Shield fails with EACCES.
    pub fn thaw(&mut self, i: usize) {
        let Some(app) = self.apps[i].as_mut() else { return };
        let Some(f) = app.frozen.take() else { return };
        for slot in f.suspended {
            if let Some(t) = app.threads.get(slot) {
                if t.alive {
                    sys::tcb_resume(t.tcb);
                }
            }
        }
        let held: Vec<usize> = app.threads.iter().enumerate().filter(|(_, t)| matches!(t.wait, Some(Wait::Frozen))).map(|(s, _)| s).collect();
        let pid = app.pid;
        for slot in held {
            self.wake(i, slot, super::syscall::EACCES);
        }
        self.sentinel.frozen_apps = self.sentinel.frozen_apps.saturating_sub(1);
        crate::log!("shield {}: thawed pid {}", f.shield_id, pid);
        let ev = TelemetryEvent::new(sys::now_ns(), AppId(pid), EventKind::Thawed, shield::FLAG_HIGH_PRIORITY, b"");
        self.shield.ring.emit(ev);
        if self.focused && self.foreground == Some(i) {
            self.refresh(i);
        }
        self.shell.dirty = true;
    }

    fn index_of(&self, pid: u32) -> Option<usize> {
        (0..MAX_APPS).find(|&i| self.apps[i].as_ref().map_or(false, |a| a.pid == pid))
    }

    /// A TCP connection by app `i` opened in net slot `slot`.
    pub fn track_connection(&mut self, i: usize, slot: usize, host: &str, port: u16) {
        let pid = self.apps[i].as_ref().unwrap().pid;
        self.tx_stats[slot] = Some(TxStat { pid, index: i, host: String::from(host), port, pending: 0, reported: 0, last_ns: sys::now_ns() });
    }

    pub fn count_sent(&mut self, slot: usize, n: usize) {
        if let Some(s) = self.tx_stats[slot].as_mut() {
            s.pending = s.pending.saturating_add(n as u32);
            if s.pending >= TX_REPORT_BYTES {
                self.flush_tx(slot);
            }
        }
    }

    fn flush_tx(&mut self, slot: usize) {
        let now = sys::now_ns();
        let Some(s) = self.tx_stats[slot].as_mut() else { return };
        if s.pending == 0 {
            return;
        }
        let mut flags = 0;
        if self.apps[s.index].as_ref().map_or(false, |a| a.pid == s.pid) && !(self.focused && self.foreground == Some(s.index)) {
            flags |= shield::FLAG_BACKGROUND;
        }
        let mut ev = TelemetryEvent::new(now, AppId(s.pid), EventKind::NetTxSummary, flags, s.host.as_bytes());
        ev.port = s.port;
        ev.aux_u32 = s.pending;
        s.reported += u64::from(s.pending);
        s.pending = 0;
        s.last_ns = now;
        self.shield.ring.emit(ev);
    }

    /// Every 100 ms: expire Shields and freezes, report bytes sent, and run the Sentinel.
    pub fn shield_tick(&mut self, now: u64) {
        for id in self.shield.expire(now) {
            crate::log!("shield {}: expired", id);
            self.sentinel.note_unload(id);
        }
        for i in 0..MAX_APPS {
            let due = self.apps[i].as_ref().and_then(|a| a.frozen.as_ref()).map_or(false, |f| f.until_ns != 0 && now >= f.until_ns);
            if due {
                self.thaw(i);
            }
        }
        for slot in 0..self.tx_stats.len() {
            let Some(s) = self.tx_stats[slot].as_ref() else { continue };
            let closed = !super::net::in_use(slot) || self.apps[s.index].as_ref().map_or(true, |a| a.pid != s.pid);
            if s.pending > 0 && (closed || now - s.last_ns >= TX_REPORT_NS) {
                self.flush_tx(slot);
            }
            if closed {
                self.tx_stats[slot] = None;
            }
        }
        let events = self.shield.ring.drain();
        let idle = self.idle_secs();
        self.sentinel.queue(&events, idle);
        let active = self.shield.active();
        let dropped = self.shield.ring.dropped;
        let was_online = self.sentinel.online();
        for cmd in self.sentinel.tick(&mut self.net, now, active, dropped) {
            self.apply(cmd, now);
            self.shell.dirty = true;
        }
        if was_online != self.sentinel.online() || active != self.shield.active() {
            self.shell.dirty = true;
        }
    }

    fn apply(&mut self, cmd: Command, now: u64) {
        match cmd {
            Command::Warning => self.sentinel.warnings = self.sentinel.warnings.saturating_add(1),
            Command::Install(rule) => {
                let id = rule.shield_id;
                if self.shield.install_rule(rule) {
                    crate::log!("shield {}: installed", id);
                } else {
                    crate::log!("shield {}: rule table full", id);
                }
            }
            Command::Revoke(id) => {
                if self.shield.revoke_rule(id) {
                    self.sentinel.note_unload(id);
                    crate::log!("shield {}: revoked", id);
                }
                // Revoking a freeze's Shield thaws what it froze.
                for i in 0..MAX_APPS {
                    if self.apps[i].as_ref().and_then(|a| a.frozen.as_ref()).map_or(false, |f| f.shield_id == id) {
                        self.thaw(i);
                    }
                }
            }
            Command::Freeze { pid, until_ns, shield_id } => match self.index_of(pid) {
                Some(i) => {
                    crate::log!("shield {}: froze pid {} for {}s", shield_id, pid, (until_ns.saturating_sub(now)) / 1_000_000_000);
                    self.freeze(i, until_ns, shield_id);
                }
                None => crate::log!("shield {}: no app with pid {}", shield_id, pid),
            },
            Command::Unfreeze { pid } => {
                if let Some(i) = self.index_of(pid) {
                    self.thaw(i);
                }
            }
            Command::Kill { pid, shield_id } => {
                if let Some(i) = self.index_of(pid) {
                    crate::log!("shield {}: ended pid {}", shield_id, pid);
                    self.apps[i].as_mut().unwrap().exit_code = Some(137);
                    self.reap(i);
                }
            }
        }
    }
}

fn describe(action: &Action<'_>) -> String {
    match action {
        Action::OpenPath { path, mode } => alloc::format!("{} {}", if *mode == AccessMode::Read { "open" } else { "write" }, path),
        Action::Connect { host, port } => alloc::format!("connect {}:{}", host, port),
        Action::SpawnApp { path } => alloc::format!("exec {}", path),
        Action::MapWriteExec { addr, len } => alloc::format!("W+X map {:#x}+{}", addr, len),
        _ => String::from("call"),
    }
}
