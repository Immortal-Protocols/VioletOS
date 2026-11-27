//! The trusted system UI's rules, without the screen or seL4, so the host
//! tests (runtime/open/tests/sysui_host.rs, `make test-unit`) run them as is.
//! trusted_ui.rs draws them and connects them to the runtime.
//!
//! - The band: the top BAND_H rows of the screen belong to the runtime while
//!   an app is in front (`app_rows`); apps never write them.
//! - Secure prompts (`Consent`), the passcode pad (`Pass`) and the lock
//!   screen own every touch while they are up; a tap counts only if the
//!   finger came down after the overlay had been on screen for ARM_NS and
//!   lifted on the same button (`Taps`).
//! - The passcode: PBKDF2-HMAC-SHA-256 over the digits with a random salt,
//!   the attempt counter saved before an attempt is checked, and a delay
//!   that doubles after FREE_ATTEMPTS failures (`backoff_secs`).
//! - Which /dev/violet/sys commands an app may give (`may`).
use alloc::string::String;
use alloc::vec::Vec;

pub const BAND_H: u32 = 32;
pub const WIDTH: u32 = 720;
pub const HEIGHT: u32 = 1600;

/// The rows y0..y1 of an app's present that may reach scanout: none while an
/// overlay is up, and never the band.
pub fn app_rows(y0: u32, y1: u32, covered: bool) -> Option<(u32, u32)> {
    let y0 = y0.max(BAND_H);
    (!covered && y0 < y1).then_some((y0, y1))
}

/// A capability a prompt asks for, in policy.rs's expected shape (one bit of
/// its `Caps`): only these are ever prompted for. They are exactly
/// policy.rs's CONSENT bits: for any other capability the user's answer
/// would not be what decides (policy::ask settles it from the manifest), so
/// no prompt may ask it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap(pub u32);
impl Cap {
    pub const NET: Cap = Cap(1 << 0);
    pub const MIC: Cap = Cap(1 << 2);
    pub const CAMERA: Cap = Cap(1 << 4);
    pub const HCI: Cap = Cap(1 << 5);
    /// What the prompt says the app wants to do.
    pub fn words(self) -> Option<&'static str> {
        Some(match self {
            Cap::NET => "USE THE INTERNET",
            Cap::MIC => "USE THE MICROPHONE",
            Cap::CAMERA => "USE THE CAMERA",
            Cap::HCI => "USE BLUETOOTH",
            _ => return None,
        })
    }
    pub fn named(name: &str) -> Option<Cap> {
        Some(match name {
            "net" => Cap::NET,
            "mic" => Cap::MIC,
            "camera" => Cap::CAMERA,
            "bluetooth" | "hci" => Cap::HCI,
            _ => return None,
        })
    }
}

/// policy.rs's Decision: Prompt here means "asked, not answered yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Prompt,
}

/// A button on an overlay counts only for a press that began this long
/// after the overlay appeared (and after any finger already down lifted).
pub const ARM_NS: u64 = 400_000_000;
/// A prompt's buttons need more: the press must start after this long with
/// no finger on the glass at all (since the prompt appeared, and since the
/// last touch began or ended). Someone kept tapping where ALLOW will be (a
/// "tap fast" game) never reaches it; a person who reads the prompt does.
pub const QUIET_NS: u64 = 700_000_000;

/// Taps on an overlay's buttons. A button fires when the finger lifts on the
/// button it came down on; sliding off cancels.
#[derive(Default)]
pub struct Taps {
    /// When the overlay appeared, or (`quiet`) when a finger last came down or lifted.
    since_ns: u64,
    lifted: bool,
    pub held: bool,
    press: Option<u8>,
    /// Every touch restarts the wait (prompts), not only the overlay appearing (the keypad).
    quiet: bool,
}
impl Taps {
    /// The keypad's: armed ARM_NS after it appears.
    pub fn new(now: u64, finger_down: bool) -> Taps {
        Taps { since_ns: now, lifted: !finger_down, held: finger_down, press: None, quiet: false }
    }
    /// A prompt's: armed after QUIET_NS without any touch.
    pub fn prompt(now: u64, finger_down: bool) -> Taps {
        Taps { quiet: true, ..Taps::new(now, finger_down) }
    }
    pub fn touch(&mut self, target: Option<u8>, down: bool, now: u64) -> Option<u8> {
        let wait = if self.quiet { QUIET_NS } else { ARM_NS };
        match (down, self.held) {
            (true, false) => {
                self.held = true;
                let armed = self.lifted && now.wrapping_sub(self.since_ns) >= wait;
                if self.quiet {
                    self.since_ns = now;
                }
                self.press = target.filter(|_| armed);
                None
            }
            (true, true) => {
                if target != self.press {
                    self.press = None;
                }
                None
            }
            (false, true) => {
                self.held = false;
                self.lifted = true;
                if self.quiet {
                    self.since_ns = now;
                }
                self.press.take().filter(|&p| Some(p) == target)
            }
            (false, false) => {
                self.lifted = true;
                None
            }
        }
    }
}

