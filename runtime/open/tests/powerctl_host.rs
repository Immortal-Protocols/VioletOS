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
            Ok(()) => Message::result(m.locks.blockers(now)),
            Err(e) => {
                let (op, arg) = e.reply();
                Message::error(op).with_arg(arg)
            }
        }
    })
}

/// runtime/open/mod.rs `request_with` over Runtime's tickets (see the top).
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
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
    let log = logged();
    assert_eq!(log.iter().filter(|l| l.contains("lost its lock with the screen off")).count(), 1, "{log:?}");
    // Its exit then costs Power nothing: the lock is already gone.
    powerctl::app_gone(5);
    assert!(logged().is_empty());
    // The user is back.
    SERVICE.with(|p| {
        let (m, board) = &mut *p.borrow_mut();
        assert!(m.wake(board));
    });
    assert_eq!(powerctl::command(5, "/apps/music", "wakelock 1"), "ok\n");
}
