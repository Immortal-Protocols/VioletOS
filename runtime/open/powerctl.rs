//! Open apps' wake locks and Power's state, as /dev/violet/sys commands:
//!
//!   wakelock 1   keep the CPU awake after the screen goes off (playing in the
//!                background, finishing a download). It lapses after ten
//!                minutes unless taken again, and ends with the app. With
//!                the screen off it cannot be newly taken and lasts at most
//!                thirty minutes ("error screen-off"); "error busy" means
//!                Power's lock table is full.
//!   wakelock 0   release it.
//!   wakelocks    who holds locks now and each holder's totals.
//!   power        the thermal level, operating point and battery estimate.
//!
//! The accounting lives in the Power service (services/power/policy.rs),
//! which knows an app by its pid; the log says which program that was when
//! a lock is taken, released or left held at exit (a lock Power ends with
//! the screen off shows in its `lapsed` count).
//!
//! A held lock also keeps its holder running behind (`left_ns`, called by
//! mod.rs `own_facts`): an app that asked to stay awake is not frozen
//! fifteen seconds after it left the screen. It is not a reason to rank the
//! app above others for memory, though: anyone may take a lock (no
//! capability), so a holder stays Cached, which Low pressure freezes and
//! Critical pressure or a background app's fault may close. It ends when
//! Power's lock does: released, refused, or at the expiry Power gave with
//! its last OK (ten minutes on, or the screen-off bound if that is sooner).
use super::lifecycle::GRACE_NS;
use super::request_with;
use crate::protocol::*;
use alloc::string::String;

/// An app's lock lapses this long after it was last taken with the screen
/// on (policy::APP_LOCK_MS); Power's OK says when exactly.
pub const LAPSE_NS: u64 = 600_000_000_000;
/// Who took a lock here and has not released it, and when it lapses (ns);
/// pid 0 is a free row. An app's exit only costs a request to Power if it
/// held one.
static mut HOLDERS: [(u32, u64); super::MAX_APPS] = [(0, 0); super::MAX_APPS];
fn holders() -> &'static mut [(u32, u64); super::MAX_APPS] {
    // The Open Runtime is single-threaded.
    unsafe { &mut *core::ptr::addr_of_mut!(HOLDERS) }
}

/// Whether the app with this pid holds a wake lock that has not lapsed.
pub fn holds(pid: u32, now_ns: u64) -> bool {
    holders().iter().any(|h| h.0 == pid && now_ns < h.1)
}

/// The lifecycle's `Facts::left_ns` for the app with this pid, which left
/// the front at `left`: with a lock it counts as having just finished its
/// grace period, so it stays Cached and running however long it is behind.
pub fn left_ns(pid: u32, left: u64, now_ns: u64) -> u64 {
    // (In the first GRACE_NS after boot this is in the future: Visible.)
    if holds(pid, now_ns) { left.max(now_ns.wrapping_sub(GRACE_NS)) } else { left }
}

fn word(m: &Message, i: usize) -> u32 {
    u32::from_le_bytes([m.data[i * 4], m.data[i * 4 + 1], m.data[i * 4 + 2], m.data[i * 4 + 3]])
}
fn long(m: &Message, i: usize) -> u64 {
    u64::from(word(m, i)) | u64::from(word(m, i + 1)) << 32
}

/// The app with this pid exited: release what it held.
pub fn app_gone(pid: u32) {
    if let Some(h) = holders().iter_mut().find(|h| h.0 == pid && pid != 0) {
        h.0 = 0;
        request_with(EXECUTE, POWER, POWER_LOCK | 2 << 4, pid);
        crate::log!("wakelock: pid {} ended holding its lock", pid);
    }
}

/// Power numbers apps from 0x100 (policy::APP_OWNER) and services by component.
fn owner(id: u32) -> (&'static str, u32) {
    if id >= 0x100 { ("pid", id - 0x100) } else { ("service", id) }
}

/// `name` is the calling program's path, for the log.
pub fn command(pid: u32, name: &str, line: &str) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("wakelock"), Some(v @ ("0" | "1")), None) if pid != 0 => {
            let on = v == "1";
            let held = holders().iter().position(|h| h.0 == pid);
            let r = request_with(EXECUTE, POWER, POWER_LOCK | (on as u32) << 4, pid);
            // After any answer the row says what Power holds: a lock until
            // the expiry its OK to a take gave (arg, ms; 0 from a Power that
            // gives none: it lapses at once here), else none.
            let taken = on && r.op == OK;
            if let Some(i) = if taken { held.or_else(|| holders().iter().position(|h| h.0 == 0)) } else { held } {
                holders()[i] = if taken { (pid, super::sys::now_ns() + u64::from(r.arg) * 1_000_000) } else { (0, 0) };
            }
            match r.op {
                // EMPTY: releasing a lock that already lapsed.
                OK | EMPTY => {
                    // Renewals are not logged: one line when it starts, one when it ends.
                    if r.op == OK && on != held.is_some() {
                        crate::log!("wakelock: pid {} {} {}", pid, name, if on { "took a lock" } else { "released its lock" });
                    }
                    String::from("ok\n")
                }
                DENIED if r.arg == POWER_LOCK_FULL => String::from("error busy\n"),
                // Power dropped the lock (or never gave one); its `lapsed` count says so.
                DENIED if r.arg == POWER_LOCK_SCREEN_OFF => String::from("error screen-off\n"),
                _ => String::from("error\n"),
            }
        }
        (Some("wakelocks"), None, None) => {
            use core::fmt::Write;
            let mut out = String::new();
            // The held locks, then each owner's totals.
            for totals in 0..2 {
                for i in 0..16 {
                    let r = request_with(GET, POWER, POWER_LOCKS | i, totals);
                    if r.op != OK {
                        break;
                    }
                    let (who, n) = owner(r.value);
                    let _ = if totals == 0 {
                        write!(out, "held {}:{} kind={} ms={}\n", who, n, r.arg, long(&r, 0))
                    } else {
                        write!(
                            out,
                            "owner {}:{} holding={} taken={} lapsed={} total_ms={} longest_ms={}\n",
                            who, n, r.arg, word(&r, 4), word(&r, 5), long(&r, 0), long(&r, 2)
                        )
                    };
                }
            }
            if out.is_empty() {
                out += "none\n";
            }
            out
        }
        (Some("power"), None, None) => {
            let r = request_with(GET, POWER, POWER_INFO, 0);
            if r.op != OK {
                return String::from("error\n");
            }
            // sensor: 0 none on this board (no thermal control), 1 working, 2 failing.
            // (Power reports a failing sensor only where there is one.)
            let sensor = (r.arg & POWER_INFO_SENSOR) + (r.arg & POWER_INFO_SENSOR_FAILED) / POWER_INFO_SENSOR_FAILED;
            alloc::format!(
                "sensor={} temp_mc={} level={} dvfs={} opp={} khz={} battery={} locks={} held={} screen={} deferred={}\n",
                sensor, word(&r, 0) as i32, r.value, (r.arg & POWER_INFO_DVFS != 0) as u32, word(&r, 1), word(&r, 2),
                word(&r, 5) as i32, word(&r, 4), word(&r, 3),
                (r.arg & POWER_INFO_SCREEN_OFF == 0) as u32, (r.arg & POWER_INFO_SLEEP_DEFERRED != 0) as u32
            )
        }
        _ => String::from("error\n"),
    }
}