// ---- the keypad ----

pub const KEY_W: u32 = 200;
pub const KEY_H: u32 = 140;
const GAP: u32 = 20;
const PAD_X: u32 = 40;
pub const PAD_Y: u32 = 800;
pub const DEL: u8 = 10;
pub const OK: u8 = 11;
pub const CANCEL: u8 = 12;
/// The keys in screen order: 1-9, then DEL 0 OK; CANCEL is a bar below.
const ORDER: [u8; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, DEL, 0, OK, CANCEL];

/// x, y, width, height of the n-th key (ORDER).
pub fn pad_rect(n: usize) -> [u32; 4] {
    if n == 12 {
        return [PAD_X, PAD_Y + 4 * (KEY_H + GAP), WIDTH - 2 * PAD_X, 100];
    }
    let (col, row) = ((n % 3) as u32, (n / 3) as u32);
    [PAD_X + col * (KEY_W + GAP), PAD_Y + row * (KEY_H + GAP), KEY_W, KEY_H]
}
pub fn pad_key(n: usize) -> u8 {
    ORDER[n]
}
pub fn pad_hit(x: i32, y: i32, cancel: bool) -> Option<u8> {
    let keys = if cancel { 13 } else { 12 };
    (0..keys).find(|&n| inside(pad_rect(n), x, y)).map(pad_key)
}
fn inside(r: [u32; 4], x: i32, y: i32) -> bool {
    x >= r[0] as i32 && y >= r[1] as i32 && x < (r[0] + r[2]) as i32 && y < (r[1] + r[3]) as i32
}

/// The prompt's buttons: Deny on the left, Allow on the right.
pub const DENY: u8 = 0;
pub const ALLOW: u8 = 1;
pub const PROMPT_Y: u32 = 520;
pub const PROMPT_H: u32 = 560;
pub fn prompt_rect(button: u8) -> [u32; 4] {
    [if button == ALLOW { 380 } else { 80 }, PROMPT_Y + PROMPT_H - 180, 260, 130]
}
pub fn prompt_hit(x: i32, y: i32) -> Option<u8> {
    [DENY, ALLOW].into_iter().find(|&b| inside(prompt_rect(b), x, y))
}

// ---- the passcode ----

pub type Sha = fn(&[u8]) -> [u8; 32];

/// HMAC-SHA-256 (RFC 2104) for a key of at most 64 bytes and a message of
/// at most 64, which is all PBKDF2 over a passcode needs.
pub fn hmac(sha: Sha, key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut inner = [0x36u8; 128];
    let mut outer = [0x5cu8; 96];
    for (i, k) in key.iter().take(64).enumerate() {
        inner[i] ^= k;
        outer[i] ^= k;
    }
    let n = msg.len().min(64);
    inner[64..64 + n].copy_from_slice(&msg[..n]);
    outer[64..].copy_from_slice(&sha(&inner[..64 + n]));
    sha(&outer)
}

