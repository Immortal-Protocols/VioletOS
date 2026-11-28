//! The trusted system UI: what the Open Runtime itself draws on the screen,
//! which apps can neither draw over nor see touches on. The rules are in
//! sysui.rs (host-tested); this file draws them and connects them to the
//! runtime's loop, touches and /dev/violet/sys.
//!
//! - The band: the top sysui::BAND_H rows, drawn by the runtime over every
//!   app: the title of the app in front (from the image's /apps/.home, with
//!   "INSTALLED" for an app the Agent installed, whatever title it chose),
//!   MIC and CAM while they are on, and "VIOLET SYSTEM" in the accent colour
//!   while a prompt, the passcode pad or the lock screen is up. Presents never
//!   write these rows (device.rs flush_present), and touches that start in
//!   them never reach the app.
//! - Secure prompts (`request_consent`), the passcode pad and the lock
//!   screen: while one is up the app in front hears FOCUS_LOST, its presents
//!   are dropped, every real touch goes here, and synthetic touches (`bench`)
//!   stop.
//! - The passcode, set in Settings on the runtime's own keypad (Settings never
//!   sees the digits), kept in LOCK_FILE, asked for at boot, after
//!   sysui::AUTO_LOCK_NS without a touch, after Home's Lock or `lock`, and
//!   after the screen goes off.
use super::device::{self, ACCENT, ALERT, BG, INK, OK_GREEN, PANEL};
use super::policy::{Cap, Decision};
use super::sys::{self, FRAMEBUFFER};
use super::sysui::{self, Mode, Ui, BAND_H, DONE, ERASE, REDRAW, SAVE};
use super::Runtime;
use alloc::string::String;
use crate::protocol::*;

/// The passcode record (sysui::Record). A dot name in /data: the runtime's,
/// never an app's (syscall.rs `at`, and policy.rs path_rule).
pub const LOCK_FILE: &str = "/data/.lock";
const AMBER: u32 = 0xffeeb264;
const BAND_BG: u32 = 0xff0c0a10;

struct State {
    ui: Ui,
    /// The pid of the app on screen (0: none, or the runtime not in front).
    front: u32,
    /// The overlay as last switched to, and whether the runtime was focused.
    shown: Mode,
    focused: bool,
    /// What the band shows, as last drawn.
    band: [u32; 4],
    band_dirty: bool,
    dirty: bool,
    /// Seconds of back-off last drawn.
    wait: u64,
    /// The command being run came over USB (handset "@sys").
    usb: bool,
    /// Where the finger was last seen down. A release is judged there: a
    /// touch controller's lift sample need not carry a position (QEMU's is 0, 0).
    last_xy: (i32, i32),
    /// The last finger down that system UI took (Runtime.last_touch_ns
    /// counts only touches that reached an app or the app list, for Violet
    /// Shield); the idle lock counts both.
    touched_ns: u64,
    /// The lock screen or passcode pad was up: clear it from behind a prompt.
    clear: bool,
}

static mut STATE: Option<State> = None;
fn st() -> &'static mut State {
    // The runtime is one thread; this is touched only from its loop.
    unsafe {
        (*core::ptr::addr_of_mut!(STATE)).get_or_insert_with(|| State {
            ui: Ui::new(),
            front: 0,
            shown: Mode::App,
            focused: false,
            band: [0; 4],
            band_dirty: true,
            dirty: true,
            wait: 0,
            usb: false,
            last_xy: (-1, -1),
            touched_ns: 0,
            clear: false,
        })
    }
}

/// At start, before anything is shown: a saved passcode locks the phone.
/// `listed`: data.rs `load` read every file Storage keeps; if not, a missing
/// LOCK_FILE proves nothing and the phone stays locked (sysui.rs `boot`).
pub fn boot(fs: &super::fs::Fs, listed: bool) {
    st().ui.pass.boot(fs.contents(LOCK_FILE), listed, sys::now_ns());
}
/// A prompt, the passcode pad or the lock screen owns the screen and touches.
pub fn covers() -> bool {
    st().ui.mode() != Mode::App
}
/// An overlay has been switched to (ui_switch ran): the app in front was
/// told FOCUS_LOST then, and hears FOCUS_GAINED only when it goes.
pub fn shown_over() -> bool {
    st().shown != Mode::App
}
pub fn locked() -> bool {
    st().ui.pass.locked
}
/// The screen went off: lock, if there is a passcode.
pub fn screen_off() {
    st().ui.pass.lock();
}
/// Something other than the band drew over its rows.
pub fn band_damaged() {
    st().band_dirty = true;
}
pub fn set_usb(on: bool) {
    st().usb = on;
}
/// Files no app may open, list, rename or remove.
pub fn private_path(path: &str) -> bool {
    path == LOCK_FILE
}

