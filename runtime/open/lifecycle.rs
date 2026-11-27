//! Open apps' background lifecycle and the low-memory killer: the policy,
//! pure so the host tests it (make test-unit); mod.rs applies it.
//!
//! States, re-evaluated every second and whenever the front app changes:
//!   Foreground  the app on screen. Runs at the foreground tier; never closed.
//!   Visible     behind, but doing something the user notices or just left:
//!               Home, the agent, an app holding an audio stream, the
//!               microphone or Bluetooth, uploading or still connecting over TCP, and any app
//!               for GRACE_NS after it left the front (to save its state).
//!               Runs. (A wake lock is not yet a reason: powerctl.rs keeps
//!               its holders private.)
//!   Cached      behind with no such reason. Still runs for CACHED_RUN_NS, so
//!               a fetch or a save in progress can finish.
//!   Frozen      a cached app past that, or any cached app while memory is
//!               low: its threads are suspended, its memory kept. It runs
//!               again the moment it comes back to the front.
//! Home is never frozen, nor closed to make room for another app (only when
//! its own page fault finds nothing else to close). A child process (fork,
//! posix_spawn) takes its state from the app that started it (`inherit`): a
//! child of the app in front or of Home runs and is never closed for another.
//! A frozen app runs no code at all: an answer to a call it is blocked in
//! (a child's exit, a timer's signal, a futex) is held until it thaws, and a
//! call that slipped in before the freeze runs again then (mod.rs `wake`).
//! A Violet Shield freeze owns an app's threads while it lasts: such an app
//! counts as Cached, is never parked or resumed here and is never closed for
//! memory.
//!
//! Memory pressure, from the pool's free pages with hysteresis: Low below
//! twice the launch reserve (64 MiB): background apps hear LOW_MEMORY once
//! and cached apps freeze at once. Critical below half the reserve (16 MiB):
//! the runtime closes a Frozen or Cached app each second until it is out of
//! Critical. A launch, or an app whose page fault finds the pool empty,
//! closes apps on demand (`pick_victim`): a launch or a fault by the app in
//! front, Home or the agent from any class; a fault by an app behind only
//! frozen or cached ones (`fault_victim`), so an app cannot rank itself
//! Visible and evict the user's. Runtime heap short (pipe buffers and the
//! like): the app holding the most of it, whatever its state, else a cached
//! one (`heap_victim`); pipes are capped per pipe and per app (`pipe_room`).
//! Closing is exit status 137, logged with the reason, remembered for
//! `memory`; the app starts again from scratch when opened.

use alloc::string::String;
use alloc::vec::Vec;

/// Full speed after leaving the front, to save state.
pub const GRACE_NS: u64 = 5_000_000_000;
/// Then cached, still running, this long before it is frozen.
pub const CACHED_RUN_NS: u64 = 10_000_000_000;
/// EVENT_LIFECYCLE value: memory is low, free what you can (caches, buffers).
pub const LIFECYCLE_LOW_MEMORY: i32 = 4;
/// Exit status of an app closed for memory (SIGKILL, as Linux's OOM killer).
pub const EXIT_CLOSED: i32 = 137;
/// Closings remembered for `memory`.
pub const KEPT: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    Foreground,
    /// Until its first tick (the app in front is made Foreground at once).
    #[default]
    Visible,
    Cached,
    Frozen,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Foreground => "fg",
            State::Visible => "visible",
            State::Cached => "cached",
            State::Frozen => "frozen",
        }
    }
}

/// One app's lifecycle: its state and the threads a freeze suspended (the
/// others were already blocked in a call; their waits are not served while
/// it is frozen).
#[derive(Default)]
pub struct Life {
    pub state: State,
    pub parked: Vec<usize>,
}

/// Whether an app's threads are held, by a Shield freeze (`shield`) or the
/// lifecycle: an answer to a call is kept (Wait::Held), no wait is served
/// and a call that arrives is made again after the thaw (mod.rs `wake`,
/// `defer_call`, check_waiters; syscall.rs `held`).
pub fn held(shield: bool, life: &Life) -> bool {
    shield || life.state == State::Frozen
}

