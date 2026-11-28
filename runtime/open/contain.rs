//! Fault containment for the Open Runtime: what survives a fault in its own
//! code (a panic, a failed assertion, a bad pointer) so that the fault ends
//! one app instead of all of them.
//!
//! The supervisor is this process's fault handler. On a fault it starts the
//! thread again at the package entry on a fresh stack, with memory as the
//! fault left it (supervisor.c, `open_warm_restart`; at most three times in
//! ten seconds and eight times per boot, open_recover.h, then the thread is
//! suspended as it always was). `run` in
//! mod.rs asks `enter` which start this is:
//!
//! - `Cold`: the first. The runtime boots.
//! - `Boot`: a fault during boot. Nothing can be trusted: the thread
//!   suspends itself (mod.rs `halt`), as the supervisor used to.
//! - `Warm`: a fault after boot. The runtime, which lives on the heap, is
//!   picked up again; the app it was working for (`serve`, `done`) is closed
//!   and its kernel memory revoked, and the event loop goes on for the rest.
//!   A fault while it was working for no app (`Verdict::Nothing`: the poll,
//!   UI, Shield, lifecycle and network ticks, or a second fault while
//!   closing an app) is not contained: the thread suspends itself.
//!
//! What this does not do: nothing is unwound, so whatever the lost stack was
//! in the middle of stays half done (memory it held is leaked, a file it had
//! borrowed stays borrowed and ends the next app to touch it). A fault that
//! follows memory corruption restarts into corrupted state; the supervisor's
//! limit turns a fault loop into the old suspension. It is a way to keep one
//! bad call from taking every app down, not a security boundary between apps.
//!
//! Plain data and functions with no kernel calls: tests/tcb_host.rs.
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

/// `home` while the runtime boots (never a heap address).
const BOOTING: usize = 1;

pub struct State {
    /// 0 before the first start, BOOTING during it, then the runtime's address.
    home: AtomicUsize,
    /// The app the runtime is working for, as index + 1 (0: none).
    serving: AtomicU32,
    /// Warm starts since boot.
    recovered: AtomicU32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    Cold,
    Boot,
    Warm { runtime: usize, served: Option<usize> },
}

/// What a warm start does about the app that was being served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The app is in the table: close it as any crashed app.
    Close(usize),
    /// Its slot is empty (the fault came while it was being torn down or
    /// replaced): revoke the slot's kernel memory so no thread of it runs on.
    Reclaim(usize),
    /// The runtime was working for no app (or the record is not an app slot).
    Nothing,
}

impl State {
    pub const fn new() -> State {
        State { home: AtomicUsize::new(0), serving: AtomicU32::new(0), recovered: AtomicU32::new(0) }
    }
    /// Which start this is; called once, first thing, at every start. A warm
    /// start takes the record of the app being served, so a second fault
    /// while closing it does not close it again.
    pub fn enter(&self) -> Entry {
        match self.home.load(Relaxed) {
            0 => {
                self.home.store(BOOTING, Relaxed);
                Entry::Cold
            }
            BOOTING => Entry::Boot,
            runtime => {
                self.recovered.fetch_add(1, Relaxed);
                let s = self.serving.swap(0, Relaxed) as usize;
                Entry::Warm { runtime, served: s.checked_sub(1) }
            }
        }
    }
    /// Boot is over: the runtime is at `runtime` for good (a heap address).
    /// Zero or BOOTING would not be one and is ignored (the next fault is
    /// then a boot fault: fail closed).
    pub fn settle(&self, runtime: usize) {
        if runtime > BOOTING {
            self.home.store(runtime, Relaxed);
        }
    }
    /// The runtime starts work for app `index`: a fault from here on closes it.
    pub fn serve(&self, index: usize) {
        self.serving.store(index as u32 + 1, Relaxed);
    }
    /// That work is finished.
    pub fn done(&self) {
        self.serving.store(0, Relaxed);
    }
    pub fn recovered(&self) -> u32 {
        self.recovered.load(Relaxed)
    }
}

/// What to do about `served` in a table of `slots` apps, where `present`
/// says whether a slot holds one.
pub fn verdict(served: Option<usize>, slots: usize, present: impl Fn(usize) -> bool) -> Verdict {
    match served {
        Some(i) if i < slots => {
            if present(i) {
                Verdict::Close(i)
            } else {
                Verdict::Reclaim(i)
            }
        }
        _ => Verdict::Nothing,
    }
}

/// What a warm start needs from the runtime: mod.rs implements it on
/// `Runtime`, tests/tcb_host.rs on a model, so the tests run this code.
pub trait Host {
    /// App slots in the table.
    fn slots(&self) -> usize;
    fn present(&self, i: usize) -> bool;
    /// Close app `i` as a crashed app.
    fn close(&mut self, i: usize);
    /// Revoke the kernel memory of empty slot `i`.
    fn revoke(&mut self, i: usize);
    /// Home's slot, and the slot in front.
    fn home(&mut self) -> &mut Option<usize>;
    fn front(&mut self) -> &mut Option<usize>;
    /// Home was started once since boot (it starts on first focus).
    fn home_tried(&self) -> bool;
    /// Start Home only (not the agent behind it).
    fn start_home(&mut self);
    fn show_home(&mut self);
}

/// A warm start: close the app that was served, revoke what empty slots
/// still hold, and repair what the lost stack may have left half done
/// (Home or the app in front named by a slot that is now empty: a fault in
/// the middle of `reap`). `Nothing` is returned untouched and the caller
/// stops for good: a fault the runtime cannot pin on one app (its own
/// housekeeping, a second fault while closing one) is not contained.
pub fn recover(h: &mut impl Host, served: Option<usize>) -> Verdict {
    let v = verdict(served, h.slots(), |i| h.present(i));
    match v {
        Verdict::Close(i) => h.close(i),
        Verdict::Reclaim(_) => {}
        Verdict::Nothing => return v,
    }
    for i in 0..h.slots() {
        if !h.present(i) {
            h.revoke(i);
        }
    }
    let home = *h.home();
    if home.is_some_and(|i| !h.present(i)) {
        *h.home() = None;
    }
    let front = *h.front();
    if front.is_some_and(|i| !h.present(i)) {
        *h.front() = None;
        h.show_home();
    }
    if h.home_tried() && h.home().is_none() && h.front().is_none() {
        h.start_home();
    }
    v
}

/// The Open Runtime's own record (one process, one thread).
pub static STATE: State = State::new();
