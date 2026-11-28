//! Host test for powerctl.rs (`make test-unit`): the real "wakelock" command
//! against the real Power policy, joined the way the phone joins them.
//!
//! On the phone a request crosses Runtime as a job: the client polls its
//! ticket, Runtime answers BUSY while the job runs, and `request_with`
//! (runtime/open/mod.rs) polls again. A *result* whose op is BUSY is
//! therefore never seen: the second poll finds the ticket gone and gets
//! INVALID. `request_with` below does the same, so a refusal that Power
//! sends as BUSY shows up here as the plain "error" an app would read.
#![allow(dead_code)]
extern crate alloc;

#[path = "../../protocol.rs"]
mod protocol;
#[path = "../../../services/power/policy.rs"]
mod policy;

use policy::{Board, DvfsStep, Manager, Opp, Reading, Snapshot, Zone};
use protocol::*;
use std::cell::RefCell;

thread_local! {
    static LOG: RefCell<Vec<String>> = RefCell::new(Vec::new());
    static SERVICE: RefCell<(Manager, Quiet)> = RefCell::new((Manager::new(ZONE, 0), Quiet));
    static NOW: RefCell<u64> = RefCell::new(0);
    // Power answers a take without its expiry (arg 0), as an older one did.
    static NO_EXPIRY: RefCell<bool> = RefCell::new(false);
    // Runtime's ticket is lost: INVALID.
    static FAIL: RefCell<bool> = RefCell::new(false);
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        $crate::LOG.with(|l| l.borrow_mut().push(format!($($arg)*)));
    }};
}

pub const MAX_APPS: usize = 8;
#[path = "../powerctl.rs"]
mod powerctl;
#[path = "../lifecycle.rs"]
mod lifecycle;
/// The runtime's clock: the same instant Power's stand-in is asked at.
pub mod sys {
    pub fn now_ns() -> u64 {
        super::NOW.with(|n| *n.borrow()) * 1_000_000
    }
}
/// The state of an app behind with nothing else to keep it running, through
/// the same `powerctl::left_ns` mod.rs `own_facts` builds its facts with.
fn state_behind(pid: u32, left_ms: u64) -> lifecycle::State {
    state_under(pid, left_ms, lifecycle::Pressure::Normal)
}
fn state_under(pid: u32, left_ms: u64, p: lifecycle::Pressure) -> lifecycle::State {
    let now = sys::now_ns();
    let facts = lifecycle::Facts { left_ns: powerctl::left_ns(pid, left_ms * 1_000_000, now), ..Default::default() };
    lifecycle::next_state(&facts, now, p)
}
/// Its class for the memory killer (lifecycle::class), as mod.rs `candidates` builds it.
fn class_under(pid: u32, left_ms: u64, p: lifecycle::Pressure) -> Option<u8> {
    lifecycle::class(&lifecycle::Candidate { state: state_under(pid, left_ms, p), ..Default::default() })
}

const ZONE: Zone = Zone { warm: 60_000, hot: 75_000, critical: 95_000, hysteresis: 5_000 };

/// A board with no sensor and nothing to switch.
struct Quiet;
impl Board for Quiet {
    fn temperature(&mut self) -> Reading {
        Reading::Absent
    }
    fn load_percent(&mut self) -> Option<u32> {
        None
    }
    fn opps(&self) -> &'static [Opp] {
        &[]
    }
    fn apply(&mut self, _: DvfsStep) -> bool {
        true
    }
    fn power(&mut self, _: u32) -> bool {
        true
    }
    fn screen_peripherals(&mut self, _: bool) {}
    fn publish(&mut self, _: &Snapshot) {}
}

/// Power's answer to an open app's lock request: `lock` in
/// services/power/process.rs, which takes its refusals from the same
/// `LockError::reply`.
fn power(op: u32, key: u32, value: u32, arg: u32) -> Message {
    if (op, key) != (EXECUTE, POWER) || value & !0xff != POWER_LOCK {
        return Message::error(DENIED);
    }
    let now = NOW.with(|n| *n.borrow());
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        let owner = policy::APP_OWNER.saturating_add(arg);
        let (action, kind) = ((value >> 4) & 0xf, (value & 0xf) as u8);
        let result = match action {
            0 | 1 => m.lock(board, owner, kind, action == 1, now),
            2 => {
                m.owner_gone(board, owner, now);
                Ok(())
            }
            _ => Err(policy::LockError::BadKind),
        };
        match result {
            Ok(()) => Message::result(m.locks.blockers(now))
                .with_arg(if action == 1 && !NO_EXPIRY.with(|n| *n.borrow()) { m.locks.remaining(owner, kind, now) } else { 0 }),
            Err(e) => {
                let (op, arg) = e.reply();
                Message::error(op).with_arg(arg)
            }
        }
    })
}