/// Whether a Shield thaw may resume thread `slot` (guard.rs `thaw`): not one
/// the lifecycle parked while the app is still frozen.
pub fn shield_may_resume(life: &Life, slot: usize) -> bool {
    !(life.state == State::Frozen && life.parked.contains(&slot))
}

/// What the runtime knows about an app when deciding its state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub front: bool,
    pub home: bool,
    /// Audio, microphone or Bluetooth open, or the agent.
    pub reason: bool,
    /// Held by a Violet Shield freeze.
    pub shield: bool,
    /// When it last left the front (or started), ns.
    pub left_ns: u64,
}

/// A child process's facts: its own (an audio stream, a Shield freeze, when
/// it started) and those of `root`, the oldest ancestor still running, so a
/// child of the app in front or of Home keeps running (a reason) and one of
/// a cached app is cached with it.
pub fn inherit(own: Facts, root: &Facts) -> Facts {
    Facts {
        front: false,
        home: false,
        reason: own.reason || root.front || root.home || root.reason,
        shield: own.shield,
        left_ns: own.left_ns.max(root.left_ns),
    }
}

/// The oldest ancestor of `i` still in the table: `parent[n]` is n's
/// parent's index, if it runs. Bounded, so a cycle cannot hang the loop.
pub fn root_of(parent: &[Option<usize>], i: usize) -> usize {
    let mut at = i;
    for _ in 0..parent.len() {
        match parent.get(at).copied().flatten() {
            Some(p) if p != i && p < parent.len() => at = p,
            _ => break,
        }
    }
    at
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Low,
    Critical,
}

pub fn next_state(f: &Facts, now: u64, p: Pressure) -> State {
    if f.front {
        return State::Foreground;
    }
    let away = now.saturating_sub(f.left_ns);
    if f.home || f.reason || away < GRACE_NS {
        return State::Visible;
    }
    if !f.shield && (p >= Pressure::Low || away >= GRACE_NS + CACHED_RUN_NS) {
        State::Frozen
    } else {
        State::Cached
    }
}

/// Pressure from free pool pages; `reserve` is the launch reserve. Leaving a
/// level takes a quarter reserve more than entering it, so it does not flap.
pub fn pressure(free: usize, reserve: usize, was: Pressure) -> Pressure {
    let slack = |level: Pressure| if was >= level { reserve / 4 } else { 0 };
    if free < reserve / 2 + slack(Pressure::Critical) {
        Pressure::Critical
    } else if free < 2 * reserve + slack(Pressure::Low) {
        Pressure::Low
    } else {
        Pressure::Normal
    }
}

/// An app as the killer sees it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidate {
    pub state: State,
    /// Never closed: the front app, Home, the caller, the agent, a Shield-held app.
    pub protected: bool,
    /// Settings: closed only after every other cached app, never while Visible.
    pub core: bool,
    pub audio: bool,
    pub used_ns: u64,
    /// Runtime heap it holds (bytes in its pipes), for `heap_victim`.
    pub heap: usize,
}

/// The order apps are closed in: 0 Frozen, 1 Cached, 2 a core app in the
/// background, 3 Visible (with an audio stream last of all). None: never.
pub fn class(c: &Candidate) -> Option<u8> {
    if c.protected || c.state == State::Foreground || (c.core && c.state == State::Visible) {
        return None;
    }
    Some(match (c.core, c.state) {
        (true, _) => 2,
        (_, State::Frozen) => 0,
        (_, State::Cached) => 1,
        _ => 3,
    })
}

/// The app to close: lowest class up to `max_class`, then without audio
/// before with, then least recently used. An index into `apps`.
pub fn pick_victim(apps: &[Option<Candidate>], max_class: u8) -> Option<usize> {
    apps.iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_ref().and_then(|c| class(c).filter(|&k| k <= max_class).map(|k| (i, (k, c.audio, c.used_ns)))))
        .min_by_key(|&(_, key)| key)
        .map(|(i, _)| i)
}

