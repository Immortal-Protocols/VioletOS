//! Violet Shield at the Open Runtime's choke point: the check every guarded
//! system call goes through, quarantine, freezing and thawing apps, bytes-sent
//! telemetry, the on-device scorer's turn and the Sentinel's
//! (docs/violet-shield-poc.md). The decisions themselves are in shield.rs and
//! scorer.rs, which the host tests run; this file carries them out.
use super::policy::{AccessMode, Action, AppId};
use super::scorer;
use super::sentinel::Command;
use super::shield::{self, EventKind, Mode, ShieldVerdict, Subject, TelemetryEvent};
use super::{sys, Frozen, Gate, Runtime, TxStat, Wait, MAX_APPS};
use alloc::string::String;
use alloc::vec::Vec;

/// Report bytes sent after this many accumulate, or after a pause.
const TX_REPORT_BYTES: u32 = 64 * 1024;
const TX_REPORT_NS: u64 = 500_000_000;
/// Where the Shield's mode is kept across restarts. Apps cannot write it
/// (shield.rs `STATE_PREFIX`); only the runtime does, for Settings.
const MODE_FILE: &str = "/data/.shield-mode";

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut n = 0;
    while n < a.len() && n < b.len() && a[n] == b[n] {
        n += 1;
    }
    n == a.len() && n == b.len()
}
// shield.rs names the core apps itself so the host tests cover the roles.
const _: () = assert!(same(shield::CORE_HOME, super::HOME_APP) && same(shield::CORE_SETTINGS, super::wifi::SETTINGS_APP) && same(shield::CORE_INSTALLER, super::appstore::INSTALLER));

impl Runtime {
    /// Who app `i` is to the Shield. The conclusions are drawn in shield.rs
    /// (`Subject::derive`, which the host tests run) from where the program
    /// was started, whose it is, and whether its file is the boot image's.
    pub fn subject(&self, i: usize) -> Subject {
        let app = self.apps[i].as_ref().unwrap();
        let shown = self.focused && self.foreground == Some(i);
        let front = shield::in_front(shown, app.pid, self.shield.pushed, self.last_touch_ns);
        Subject::derive(app.pid, &app.path, &app.owner, app.digest[..4].try_into().unwrap(), !app.new_hash, front)
    }

    /// The app a program about to start from `path` belongs to (`owner_of`):
    /// the app whose folder it is in, else the app that asked for it.
    pub fn shield_owner(&self, path: &str, digest: &[u8; 32], caller: Option<usize>) -> String {
        let parent = caller.filter(|&c| self.apps[c].is_some()).map(|c| self.subject(c)).filter(|s| s.role & shield::ROLE_CORE == 0);
        shield::owner_of(path, parent.as_ref().map(|s| s.name()), digest[..4].try_into().unwrap())
    }

    /// App `by` asked for app `n` to be in front ("open", "launch"). Unless
    /// `by` is Home, Settings or the Agent, that is not the user's doing:
    /// `n` stays a background app to the Shield until the user touches it.
    pub fn shield_fronted(&mut self, by: usize, n: usize) {
        let Some(pid) = self.apps[n].as_ref().map(|a| a.pid) else { return };
        let core = self.apps[by].is_some() && self.subject(by).role & shield::ROLE_CORE != 0;
        if !core {
            self.shield.pushed = (pid, sys::now_ns());
        } else if self.shield.pushed.0 == pid {
            self.shield.pushed = (0, 0);
        }
    }

    /// A `bench press` on the app list: it works like a finger, but it is
    /// not the user. It does not count as a touch, and an app it opens is
    /// not in front as far as the Shield is concerned.
    pub fn synthetic_touch(&mut self, x: i32, y: i32, down: bool) {
        let real = self.last_touch_ns;
        self.touch(x, y, down);
        self.last_touch_ns = real;
        if let Some(pid) = self.foreground.and_then(|n| self.apps[n].as_ref()).map(|a| a.pid) {
            self.shield.pushed = (pid, sys::now_ns());
        }
    }

    /// Log a quarantine the policy just started.
    fn shield_notice(&mut self) {
        if let Some(n) = self.shield.notice.take() {
            crate::log!("shield {}: quarantined pid {} for {}s", n.shield_id, n.pid, n.secs);
            self.shell.dirty = true;
        }
    }

    /// Is app `i` quarantined right now?
    pub fn quarantined(&mut self, i: usize) -> bool {
        self.shield.mode != Mode::Monitor && self.shield.guards.iter().any(|g| g.until_ns != 0) && self.apps[i].is_some() && {
            let who = self.subject(i);
            self.shield.holds(sys::now_ns(), &who)
        }
    }

