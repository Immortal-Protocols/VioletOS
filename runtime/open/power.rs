//! The power key and power off.
//!
//! A short press of the power key turns the screen off; the next turns it on.
//! Turning the screen off locks the phone if a passcode is set: the trusted
//! UI (trusted_ui.rs) then owns the screen and every touch until the right
//! passcode is typed on its own keypad. Holding the key brings Home's power
//! sheet, which can power the phone off. The key's rules are in lock.rs.
//!
//! In /dev/violet/sys:
//!   power off               Home and Settings: save /data and power down
use alloc::string::String;

use super::abi::*;
use super::lock::{self, Press};
use super::{data, request, sys, trusted_ui, Runtime, HOME_APP};
use crate::log;
use crate::protocol::*;

const KEY_POLL_NS: u64 = 40_000_000;

#[derive(Default)]
pub struct Power {
    key: lock::Key,
    polled_ns: u64,
    /// Power does not answer for the key (no Power service): ask rarely.
    quiet: bool,
    /// The screen is off.
    pub asleep: bool,
}

impl Runtime {
    /// Read the power key and act on it. Called on every pass of the loop.
    pub fn power_tick(&mut self, now: u64) {
        if now.wrapping_sub(self.power.polled_ns) < if self.power.quiet { 2_000_000_000 } else { KEY_POLL_NS } {
            return;
        }
        self.power.polled_ns = now;
        let key = request(GET, POWER_KEY, 0);
        self.power.quiet = key.op != OK;
        if key.op != OK {
            return;
        }
        match self.power.key.update(key.value & 1 != 0, key.arg, now, !self.power.asleep) {
            Some(Press::Short) if self.power.asleep => self.screen_on(),
            Some(Press::Short) => self.screen_off(),
            Some(Press::Long) => self.power_sheet(),
            None => {}
        }
    }

    fn screen_off(&mut self) {
        // With a passcode set, the trusted UI's lock screen is what comes back.
        trusted_ui::screen_off();
        if self.focused {
            if let Some(i) = self.foreground {
                self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
            }
        }
        // Off the screen before Runtime says so, so nothing is drawn after the blank.
        self.focused = false;
        super::device::fill(0, 0, super::device::WIDTH, super::device::HEIGHT, 0xff00_0000);
        self.power.asleep = true;
        log!("screen off{}", if trusted_ui::locked() { ", locked" } else { "" });
        request(EXECUTE, POWER, 1);
    }

    /// The next poll of Runtime finds this process in front again and shows
    /// the app in front, or the lock screen over it.
    fn screen_on(&mut self) {
        self.power.asleep = false;
        log!("screen on");
        request(EXECUTE, POWER, 0);
    }

    /// The key was held: Home shows its power sheet (not over the lock screen,
    /// which owns the screen: there the press only turns the screen on).
    fn power_sheet(&mut self) {
        if self.power.asleep {
            self.screen_on();
        }
        if trusted_ui::locked() {
            return;
        }
        if self.home.is_none() {
            self.start_home(false);
        }
        let Some(home) = self.home else { return };
        if self.foreground != Some(home) {
            self.set_foreground(Some(home));
            if self.focused {
                self.refresh(home);
                self.lifecycle(home, LIFECYCLE_FOCUS_GAINED);
            }
        }
        log!("power key held");
        self.event(home, InputEvent { timestamp_us: sys::now_ns() / 1000, kind: EVENT_KEY_DOWN, code: KEY_POWER, x: 0, y: 0, value: 1 });
    }

    /// "power off" in /dev/violet/sys (Home and Settings; the policy also asks
    /// for the system capability).
    pub fn power_command(&mut self, i: usize, words: &mut core::str::SplitWhitespace) -> String {
        let path = self.apps[i].as_ref().map_or("", |a| a.path.as_str());
        if path != HOME_APP && path != super::wifi::SETTINGS_APP {
            return String::from("error not allowed\n");
        }
        match (words.next(), words.next()) {
            (Some("off"), None) => {
                log!("powering off");
                // Everything apps saved goes to Storage first.
                for _ in 0..64 {
                    if self.fs.changed.is_empty() {
                        break;
                    }
                    data::flush(&mut self.fs);
                }
                self.focused = false;
                super::device::fill(0, 0, super::device::WIDTH, super::device::HEIGHT, 0xff00_0000);
                let r = request(EXECUTE, POWER, 2);
                if r.op == OK {
                    String::from("ok\n")
                } else {
                    log!("Power refused to power off ({})", r.op);
                    String::from("error the phone did not power off\n")
                }
            }
            _ => String::from("error usage: power off\n"),
        }
    }
}