/// A page fault the pool cannot serve: the app to close for it (the
/// faulting app itself is `protected`). `privileged`: it is the app in front,
/// Home, the agent or a child of one, and may close any app behind (up to
/// class 3). Any other app may close only frozen and cached ones; None: it
/// gets no page and is closed itself.
pub fn fault_victim(apps: &[Option<Candidate>], privileged: bool) -> Option<usize> {
    pick_victim(apps, if privileged { 3 } else { 1 })
}

/// Heap an app must hold to be closed for it before cached apps are.
pub const HEAP_HOG: usize = 256 << 10;

/// The runtime heap is short: the unprotected app holding the most of it
/// (at least HEAP_HOG, in any class), else the usual frozen or cached one.
pub fn heap_victim(apps: &[Option<Candidate>]) -> Option<usize> {
    apps.iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_ref().filter(|c| c.heap >= HEAP_HOG && class(c).is_some()).map(|c| (i, c.heap)))
        .max_by_key(|&(_, h)| h)
        .map(|(i, _)| i)
        .or_else(|| pick_victim(apps, 1))
}

/// Bytes one pipe holds at most (Linux's default capacity).
pub const PIPE_CAP: usize = 64 << 10;
/// Past this much in all its pipes, an app's pipes hold one page each.
pub const APP_PIPE_SOFT: usize = 512 << 10;
/// An app's pipes hold no more than this in all (the runtime heap holds them).
pub const APP_PIPE_HARD: usize = 1 << 20;
/// Writes up to this size go into a pipe whole or not at all (POSIX PIPE_BUF).
pub const PIPE_BUF: usize = 4096;

/// How much of a `want`-byte write fits in a pipe holding `in_pipe` bytes,
/// when the writer's pipes hold `app_total` in all. 0: the write waits (or
/// EAGAIN, non-blocking).
pub fn pipe_room(in_pipe: usize, app_total: usize, want: usize) -> usize {
    let cap = if app_total >= APP_PIPE_SOFT { PIPE_BUF } else { PIPE_CAP };
    let room = cap.saturating_sub(in_pipe).min(APP_PIPE_HARD.saturating_sub(app_total));
    if want <= PIPE_BUF && room < want {
        0
    } else {
        room.min(want)
    }
}

/// An app's name as `memory` prints it: letters, digits, '.', '-' and '_'
/// (anything else is '_', at most 32), so a name cannot forge fields.
pub fn clean(name: &str) -> String {
    name.chars().take(32).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

/// How many apps may run at once: the table's `capacity`, the kernel-memory
/// `slices` carved at boot, and what the pool holds: the launch reserve for
/// the app in front plus `per_app` pages for each one behind it.
pub fn app_limit(capacity: usize, slices: usize, app_pages: usize, reserve: usize, per_app: usize) -> usize {
    let by_pool = 1 + app_pages.saturating_sub(reserve) / per_app.max(1);
    capacity.min(slices).min(by_pool)
}

/// An app closed for memory, for `memory` and the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closed {
    pub name: String,
    pub reason: &'static str,
    pub kib: usize,
    pub at_s: u64,
}

/// The runtime's lifecycle book: pressure, when it last looked, recent closings.
#[derive(Default)]
pub struct Book {
    pub pressure: Pressure,
    pub last_ns: u64,
    pub closed: Vec<Closed>,
    pub total_closed: u32,
}

impl Book {
    pub fn note(&mut self, c: Closed) {
        if self.closed.len() >= KEPT {
            self.closed.remove(0);
        }
        self.closed.push(c);
        self.total_closed = self.total_closed.saturating_add(1);
    }
    /// " closed=N last=name:reason:KiB@s,..." for `memory`.
    pub fn report(&self, out: &mut String) {
        use core::fmt::Write;
        let _ = write!(out, " pressure={} closed={}", ["normal", "low", "critical"][self.pressure as usize], self.total_closed);
        for (n, c) in self.closed.iter().enumerate() {
            let _ = write!(out, "{}{}:{}:{}@{}", if n == 0 { " last=" } else { "," }, clean(&c.name), c.reason.replace(' ', "_"), c.kib, c.at_s);
        }
    }
}

