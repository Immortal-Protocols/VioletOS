//! The stand-in for runtime/open/trusted_ui.rs, which draws straight into
//! the framebuffer at its fixed address and keeps its state in a static.
//! The rules are all in runtime/open/sysui.rs (`Ui`), built unchanged; this
//! holds one `Ui` per machine and joins it to the screen, the touch
//! controller and /data/.lock the way trusted_ui.rs does (each function
//! names the one it stands in for).
use super::policy::{Cap, Decision};
use super::sysui::{self, Mode, Ui, BAND_H, DONE, ERASE, REDRAW, SAVE, UNLOCKED};
use super::Runtime;
use crate::kernel::{clock, Screen};
use std::cell::RefCell;

pub const LOCK_FILE: &str = "/data/.lock";
/// device.rs and trusted_ui.rs colours, as the QEMU test read them off the screen.
pub const ACCENT: u32 = 0xffd1b0fe;
pub const PANEL: u32 = 0xff302a39;
pub const BG: u32 = 0xff191620;
pub const INK: u32 = 0xffeeeaf8;
pub const BAND_BG: u32 = 0xff0c0a10;

pub struct State {
    pub ui: Ui,
    /// The pid of the app on screen (0: none).
    pub front: u32,
    pub usb: bool,
    pub last_xy: (i32, i32),
    pub touched_ns: u64,
    /// The overlay as last switched to (drawn and armed).
    pub shown: Mode,
    /// A finger is on the glass (device.rs `Shell` on the phone).
    pub down: bool,
}
thread_local!(static STATE: RefCell<State> = RefCell::new(fresh()));
fn fresh() -> State {
    State { ui: Ui::new(), front: 0, usb: false, last_xy: (-1, -1), touched_ns: 0, shown: Mode::App, down: false }
}
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}
/// A new boot: the state a restart does not keep.
pub fn reset() {
    with(|s| *s = fresh());
}

/// trusted_ui.rs `boot`: a saved passcode locks the phone before anything is shown.
pub fn boot(fs: &super::fs::Fs, listed: bool) {
    with(|s| s.ui.pass.boot(fs.contents(LOCK_FILE), listed, clock::now_ns()));
}
pub fn covers() -> bool {
    with(|s| s.ui.mode() != Mode::App)
}
pub fn locked() -> bool {
    with(|s| s.ui.pass.locked)
}
pub fn mode() -> Mode {
    with(|s| s.ui.mode())
}
/// trusted_ui.rs `screen_off`.
pub fn screen_off() {
    with(|s| s.ui.pass.lock());
}
/// trusted_ui.rs `request_consent`, as written there.
pub fn request_consent(pid: u32, owner: &str, cap: Cap) -> Decision {
    with(|s| {
        let blocked = s.ui.pass.locked || s.ui.pass.step.is_some();
        let front = pid != 0 && s.front == pid;
        match s.ui.consent.request(pid, owner, sysui::Cap(cap.bit() as u32), front, blocked) {
            sysui::Decision::Allow => Decision::Allow,
            sysui::Decision::Deny => Decision::Deny,
            sysui::Decision::Prompt => Decision::Ask,
        }
    })
}
pub fn pending(pid: u32, cap: Cap) -> bool {
    with(|s| s.ui.consent.prompt.as_ref().is_some_and(|p| p.pid == pid && p.cap.0 == cap.bit() as u32))
}
pub fn user_answer(owner: &str, cap: Cap) -> Option<bool> {
    with(|s| s.ui.consent.decided(owner, sysui::Cap(cap.bit() as u32)))
}
pub fn forget_consent(owner: &str) {
    with(|s| s.ui.consent.forget(owner));
}