/// PBKDF2-HMAC-SHA-256 (RFC 8018), one 32-byte block, computed a slice at a
/// time so the runtime's loop keeps running.
pub struct Pbkdf2 {
    key: [u8; 16],
    key_len: usize,
    u: [u8; 32],
    t: [u8; 32],
    left: u32,
}
impl Pbkdf2 {
    pub fn start(sha: Sha, key: &[u8], salt: &[u8], iterations: u32) -> Pbkdf2 {
        let mut msg = [0u8; 64];
        let s = salt.len().min(60);
        msg[..s].copy_from_slice(&salt[..s]);
        msg[s..s + 4].copy_from_slice(&1u32.to_be_bytes());
        let mut k = [0u8; 16];
        let key_len = key.len().min(16);
        k[..key_len].copy_from_slice(&key[..key_len]);
        let u = hmac(sha, &k[..key_len], &msg[..s + 4]);
        Pbkdf2 { key: k, key_len, u, t: u, left: iterations.max(1) - 1 }
    }
    /// Up to `n` more iterations; the key once all are done.
    pub fn step(&mut self, sha: Sha, n: u32) -> Option<[u8; 32]> {
        for _ in 0..n.min(self.left) {
            self.u = hmac(sha, &self.key[..self.key_len], &self.u);
            for (t, u) in self.t.iter_mut().zip(self.u) {
                *t ^= u;
            }
            self.left -= 1;
        }
        (self.left == 0).then_some(self.t)
    }
}
impl Drop for Pbkdf2 {
    fn drop(&mut self) {
        for b in self.key.iter_mut() {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

pub fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Iterations for a new passcode: about a quarter second on the handset.
pub const ITERATIONS: u32 = 100_000;
/// Failures before any delay.
pub const FREE_ATTEMPTS: u32 = 5;
/// A phone with a passcode locks after this long without a touch.
pub const AUTO_LOCK_NS: u64 = 300_000_000_000;
/// Seconds to wait after `failures` wrong passcodes: 0 for the first five,
/// then 30 s doubling each time, at most an hour.
pub fn backoff_secs(failures: u32) -> u64 {
    if failures < FREE_ATTEMPTS {
        0
    } else {
        (30u64 << (failures - FREE_ATTEMPTS).min(7)).min(3600)
    }
}

/// The record in the runtime's private file: "VPL1", iterations, failures,
/// salt, PBKDF2 output.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Record {
    pub iterations: u32,
    pub failures: u32,
    pub salt: [u8; 16],
    pub hash: [u8; 32],
}
pub const RECORD_LEN: usize = 60;
impl Record {
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[..4].copy_from_slice(b"VPL1");
        b[4..8].copy_from_slice(&self.iterations.to_le_bytes());
        b[8..12].copy_from_slice(&self.failures.to_le_bytes());
        b[12..28].copy_from_slice(&self.salt);
        b[28..].copy_from_slice(&self.hash);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Record> {
        if b.len() != RECORD_LEN || &b[..4] != b"VPL1" {
            return None;
        }
        let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let iterations = word(4);
        // Fewer than this is not a record this runtime wrote.
        if !(1000..=10_000_000).contains(&iterations) {
            return None;
        }
        Some(Record { iterations, failures: word(8), salt: b[12..28].try_into().unwrap(), hash: b[28..].try_into().unwrap() })
    }
}

/// data.rs `load`'s verdict, after retries: `failed` of Storage's `slots`
/// files could not be listed or read, `offline` of them because Storage
/// answered OFFLINE. Complete if none failed, or if every slot was OFFLINE:
/// Storage has no disk this boot, so nothing can have been kept (a disk
/// that cannot be read at all, or none, is the offline attacker's case,
/// which the passcode does not cover: see runtime/open/README.md).
pub fn listing_complete(failed: u32, offline: u32, slots: u32) -> bool {
    failed == 0 || offline == slots
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stored {
    None,
    /// A file that is not a record, or a /data that could not be read: the
    /// phone stays locked (fail closed).
    Damaged,
    Set(Record),
}

/// What the pad is asking for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Step {
    Unlock,
    /// The current passcode, before changing it (true) or turning it off.
    Current(bool),
    New,
    Confirm,
}

#[derive(Default)]
pub struct Digits {
    d: [u8; 12],
    pub len: usize,
}
impl Digits {
    pub fn push(&mut self, digit: u8) {
        if self.len < 12 {
            self.d[self.len] = b'0' + digit;
            self.len += 1;
        }
    }
    pub fn get(&self) -> &[u8] {
        &self.d[..self.len]
    }
    pub fn clear(&mut self) {
        for b in self.d.iter_mut() {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        self.len = 0;
    }
}

/// Things the caller must do after a key or a slice of work (bits).
pub const SAVE: u8 = 1;
pub const ERASE: u8 = 2;
pub const UNLOCKED: u8 = 4;
pub const DONE: u8 = 8;
pub const REDRAW: u8 = 16;

#[derive(Clone, Copy, PartialEq)]
enum Job {
    Verify,
    Derive,
}

pub struct Pass {
    pub stored: Stored,
    pub locked: bool,
    /// No attempt is checked before this (monotonic ns).
    pub not_before: u64,
    pub step: Option<Step>,
    pub digits: Digits,
    first: Digits,
    pub note: &'static str,
    job: Option<(Pbkdf2, Job)>,
    /// A salt for a new passcode, drawn when a change starts.
    pub salt: Option<[u8; 16]>,
    /// What to go back to if the last save fails.
    backup: Option<Stored>,
}

impl Pass {
    pub fn new() -> Pass {
        Pass {
            stored: Stored::None,
            locked: false,
            not_before: 0,
            step: None,
            digits: Digits::default(),
            first: Digits::default(),
            note: "",
            job: None,
            salt: None,
            backup: None,
        }
    }

    /// At start: the saved file, if any. A set passcode locks the phone, and
    /// a delay that was running starts again in full. `listed`: Storage
    /// answered for every one of its files (data.rs `load`). Without that a
    /// missing file proves nothing, so the phone stays locked (fail closed)
    /// until a restart reads it.
    pub fn boot(&mut self, file: Option<&[u8]>, listed: bool, now: u64) {
        self.stored = match file {
            None if listed => Stored::None,
            None => Stored::Damaged,
            Some(b) => Record::decode(b).map_or(Stored::Damaged, Stored::Set),
        };
        if let Stored::Set(r) = self.stored {
            self.not_before = now + backoff_secs(r.failures) * 1_000_000_000;
        }
        self.lock();
        if file.is_none() && !listed {
            self.note = "STORAGE UNREADABLE - RESTART";
        }
    }

    /// No touch since `last_active` for AUTO_LOCK_NS: lock. True if it locked now.
    pub fn idle(&mut self, now: u64, last_active: u64) -> bool {
        !self.locked && now.saturating_sub(last_active) >= AUTO_LOCK_NS && self.lock()
    }

    pub fn is_set(&self) -> bool {
        !matches!(self.stored, Stored::None)
    }

    /// Lock now (screen off, boot, "lock"). False if there is no passcode.
    pub fn lock(&mut self) -> bool {
        if !self.is_set() {
            return false;
        }
        if !self.locked {
            self.end();
            self.locked = true;
            self.step = Some(Step::Unlock);
        }
        true
    }

    fn end(&mut self) {
        self.step = None;
        self.job = None;
        self.salt = None;
        self.digits.clear();
        self.first.clear();
        self.note = "";
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Settings asked to set or change the passcode (`salt`: fresh random
    /// bytes), or to turn it off (None).
    pub fn begin(&mut self, salt: Option<[u8; 16]>) -> bool {
        if self.locked || self.step.is_some() || matches!(self.stored, Stored::Damaged) || (salt.is_none() && !self.is_set()) {
            return false;
        }
        self.step = Some(if self.is_set() { Step::Current(salt.is_some()) } else { Step::New });
        self.salt = salt;
        true
    }

    pub fn key(&mut self, k: u8, now: u64, sha: Sha) -> u8 {
        let Some(step) = self.step else { return 0 };
        if self.job.is_some() {
            return 0;
        }
        match k {
            0..=9 => {
                self.digits.push(k);
                self.note = "";
            }
            DEL => self.digits.len = self.digits.len.saturating_sub(1),
            CANCEL if step != Step::Unlock => {
                self.end();
                return DONE | REDRAW;
            }
            OK if self.digits.len < 4 => self.note = "AT LEAST 4 DIGITS",
            OK => return self.submit(step, now, sha),
            _ => return 0,
        }
        REDRAW
    }

    fn submit(&mut self, step: Step, now: u64, sha: Sha) -> u8 {
        match step {
            Step::Unlock | Step::Current(_) => {
                if now < self.not_before {
                    self.digits.clear();
                    self.note = "WAIT";
                    return REDRAW;
                }
                let Stored::Set(ref mut r) = self.stored else {
                    self.digits.clear();
                    self.note = "PASSCODE FILE DAMAGED";
                    return REDRAW;
                };
                // Counted, and saved, before it is checked.
                r.failures = r.failures.saturating_add(1);
                self.job = Some((Pbkdf2::start(sha, self.digits.get(), &r.salt, r.iterations), Job::Verify));
                self.digits.clear();
                self.note = "CHECKING";
                SAVE | REDRAW
            }
            Step::New => {
                core::mem::swap(&mut self.first, &mut self.digits);
                self.digits.clear();
                self.note = "";
                self.step = Some(Step::Confirm);
                REDRAW
            }
            Step::Confirm => {
                let matched = self.first.get() == self.digits.get();
                let salt = self.salt.take();
                match salt.filter(|_| matched) {
                    Some(s) => {
                        self.job = Some((Pbkdf2::start(sha, self.digits.get(), &s, ITERATIONS), Job::Derive));
                        self.salt = Some(s);
                        self.note = "SAVING";
                    }
                    None => {
                        self.salt = salt;
                        self.note = if matched { "NO RANDOM SALT" } else { "DID NOT MATCH" };
                        self.step = Some(Step::New);
                    }
                }
                self.digits.clear();
                self.first.clear();
                REDRAW
            }
        }
    }

    /// Up to `n` PBKDF2 iterations of a running check.
    pub fn work(&mut self, sha: Sha, n: u32, now: u64) -> u8 {
        let Some((job, what)) = self.job.as_mut() else { return 0 };
        let Some(out) = job.step(sha, n) else { return 0 };
        let what = *what;
        self.job = None;
        if what == Job::Derive {
            self.backup = Some(self.stored);
            let salt = self.salt.take().unwrap_or([0; 16]);
            self.stored = Stored::Set(Record { iterations: ITERATIONS, failures: 0, salt, hash: out });
            self.end();
            return SAVE | DONE | REDRAW;
        }
        let Stored::Set(ref mut r) = self.stored else { return REDRAW };
        if !same(&out, &r.hash) {
            self.not_before = now + backoff_secs(r.failures) * 1_000_000_000;
            self.note = "WRONG PASSCODE";
            return REDRAW;
        }
        r.failures = 0;
        self.note = "";
        match self.step {
            Some(Step::Current(true)) => {
                self.step = Some(Step::New);
                SAVE | REDRAW
            }
            Some(Step::Current(false)) => {
                self.backup = Some(self.stored);
                self.stored = Stored::None;
                self.end();
                ERASE | DONE | REDRAW
            }
            _ => {
                self.end();
                self.locked = false;
                SAVE | UNLOCKED | REDRAW
            }
        }
    }

    /// The file could not be written: a check that was not counted is not
    /// made, and a new or removed passcode is undone.
    pub fn save_failed(&mut self) {
        if self.job.as_ref().is_some_and(|j| j.1 == Job::Verify) {
            self.job = None;
            self.note = "CANNOT SAVE - TRY AGAIN";
            // Nothing was checked, so it is not a failure either.
            if let Stored::Set(ref mut r) = self.stored {
                r.failures = r.failures.saturating_sub(1);
            }
        }
        if let Some(b) = self.backup.take() {
            self.stored = b;
            self.note = "CANNOT SAVE - TRY AGAIN";
        }
    }
    pub fn saved(&mut self) {
        self.backup = None;
    }

    pub fn title(&self) -> &'static str {
        match self.step {
            Some(Step::Unlock) => "LOCKED",
            Some(Step::Current(_)) => "CURRENT PASSCODE",
            Some(Step::New) => "NEW PASSCODE",
            Some(Step::Confirm) => "CONFIRM PASSCODE",
            None => "",
        }
    }
}

// ---- secure prompts ----

pub struct Ask {
    pub pid: u32,
    pub owner: String,
    pub cap: Cap,
}

#[derive(Default)]
pub struct Consent {
    pub prompt: Option<Ask>,
    /// Answers this boot: (owner, cap bit, allowed), oldest first.
    answers: Vec<(String, u32, bool)>,
}

impl Consent {
    pub fn decided(&self, owner: &str, cap: Cap) -> Option<bool> {
        self.answers.iter().find(|a| a.0 == owner && a.1 == cap.0).map(|a| a.2)
    }
    /// An app asks. `front`: the caller is the app on screen; `blocked`: the
    /// phone is locked or the passcode pad is up. Only the app in front can
    /// raise a prompt, one at a time; an answer is kept for the boot, so a
    /// denied app cannot ask again and again.
    pub fn request(&mut self, pid: u32, owner: &str, cap: Cap, front: bool, blocked: bool) -> Decision {
        if let Some(allowed) = self.decided(owner, cap) {
            return if allowed { Decision::Allow } else { Decision::Deny };
        }
        if cap.words().is_none() || blocked || !front {
            return Decision::Deny;
        }
        match &self.prompt {
            Some(p) if p.pid == pid && p.owner == owner && p.cap == cap => Decision::Prompt,
            Some(_) => Decision::Deny,
            None => {
                self.prompt = Some(Ask { pid, owner: String::from(owner), cap });
                Decision::Prompt
            }
        }
    }
    pub fn answer(&mut self, allow: bool) {
        if let Some(p) = self.prompt.take() {
            if self.answers.len() >= 32 {
                self.answers.remove(0);
            }
            self.answers.push((p.owner, p.cap.0, allow));
        }
    }
    /// A program was installed or removed under `owner`: this boot's answers
    /// for that name (and a prompt up for it) no longer hold.
    pub fn forget(&mut self, owner: &str) {
        self.answers.retain(|a| a.0 != owner);
        if self.prompt.as_ref().is_some_and(|p| p.owner == owner) {
            self.prompt = None;
        }
    }
    /// The asking app went away or left the front: no answer is kept.
    pub fn cancel(&mut self, pid: u32) -> bool {
        let gone = self.prompt.as_ref().is_some_and(|p| p.pid == pid);
        if gone {
            self.prompt = None;
        }
        gone
    }
}

// ---- notifications ----

#[derive(Default)]
pub struct Notes {
    /// "TITLE<TAB>TEXT" lines, newest last.
    pub list: Vec<String>,
}
impl Notes {
    /// `title`: the runtime's name for the app (trusted_ui.rs `app_title`);
    /// `installed`: not from the boot image. An installed app's title is
    /// whatever its installer chose, so it is always marked, and cannot pass
    /// for the system's.
    pub fn post(&mut self, title: &str, installed: bool, text: &str) {
        if self.list.len() >= 8 {
            self.list.remove(0);
        }
        // Control characters (tabs, newlines) would add fields or lines.
        let clean = |s: &str, n: usize| -> String { s.chars().take(n).map(|c| if c.is_control() { ' ' } else { c }).collect() };
        let mark = if installed { "INSTALLED " } else { "" };
        self.list.push(String::from(mark) + &clean(title, 24) + "\t" + &clean(text, 80) + "\n");
    }
    /// What the lock screen shows: how many, never what they say or from whom.
    pub fn summary(&self) -> String {
        match self.list.len() {
            0 => String::new(),
            1 => String::from("1 NOTIFICATION"),
            n => dec(n as u64) + " NOTIFICATIONS",
        }
    }
    pub fn lines(&self) -> String {
        self.list.concat()
    }
}

/// A number in decimal (smaller than formatting it).
pub fn dec(mut n: u64) -> String {
    let mut b = [0u8; 20];
    let mut at = 20;
    loop {
        at -= 1;
        b[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    String::from(core::str::from_utf8(&b[at..]).unwrap_or(""))
}

// ---- who may give which /dev/violet/sys command ----

#[derive(Clone, Copy, Default)]
pub struct Caller {
    /// The app on screen (the runtime focused, the app in front, no overlay).
    pub front: bool,
    /// Home, Settings or the Agent from the boot image.
    pub system: bool,
    /// Home or the Agent from the boot image (they open apps for the user).
    pub opener: bool,
    pub settings: bool,
    pub home: bool,
    /// USB Diagnostics' "@sys" (handset): the developer at the cable.
    pub usb: bool,
    pub locked: bool,
    /// A prompt, the passcode pad or the lock screen is up.
    pub covered: bool,
}

/// Who may give a command: any of these callers (bits), and whether it is
/// refused while the phone is locked or system UI is up.
const FRONT: u8 = 1;
const SYSTEM: u8 = 2;
const OPENER: u8 = 4;
const USB: u8 = 8;
const HOME: u8 = 16;
const NOT_LOCKED: u8 = 32;
const NOT_COVERED: u8 = 64;
const RULES: [(&str, u8); 10] = [
    // Synthetic touches: never from an app, never onto system UI.
    ("bench", USB | NOT_COVERED),
    ("open", FRONT | OPENER | USB | NOT_LOCKED | NOT_COVERED),
    ("launch", FRONT | OPENER | USB | NOT_LOCKED | NOT_COVERED),
    ("home", FRONT | NOT_LOCKED | NOT_COVERED),
    ("apps", FRONT | NOT_LOCKED | NOT_COVERED),
    ("volume", FRONT | SYSTEM),
    ("sound", FRONT | SYSTEM),
    ("mic", FRONT | NOT_LOCKED),
    ("lock", FRONT | SYSTEM),
    ("notifications", HOME | NOT_LOCKED),
];

/// Ok, or the answer that refuses the command.
pub fn may(verb: &str, arg: Option<&str>, c: &Caller) -> Result<(), &'static str> {
    let rule = match (verb, arg) {
        // Where the agent connects: the developer or a system app only.
        ("set", Some("relay" | "stun")) => SYSTEM | USB,
        ("set", _) => FRONT | SYSTEM | USB,
        // Anyone may ask whether a passcode is set; only Settings in front changes it.
        ("passcode", None) => return Ok(()),
        ("passcode", _) => return if c.settings && c.front { Ok(()) } else { Err("error not allowed\n") },
        _ => match RULES.iter().find(|r| r.0 == verb) {
            Some(r) => r.1,
            None => return Ok(()),
        },
    };
    if rule & NOT_LOCKED != 0 && c.locked {
        return Err("error locked\n");
    }
    if rule & NOT_COVERED != 0 && c.covered {
        return Err("error busy\n");
    }
    let who = (c.front as u8) * FRONT | (c.system as u8) * SYSTEM | (c.opener as u8) * OPENER | (c.usb as u8) * USB | (c.home as u8) * HOME;
    if rule & who != 0 { Ok(()) } else { Err("error not allowed\n") }
}

// ---- what the screen shows ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    App,
    Lock,
    Entry,
    Prompt,
}

pub struct Ui {
    pub pass: Pass,
    pub consent: Consent,
    pub notes: Notes,
    pub taps: Taps,
    /// The overlay `taps` was armed for (`arm`, when it is drawn).
    armed: Mode,
}

/// What the band shows, to redraw it when any of it changes: the overlay,
/// the app in front (its pid and its program, which an in-place execve
/// changes under the same pid), and the MIC and CAM indicators.
pub fn band_key(mode: Mode, pid: u32, path: &str, indicators: u32) -> [u32; 4] {
    let hash = path.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    [mode as u32 + 1, pid, indicators, hash]
}

impl Ui {
    pub fn new() -> Ui {
        Ui { pass: Pass::new(), consent: Consent::default(), notes: Notes::default(), taps: Taps::default(), armed: Mode::App }
    }
    /// The overlay now up has just been drawn: its buttons arm from `now`.
    /// `held`: a finger is on the glass.
    pub fn arm(&mut self, now: u64, held: bool) {
        self.armed = self.mode();
        self.taps = if self.armed == Mode::Prompt { Taps::prompt(now, held) } else { Taps::new(now, held) };
    }
    pub fn mode(&self) -> Mode {
        if self.pass.locked {
            Mode::Lock
        } else if self.pass.step.is_some() {
            Mode::Entry
        } else if self.consent.prompt.is_some() {
            Mode::Prompt
        } else {
            Mode::App
        }
    }
    /// A real touch while an overlay is up. Returns the pass bits, with
    /// REDRAW also for a prompt answered.
    pub fn touch(&mut self, x: i32, y: i32, down: bool, now: u64, sha: Sha) -> u8 {
        let mode = self.mode();
        let target = match mode {
            Mode::Lock | Mode::Entry => pad_hit(x, y, mode == Mode::Entry),
            Mode::Prompt => prompt_hit(x, y),
            Mode::App => return 0,
        };
        // An overlay raised since the last was armed (and not drawn yet):
        // no button counts until it is shown and armed (`arm`); only the
        // finger is followed, so a press already down must lift first.
        if mode != self.armed {
            self.taps = Taps::new(now, down);
            return 0;
        }
        let Some(hit) = self.taps.touch(target, down, now) else { return 0 };
        let acts = if mode == Mode::Prompt {
            self.consent.answer(hit == ALLOW);
            REDRAW | DONE
        } else {
            self.pass.key(hit, now, sha)
        };
        // Closed: whatever comes up next is armed afresh.
        if self.mode() != mode {
            self.armed = Mode::App;
        }
        acts
    }
}