/// runtime/open/mod.rs `request_with` over Runtime's tickets (see the top).
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
    if FAIL.with(|n| *n.borrow()) {
        return Message::error(INVALID);
    }
    let result = power(op, key, value, arg);
    if result.op == BUSY {
        return Message::error(INVALID);
    }
    result
}

fn at(ms: u64) {
    NOW.with(|n| *n.borrow_mut() = ms);
}
fn locks() -> u32 {
    SERVICE.with(|p| p.borrow().0.locks.count())
}
fn logged() -> Vec<String> {
    LOG.with(|l| core::mem::take(&mut *l.borrow_mut()))
}

// One test: powerctl keeps its holders in a static, as the single-threaded
// runtime does.
#[test]
fn wakelock_command() {
    // policy.rs's numbers are protocol.rs's (process.rs asserts the same
    // when the service is built).
    assert_eq!(
        (policy::REPLY_DENIED, policy::REPLY_INVALID, policy::REPLY_EMPTY, policy::REPLY_PENDING),
        (DENIED, INVALID, EMPTY, BUSY)
    );
    assert_eq!((policy::REFUSED_FULL, policy::REFUSED_SCREEN_OFF), (POWER_LOCK_FULL, POWER_LOCK_SCREEN_OFF));

    // Taken, renewed (one lock, one log line), released; the log names the program.
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "ok\n");
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "ok\n");
    assert_eq!(locks(), 1);
    assert_eq!(logged(), ["wakelock: pid 5 /apps/music took a lock"]);
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 0"), "ok\n");
    assert_eq!(logged(), ["wakelock: pid 5 /apps/music released its lock"]);
    assert_eq!(locks(), 0);
    // Releasing what is not held is not an error; nonsense is.
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 0"), "ok\n");
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 2"), "error\n");
    assert_eq!(powerctl::command(0, "", "wakelock 1"), "error\n");
    assert!(logged().is_empty());

    // The table fills: the app is told so, not just "error".
    for pid in 1..=policy::LOCKS as u32 {
        assert_eq!(powerctl::command(pid, "/apps/filler", "wakelock 1"), "ok\n", "pid {pid}");
    }
    assert_eq!(locks(), policy::LOCKS as u32);
    assert_eq!(powerctl::command(99, "/apps/late", "wakelock 1"), "error busy\n");
    // An app that exits gives its lock back, and that is logged.
    logged();
    powerctl::app_gone(3);
    assert_eq!(locks(), policy::LOCKS as u32 - 1);
    assert_eq!(logged(), ["wakelock: pid 3 ended holding its lock"]);
    assert_eq!(powerctl::command(99, "/apps/late", "wakelock 1"), "ok\n");
    for pid in (1..=policy::LOCKS as u32).chain([99]) {
        powerctl::command(pid, "/apps/filler", "wakelock 0");
    }
    assert_eq!(locks(), 0);
    logged();

    // The screen goes off: a holder may renew for half an hour in all,
    // nobody starts a lock, and the app is told why.
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "ok\n");
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        assert_eq!(m.sleep(board, 1000), Ok(1));
    });
    assert_eq!(powerctl::command(6, "/apps/sneaky", "wakelock 1"), "error screen-off\n");
    let mut refused = None;
    for minutes in (9..=45).step_by(9) {
        at(1000 + minutes * 60_000);
        if powerctl::command(5, "/apps/music", "wakelock 1") != "ok\n" {
            refused.get_or_insert(minutes);
        }
    }
    assert_eq!(refused, Some(36));
    assert_eq!(locks(), 0);
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "error screen-off\n");
    // The end is in Power's accounting (a lapse), not in the runtime's log.
    assert_eq!(logged(), ["wakelock: pid 5 /apps/music took a lock"]);
    assert!(!powerctl::holds(5, sys::now_ns()));
    SERVICE.with(|p| assert_eq!(p.borrow().0.locks.owner_stat(0, 0).map(|s| s.timeouts), Some(1)));
    // Its exit then costs Power nothing: the lock is already gone.
    powerctl::app_gone(5);
    assert!(logged().is_empty());
    // The user is back.
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        assert!(m.wake(board));
    });
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "ok\n");
    powerctl::command(5, "/apps/music", "wakelock 0");

    // The lifecycle respects a lock: its holder keeps running behind, where
    // an app without one is frozen fifteen seconds after it left the screen.
    use lifecycle::{Pressure, State};
    let t0 = 50 * 60_000;
    at(t0);
    assert!(!powerctl::holds(7, sys::now_ns()) && !powerctl::holds(0, sys::now_ns()));
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    at(t0 + 60_000);
    assert!(powerctl::holds(7, sys::now_ns()));
    assert_eq!(state_behind(7, t0), State::Cached, "running");
    assert_eq!(state_behind(8, t0), State::Frozen, "no lock, no reason");
    // Just behind, the grace period still applies as without a lock.
    assert_eq!(state_behind(7, t0 + 60_000 - 1), State::Visible);
    // Anyone may take a lock, so it buys no rank for memory: frozen under
    // Low pressure, and as closable as any cached app under Critical,
    // including by a background app's fault (lifecycle::fault_victim, class <= 1).
    assert_eq!(state_under(7, t0, Pressure::Low), State::Frozen);
    assert_eq!(class_under(7, t0, Pressure::Normal), Some(1));
    assert_eq!(class_under(7, t0, Pressure::Critical), Some(0));
    let apps = [Some(lifecycle::Candidate { state: state_behind(7, t0), ..Default::default() })];
    assert_eq!(lifecycle::fault_victim(&apps, false), Some(0));
    // A child takes its parent's facts (lifecycle::inherit): running, still Cached.
    let parent = lifecycle::Facts { left_ns: powerctl::left_ns(7, t0 * 1_000_000, sys::now_ns()), ..Default::default() };
    let child = lifecycle::inherit(lifecycle::Facts { left_ns: t0 * 1_000_000, ..Default::default() }, &parent);
    assert_eq!(lifecycle::next_state(&child, sys::now_ns(), Pressure::Normal), State::Cached);
    // Released: cached and then frozen like any other app.
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 0"), "ok\n");
    assert_eq!(state_behind(7, t0), State::Frozen);
    // Not renewed: the reason lapses when Power's lock does, ten minutes on.
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    assert_eq!(powerctl::LAPSE_NS, policy::APP_LOCK_MS * 1_000_000);
    at(t0 + 60_000 + policy::APP_LOCK_MS - 1);
    assert_eq!((locks(), state_behind(7, t0)), (1, State::Cached));
    at(t0 + 60_000 + policy::APP_LOCK_MS);
    assert_eq!(state_behind(7, t0), State::Frozen);
    SERVICE.with(|p| assert_eq!(p.borrow_mut().0.locks.blockers(t0 + 60_000 + policy::APP_LOCK_MS), 0));
    // Taken again in time, it carries on.
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    at(t0 + 60_000 + policy::APP_LOCK_MS + 5 * 60_000);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    at(t0 + 60_000 + 2 * policy::APP_LOCK_MS + 4 * 60_000);
    assert_eq!(state_behind(7, t0), State::Cached);
    // Power takes it away with the screen off: the reason goes with it.
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        m.sleep(board, t0 + 3 * policy::APP_LOCK_MS).unwrap();
    });
    at(t0 + 3 * policy::APP_LOCK_MS + policy::APP_SCREEN_OFF_MS);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "error screen-off\n");
    assert_eq!(state_behind(7, t0), State::Frozen);
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        m.wake(board);
    });
    // Renewed late with the screen off, Power ends the lock at the
    // screen-off bound, five minutes on, not ten: the runtime's record ends
    // with it, with no further request from the app.
    let off = t0 + 7 * policy::APP_LOCK_MS;
    at(off - 1);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        m.sleep(board, off).unwrap();
    });
    for minutes in [8, 16, 25] {
        at(off + minutes * 60_000);
        assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n", "{minutes}");
    }
    at(off + policy::APP_SCREEN_OFF_MS - 1);
    assert!(powerctl::holds(7, sys::now_ns()));
    at(off + policy::APP_SCREEN_OFF_MS);
    SERVICE.with(|p| assert_eq!(p.borrow_mut().0.locks.blockers(off + policy::APP_SCREEN_OFF_MS), 0));
    assert!(!powerctl::holds(7, sys::now_ns()), "lapsed with Power's lock");
    assert_eq!(state_behind(7, t0), State::Frozen);
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        m.wake(board);
    });
    // Awake again: ten minutes, to the millisecond Power gave.
    let on = off + policy::APP_SCREEN_OFF_MS + 1;
    at(on);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    at(on + policy::APP_LOCK_MS - 1);
    assert!(powerctl::holds(7, sys::now_ns()));
    at(on + policy::APP_LOCK_MS);
    assert!(!powerctl::holds(7, sys::now_ns()));
    // A Power that gives no expiry (arg 0) is not taken to grant one: the
    // runtime keeps no lock for the app (it is frozen as without one).
    NO_EXPIRY.with(|n| *n.borrow_mut() = true);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    NO_EXPIRY.with(|n| *n.borrow_mut() = false);
    assert!(!powerctl::holds(7, sys::now_ns()));
    assert_eq!(state_behind(7, t0), State::Frozen);
    // An error from Power or Runtime for a renewal ends the runtime's record too.
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    assert!(powerctl::holds(7, sys::now_ns()));
    FAIL.with(|n| *n.borrow_mut() = true);
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "error\n");
    FAIL.with(|n| *n.borrow_mut() = false);
    assert!(!powerctl::holds(7, sys::now_ns()));

    // The app exits holding one: the pid's reason does not outlive it.
    assert_eq!(powerctl::command(7, "/apps/podcast", "wakelock 1"), "ok\n");
    powerctl::app_gone(7);
    assert!(!powerctl::holds(7, sys::now_ns()));
    assert_eq!(locks(), 0);
}