/// Ask the user whether app `pid` (Violet Shield owner `owner`) may use
/// `cap`. Allow or Deny once answered (kept for this boot; policy.rs keeps
/// it across restarts in /data/.grants); Ask while the prompt is up, and the
/// caller asks again later (EAGAIN, or a wait that asks again). Deny at once,
/// with nothing remembered, when the app is not the one on screen, the phone
/// is locked or the passcode pad is up, or another prompt is up.
pub fn request_consent(pid: u32, owner: &str, cap: Cap) -> Decision {
    let s = st();
    let blocked = s.ui.pass.locked || s.ui.pass.step.is_some();
    let front = pid != 0 && s.front == pid;
    match s.ui.consent.request(pid, owner, sysui::Cap(cap.bit() as u32), front, blocked) {
        sysui::Decision::Allow => Decision::Allow,
        sysui::Decision::Deny => Decision::Deny,
        sysui::Decision::Prompt => {
            s.dirty = true;
            Decision::Ask
        }
    }
}

/// App `pid`'s prompt for `cap` is up and unanswered: a system call that
/// needs the answer waits for it (Wait::Consent) and then runs again.
pub fn pending(pid: u32, cap: Cap) -> bool {
    st().ui.consent.prompt.as_ref().is_some_and(|p| p.pid == pid && p.cap.0 == cap.bit() as u32)
}
/// What device.rs `ready` answers for a call to run again from the start.
pub const RESTART: i32 = i32::MIN;

/// What the user answered this boot for `owner` and `cap`, if they were
/// asked. Only this is the user's word: a Deny from `request_consent` may
/// just mean "not now" (not on screen, locked, another prompt up), so keep
/// (in /data/.grants) only what this returns.
pub fn user_answer(owner: &str, cap: Cap) -> Option<bool> {
    st().ui.consent.decided(owner, sysui::Cap(cap.bit() as u32))
}

/// App `owner` was installed or uninstalled (manifest.rs `forget_consent`):
/// this boot's answers for the name go, so they never reach a new program.
pub fn forget_consent(owner: &str) {
    let s = st();
    s.ui.consent.forget(owner);
    s.dirty = true;
}

// ---- drawing, straight into scanout ----

fn rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    let (x0, x1) = (x.min(720) as usize, (x + w).min(720) as usize);
    for row in y as usize..(y + h).min(1600) as usize {
        let line = unsafe { core::slice::from_raw_parts_mut((FRAMEBUFFER as *mut u32).add(row * 720 + x0), x1 - x0) };
        line.fill(color);
    }
}
fn text(x: u32, y: u32, s: &str, scale: u32, color: u32) {
    for (n, c) in s.bytes().enumerate() {
        let Some(g) = device::glyph(c) else { continue };
        for (col, &bits) in g.iter().enumerate() {
            for bit in 0..7 {
                if bits & (1 << bit) != 0 {
                    rect(x + (6 * n as u32 + col as u32) * scale, y + bit * scale, scale, scale, color);
                }
            }
        }
    }
}
fn centered(y: u32, s: &str, scale: u32, color: u32) {
    let w = (s.len() as u32 * 6 * scale).min(720);
    text((720 - w) / 2, y, s, scale, color);
}
/// "[open] trusted ui: WHAT" in the open log (the tests wait for these).
#[inline(never)]
fn note(what: &str) {
    crate::log!("trusted ui: {}", what);
}
fn shown(y0: u32, y1: u32) {
    sys::clean_framebuffer(y0 as usize, y1 as usize);
    super::scanout::mark_dirty(y0, y1);
}