impl Runtime {
    /// trusted_ui.rs `caller`.
    pub fn caller(&self, i: usize) -> sysui::Caller {
        let path = self.apps[i].as_ref().map_or("", |a| a.path.as_str());
        let image = self.apps[i].as_ref().is_some_and(|a| !a.new_hash) && !self.fs.from_layer(path);
        let is = |p: &str| image && path == p;
        let (home, settings, agent) = (is(super::HOME_APP), is(super::SETTINGS_APP), is(super::INSTALLER));
        with(|s| {
            let covered = s.ui.mode() != Mode::App;
            sysui::Caller {
                front: self.focused && self.foreground == Some(i) && !covered,
                system: home || settings || agent,
                opener: home || agent,
                settings,
                home,
                usb: s.usb,
                locked: s.ui.pass.locked,
                covered,
            }
        })
    }
    /// trusted_ui.rs `ui_may`.
    pub fn ui_may(&self, i: usize, line: &str) -> Result<(), &'static str> {
        let mut w = line.split_whitespace();
        let Some(verb) = w.next() else { return Ok(()) };
        sysui::may(verb, w.next(), &self.caller(i))
    }
    /// trusted_ui.rs `ui_sys`.
    pub fn ui_sys(&mut self, i: usize, verb: &str, line: &str) -> String {
        let mut words = line.split_whitespace().skip(1);
        let ok = |b: bool| String::from(if b { "ok\n" } else { "error\n" });
        match (verb, words.next()) {
            ("passcode", None) => with(|s| String::from(if s.ui.pass.is_set() { "set=1" } else { "set=0" }) + if s.ui.pass.step.is_some() { " busy=1\n" } else { " busy=0\n" }),
            ("passcode", Some("set")) => {
                let mut salt = [0u8; 16];
                if !self.entropy.fill(&mut salt) {
                    return String::from("error no entropy\n");
                }
                ok(with(|s| s.ui.pass.begin(Some(salt))))
            }
            ("passcode", Some("off")) => ok(with(|s| s.ui.pass.begin(None))),
            ("lock", _) => ok(with(|s| s.ui.pass.lock())),
            ("consent", Some(word)) => {
                let Some(cap) = Cap::parse(word) else { return String::from("error usage: consent net|mic|camera|hci\n") };
                String::from(match self.consent_verb(i, cap) {
                    Decision::Allow => "allow\n",
                    Decision::Deny => "deny\n",
                    Decision::Ask => "pending\n",
                })
            }
            ("notify", Some(_)) => {
                let (title, installed) = self.apps[i].as_ref().map_or((String::new(), false), |a| (a.name.clone(), a.new_hash));
                let text = line.trim_start().strip_prefix("notify").unwrap_or("").trim();
                with(|s| s.ui.notes.post(&title, installed, text));
                ok(true)
            }
            ("notifications", None) => with(|s| s.ui.notes.lines()),
            _ => ok(false),
        }
    }

    /// trusted_ui.rs `ui_touch`: a real touch, before anything else sees
    /// it. True if the trusted UI took it.
    pub fn ui_touch(&mut self, x: i32, y: i32, down: bool) -> bool {
        let now = clock::now_ns();
        if mode() == Mode::App {
            return false;
        }
        let (asked, acts) = with(|s| {
            let asked = s.ui.consent.prompt.as_ref().map(|p| (p.owner.clone(), p.cap));
            if down {
                s.touched_ns = now;
                s.last_xy = (x, y);
            }
            let (x, y) = s.last_xy;
            let acts = s.ui.touch(x, y, down, now, super::sha256);
            if acts != 0 && s.ui.pass.note == "WAIT" {
                crate::log!("trusted ui: passcode not checked: waiting");
            }
            (asked, acts)
        });
        // The user answered a prompt: policy.rs keeps it across restarts.
        if let Some((owner, cap)) = asked.filter(|_| with(|s| s.ui.consent.prompt.is_none())) {
            let allow = with(|s| s.ui.consent.decided(&owner, cap)) == Some(true);
            if let Some(&(c, _)) = super::policy::CAPS.iter().find(|(c, _)| c.bit() as u32 == cap.0) {
                self.record_consent(&owner, c, allow);
            }
        }
        self.ui_apply(acts);
        true
    }
    /// trusted_ui.rs `ui_apply` and `ui_save`: the attempt counter is
    /// written before the check it counts begins.
    pub fn ui_apply(&mut self, acts: u8) {
        if acts & (SAVE | ERASE) != 0 {
            let record = with(|s| match s.ui.pass.stored {
                sysui::Stored::Set(r) if acts & ERASE == 0 => Some(r.encode()),
                _ => None,
            });
            let saved = match record {
                Some(r) => self.fs.writable(LOCK_FILE, true, true, 0).map(|f| *f = r.to_vec()).is_some(),
                None => self.fs.unlink(LOCK_FILE, false).is_ok() || self.fs.stat(LOCK_FILE).is_none(),
            };
            with(|s| {
                if saved {
                    s.ui.pass.saved();
                    if acts & DONE != 0 {
                        crate::log!("trusted ui: {}", if s.ui.pass.is_set() { "passcode set" } else { "passcode off" });
                    }
                } else {
                    crate::log!("trusted ui: could not save the passcode file");
                    s.ui.pass.save_failed();
                }
            });
        }
        let _ = (REDRAW, UNLOCKED);
    }
    /// trusted_ui.rs `ui_tick` and `ui_switch`: the passcode check's next
    /// slice, the idle lock, overlays coming and going (armed when drawn).
    pub fn ui_tick(&mut self) {
        let now = clock::now_ns();
        let front = if self.focused { self.foreground.and_then(|i| self.apps[i].as_ref()).map_or(0, |a| a.pid) } else { 0 };
        let last_touch = self.last_touch_ns;
        let acts = with(|s| {
            s.front = front;
            // A prompt whose app went away or left the screen is withdrawn.
            if let Some(pid) = s.ui.consent.prompt.as_ref().map(|p| p.pid).filter(|&p| p != s.front) {
                s.ui.consent.cancel(pid);
            }
            let mut acts = 0;
            while s.ui.pass.busy() {
                acts |= s.ui.pass.work(super::sha256, 4096, now);
            }
            if acts != 0 && s.ui.pass.note == "WRONG PASSCODE" {
                crate::log!("trusted ui: wrong passcode");
            }
            acts
        });
        self.ui_apply(acts);
        with(|s| {
            if s.ui.pass.idle(now, last_touch.max(s.touched_ns)) {
                crate::log!("trusted ui: idle");
            }
            let (to, from) = (s.ui.mode(), s.shown);
            if to != from {
                s.shown = to;
                let held = s.ui.taps.held || (from == Mode::App && s.down);
                s.ui.arm(now, held);
                crate::log!("trusted ui: {}", match to {
                    Mode::Lock => "locked",
                    Mode::Entry => "passcode pad",
                    Mode::Prompt => "prompt",
                    Mode::App => if from == Mode::Lock { "unlocked" } else { "closed" },
                });
            }
        });
    }
    /// trusted_ui.rs `draw_band` and the overlays, as the screen shows them:
    /// the top BAND_H rows are the runtime's whatever the app presents.
    pub fn ui_draw(&self, screen: &mut Screen) {
        let m = mode();
        let band = if m == Mode::App { BAND_BG } else { ACCENT };
        screen.fill(0, 0, crate::kernel::WIDTH, BAND_H as usize, band);
        if m == Mode::Lock || m == Mode::Entry {
            screen.fill(0, BAND_H as usize, crate::kernel::WIDTH, crate::kernel::HEIGHT - BAND_H as usize, BG);
            for n in 0..12 {
                let [x, y, w, h] = sysui::pad_rect(n);
                screen.fill(x as usize, y as usize, w as usize, h as usize, PANEL);
            }
            if m == Mode::Lock {
                // Notifications show on the lock screen only as a count (sysui.rs `summary`).
                let summary = with(|s| s.ui.notes.summary());
                if !summary.is_empty() {
                    screen.fill(200, 440, 320, 22, INK);
                }
            }
        } else if m == Mode::Prompt {
            screen.fill(0, sysui::PROMPT_Y as usize, crate::kernel::WIDTH, sysui::PROMPT_H as usize, PANEL);
        }
    }
}
