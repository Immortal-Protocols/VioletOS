//! The power key, the screen lock and power off.
//!
//! A short press of the power key turns the screen off; the next turns it on.
//! With a passcode set (Settings › Passcode), turning the screen off also
//! locks the phone, and so does starting it: Home comes to the front and stays
//! there, showing its lock screen, until it passes the right passcode to
//! "unlock". Holding the key brings Home's power sheet, which can power the
//! phone off. The rules are in lock.rs; the passcode's record is in lock::FILE,
//! which apps cannot open.
//!
//! In /dev/violet/sys:
//!   lock                    -> "locked=0|1 set=0|1 wait=SECONDS"
//!   unlock CODE             Home only: "ok", "error wrong" or "error wait SECONDS"
//!   passcode check OLD      Settings only: "ok", "error wrong" or "error wait SECONDS"
//!   passcode set NEW [OLD]  Settings only; OLD is required once one is set
//!   passcode clear OLD      Settings only
//!   power off               Home and Settings: save /data and power down
use alloc::string::String;

use super::abi::*;
use super::lock::{self, Press};
use super::{data, request, sentinel, sys, Runtime, HOME_APP};
use crate::log;
use crate::protocol::*;

const KEY_POLL_NS: u64 = 40_000_000;

#[derive(Default)]
pub struct Power {
    key: lock::Key,
    attempts: lock::Attempts,
    polled_ns: u64,
    /// Power does not answer for the key (no Power service): ask rarely.
    quiet: bool,
    /// The screen is off.
    pub asleep: bool,
    /// Only Home may be in front.
    pub locked: bool,
    home_retry_ns: u64,
}

impl Runtime {
    fn passcode(&self) -> Option<String> {
        let record = self.fs.contents(lock::FILE)?;
        (!record.is_empty()).then(|| String::from_utf8_lossy(record).into_owned())
    }

    /// Once, as Home first starts: a phone with a passcode starts locked.
    pub fn lock_at_start(&mut self) {
        let Some(record) = self.passcode() else { return };
        self.power.locked = true;
        // Wrong guesses made before the restart still count.
        self.power.attempts.restore(lock::wrong_in(&record), sys::now_ns());
        log!("locked at start");
    }