/// The band while system UI is up: the accent colour no app can put there,
/// and still MIC and CAM while an app holds them.
fn band_system(mode: Mode, indicators: u32) {
    rect(0, 0, 720, BAND_H, ACCENT);
    text(16, 6, "VIOLET SYSTEM", 3, BG);
    let what = match mode {
        Mode::Lock => "LOCKED",
        Mode::Entry => "PASSCODE",
        _ => "PERMISSION",
    };
    text(720 - 16 - what.len() as u32 * 18, 6, what, 3, BG);
    if indicators != 0 {
        rect(282, 2, 144, BAND_H - 4, BG);
    }
    lamps(indicators, 290, 362);
}

/// MIC and CAM in the band while an app holds them, then the band is shown.
fn lamps(indicators: u32, mic_x: u32, cam_x: u32) {
    if indicators & 1 != 0 {
        text(mic_x, 6, "MIC", 3, OK_GREEN);
    }
    if indicators & 2 != 0 {
        text(cam_x, 6, "CAM", 3, ALERT);
    }
    shown(0, BAND_H);
}

impl Runtime {
    /// One pass of the loop: the passcode check's next slice, overlays coming
    /// and going, and the band and overlay drawn when they changed.
    pub fn ui_tick(&mut self, now: u64) {
        // st() is fetched again after each call that may use it itself.
        st().front = if self.focused { self.foreground.and_then(|f| self.apps[f].as_ref()).map_or(0, |a| a.pid) } else { 0 };
        let s = st();
        // A prompt whose app went away or left the screen is withdrawn.
        if let Some(pid) = s.ui.consent.prompt.as_ref().map(|p| p.pid).filter(|&p| p != s.front) {
            s.ui.consent.cancel(pid);
        }
        if s.ui.pass.busy() {
            let mut acts = 0;
            while s.ui.pass.busy() && sys::now_ns() - now < 4_000_000 {
                acts |= s.ui.pass.work(super::sentinel::sha256, 500, now);
            }
            if acts != 0 && s.ui.pass.note == "WRONG PASSCODE" {
                note("wrong passcode");
            }
            self.ui_apply(acts);
        }
        // Nothing turns the screen off by itself yet (Runtime's `asleep` only
        // follows a POWER 1 that no launcher sends), so a phone left alone
        // locks after sysui::AUTO_LOCK_NS without a touch.
        let s = st();
        if s.ui.pass.idle(now, self.last_touch_ns.max(s.touched_ns)) {
            note("idle");
        }
        let (mode, was) = (st().ui.mode(), st().shown);
        if mode != was {
            st().shown = mode;
            self.ui_switch(was, mode, now);
        }
        let s = st();
        if self.focused != s.focused {
            s.focused = self.focused;
            // Another screen was up: everything of ours has to be drawn again.
            s.dirty = true;
            s.band_dirty = true;
        }
        if !self.focused {
            return;
        }
        let wait = s.ui.pass.not_before.saturating_sub(now).div_ceil(1_000_000_000);
        if wait != s.wait {
            s.wait = wait;
            s.dirty = true;
        }
        if mode != Mode::App && s.dirty {
            s.dirty = false;
            self.ui_draw(mode);
        }
        let indicators = device::mic::active() as u32 | ((self.camera && device::camera::wanted()) as u32) << 1;
        let app = self.foreground.and_then(|f| self.apps[f].as_ref());
        // The program too: an execve in place keeps the pid.
        let band = sysui::band_key(mode, app.map_or(0, |a| a.pid), app.map_or("", |a| a.path.as_str()), indicators);
        let front = self.foreground.filter(|&f| self.apps[f].is_some());
        let s = st();
        if band != s.band || s.band_dirty {
            s.band = band;
            s.band_dirty = false;
            self.draw_band(mode, front, indicators);
        }
    }

