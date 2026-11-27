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
//! which knows an app by its pid; the log says which program that was.
use super::request_with;
use crate::protocol::*;
use alloc::string::String;

/// Pids that took a lock here and have not released it (0: free), so an
/// app's exit only costs a request to Power if it held one.
static mut HOLDERS: [u32; super::MAX_APPS] = [0; super::MAX_APPS];
fn holders() -> &'static mut [u32; super::MAX_APPS] {
    // The Open Runtime is single-threaded.
    unsafe { &mut *core::ptr::addr_of_mut!(HOLDERS) }
}

fn word(m: &Message, i: usize) -> u32 {
    u32::from_le_bytes([m.data[i * 4], m.data[i * 4 + 1], m.data[i * 4 + 2], m.data[i * 4 + 3]])
}
fn long(m: &Message, i: usize) -> u64 {
    u64::from(word(m, i)) | u64::from(word(m, i + 1)) << 32
}

/// The app with this pid exited: release what it held.
pub fn app_gone(pid: u32) {
    if let Some(h) = holders().iter_mut().find(|h| **h == pid && pid != 0) {
        *h = 0;
        request_with(EXECUTE, POWER, POWER_LOCK | 2 << 4, pid);
        crate::log!("wakelock: pid {} ended holding its lock", pid);
    }
}

fn owner(id: u32) -> String {
    // Power numbers apps from 0x100 (policy::APP_OWNER) and services by component.
    if id >= 0x100 { alloc::format!("pid:{}", id - 0x100) } else { alloc::format!("service:{}", id) }
}

/// `name` is the calling program's path, for the log.
pub fn command(pid: u32, name: &str, line: &str) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("wakelock"), Some(v @ ("0" | "1")), None) if pid != 0 => {
            let on = v == "1";
            let held = holders().iter().position(|h| *h == pid);
            let free = holders().iter().position(|h| *h == 0);
            let r = request_with(EXECUTE, POWER, POWER_LOCK | (on as u32) << 4, pid);
            match r.op {
                OK => {
                    if let Some(i) = if on { held.or(free) } else { held } {
                        holders()[i] = if on { pid } else { 0 };
                    }
                    // Renewals are not logged: one line when it starts, one when it ends.
                    if on != held.is_some() {
                        crate::log!("wakelock: pid {} {} {}", pid, name, if on { "took a lock" } else { "released its lock" });
                    }
                    String::from("ok\n")
                }
                // Releasing a lock that already lapsed.
                EMPTY => {
                    if let Some(i) = held {
                        holders()[i] = 0;
                    }
                    String::from("ok\n")
                }
                DENIED if r.arg == POWER_LOCK_FULL => String::from("error busy\n"),
                DENIED if r.arg == POWER_LOCK_SCREEN_OFF => {
                    // Power dropped the lock (or never gave one).
                    if let Some(i) = held {
                        holders()[i] = 0;
                        crate::log!("wakelock: pid {} {} lost its lock with the screen off", pid, name);
                    }
                    String::from("error screen-off\n")
                }
                _ => String::from("error\n"),
            }
        }
        (Some("wakelocks"), None, None) => {
            let mut out = String::new();
            for i in 0..16 {
                let r = request_with(GET, POWER, POWER_LOCKS | i, 0);
                if r.op != OK {
                    break;
                }
                out += &alloc::format!("held {} kind={} ms={}\n", owner(r.value), r.arg, long(&r, 0));
            }
            for i in 0..16 {
                let r = request_with(GET, POWER, POWER_LOCKS | i, 1);
                if r.op != OK {
                    break;
                }
                out += &alloc::format!(
                    "owner {} holding={} taken={} lapsed={} total_ms={} longest_ms={}\n",
                    owner(r.value), r.arg, word(&r, 4), word(&r, 5), long(&r, 0), long(&r, 2)
                );
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
            let sensor = if r.arg & POWER_INFO_SENSOR == 0 { 0 } else if r.arg & POWER_INFO_SENSOR_FAILED != 0 { 2 } else { 1 };
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