    /// Is this a network send (or connect) by a quarantined app? Checked
    /// before the call is dispatched, so connections it opened earlier are
    /// cut off too. `number` is the Linux system call, `fd` its first
    /// argument. These are all the calls that send: connect, send, sendto,
    /// sendmsg here, and write, writev and pwrite64, which all end in
    /// `Runtime::write`; that asks `quarantined` again for sockets and the
    /// Bluetooth controller, so a send that was already waiting when the
    /// quarantine began is refused too. (sendmmsg is not implemented.)
    pub fn quarantine_blocks(&mut self, i: usize, number: u32, fd: i32) -> bool {
        if !self.shield.guards.iter().any(|g| g.until_ns != 0) {
            return false;
        }
        let socket = match number {
            283 | 289 | 290 | 296 => true,
            // write, writev, pwrite64: only when the descriptor is a socket.
            4 | 146 | 181 => self.apps[i].as_ref().and_then(|a| a.fds.get(fd as usize)).and_then(|f| f.as_ref())
                .map_or(false, |f| matches!(f.borrow().kind, super::device::Kind::Tcp { .. } | super::device::Kind::Udp { .. })),
            _ => false,
        };
        socket && self.quarantined(i)
    }

    /// After a call returned `r`: bytes sent on a UDP socket (name lookups
    /// included) count toward the exfiltration limit like TCP's, which
    /// `count_sent` counts where they are sent.
    pub fn shield_after(&mut self, i: usize, number: u32, fd: i32, r: i32) {
        if r <= 0 || !matches!(number, 4 | 146 | 181 | 289 | 290 | 296) {
            return;
        }
        let udp = self.apps[i].as_ref().and_then(|a| a.fds.get(fd as usize)).and_then(|f| f.as_ref())
            .map_or(false, |f| matches!(f.borrow().kind, super::device::Kind::Udp { .. }));
        if udp {
            let who = self.subject(i);
            self.shield.note_sent(sys::now_ns(), &who, r as u32);
            self.shield_notice();
        }
    }

    /// The Shield's say on open(): the rules see what the open does
    /// (`open_mode`), and an O_RDWR open of another app's file is a read as
    /// well as a write (a write-only descriptor cannot be read: `read_now`).
    pub fn gate_open(&mut self, i: usize, path: &str, flags: u32) -> Gate {
        let exists = self.fs.stat(path).is_some();
        let mode = shield::open_mode(flags, exists);
        if mode != AccessMode::Read && shield::fd_reads(flags) && exists && shield::cross_app(path, self.subject(i).name()) {
            match self.gate(i, &Action::OpenPath { path, mode: AccessMode::Read }) {
                Gate::Allow => {}
                other => return other,
            }
        }
        self.gate(i, &Action::OpenPath { path, mode })
    }

    /// A write or truncate through an open descriptor on `path`, opened with
    /// `flags`: None to go ahead, else the error. The descriptor must have
    /// been opened for writing, and the built-in rules are asked again.
    pub fn fd_write_error(&mut self, i: usize, path: &str, flags: u32) -> Option<i32> {
        if !shield::fd_writes(flags) {
            return Some(super::syscall::EBADF);
        }
        let who = self.subject(i);
        if self.shield.write_refused(sys::now_ns(), &who, path) {
            crate::log!("shield: denied write to {} through an open file (pid {})", path, who.app.0);
            return Some(super::syscall::EACCES);
        }
        None
    }

    /// Seconds since the last touch on the open apps' screen.
    pub fn idle_secs(&self) -> u32 {
        ((sys::now_ns().saturating_sub(self.last_touch_ns)) / 1_000_000_000).min(u32::MAX as u64) as u32
    }

    /// Check `action` by app `i` against the active Shields and record it.
    pub fn gate(&mut self, i: usize, action: &Action<'_>) -> Gate {
        self.gate_with_flags(i, action, 0)
    }