    /// Read the power key and act on it. Called on every pass of the loop.
    pub fn power_tick(&mut self, now: u64) {
        if now.wrapping_sub(self.power.polled_ns) < if self.power.quiet { 2_000_000_000 } else { KEY_POLL_NS } {
            return;
        }
        self.power.polled_ns = now;
        // A lock needs its screen: bring Home back if it stopped.
        if self.power.locked && self.focused && self.home.is_none() && now.wrapping_sub(self.power.home_retry_ns) > 2_000_000_000 {
            self.power.home_retry_ns = now;
            self.start_home();
        }
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

    /// Only Home in front from now on, if there is a passcode to ask for.
    fn lock(&mut self) {
        if self.power.locked || self.passcode().is_none() {
            return;
        }
        if self.home.is_none() {
            self.start_home();
        }
        let Some(home) = self.home else {
            log!("no Home app: the screen cannot lock");
            return;
        };
        self.power.locked = true;
        self.set_foreground(Some(home));
        log!("locked");
    }

    fn screen_off(&mut self) {
        self.lock();
        if self.focused {
            if let Some(i) = self.foreground {
                self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
            }
        }
        // Off the screen before Runtime says so, so nothing is drawn after the blank.
        self.focused = false;
        super::device::fill(0, 0, super::device::WIDTH, super::device::HEIGHT, 0xff00_0000);
        self.power.asleep = true;
        log!("screen off{}", if self.power.locked { ", locked" } else { "" });
        request(EXECUTE, POWER, 1);
    }

    /// The next poll of Runtime finds this process in front again and shows
    /// the app in front (Home's lock screen while locked).
    fn screen_on(&mut self) {
        self.power.asleep = false;
        log!("screen on");
        request(EXECUTE, POWER, 0);
    }

    /// The key was held: Home shows its power sheet.
    fn power_sheet(&mut self) {
        if self.power.asleep {
            self.screen_on();
        }
        if self.home.is_none() {
            self.start_home();
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

    fn lock_state(&self) -> String {
        alloc::format!(
            "locked={} set={} wait={}\n",
            self.power.locked as u32,
            self.passcode().is_some() as u32,
            self.power.attempts.wait(sys::now_ns())
        )
    }

    /// Check a typed passcode, counting wrong ones: Ok, or the answer to give.
    fn check_code(&mut self, code: &str) -> Result<(), String> {
        let now = sys::now_ns();
        let wait = self.power.attempts.wait(now);
        if wait > 0 {
            return Err(alloc::format!("error wait {}\n", wait));
        }
        let Some(record) = self.passcode() else { return Ok(()) };
        if lock::matches(&record, code, sentinel::sha256) {
            self.power.attempts.right();
            if lock::wrong_in(&record) != 0 {
                self.save_passcode(&lock::with_wrong(&record, 0));
            }
            return Ok(());
        }
        self.power.attempts.wrong(now);
        self.save_passcode(&lock::with_wrong(&record, self.power.attempts.count()));
        log!("wrong passcode");
        Err(match self.power.attempts.wait(now) {
            0 => String::from("error wrong\n"),
            wait => alloc::format!("error wait {}\n", wait),
        })
    }

    fn save_passcode(&mut self, record: &str) -> bool {
        match self.fs.writable(lock::FILE, true, true, 0) {
            Some(f) => {
                f.extend_from_slice(record.as_bytes());
                true
            }
            None => false,
        }
    }

    /// The "lock", "unlock", "passcode" and "power" commands of /dev/violet/sys.
    pub fn power_command(&mut self, i: usize, verb: &str, words: &mut core::str::SplitWhitespace) -> String {
        let path = self.apps[i].as_ref().map_or("", |a| a.path.as_str());
        let (home, settings) = (path == HOME_APP, path == super::wifi::SETTINGS_APP);
        match (verb, words.next(), words.next(), words.next()) {
            ("lock", None, ..) => self.lock_state(),
            ("unlock", Some(code), None, _) if home => {
                if self.power.locked {
                    if let Err(answer) = self.check_code(code) {
                        return answer;
                    }
                    self.power.locked = false;
                    log!("unlocked");
                }
                String::from("ok\n")
            }
            ("passcode", Some("set"), Some(new), old) if settings && !self.power.locked && words.next().is_none() => {
                if self.passcode().is_some() {
                    if let Err(answer) = self.check_code(old.unwrap_or("")) {
                        return answer;
                    }
                }
                let mut salt = [0u8; 16];
                salt[..8].copy_from_slice(&self.next_random().to_le_bytes());
                salt[8..].copy_from_slice(&(self.next_random() ^ sys::now_ns()).to_le_bytes());
                match lock::record(new, salt, sentinel::sha256) {
                    Some(record) if self.save_passcode(&record) => {
                        log!("passcode set");
                        String::from("ok\n")
                    }
                    Some(_) => String::from("error could not save\n"),
                    None => String::from("error a passcode is 4 to 8 digits\n"),
                }
            }
            ("passcode", Some("check"), Some(old), None) if settings && !self.power.locked => match self.check_code(old) {
                Ok(()) => String::from("ok\n"),
                Err(answer) => answer,
            },
            ("passcode", Some("clear"), old, None) if settings && !self.power.locked => {
                if let Err(answer) = self.check_code(old.unwrap_or("")) {
                    return answer;
                }
                if self.passcode().is_some() && !self.save_passcode("") {
                    return String::from("error could not save\n");
                }
                log!("passcode cleared");
                String::from("ok\n")
            }
            ("power", Some("off"), None, _) if home || settings => {
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
            _ => String::from("error\n"),
        }
    }
}