    fn ui_switch(&mut self, from: Mode, to: Mode, now: u64) {
        st().dirty = true;
        st().band_dirty = true;
        // A prompt over the lock screen or the pad: clear what they drew.
        st().clear = to == Mode::Prompt && from != Mode::App;
        let held = if from == Mode::App {
            // Covered: the app stops drawing and recording, drops any press
            // in progress, and no synthetic touch follows.
            self.frames.bench = None;
            let finger = self.shell.forget_finger();
            if let Some(f) = self.foreground.filter(|_| self.focused) {
                self.lifecycle(f, super::abi::LIFECYCLE_FOCUS_LOST);
            }
            // The finger system UI has followed since the overlay came up.
            finger || st().ui.taps.held
        } else {
            st().ui.taps.held
        };
        // Armed from now, when it is drawn: nothing counted before this.
        st().ui.arm(now, held);
        note(match to {
            Mode::Lock => "locked",
            Mode::Entry => "passcode pad",
            Mode::Prompt => "prompt",
            Mode::App => if from == Mode::Lock { "unlocked" } else { "closed" },
        });
        if from == Mode::App {
            // Under any system UI nothing records: not the app in front (its
            // FOCUS_LOST did that), not the agent listening from behind, so
            // no one hears the keypad or the room while a prompt is read.
            if self.audio {
                super::request(EXECUTE, MICROPHONE, MIC_OFF);
            }
            if self.camera {
                device::camera::want(false);
            }
        }
        if to == Mode::App {
            if self.audio && self.background_listener().is_some() {
                device::mic::resume();
            }
            self.shell.arm_with(held);
            match self.foreground {
                // Only on screen: the refocus in poll_runtime does it later.
                Some(f) if self.focused => {
                    self.refresh(f);
                    self.lifecycle(f, super::abi::LIFECYCLE_FOCUS_GAINED);
                }
                Some(_) => {}
                None => self.shell.dirty = true,
            }
        }
    }

    /// A call refused for `cap` while app `i`'s prompt for it is up waits for
    /// the user's answer instead (Wait::Consent), then runs again.
    pub fn consent_wait(&self, i: usize, cap: Cap) -> Option<super::Wait> {
        let pid = self.apps[i].as_ref()?.pid;
        pending(pid, cap).then_some(super::Wait::Consent { cap })
    }

    /// A real touch, before anything else sees it. True if the trusted UI took it.
    pub fn ui_touch(&mut self, x: i32, y: i32, down: bool) -> bool {
        let s = st();
        if s.ui.mode() == Mode::App {
            return false;
        }
        let asked = s.ui.consent.prompt.as_ref().map(|p| (p.owner.clone(), p.cap));
        if down {
            s.touched_ns = sys::now_ns();
        }
        let (x, y) = if down { s.last_xy = (x, y); (x, y) } else { s.last_xy };
        let acts = s.ui.touch(x, y, down, sys::now_ns(), super::sentinel::sha256);
        if acts != 0 && s.ui.pass.note == "WAIT" {
            note("passcode not checked: waiting");
        }
        // The user answered a prompt: policy.rs keeps it across restarts.
        if let Some((owner, cap)) = asked.filter(|_| st().ui.consent.prompt.is_none()) {
            let allow = st().ui.consent.decided(&owner, cap) == Some(true);
            if let Some(&(c, _)) = super::policy::CAPS.iter().find(|(c, _)| c.bit() as u32 == cap.0) {
                self.record_consent(&owner, c, allow);
            }
        }
        self.ui_apply(acts);
        true
    }

    /// Carry out what a key or a finished check asked for. The attempt
    /// counter is written here, before the check it counts begins.
    fn ui_apply(&mut self, acts: u8) {
        let s = st();
        if acts & (SAVE | ERASE) != 0 {
            let record = s.ui.pass.file(acts & ERASE != 0);
            if self.ui_save(record.as_deref()) {
                s.ui.pass.saved();
                if acts & DONE != 0 {
                    note(if s.ui.pass.is_set() { "passcode set" } else { "passcode off" });
                }
            } else {
                note("could not save the passcode file");
                s.ui.pass.save_failed();
            }
        }
        if acts & REDRAW != 0 {
            s.dirty = true;
        }
    }

    /// Write (or, with None, remove) LOCK_FILE and wait until Storage has it.
    fn ui_save(&mut self, data: Option<&[u8]>) -> bool {
        match data {
            Some(d) => match self.fs.writable(LOCK_FILE, true, true, 0) {
                Some(f) => f.extend_from_slice(d),
                None => return false,
            },
            None => {
                if self.fs.unlink(LOCK_FILE, false).is_err() && self.fs.contents(LOCK_FILE).is_some() {
                    return false;
                }
            }
        }
        // The limiter's last slot: no app's allowance is charged.
        for _ in 0..256 {
            match super::data::fsync(&mut self.fs, Some(LOCK_FILE), 15, sys::now_ns()).0 {
                0 => return true,
                super::syscall::EINTR => sys::pause(),
                _ => return false,
            }
        }
        false
    }