    pub fn gate_with_flags(&mut self, i: usize, action: &Action<'_>, flags: u8) -> Gate {
        let mut who = self.subject(i);
        who.flags |= flags;
        let now = sys::now_ns();
        let verdict = self.shield.evaluate(now, &who, action);
        self.shield_notice();
        match verdict {
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
                // Forked processes of the same app do not outlive the verdict.
                self.end_family(i, 137);
                let app = self.apps[i].as_mut().unwrap();
                app.exit_code = Some(137);
                self.reap(i);
                Gate::Killed
            }
        }
    }

    /// The Shield's say on changing `path` without opening it (mkdir, unlink,
    /// rename, truncate): None to go ahead, else what the call returns.
    pub fn gate_change(&mut self, i: usize, path: &str) -> Option<super::syscall::Outcome> {
        self.gate_path(i, path, AccessMode::Write)
    }

    fn gate_path(&mut self, i: usize, path: &str, mode: AccessMode) -> Option<super::syscall::Outcome> {
        use super::syscall::Outcome;
        match self.gate(i, &Action::OpenPath { path, mode }) {
            Gate::Allow => None,
            Gate::Deny => Some(Outcome::Done(super::syscall::EACCES)),
            Gate::Frozen => Some(Outcome::Block(Wait::Frozen, None)),
            Gate::Killed => Some(Outcome::Gone),
        }
    }

    /// The Shield's say on a rename. It changes both names, and moving a
    /// file takes its contents along: the old name is checked as a read too,
    /// so moving another app's data into one's own folder marks the app as
    /// having read it (and is refused under quarantine or over the limit).
    pub fn gate_rename(&mut self, i: usize, from: &str, to: &str) -> Option<super::syscall::Outcome> {
        let own = self.subject(i);
        if shield::cross_app(from, own.name()) {
            if let Some(o) = self.gate_path(i, from, AccessMode::Read) {
                return Some(o);
            }
        }
        if let Some(o) = self.gate_change(i, from) {
            return Some(o);
        }
        self.gate_change(i, to)
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
        // Threads the lifecycle parked stay parked until it thaws the app (mod.rs `unpark`).
        for slot in f.suspended {
            if !super::lifecycle::shield_may_resume(&app.life, slot) {
                continue;
            }
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
        let Some(s) = self.tx_stats[slot].as_mut() else { return };
        s.pending = s.pending.saturating_add(n as u32);
        let (index, pid, full) = (s.index, s.pid, s.pending >= TX_REPORT_BYTES);
        if full {
            self.flush_tx(slot);
        }
        // The exfiltration limit counts every send as it happens.
        if n > 0 && self.apps[index].as_ref().map_or(false, |a| a.pid == pid) {
            let who = self.subject(index);
            self.shield.note_sent(sys::now_ns(), &who, n.min(u32::MAX as usize) as u32);
            self.shield_notice();
        }
    }

    fn flush_tx(&mut self, slot: usize) {
        let now = sys::now_ns();
        let Some(s) = self.tx_stats[slot].as_mut() else { return };
        if s.pending == 0 {
            return;
        }
        let mut flags = 0;
        let shown = self.focused && self.foreground == Some(s.index);
        if self.apps[s.index].as_ref().map_or(false, |a| a.pid == s.pid) && !shield::in_front(shown, s.pid, self.shield.pushed, self.last_touch_ns) {
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

    /// The Shield's mode at start: the build's, or the saved one if that is
    /// stricter (`Mode::at_boot`: the saved file is not authenticated, so it
    /// cannot switch enforcement off). The mode goes into the measurement
    /// chain on every start.
    fn load_shield_mode(&mut self) {
        let saved = self.fs.contents(MODE_FILE).and_then(|d| core::str::from_utf8(d).ok()).and_then(Mode::parse);
        let mode = Mode::at_boot(self.shield.mode, saved);
        if saved.is_some() && saved != Some(mode) {
            crate::log!("shield: saved mode {} is weaker than this build's and is ignored", saved.map_or("", Mode::name));
        }
        self.shield.mode = mode;
        self.sentinel.note_mode(mode as u8);
        crate::log!("shield: mode {} (scorer {:08x})", self.shield.mode.name(), super::scorer_model::MODEL_ID);
    }

    /// "shield-mode [monitor|enforce|strict]", "shield-release NAME" and
    /// "shield-local" in /dev/violet/sys. Anyone may read; only Settings from
    /// the boot image may change the mode or release a quarantine.
    pub fn shield_sys(&mut self, i: usize, verb: &str, arg: Option<&str>) -> String {
        let now = sys::now_ns();
        let settings = self.apps[i].as_ref().map_or(false, |a| !a.new_hash && a.path == super::wifi::SETTINGS_APP);
        match (verb, arg) {
            ("shield-local", _) | ("shield-mode", None) => alloc::format!(
                "mode={} quarantined={} denied={} reported={} scored={} warned={} model={:08x}\n",
                self.shield.mode.name(),
                self.shield.quarantined_names(now),
                self.shield.denied,
                self.shield.shadow_hits,
                self.scorer.scored,
                self.scorer.warned,
                super::scorer_model::MODEL_ID
            ),
            _ if !settings => String::from("error only Settings changes Violet Shield\n"),
            ("shield-mode", Some(text)) => match Mode::parse(text) {
                Some(mode) => {
                    if let Some(f) = self.fs.writable(MODE_FILE, true, true, 0) {
                        f.extend_from_slice(mode.name().as_bytes());
                    }
                    // Saved means Storage holds it now, not that it was queued.
                    super::data::sync(&mut self.fs);
                    let saved = self.fs.stored.get(MODE_FILE).map_or(false, |d| d.as_slice() == mode.name().as_bytes());
                    self.shield.set_mode(now, mode);
                    if mode == Mode::Monitor {
                        // Monitoring blocks nothing: what was frozen runs again.
                        for n in 0..MAX_APPS {
                            self.thaw(n);
                        }
                    }
                    self.sentinel.note_mode(mode as u8);
                    crate::log!("shield: mode {}{}", mode.name(), if saved { "" } else { " (not saved)" });
                    self.shell.dirty = true;
                    // "ok saved": a restart keeps it, if it is at least as
                    // strict as the build's mode. "ok unsaved": until restart only.
                    String::from(if saved { "ok saved\n" } else { "ok unsaved\n" })
                }
                None => String::from("error usage: shield-mode monitor|enforce|strict\n"),
            },
            ("shield-release", Some(name)) if self.shield.release_name(now, name) => {
                crate::log!("shield: {} released from quarantine", name);
                self.shell.dirty = true;
                String::from("ok\n")
            }
            _ => String::from("error\n"),
        }
    }

    /// The on-device scorer's turn: score the windows that changed and carry
    /// out what the policy decided (scorer.rs `step`).
    fn score_tick(&mut self, events: &[TelemetryEvent], idle: u32, now: u64) {
        if events.is_empty() && !self.scorer.windows.iter().any(|w| w.pending()) {
            return;
        }
        let subjects: Vec<Subject> = (0..MAX_APPS).filter(|&i| self.apps[i].is_some()).map(|i| self.subject(i)).collect();
        for local in self.scorer.step(&mut self.shield, events, idle, now, &subjects) {
            let Some(i) = self.index_of(local.pid) else { continue };
            let who = self.subject(i);
            crate::log!("shield {}: scorer says {} for pid {} (verdict {}, margin {}){}", shield::ID_SCORER,
                ["", "warn", "quarantine", "freeze"][local.action as usize], local.pid, local.verdict, local.margin,
                if local.action >= scorer::QUARANTINE && !local.enforced { ", reported only" } else { "" });
            // Home's warning count: only for programs the phone may contain.
            if self.shield.eligible(&who) {
                self.sentinel.warnings = self.sentinel.warnings.saturating_add(1);
                self.shell.dirty = true;
            }
            if local.enforced && local.action == scorer::FREEZE {
                self.freeze(i, now + u64::from(scorer::FREEZE_SECS) * 1_000_000_000, shield::ID_SCORER);
            }
        }
        self.shield_notice();
    }

    /// Every 100 ms: expire Shields, quarantines and freezes, report bytes
    /// sent, score what changed, and run the Sentinel.
    pub fn shield_tick(&mut self, now: u64) {
        if !self.shield.loaded {
            self.shield.loaded = true;
            self.load_shield_mode();
        }
        for id in self.shield.expire(now) {
            crate::log!("shield {}: expired", id);
            self.sentinel.note_unload(id);
        }
        for pid in self.shield.expire_quarantines(now) {
            crate::log!("shield {}: quarantine of pid {} ended", shield::ID_QUARANTINE, pid);
            self.shell.dirty = true;
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
        self.score_tick(&events, idle, now);
        let active = self.shield.active();
        let dropped = self.shield.ring.dropped;
        let was_online = self.sentinel.online();
        self.sentinel.enforce = self.shield.mode != Mode::Monitor;
        self.sentinel.local = [self.shield.denied, self.shield.quarantines, self.shield.shadow_hits];
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
                // Revoking a freeze's Shield thaws what it froze. The phone's
                // own Shields (ids below 16) cannot be revoked; the companion
                // lifts their effect on one app with a signed release.
                for i in 0..MAX_APPS {
                    if id >= shield::FIRST_COMPANION_ID && self.apps[i].as_ref().and_then(|a| a.frozen.as_ref()).map_or(false, |f| f.shield_id == id) {
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
                // The companion's release also ends a quarantine, local or signed.
                if self.shield.release(now, pid) {
                    crate::log!("shield: pid {} released from quarantine", pid);
                }
            }
            Command::Quarantine { pid, secs, shield_id } => match self.index_of(pid) {
                Some(i) => {
                    let who = self.subject(i);
                    self.shield.quarantine(now, &who, shield_id, secs);
                    self.shield_notice();
                }
                None => crate::log!("shield {}: no app with pid {}", shield_id, pid),
            },
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