/// Free pool pages as a bitmap, beside the free list, so a page given back
/// while still free, taken while in use, or that was never the apps' (the
/// heap below, the unpacked image above) is caught instead of corrupting
/// memory. Pages carry no owner: a stale give-back after the page was taken
/// again (by another app) is not caught.
pub struct PageMap {
    free: Vec<u32>,
    /// Pages apps may ever hold: [lo, hi).
    lo: usize,
    hi: usize,
}

impl PageMap {
    /// The apps' pages are `managed`, all free at first.
    pub fn new(managed: core::ops::Range<usize>) -> PageMap {
        let mut m = PageMap { free: alloc::vec![0; (managed.end + 31) / 32], lo: managed.start, hi: managed.end };
        for i in managed {
            m.free[i / 32] |= 1 << (i % 32);
        }
        m
    }
    fn managed(&self, i: u32) -> bool {
        (self.lo..self.hi).contains(&(i as usize))
    }
    pub fn is_free(&self, i: u32) -> bool {
        self.managed(i) && self.free[i as usize / 32] & (1 << (i % 32)) != 0
    }
    /// Mark `i` in use; false if it was not free (or is not the apps').
    pub fn take(&mut self, i: u32) -> bool {
        if !self.is_free(i) {
            return false;
        }
        self.free[i as usize / 32] &= !(1 << (i % 32));
        true
    }
    /// Mark `i` free; false if it already was (a double give-back) or is not the apps'.
    pub fn give(&mut self, i: u32) -> bool {
        if !self.managed(i) || self.is_free(i) {
            return false;
        }
        self.free[i as usize / 32] |= 1 << (i % 32);
        true
    }
    /// The top `count` pages leave the apps for good (take_top).
    pub fn retire_top(&mut self, count: usize) {
        self.hi = self.hi.saturating_sub(count).max(self.lo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const S: u64 = 1_000_000_000;

    fn away(left: u64) -> Facts {
        Facts { left_ns: left, ..Facts::default() }
    }

    #[test]
    fn states_follow_the_policy() {
        let n = Pressure::Normal;
        assert_eq!(next_state(&Facts { front: true, ..away(0) }, 100 * S, n), State::Foreground);
        // Grace, then cached while it may still finish work, then frozen.
        assert_eq!(next_state(&away(10 * S), 14 * S, n), State::Visible);
        assert_eq!(next_state(&away(10 * S), 15 * S, n), State::Cached);
        assert_eq!(next_state(&away(10 * S), 24 * S, n), State::Cached);
        assert_eq!(next_state(&away(10 * S), 25 * S, n), State::Frozen);
        // Low memory freezes a cached app at once, never one in its grace.
        assert_eq!(next_state(&away(10 * S), 15 * S, Pressure::Low), State::Frozen);
        assert_eq!(next_state(&away(10 * S), 12 * S, Pressure::Critical), State::Visible);
        // Home and apps with a reason keep running however long they are behind.
        assert_eq!(next_state(&Facts { home: true, ..away(0) }, 999 * S, Pressure::Critical), State::Visible);
        assert_eq!(next_state(&Facts { reason: true, ..away(0) }, 999 * S, Pressure::Critical), State::Visible);
        // A Shield-held app is never parked here.
        assert_eq!(next_state(&Facts { shield: true, ..away(0) }, 999 * S, Pressure::Critical), State::Cached);
        // A clock that went backwards counts as just left.
        assert_eq!(next_state(&away(50 * S), 10 * S, n), State::Visible);
    }

    #[test]
    fn pressure_has_hysteresis() {
        let r = 512;
        assert_eq!(pressure(5000, r, Pressure::Normal), Pressure::Normal);
        assert_eq!(pressure(1023, r, Pressure::Normal), Pressure::Low);
        assert_eq!(pressure(1024, r, Pressure::Normal), Pressure::Normal);
        // Leaving Low takes 128 pages more.
        assert_eq!(pressure(1100, r, Pressure::Low), Pressure::Low);
        assert_eq!(pressure(1152, r, Pressure::Low), Pressure::Normal);
        assert_eq!(pressure(255, r, Pressure::Low), Pressure::Critical);
        assert_eq!(pressure(300, r, Pressure::Critical), Pressure::Critical);
        assert_eq!(pressure(384, r, Pressure::Critical), Pressure::Low);
        assert_eq!(pressure(0, r, Pressure::Normal), Pressure::Critical);
    }

    fn c(state: State, used: u64) -> Option<Candidate> {
        Some(Candidate { state, used_ns: used, ..Candidate::default() })
    }

    #[test]
    fn victims_in_order() {
        // Frozen before cached before visible; least recently used first.
        let apps = [c(State::Visible, 1), c(State::Cached, 5), c(State::Frozen, 9), c(State::Frozen, 7), None];
        assert_eq!(pick_victim(&apps, 3), Some(3));
        assert_eq!(pick_victim(&apps[..2], 3), Some(1));
        assert_eq!(pick_victim(&apps[..1], 3), Some(0));
        // A proactive pass only closes frozen and cached apps.
        assert_eq!(pick_victim(&apps[..1], 1), None);
        // Audio goes last within its class.
        let music = Some(Candidate { audio: true, ..c(State::Visible, 1).unwrap() });
        assert_eq!(pick_victim(&[music, c(State::Visible, 9)], 3), Some(1));
        assert_eq!(pick_victim(&[music], 3), Some(0));
    }

    #[test]
    fn protected_apps_are_never_closed() {
        let p = |state| Some(Candidate { protected: true, ..c(state, 0).unwrap() });
        let apps = [p(State::Frozen), p(State::Cached), c(State::Foreground, 0), p(State::Visible)];
        assert_eq!(pick_victim(&apps, 3), None);
        // Settings: after every other cached app; never in its grace.
        let settings = |state, used| Some(Candidate { core: true, ..c(state, used).unwrap() });
        assert_eq!(pick_victim(&[settings(State::Frozen, 0), c(State::Cached, 9)], 3), Some(1));
        assert_eq!(pick_victim(&[settings(State::Frozen, 0), c(State::Visible, 9)], 3), Some(0));
        assert_eq!(pick_victim(&[settings(State::Frozen, 0)], 1), None);
        assert_eq!(pick_victim(&[settings(State::Visible, 0)], 3), None);
    }

    #[test]
    fn app_limit_is_the_tightest_bound() {
        // Handset: 4096 pages less a 768-page heap and a 320-page image.
        let surface = 2 * 71;
        assert_eq!(app_limit(12, 6, 3008, 512, surface + 32), 6);
        assert_eq!(app_limit(12, 14, 3008, 512, surface + 32), 12);
        assert_eq!(app_limit(12, 14, 1200, 512, surface + 32), 4);
        assert_eq!(app_limit(12, 14, 100, 512, surface + 32), 1);
        assert_eq!(app_limit(12, 0, 3008, 512, surface + 32), 0);
    }

    #[test]
    fn book_keeps_the_last_closings() {
        let mut b = Book::default();
        for n in 0..10 {
            b.note(Closed { name: alloc::format!("a{}", n), reason: "low memory", kib: 64, at_s: n });
        }
        assert_eq!(b.closed.len(), KEPT);
        assert_eq!(b.closed[0].name, "a2");
        let mut out = String::new();
        b.report(&mut out);
        assert!(out.starts_with(" pressure=normal closed=10 last=a2:low_memory:64@2,a3:"), "{}", out);
    }

    #[test]
    fn children_follow_the_app_that_started_them() {
        // A job started by the app in front runs however long it takes.
        let front = Facts { front: true, left_ns: 0, ..Facts::default() };
        let child = inherit(away(10 * S), &front);
        assert_eq!(next_state(&child, 999 * S, Pressure::Critical), State::Visible);
        assert!(!child.front && !child.home);
        // So does one of Home.
        assert_eq!(next_state(&inherit(away(0), &Facts { home: true, ..away(0) }), 999 * S, Pressure::Low), State::Visible);
        // One of a cached app freezes with it, counted from the later of the two.
        let cached = away(100 * S);
        assert_eq!(next_state(&inherit(away(10 * S), &cached), 104 * S, Pressure::Normal), State::Visible);
        assert_eq!(next_state(&inherit(away(10 * S), &cached), 115 * S, Pressure::Normal), State::Frozen);
        // Its own audio stream is still its own reason.
        assert_eq!(next_state(&inherit(Facts { reason: true, ..away(0) }, &cached), 999 * S, Pressure::Normal), State::Visible);
        // A child's Shield freeze is its own.
        assert!(inherit(Facts { shield: true, ..away(0) }, &front).shield);
        assert!(!inherit(away(0), &Facts { shield: true, ..away(0) }).shield);
    }

    #[test]
    fn root_is_the_oldest_running_ancestor() {
        // 0 started 1, 1 started 2; 3 has no parent; 4 and 5 point at each other.
        let parent = [None, Some(0), Some(1), None, Some(5), Some(4)];
        assert_eq!(root_of(&parent, 2), 0);
        assert_eq!(root_of(&parent, 1), 0);
        assert_eq!(root_of(&parent, 0), 0);
        assert_eq!(root_of(&parent, 3), 3);
        let r = root_of(&parent, 4);
        assert!(r == 4 || r == 5);
        assert_eq!(root_of(&[Some(0)], 0), 0);
        assert_eq!(root_of(&[Some(7)], 0), 0, "a parent outside the table is no parent");
    }

    #[test]
    fn a_background_fault_cannot_evict_visible_apps() {
        // The faulting app (protected) is a background app keeping itself
        // Visible with a TCP send and an audio fd; the others are the user's.
        let me = Some(Candidate { protected: true, audio: true, ..c(State::Visible, 99).unwrap() });
        let music = Some(Candidate { audio: true, ..c(State::Visible, 1).unwrap() });
        let settings = Some(Candidate { core: true, ..c(State::Cached, 2).unwrap() });
        let apps = [me, music, settings, c(State::Visible, 3), c(State::Cached, 4), c(State::Frozen, 5)];
        assert_eq!(fault_victim(&apps, false), Some(5));
        assert_eq!(fault_victim(&[me, music, settings, c(State::Visible, 3), c(State::Cached, 4)], false), Some(4));
        // Nothing frozen or cached left: it is closed itself, not Settings,
        // a Visible app or the music player.
        assert_eq!(fault_victim(&[me, music, settings, c(State::Visible, 3)], false), None);
        // The app in front (or Home, the agent) may close any of them.
        assert_eq!(fault_victim(&[me, music, settings, c(State::Visible, 3)], true), Some(2));
        assert_eq!(fault_victim(&[me, music, c(State::Visible, 3)], true), Some(2));
        assert_eq!(fault_victim(&[me, music], true), Some(1));
    }

    #[test]
    fn the_heap_hog_goes_first() {
        let hog = |state, heap| Some(Candidate { heap, ..c(state, 50).unwrap() });
        // A Visible app holding megabytes of pipe data, before an innocent cached one.
        let apps = [c(State::Cached, 1), hog(State::Visible, 3 << 20), hog(State::Visible, 1 << 20), c(State::Frozen, 2)];
        assert_eq!(heap_victim(&apps), Some(1));
        // Below HEAP_HOG nobody is a hog: the usual frozen-then-cached order.
        assert_eq!(heap_victim(&[c(State::Cached, 1), hog(State::Visible, HEAP_HOG - 1), c(State::Frozen, 2)]), Some(2));
        assert_eq!(heap_victim(&[hog(State::Visible, HEAP_HOG - 1)]), None);
        // Never the app in front, Home or a protected app, however much it holds.
        let fg = hog(State::Foreground, 8 << 20);
        let home = Some(Candidate { protected: true, ..hog(State::Visible, 8 << 20).unwrap() });
        assert_eq!(heap_victim(&[fg, home]), None);
        assert_eq!(heap_victim(&[fg, home, c(State::Cached, 1)]), Some(2));
    }

    #[test]
    fn pipes_are_bounded() {
        assert_eq!(pipe_room(0, 0, 10 << 20), PIPE_CAP, "a huge write takes what fits");
        assert_eq!(pipe_room(PIPE_CAP - 10, 0, 100), 0, "a small write goes whole or waits");
        assert_eq!(pipe_room(PIPE_CAP - 10, 0, 8192), 10);
        assert_eq!(pipe_room(PIPE_CAP, 0, 1), 0);
        assert_eq!(pipe_room(0, 0, 0), 0);
        // Past the soft limit a pipe holds one page; at the hard limit nothing.
        assert_eq!(pipe_room(0, APP_PIPE_SOFT, 1 << 20), PIPE_BUF);
        assert_eq!(pipe_room(100, APP_PIPE_SOFT, 1 << 20), PIPE_BUF - 100);
        assert_eq!(pipe_room(0, APP_PIPE_HARD - 100, 8192), 100);
        assert_eq!(pipe_room(0, APP_PIPE_HARD - 100, 200), 0);
        assert_eq!(pipe_room(0, APP_PIPE_HARD, 1), 0);
        assert_eq!(pipe_room(0, APP_PIPE_HARD + 5, 1), 0);
        // So one app holds at most APP_PIPE_HARD however many pipes it fills.
        let (mut total, mut pipes) = (0, 0);
        while pipes < 2000 {
            let n = pipe_room(0, total, 1 << 20);
            if n == 0 { break; }
            total += n;
            pipes += 1;
        }
        assert!(total <= APP_PIPE_HARD, "{}", total);
    }

    #[test]
    fn names_cannot_forge_report_fields() {
        assert_eq!(clean("music"), "music");
        assert_eq!(clean("a b=1:x,y"), "a_b_1_x_y");
        assert_eq!(clean(&"x".repeat(100)).len(), 32);
        let mut b = Book::default();
        b.note(Closed { name: alloc::format!("evil cache=1 x"), reason: "low memory", kib: 1, at_s: 0 });
        let mut out = String::new();
        b.report(&mut out);
        assert_eq!(out, " pressure=normal closed=1 last=evil_cache_1_x:low_memory:1@0");
    }

    #[test]
    fn a_frozen_app_is_held_until_it_thaws() {
        let mut life = Life::default();
        assert!(!held(false, &life));
        assert!(held(true, &life), "a Shield freeze holds it");
        // The lifecycle parks threads 0 and 2 (1 was blocked in a call).
        life = Life { state: State::Frozen, parked: alloc::vec![0, 2] };
        assert!(held(false, &life), "a timer's signal or a child's exit must not answer it");
        // A Shield freeze and thaw within one lifecycle second: the threads
        // the lifecycle parked stay parked; any other the Shield suspended runs.
        assert!(!shield_may_resume(&life, 0) && !shield_may_resume(&life, 2));
        assert!(shield_may_resume(&life, 1));
        // Once the lifecycle let it go (Cached, Visible, in front) the Shield may resume all.
        life.state = State::Cached;
        assert!(shield_may_resume(&life, 0) && !held(false, &life));
    }

    #[test]
    fn a_new_app_is_not_reported_in_front() {
        assert_eq!(Life::default().state, State::Visible);
    }

    #[test]
    fn page_map_catches_double_frees() {
        let mut m = PageMap::new(10..100);
        assert!(!m.is_free(9) && m.is_free(10) && m.is_free(99) && !m.is_free(100));
        assert!(m.take(50));
        assert!(!m.take(50), "taken twice");
        assert!(m.give(50));
        assert!(!m.give(50), "given back twice");
        assert!(!m.give(100) && !m.take(100) && !m.give(5000) && !m.take(5000));
        // A heap page was never the apps': giving it back is refused.
        assert!(!m.give(3) && !m.take(3));
        // The image's pages at the top leave for good.
        assert!(m.take(95));
        m.retire_top(10);
        assert!(!m.give(95) && !m.is_free(99) && m.is_free(89));
    }
}