    /// The app's name for the band and prompts: the image's title for an
    /// app from the boot image, its file name for anything installed (whose
    /// title the installer chose). Upper case, at most 20 letters.
    fn app_title(&self, i: usize) -> (String, bool) {
        let Some(a) = self.apps[i].as_ref() else { return (String::new(), false) };
        let name = a.path.trim_start_matches("/apps/");
        let installed = self.fs.from_layer(&a.path) || a.new_hash;
        let listed = core::str::from_utf8(self.fs.contents("/apps/.home").unwrap_or(&[])).unwrap_or("");
        let title = listed.lines().filter(|_| !installed).find_map(|l| l.strip_prefix(name)?.strip_prefix('\t')).unwrap_or(name);
        (title.bytes().take(20).map(|b| b.to_ascii_uppercase() as char).collect(), installed)
    }

    fn draw_band(&mut self, mode: Mode, front: Option<usize>, indicators: u32) {
        if mode != Mode::App {
            return band_system(mode, indicators);
        }
        // Without an app in front the app list is on screen, and draws itself.
        let Some(f) = front else { return };
        rect(0, 0, 720, BAND_H, BAND_BG);
        let (title, installed) = self.app_title(f);
        text(16, 6, &title, 3, INK);
        if installed {
            text(16 + (title.len() as u32 + 1) * 18, 9, "INSTALLED", 2, AMBER);
        }
        lamps(indicators, 720 - 16 - 3 * 18, 720 - 16 - 7 * 18);
    }

    fn ui_draw(&mut self, mode: Mode) {
        if mode == Mode::Prompt {
            // Over the lock screen or the pad (the app's last frame cannot
            // be shown while covered): nothing of them stays around it.
            if core::mem::take(&mut st().clear) {
                rect(0, BAND_H, 720, 1600 - BAND_H, BG);
                shown(BAND_H, 1600);
            }
            return self.draw_prompt();
        }
        let s = st();
        let p = &s.ui.pass;
        rect(0, BAND_H, 720, 1600 - BAND_H, BG);
        centered(150, p.title(), 5, INK);
        let line = if s.wait > 0 { String::from("TRY AGAIN IN ") + &sysui::dec(s.wait) + " S" } else { String::from(p.note) };
        centered(250, &line, 3, ALERT);
        let n = p.digits.len as u32;
        let x0 = 360 - (n * 48) / 2;
        for k in 0..n {
            rect(x0 + k * 48 + 9, 330, 30, 30, ACCENT);
        }
        if mode == Mode::Lock {
            // Notifications only as a count: never who sent them or what they say.
            centered(440, &s.ui.notes.summary(), 3, INK);
            if self.sentinel.warnings > 0 {
                centered(490, &(String::from("SHIELD WARNINGS: ") + &sysui::dec(self.sentinel.warnings as u64)), 3, AMBER);
            }
        } else {
            centered(440, "ON THE SYSTEM KEYPAD", 3, ACCENT);
            centered(480, "NO APP SEES THESE DIGITS", 3, ACCENT);
        }
        let keys = if mode == Mode::Entry { 13 } else { 12 };
        for k in 0..keys {
            let [x, y, w, h] = sysui::pad_rect(k);
            rect(x, y, w, h, PANEL);
            let key = sysui::pad_key(k);
            let digit = [b'0' + key];
            let (label, scale) = match key {
                sysui::DEL => ("DEL", 4),
                sysui::OK => ("OK", 5),
                sysui::CANCEL => ("CANCEL", 4),
                _ => (core::str::from_utf8(&digit).unwrap_or(""), 7),
            };
            let tw = label.len() as u32 * 6 * scale - scale;
            text(x + (w - tw) / 2, y + (h - 7 * scale) / 2, label, scale, INK);
        }
        shown(BAND_H, 1600);
    }

    fn draw_prompt(&mut self) {
        let s = st();
        let Some(ask) = s.ui.consent.prompt.as_ref() else { return };
        let i = (0..self.apps.len()).find(|&i| self.apps[i].as_ref().is_some_and(|a| a.pid == ask.pid));
        let (title, installed) = i.map_or((String::from("AN APP"), true), |i| self.app_title(i));
        let (y, h) = (sysui::PROMPT_Y, sysui::PROMPT_H);
        rect(40, y, 640, h, ACCENT);
        rect(46, y + 6, 628, h - 12, PANEL);
        centered(y + 40, "VIOLET SYSTEM", 3, ACCENT);
        centered(y + 110, &title, 5, INK);
        if installed {
            centered(y + 170, "INSTALLED BY VIOLET AGENT", 2, AMBER);
        }
        centered(y + 220, "WANTS TO", 3, INK);
        centered(y + 260, ask.cap.words().unwrap_or(""), 3, INK);
        for (b, label, bg, ink) in [(sysui::DENY, "DENY", BG, INK), (sysui::ALLOW, "ALLOW", ACCENT, BG)] {
            let [x, by, w, bh] = sysui::prompt_rect(b);
            rect(x, by, w, bh, bg);
            let tw = label.len() as u32 * 24 - 4;
            text(x + (w - tw) / 2, by + (bh - 28) / 2, label, 4, ink);
        }
        shown(y, y + h);
    }

    /// What `may` needs to know about the caller of a /dev/violet/sys command.
    fn caller(&self, i: usize) -> sysui::Caller {
        let path = self.apps[i].as_ref().map_or("", |a| a.path.as_str());
        // new_hash: written since boot, or another app's program started by
        // an app (manifest.rs `exec_identity`), which is not that app.
        let image = self.apps[i].as_ref().is_some_and(|a| !a.new_hash) && !self.fs.from_layer(path);
        let is = |p: &str| image && path == p;
        let (home, settings, agent) = (is(super::HOME_APP), is(super::wifi::SETTINGS_APP), is(super::appstore::INSTALLER));
        let s = st();
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
    }

    /// The trusted UI's check of a /dev/violet/sys command, before it runs.
    pub fn ui_may(&self, i: usize, line: &str) -> Result<(), &'static str> {
        let mut w = line.split_whitespace();
        let Some(verb) = w.next() else { return Ok(()) };
        sysui::may(verb, w.next(), &self.caller(i))
    }

    /// "passcode" (set=0|1 busy=0|1), "passcode set", "passcode off" (Settings
    /// in front; the runtime's keypad asks for the digits), "lock", "consent
    /// CAP" (allow, deny or pending), "notify TEXT", "notifications" (Home).
    pub fn ui_sys(&mut self, i: usize, verb: &str, line: &str) -> String {
        let s = st();
        let mut words = line.split_whitespace().skip(1);
        let ok = |b: bool| String::from(if b { "ok\n" } else { "error\n" });
        match (verb, words.next()) {
            ("passcode", None) => String::from(if s.ui.pass.is_set() { "set=1" } else { "set=0" }) + if s.ui.pass.step.is_some() { " busy=1\n" } else { " busy=0\n" },
            ("passcode", Some("set")) => {
                let mut salt = [0u8; 16];
                if !self.entropy.fill(&mut salt) {
                    return String::from("error no entropy\n");
                }
                ok(s.ui.pass.begin(Some(salt)))
            }
            ("passcode", Some("off")) => ok(s.ui.pass.begin(None)),
            ("lock", _) => ok(s.ui.pass.lock()),
            ("consent", Some(word)) => {
                let Some(cap) = Cap::parse(word) else { return String::from("error usage: consent net|mic|camera|hci\n") };
                // Only a power the app's manifest declares, still unanswered, raises a prompt.
                String::from(match self.consent_verb(i, cap) {
                    Decision::Allow => "allow\n",
                    Decision::Deny => "deny\n",
                    Decision::Ask => "pending\n",
                })
            }
            ("notify", Some(_)) => {
                // An installed app's notification says so (sysui.rs `post`).
                let (title, installed) = self.app_title(i);
                let text = line.trim_start().strip_prefix("notify").unwrap_or("").trim();
                st().ui.notes.post(&title, installed, text);
                ok(true)
            }
            ("notifications", None) => s.ui.notes.lines(),
            _ => ok(false),
        }
    }
}
