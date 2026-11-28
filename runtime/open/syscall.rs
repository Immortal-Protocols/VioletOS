//! Linux ARM EABI system calls for open apps.
//!
//! seL4's own system calls use negative numbers in r7; Linux uses positive
//! ones (plus 0xf0000 and up for ARM private calls). A Linux `svc #0` reaches
//! this process as an unknown-syscall fault carrying r0-r7. The answer goes
//! back in r0. seL4 restarts a faulted thread at FaultIP, which it records as
//! the svc address minus 4 in either instruction set, so the reply sets
//! FaultIP four bytes on: the instruction after the svc.
use super::abi::*;
use super::device::{Kind, OpenFile, BUFFER_BYTES, HEIGHT, STRIDE, WIDTH};
use super::fs::{self, S_IFCHR, S_IFDIR, S_IFMT};
use super::policy::{AccessMode, Action};
use super::sys::{self, MR_CPSR, MR_FAULT_IP};
use super::vma::Va;
use super::{FileRef, Log, Runtime, Wait};
use super::signal::{self, Info};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

pub const EPERM: i32 = -1;
pub const E2BIG: i32 = -7;
pub const ENOEXEC: i32 = -8;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const EBADF: i32 = -9;
pub const ECHILD: i32 = -10;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EEXIST: i32 = -17;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const EMFILE: i32 = -24;
pub const ENOTTY: i32 = -25;
pub const ESPIPE: i32 = -29;
pub const EPIPE: i32 = -32;
pub const ERANGE: i32 = -34;
pub const ENOSYS: i32 = -38;
pub const ENOTEMPTY: i32 = -39;
pub const ETIMEDOUT: i32 = -110;
pub const ENETDOWN: i32 = -100;
pub const EBUSY: i32 = -16;
pub const EAFNOSUPPORT: i32 = -97;

const AT_FDCWD: i32 = -100;
const O_ACCMODE: u32 = 3;
const O_CREAT: u32 = 0o100;
const O_EXCL: u32 = 0o200;
const O_TRUNC: u32 = 0o1000;
const O_APPEND: u32 = 0o2000;
const O_NONBLOCK: u32 = 0o4000;
const O_DIRECTORY: u32 = 0o40000; // ARM's value.
const CLONE_VM: u32 = 0x100;
const CLONE_VFORK: u32 = 0x4000;
const O_CLOEXEC: u32 = 0o2000000;
const CLONE_SETTLS: u32 = 0x80000;
const CLONE_PARENT_SETTID: u32 = 0x100000;
const CLONE_CHILD_CLEARTID: u32 = 0x200000;
const CLONE_CHILD_SETTID: u32 = 0x1000000;
const MAP_SHARED: u32 = 1;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_FIXED_NOREPLACE: u32 = 0x100000;

/// Seconds since 1970 when this image was built, as the wall clock's start.
fn epoch() -> u64 {
    option_env!("VIOLET_BUILD_EPOCH").and_then(|s| s.parse().ok()).unwrap_or(1_795_000_000)
}

pub enum Outcome {
    Done(i32),
    Block(Wait, Option<u64>),
    /// The thread or its whole app ended; nothing to answer.
    Gone,
}
use Outcome::*;

fn align_up(v: u32, a: u32) -> u32 {
    v.wrapping_add(a - 1) & !(a - 1)
}
/// `v` rounded up to `a` (a power of two), or None past the address space.
fn round_up(v: Va, a: Va) -> Option<Va> {
    Some(v.checked_add(a - 1)? & !(a - 1))
}

/// The call's arguments as app addresses and lengths of app memory, whole:
/// an AArch32 call's 32-bit registers, an AArch64 call's 64-bit ones (moved
/// with its 32-bit words where enter64 rewrites a call: abi64.rs `args`).
/// Every address the dispatcher reads or writes app memory at comes from
/// here, never from the 32-bit words, which hold such an argument only below
/// 4 GiB (enter64.rs NOWHERE).
fn vargs(m: &[u32; 16]) -> [Va; 7] {
    #[cfg(target_arch = "aarch64")]
    {
        let _ = m;
        let w = super::abi64::args();
        [w[0], w[1], w[2], w[3], w[4], w[5], 0]
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        [m[0].into(), m[1].into(), m[2].into(), m[3].into(), m[4].into(), m[5].into(), m[6].into()]
    }
}
/// An address or length as the Shield's 32-bit telemetry field records it
/// (system/sentinel common.rs): saturated, for the log only.
fn telemetry(v: Va) -> u32 {
    v.min(Va::from(u32::MAX)) as u32
}
/// The caller's stack pointer at its call, whole.
fn call_sp(m: &[u32; 16]) -> Va {
    #[cfg(target_arch = "aarch64")]
    {
        let _ = m;
        sys::wide()[sys::MR_SP]
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        m[sys::MR_SP].into()
    }
}
/// The answer of a call that answers an app address (mmap, brk): its low
/// word in the 32-bit result, and on AArch64 the whole address for x0
/// (sys.rs `set_result`). An AArch32 address is below 4 GiB.
fn address_answer(va: Va) -> Outcome {
    #[cfg(target_arch = "aarch64")]
    sys::set_result(va as u32, va);
    Done(va as u32 as i32)
}
impl Runtime {
    pub fn syscall(&mut self, i: usize, slot: usize, mrs: &[u32; 16]) {
        let mut saved = [0u32; 9];
        saved[..8].copy_from_slice(&mrs[..8]);
        saved[8] = mrs[MR_FAULT_IP].wrapping_add(4);
        let number = mrs[7];
        self.stats.count(number);
        // When calls arrive is a little unpredictable: it goes into the random generator.
        self.entropy.stir(sys::counter() as u32);
        // An app the lifecycle froze: a thread that called before it was
        // suspended makes the call again when the app thaws (mod.rs `defer_call`).
        if self.apps[i].as_ref().is_some_and(|a| a.frozen.is_none() && a.life.state == super::lifecycle::State::Frozen) {
            // (FaultIP is 4 bytes before the instruction after the svc: in Thumb, 2 before the svc.)
            let mut again = saved;
            again[8] = saved[8].wrapping_sub(if signal::thumb(mrs[MR_CPSR]) { 2 } else { 4 });
            return self.defer_call(i, slot, &again);
        }
        // A vfork child runs as its own process (descriptors, pid, actions) while it calls.
        let swapped = self.vfork_swap(i, slot);
        if let Some(t) = self.apps[i].as_mut().and_then(|a| a.threads.get_mut(slot)) {
            // (rt_sigreturn ends every handler: it is no sign a fault was fixed.)
            if signal::fault_reset(number) {
                t.sig.faults = 0;
            }
            t.sig.waiting = signal::WaitFor::None;
            t.sig.held_call = false;
        }
        // A frozen app's thread that reached here before it was suspended waits too.
        let outcome = if self.apps[i].as_ref().map_or(false, |a| a.frozen.is_some()) {
            Block(Wait::Frozen, None)
        } else if self.family_frozen(i) {
            // Violet Shield froze another process of its owner (a fork):
            // the call is made again once none is frozen (`signal_pass`).
            self.apps[i].as_mut().unwrap().threads[slot].sig.held_call = true;
            Block(Wait::Frozen, None)
        } else if self.quarantine_blocks(i, number, mrs[0] as i32) {
            // Violet Shield: a quarantined app cannot send, on old connections either.
            Done(EACCES)
        } else {
            let o = self.dispatch(i, slot, number, mrs);
            #[cfg(target_arch = "aarch64")]
            let o = self.after64(i, o);
            // Violet Shield: UDP bytes count toward the exfiltration limit too.
            if let Done(r) = o { self.shield_after(i, number, mrs[0] as i32, r); }
            self.note_cloexec(i, number, mrs, o)
        };
        self.finish(i, slot, mrs, saved, outcome);
        if swapped {
            self.vfork_swap(i, slot);
        }
    }

    /// Answer a system call: the result, or a signal handler first (signals,
    /// below), or wait.
    fn finish(&mut self, i: usize, slot: usize, mrs: &[u32; 16], mut saved: [u32; 9], outcome: Outcome) {
        let Some(app) = self.apps[i].as_ref() else { return };
        let vfork_child = app.threads.get(slot).is_some_and(|t| t.sig.vfork.is_some());
        let outcome = match outcome {
            // A vfork child's descriptors are only its own while it calls: it may not wait on one.
            Block(w, _) if vfork_child && !matches!(w, Wait::Sleep | Wait::Futex { .. } | Wait::Signal | Wait::Frozen) => Done(EAGAIN),
            o => o,
        };
        match outcome {
            Done(r) => {
                saved[0] = r as u32;
                if !self.deliverable(i, slot) {
                    return sys::reply(&saved);
                }
                let ctx = self.call_context(i, slot, mrs, r as u32, saved[8]);
                self.resume(i, slot, ctx);
            }
            Block(wait, deadline) => {
                if self.deliverable(i, slot) && self.interruptible(i, slot, &wait) {
                    // A signal is already here: the call is interrupted before it waits.
                    let ctx = self.call_context(i, slot, mrs, mrs[0], saved[8]);
                    if let Some(ctx) = self.interrupted(i, slot, ctx) {
                        return self.resume(i, slot, ctx);
                    }
                    if self.apps[i].as_ref().map_or(true, |a| !a.threads[slot].alive) {
                        return;
                    }
                }
                let reply = self.k.slot();
                if sys::save_caller(reply).is_err() || (sys::mcs() && self.k.refill_reply().is_err()) {
                    self.k.release(reply);
                    saved[0] = EAGAIN as u32;
                    sys::reply(&saved);
                    return;
                }
                let t = &mut self.apps[i].as_mut().unwrap().threads[slot];
                t.saved = saved;
                t.reply = Some(reply);
                t.wait = Some(wait);
                t.deadline = deadline;
            }
            Gone => {}
        }
    }

    // ---- app memory helpers ----
    fn rd(&mut self, i: usize, va: Va, len: usize) -> Option<Vec<u8>> {
        let app = self.apps[i].as_mut()?;
        // `len` is the app's word: checked against its regions before any
        // of the runtime's heap is spent on it (vma.rs `buffer`).
        let mut v = super::vma::buffer(&app.space.vmas, va, len)?;
        app.space.read(&mut self.k, &mut self.pool, va, &mut v).then_some(v)
    }
    fn wr(&mut self, i: usize, va: Va, data: &[u8]) -> bool {
        let app = self.apps[i].as_mut().unwrap();
        app.space.write(&mut self.k, &mut self.pool, va, data)
    }
    fn rd32(&mut self, i: usize, va: Va) -> Option<u32> {
        let app = self.apps[i].as_mut()?;
        app.space.read_u32(&mut self.k, &mut self.pool, va)
    }
    /// A pointer-sized word of the app's ABI (8 bytes for an AArch64 app,
    /// vma::WIDE), whole.
    fn rdword(&mut self, i: usize, va: Va) -> Option<Va> {
        if !super::vma::WIDE {
            return self.rd32(i, va).map(Va::from);
        }
        let b = self.rd(i, va, 8)?;
        Some(u64::from_le_bytes(b[..8].try_into().unwrap()))
    }
    fn wr32(&mut self, i: usize, va: Va, v: u32) -> bool {
        self.wr(i, va, &v.to_le_bytes())
    }
    fn rstr(&mut self, i: usize, va: Va) -> Option<String> {
        let app = self.apps[i].as_mut()?;
        app.space.read_string(&mut self.k, &mut self.pool, va, 4096)
    }
    fn file(&self, i: usize, fd: i32) -> Option<FileRef> {
        self.apps[i].as_ref()?.fds.get(fd as usize)?.clone()
    }
    fn install(&mut self, i: usize, f: OpenFile, from: usize) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        let file = Rc::new(RefCell::new(f));
        for (n, slot) in app.fds.iter_mut().enumerate().skip(from) {
            if slot.is_none() {
                *slot = Some(file);
                return n as i32;
            }
        }
        if app.fds.len() >= 1024 {
            return EMFILE;
        }
        while app.fds.len() < from {
            app.fds.push(None);
        }
        app.fds.push(Some(file));
        app.fds.len() as i32 - 1
    }
    /// An absolute path for `path` relative to `dirfd` (or the working directory).
    fn at(&mut self, i: usize, dirfd: i32, path_ptr: Va) -> Result<String, i32> {
        let path = self.rstr(i, path_ptr).ok_or(EFAULT)?;
        let base = if path.starts_with('/') || dirfd == AT_FDCWD {
            self.apps[i].as_ref().unwrap().cwd.clone()
        } else {
            let f = self.file(i, dirfd).ok_or(EBADF)?;
            let f = f.borrow();
            match &f.kind {
                Kind::Dir { path, .. } | Kind::File { path } => path.clone(),
                _ => return Err(ENOTDIR),
            }
        };
        let path = fs::normalize(&base, &path);
        // The passcode record is the runtime's alone (trusted_ui.rs).
        if super::trusted_ui::private_path(&path) {
            return Err(ENOENT);
        }
        Ok(path)
    }
    fn timespec(&mut self, i: usize, ptr: Va, time64: bool) -> Option<u64> {
        if time64 {
            let b = self.rd(i, ptr, 12)?;
            let sec = i64::from_le_bytes(b[..8].try_into().unwrap());
            let nsec = u32::from_le_bytes(b[8..12].try_into().unwrap());
            Some((sec.max(0) as u64) * 1_000_000_000 + u64::from(nsec))
        } else {
            let b = self.rd(i, ptr, 8)?;
            let sec = i32::from_le_bytes(b[..4].try_into().unwrap());
            let nsec = u32::from_le_bytes(b[4..8].try_into().unwrap());
            Some((sec.max(0) as u64) * 1_000_000_000 + u64::from(nsec))
        }
    }
    fn put_timespec(&mut self, i: usize, ptr: Va, ns: u64, time64: bool) -> bool {
        let (sec, nsec) = (ns / 1_000_000_000, (ns % 1_000_000_000) as u32);
        if time64 {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&(sec as i64).to_le_bytes());
            b[8..12].copy_from_slice(&nsec.to_le_bytes());
            self.wr(i, ptr, &b)
        } else {
            let mut b = [0u8; 8];
            b[..4].copy_from_slice(&(sec as i32).to_le_bytes());
            b[4..].copy_from_slice(&nsec.to_le_bytes());
            self.wr(i, ptr, &b)
        }
    }
    pub fn clock(&self, id: u32) -> u64 {
        let mono = sys::now_ns();
        match id {
            // The build time until a network sets the clock (timesync.rs).
            0 | 5 | 8 | 11 => super::timesync::wall_ns(mono),
            _ => mono,
        }
    }

    fn dispatch(&mut self, i: usize, slot: usize, n: u32, m: &[u32; 16]) -> Outcome {
        // `a`: the arguments as 32-bit words (ints, flags, counts); `v`: as
        // whole addresses and lengths of app memory (`vargs`).
        let a = [m[0], m[1], m[2], m[3], m[4], m[5], m[6]];
        let v = vargs(m);
        let tid = self.apps[i].as_ref().unwrap().threads[slot].tid;
        match n {
            // ---- process and threads ----
            1 | 248 if self.apps[i].as_ref().unwrap().threads[slot].sig.vfork.is_some() => {
                self.vfork_exit(i, slot, signal::exited(a[0] as i32))
            }
            1 => self.exit_thread(i, slot),
            248 => {
                let app = self.apps[i].as_mut().unwrap();
                app.exit_code = Some(a[0] as i32 & 0xff);
                self.reap(i);
                Gone
            }
            // ---- processes (signals and processes, below) ----
            2 => self.fork(i, slot, signal::SIGCHLD, 0, 0, 0, m),
            190 => self.vfork(i, slot, signal::SIGCHLD, 0, m),
            11 | 387 => {
                let (dirfd, path, argv, envp) = if n == 11 { (AT_FDCWD, v[0], v[1], v[2]) } else { (a[0] as i32, v[1], v[2], v[3]) };
                // execveat(fd, "", ..., AT_EMPTY_PATH) runs the open file itself.
                let p = if n == 387 && a[4] & 0x1000 != 0 && self.rstr(i, path).is_some_and(|s| s.is_empty()) {
                    match self.file(i, dirfd).map(|f| match &f.borrow().kind { Kind::File { path } => Some(path.clone()), _ => None }) {
                        Some(Some(p)) => p,
                        _ => return Done(EBADF),
                    }
                } else {
                    match self.at(i, dirfd, path) { Ok(p) => p, Err(e) => return Done(e) }
                };
                self.execve(i, slot, p, argv, envp)
            }
            114 => self.wait4(i, slot, a[0] as i32, v[1], a[2], v[3], None),
            280 => {
                // waitid(idtype, id, infop, options, rusage): P_ALL, P_PID, P_PGID.
                let pid = match a[0] { 0 => -1, 1 => a[1] as i32, 2 => -(a[1] as i32), _ => return Done(EINVAL) };
                if a[3] & 4 == 0 { return Done(EINVAL); } // only WEXITED is reported
                self.wait4(i, slot, pid, 0, a[3], v[4], Some(v[2]))
            }
            120 => self.clone(i, slot, a, v, m),
            435 => Done(ENOSYS), // clone3: callers fall back to clone
            224 => Done(tid as i32),
            20 => Done(self.apps[i].as_ref().unwrap().pid as i32),
            64 => Done(match self.apps[i].as_ref().unwrap().sig.ppid { 0 => 1, p => p as i32 }),
            65 | 132 | 147 | 66 => Done(self.apps[i].as_ref().unwrap().pid as i32),
            57 => Done(0),
            24 | 47 | 49 | 50 | 199 | 200 | 201 | 202 => Done(0),
            205 | 80 => Done(0),
            256 => {
                self.apps[i].as_mut().unwrap().threads[slot].clear_tid = v[0];
                Done(tid as i32)
            }
            338 | 172 | 97 | 96 | 241 | 154 | 156 => Done(0),
            155 | 157 => Done(0),
            159 | 160 => Done(0),
            398 | 389 | 424 | 434 => Done(ENOSYS),
            242 => {
                // One CPU.
                let len = a[1].min(128);
                let mut mask = alloc::vec![0u8; len as usize];
                if len == 0 {
                    return Done(EINVAL);
                }
                mask[0] = 1;
                if !self.wr(i, v[2], &mask) {
                    return Done(EFAULT);
                }
                Done(len.min(4) as i32)
            }
            345 => {
                if v[0] != 0 { self.wr32(i, v[0], 0); }
                if v[1] != 0 { self.wr32(i, v[1], 0); }
                Done(0)
            }
            // ---- signals (signals and processes, below) ----
            37 => self.kill(i, a[0] as i32, None, a[1], None),
            238 => self.kill(i, 0, Some(a[0]), a[1], None),
            268 => self.kill(i, a[0] as i32, Some(a[1]), a[2], None),
            178 | 363 => {
                // rt_sigqueueinfo(tgid, sig, info), rt_tgsigqueueinfo(tgid, tid, sig, info)
                let (tid, sig, ptr) = if n == 178 { (None, a[1], v[2]) } else { (Some(a[1]), a[2], v[3]) };
                let Some(b) = self.rd(i, ptr, 24) else { return Done(EFAULT) };
                let w = |k: usize| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
                let info = Info { signo: sig, code: w(2) as i32, a: w(3), b: w(4), c: w(5) };
                self.kill(i, a[0] as i32, tid, sig, Some(info))
            }
            174 => self.sigaction(i, a[0], v[1], v[2], a[3]),
            175 | 126 => {
                let wide = n == 175;
                if wide && a[3] != 8 {
                    return Done(EINVAL);
                }
                let size = if wide { 8 } else { 4 };
                let set = if v[1] != 0 {
                    let Some(b) = self.rd(i, v[1], size) else { return Done(EFAULT) };
                    Some(b.iter().rev().fold(0u64, |v, x| v << 8 | u64::from(*x)))
                } else {
                    None
                };
                let old = self.apps[i].as_ref().unwrap().threads[slot].sig.blocked;
                if v[2] != 0 && !self.wr(i, v[2], &old.to_le_bytes()[..size]) {
                    return Done(EFAULT);
                }
                if let Some(set) = set {
                    // The old call names the first 32 signals only.
                    let (how, set) = if wide { (a[0], set) } else if a[0] == 2 { (2, set | (old & !0xffff_ffff)) } else { (a[0], set) };
                    let Some(new) = signal::mask_change(old, how, set) else { return Done(EINVAL) };
                    self.apps[i].as_mut().unwrap().threads[slot].sig.blocked = new;
                }
                Done(0)
            }
            176 | 73 => {
                let app = self.apps[i].as_ref().unwrap();
                let t = &app.threads[slot].sig;
                let set = (signal::set_of(&t.pending) | signal::set_of(&app.sig.pending)) & t.blocked;
                let size = if n == 176 { (a[1] as usize).min(8) } else { 4 };
                Done(if self.wr(i, v[0], &set.to_le_bytes()[..size]) { 0 } else { EFAULT })
            }
            186 => self.sigaltstack(i, slot, v[0], v[1], call_sp(m)),
            179 | 29 => {
                // rt_sigsuspend(mask, size) and pause: until a handler has run; then EINTR.
                let mask = if n == 179 {
                    if a[1] != 8 {
                        return Done(EINVAL);
                    }
                    let Some(b) = self.rd(i, v[0], 8) else { return Done(EFAULT) };
                    Some(u64::from_le_bytes(b[..8].try_into().unwrap()))
                } else {
                    None
                };
                let t = &mut self.apps[i].as_mut().unwrap().threads[slot].sig;
                if let Some(mask) = mask {
                    t.suspend_mask = Some(t.blocked);
                    t.blocked = mask & !signal::UNBLOCKABLE;
                }
                t.waiting = signal::WaitFor::Suspend;
                Block(Wait::Signal, None)
            }
            177 | 421 => self.sigtimedwait(i, slot, a, v, n == 421),
            #[cfg(target_arch = "aarch64")]
            173 => self.sigreturn64(i, slot),
            #[cfg(not(target_arch = "aarch64"))]
            173 => self.sigreturn(i, slot, m),
            119 => Done(ENOSYS), // the old sigreturn: the signal gate always uses rt_sigreturn
            // ---- timers ----
            104 | 105 => self.itimer(i, n == 104, a[0], v[1], v[2]),
            257 => self.timer_create(i, a[0], v[1], v[2]),
            258 | 409 => self.timer_settime(i, a[0], a[1], v[2], v[3], n == 409),
            259 | 408 => self.timer_gettime(i, a[0], v[1], n == 408),
            260 => match self.apps[i].as_ref().unwrap().sig.timers.iter().find(|t| t.id == a[0]) {
                Some(t) => Done(t.overrun.min(i32::MAX as u32) as i32),
                None => Done(EINVAL),
            },
            261 => {
                let timers = &mut self.apps[i].as_mut().unwrap().sig.timers;
                let before = timers.len();
                timers.retain(|t| t.id != a[0] || t.id == signal::ITIMER);
                Done(if timers.len() < before { 0 } else { EINVAL })
            }
            // ---- memory ----
            45 => {
                let app = self.apps[i].as_mut().unwrap();
                let brk = app.space.set_brk(&mut self.k, &mut self.pool, v[0]);
                address_answer(brk)
            }
            192 => self.mmap(i, v[0], v[1], a[2], a[3], a[4] as i32, a[5].wrapping_mul(4096)),
            91 => {
                if v[0] & 0xfff != 0 || v[1] == 0 {
                    return Done(EINVAL);
                }
                let app = self.apps[i].as_mut().unwrap();
                let top = app.space.layout.user_top;
                let end = round_up(v[1], 4096).map_or(top, |l| v[0].saturating_add(l).min(top));
                // The trampoline page is the runtime's: it is never unmapped.
                if app.space.pinned(v[0], end) {
                    return Done(EINVAL);
                }
                app.space.remove(&mut self.k, &mut self.pool, v[0], end);
                Done(0)
            }
            125 => {
                if a[2] & 6 == 6 {
                    match self.gate(i, &Action::MapWriteExec { addr: telemetry(v[0]), len: telemetry(v[1]) }) {
                        super::Gate::Allow => {}
                        super::Gate::Deny => return Done(EACCES),
                        super::Gate::Frozen => return Block(Wait::Frozen, None),
                        super::Gate::Killed => return Gone,
                    }
                }
                // The W^X policy (vma.rs) answers EACCES: no writable and
                // executable memory, and no data turned into code.
                let Some(end) = round_up(v[1], 4096).and_then(|l| v[0].checked_add(l)) else { return Done(ENOMEM) };
                let app = self.apps[i].as_mut().unwrap();
                match app.space.protect(v[0], end, a[2] & 7) {
                    Ok(()) => Done(0),
                    Err(e) => Done(e),
                }
            }
            163 => Done(ENOMEM), // mremap: callers copy instead
            220 | 144 | 150 | 151 | 152 | 153 | 390 => Done(0),
            219 => Done(ENOSYS),
            0xf0002 => {
                let app = self.apps[i].as_mut().unwrap();
                app.space.sync_code(v[0], v[1]);
                Done(0)
            }
            // The fork gate's own call (fork, above): nothing without a fork under way.
            signal::FORK_NR => self.fork_done(i, slot, m),
            0xf0005 => {
                let tcb = self.apps[i].as_ref().unwrap().threads[slot].tcb;
                Done(if sys::tcb_set_tls(tcb, v[0]).is_ok() { 0 } else { EINVAL })
            }
            // ---- time ----
            263 | 403 => {
                let time64 = n == 403;
                let now = self.clock(a[0]);
                Done(if self.put_timespec(i, v[1], now, time64) { 0 } else { EFAULT })
            }
            264 | 406 => {
                if v[1] != 0 {
                    self.put_timespec(i, v[1], 1, n == 406);
                }
                Done(0)
            }
            78 => {
                let now = self.clock(0);
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&((now / 1_000_000_000) as u32).to_le_bytes());
                b[4..].copy_from_slice(&(((now % 1_000_000_000) / 1000) as u32).to_le_bytes());
                if v[0] != 0 {
                    self.wr(i, v[0], &b);
                }
                Done(0)
            }
            13 => Done((self.clock(0) / 1_000_000_000) as i32),
            162 => {
                let Some(d) = self.timespec(i, v[0], false) else { return Done(EFAULT) };
                Block(Wait::Sleep, Some(sys::now_ns() + d))
            }
            265 | 407 => {
                let time64 = n == 407;
                let Some(t) = self.timespec(i, v[2], time64) else { return Done(EFAULT) };
                let deadline = if a[1] & 1 != 0 {
                    // Absolute: convert the wall clock to the monotonic one.
                    let now = self.clock(a[0]);
                    sys::now_ns() + t.saturating_sub(now)
                } else {
                    sys::now_ns() + t
                };
                Block(Wait::Sleep, Some(deadline))
            }
            158 => Done(0),
            77 => {
                self.wr(i, v[1], &[0u8; 72]);
                Done(0)
            }
            116 => {
                let mut b = [0u8; 64];
                let up = (sys::now_ns() / 1_000_000_000) as u32;
                b[..4].copy_from_slice(&up.to_le_bytes());
                let total = (self.pool.total * sys::PAGE) as u32;
                let free = (self.pool.available() * sys::PAGE) as u32;
                b[16..20].copy_from_slice(&total.to_le_bytes());
                b[20..24].copy_from_slice(&free.to_le_bytes());
                b[40..42].copy_from_slice(&1u16.to_le_bytes());
                b[52..56].copy_from_slice(&1u32.to_le_bytes());
                self.wr(i, v[0], &b);
                Done(0)
            }
            // ---- futexes ----
            240 | 422 => self.futex(i, a, v, n == 422),
            // ---- files ----
            5 => self.open(i, AT_FDCWD, v[0], a[1]),
            322 | 437 => {
                let flags = if n == 437 { self.rd32(i, v[2]).unwrap_or(0) } else { a[2] };
                self.open(i, a[0] as i32, v[1], flags)
            }
            8 => self.open(i, AT_FDCWD, v[0], O_CREAT | O_TRUNC | 1),
            6 => {
                let app = self.apps[i].as_mut().unwrap();
                match app.fds.get_mut(a[0] as usize) {
                    Some(f @ Some(_)) => {
                        *f = None;
                        Done(0)
                    }
                    _ => Done(EBADF),
                }
            }
            436 => {
                let app = self.apps[i].as_mut().unwrap();
                for fd in a[0]..=a[1].min(app.fds.len() as u32) {
                    if let Some(f) = app.fds.get_mut(fd as usize) {
                        *f = None;
                    }
                }
                Done(0)
            }
            3 => self.read(i, a[0] as i32, v[1], a[2]),
            4 => {
                let r = self.write(i, a[0] as i32, v[1], a[2], None);
                let blocking_audio = r == EAGAIN && self.file(i, a[0] as i32).map_or(false, |f| {
                    let f = f.borrow();
                    matches!(f.kind, Kind::Audio { .. }) && f.flags & O_NONBLOCK == 0
                });
                if blocking_audio {
                    return Block(Wait::Audio { fd: a[0] as i32, buf: v[1], len: a[2] }, None);
                }
                let blocking_socket = r == EAGAIN && self.file(i, a[0] as i32).map_or(false, |f| {
                    let f = f.borrow();
                    matches!(f.kind, Kind::Tcp { .. } | Kind::Hci { .. } | Kind::Pipe { write: true, .. }) && f.flags & O_NONBLOCK == 0
                });
                if blocking_socket {
                    return Block(Wait::Send { fd: a[0] as i32, buf: v[1], len: a[2] }, None);
                }
                // A present the panel is still reading answers once it is copied.
                let screen = self.file(i, a[0] as i32).map_or(false, |f| matches!(f.borrow().kind, Kind::Screen));
                if screen && r > 0 && self.present_pending(i) {
                    return Block(Wait::Present { result: r }, None);
                }
                Done(r)
            }
            145 | 146 => {
                // The walk over the buffers, with its limits: iov.rs.
                struct Io<'a> {
                    rt: &'a mut Runtime,
                    i: usize,
                    fd: i32,
                    write: bool,
                    vec: Va,
                }
                impl super::iov::Vectored for Io<'_> {
                    fn entry(&mut self, k: u32) -> Option<(Va, u32)> {
                        // struct iovec: two words of the app's ABI. A length
                        // of 4 GiB or more is EFAULT, as before any copy.
                        let w: Va = if super::vma::WIDE { 8 } else { 4 };
                        let at = self.vec.checked_add(2 * w * Va::from(k))?;
                        Some((self.rt.rdword(self.i, at)?, u32::try_from(self.rt.rdword(self.i, at.checked_add(w)?)?).ok()?))
                    }
                    fn io(&mut self, base: Va, len: u32) -> i32 {
                        if self.write {
                            self.rt.write(self.i, self.fd, base, len, None)
                        } else {
                            self.rt.read_now(self.i, self.fd, base, len)
                        }
                    }
                }
                let fd = a[0] as i32;
                let (base, len) = match super::iov::run(&mut Io { rt: self, i, fd, write: n == 146, vec: v[1] }, a[2], super::device::RW_SLICE) {
                    super::iov::Outcome::Done(r) => return Done(r),
                    super::iov::Outcome::Wait { base, len } => (base, len),
                };
                // Nothing moved on a blocking socket or pipe: wait, as write and
                // read do, with this buffer (a short count is fine for writev).
                // rustls writes TLS records with writev, and a full send ring
                // used to come back as EAGAIN on a blocking socket.
                let blocking = self.file(i, fd).map_or(false, |f| {
                    let f = f.borrow();
                    let tcp = matches!(f.kind, Kind::Tcp { .. });
                    (tcp || matches!(f.kind, Kind::Pipe { .. })) && f.flags & O_NONBLOCK == 0
                });
                if !blocking {
                    Done(EAGAIN)
                } else if n == 146 {
                    Block(Wait::Send { fd, buf: base, len }, None)
                } else {
                    Block(Wait::Read { fd, buf: base, len }, self.recv_deadline(i, fd))
                }
            }
            180 | 181 => {
                let offset = u64::from(a[4]) | u64::from(a[5]) << 32;
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                // A socket has no offset, as on Linux (and pwrite is not a way around send).
                if matches!(f.borrow().kind, Kind::Tcp { .. } | Kind::Udp { .. }) { return Done(ESPIPE); }
                let saved = f.borrow().offset;
                f.borrow_mut().offset = offset;
                let r = if n == 180 {
                    self.read_now(i, a[0] as i32, v[1], a[2])
                } else {
                    self.write(i, a[0] as i32, v[1], a[2], Some(offset))
                };
                f.borrow_mut().offset = saved;
                Done(r)
            }
            19 | 140 => {
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                let (off, whence) = if n == 19 {
                    (a[1] as i32 as i64, a[2])
                } else {
                    ((u64::from(a[1]) << 32 | u64::from(a[2])) as i64, a[4])
                };
                let size = {
                    let f = f.borrow();
                    match &f.kind {
                        Kind::File { path } => self.fs.contents(path).map_or(0, |c| c.len()) as i64,
                        Kind::Dir { .. } => 0,
                        _ => return Done(ESPIPE),
                    }
                };
                let mut f = f.borrow_mut();
                let new = match whence {
                    0 => off,
                    1 => f.offset as i64 + off,
                    2 => size + off,
                    _ => return Done(EINVAL),
                };
                if new < 0 {
                    return Done(EINVAL);
                }
                f.offset = new as u64;
                drop(f);
                if n == 140 {
                    let ok = self.wr(i, v[3], &(new as u64).to_le_bytes());
                    return Done(if ok { 0 } else { EFAULT });
                }
                Done(new.min(i32::MAX as i64) as i32)
            }
            41 | 63 | 358 => {
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                if n == 41 {
                    let app = self.apps[i].as_mut().unwrap();
                    let pos = app.fds.iter().position(|s| s.is_none()).unwrap_or(app.fds.len());
                    if pos == app.fds.len() {
                        app.fds.push(None);
                    }
                    app.fds[pos] = Some(f);
                    return Done(pos as i32);
                }
                let target = a[1] as usize;
                if target >= 1024 {
                    return Done(EBADF);
                }
                let app = self.apps[i].as_mut().unwrap();
                while app.fds.len() <= target {
                    app.fds.push(None);
                }
                app.fds[target] = Some(f);
                Done(target as i32)
            }
            55 | 221 => self.fcntl(i, a[0] as i32, a[1], a[2], v[2]),
            54 => self.ioctl(i, a[0] as i32, a[1], a[2], v[2]),
            195 | 196 => {
                let p = match self.at(i, AT_FDCWD, v[0]) { Ok(p) => p, Err(e) => return Done(e) };
                Done(self.stat_path(i, &p, v[1]))
            }
            197 => Done(self.fstat(i, a[0] as i32, v[1])),
            327 => {
                if a[3] & 0x1000 != 0 && self.rstr(i, v[1]).map_or(false, |s| s.is_empty()) {
                    return Done(self.fstat(i, a[0] as i32, v[2]));
                }
                let p = match self.at(i, a[0] as i32, v[1]) { Ok(p) => p, Err(e) => return Done(e) };
                Done(self.stat_path(i, &p, v[2]))
            }
            397 => Done(ENOSYS), // statx: C libraries fall back to fstatat64
            266 | 267 => {
                // statfs64: report a small in-memory file system.
                let mut b = [0u8; 84];
                b[..4].copy_from_slice(&0x01021994u32.to_le_bytes());
                b[4..8].copy_from_slice(&4096u32.to_le_bytes());
                self.wr(i, v[2], &b);
                Done(0)
            }
            33 | 334 | 439 => {
                let (dirfd, ptr) = if n == 33 { (AT_FDCWD, v[0]) } else { (a[0] as i32, v[1]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                if let Err(e) = self.path_allowed(i, &p, false) { return Done(e); } // policy (manifest.rs)
                Done(if self.fs.stat(&p).is_some() { 0 } else { ENOENT })
            }
            217 | 141 => self.getdents(i, a[0] as i32, v[1], a[2]),
            183 => {
                let mut cwd = self.apps[i].as_ref().unwrap().cwd.clone().into_bytes();
                cwd.push(0);
                if cwd.len() > a[1] as usize {
                    return Done(ERANGE);
                }
                let len = cwd.len() as i32;
                Done(if self.wr(i, v[0], &cwd) { len } else { EFAULT })
            }
            12 | 133 => {
                let p = if n == 12 {
                    match self.at(i, AT_FDCWD, v[0]) { Ok(p) => p, Err(e) => return Done(e) }
                } else {
                    let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                    let f = f.borrow();
                    match &f.kind { Kind::Dir { path, .. } => path.clone(), _ => return Done(ENOTDIR) }
                };
                if let Err(e) = self.path_allowed(i, &p, false) { return Done(e); } // policy (manifest.rs)
                match self.fs.stat(&p) {
                    Some(s) if s.mode & S_IFMT == S_IFDIR => {
                        self.apps[i].as_mut().unwrap().cwd = p;
                        Done(0)
                    }
                    Some(_) => Done(ENOTDIR),
                    None => Done(ENOENT),
                }
            }
            39 | 323 => {
                let (dirfd, ptr) = if n == 39 { (AT_FDCWD, v[0]) } else { (a[0] as i32, v[1]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                // Violet Shield. A folder that exists changes nothing and keeps its EEXIST.
                if self.fs.stat(&p).is_none() { if let Some(o) = self.gate_change(i, &p) { return o; } }
                // Policy (manifest.rs): a folder the app can see keeps its EEXIST.
                let seen = self.fs.stat(&p).is_some() && self.path_allowed(i, &p, false).is_ok();
                if !seen { if let Err(e) = self.path_allowed(i, &p, true) { return Done(e); } }
                Done(match self.fs.mkdir(&p) { Ok(()) => 0, Err(e) => e })
            }
            40 | 10 | 328 => {
                let (dirfd, ptr, dir) = match n {
                    40 => (AT_FDCWD, v[0], true),
                    10 => (AT_FDCWD, v[0], false),
                    _ => (a[0] as i32, v[1], a[2] & 0x200 != 0),
                };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                if let Some(o) = self.gate_change(i, &p) { return o; } // Violet Shield
                if let Err(e) = self.path_allowed(i, &p, true) { return Done(e); } // policy (manifest.rs)
                Done(match self.fs.unlink(&p, dir) { Ok(()) => 0, Err(e) => e })
            }
            38 | 329 | 382 => {
                let (fa, fp, ta, tp) = if n == 38 { (AT_FDCWD, v[0], AT_FDCWD, v[1]) } else { (a[0] as i32, v[1], a[2] as i32, v[3]) };
                let from = match self.at(i, fa, fp) { Ok(p) => p, Err(e) => return Done(e) };
                let to = match self.at(i, ta, tp) { Ok(p) => p, Err(e) => return Done(e) };
                // Violet Shield: a rename changes both names and takes the contents along.
                if let Some(o) = self.gate_rename(i, &from, &to) { return o; }
                if let Err(e) = self.path_allowed(i, &from, true).and(self.path_allowed(i, &to, true)) { return Done(e); } // policy
                Done(match self.fs.rename(&from, &to) { Ok(()) => 0, Err(e) => e })
            }
            85 | 332 => {
                let (dirfd, ptr, buf, len) = if n == 85 { (AT_FDCWD, v[0], v[1], a[2]) } else { (a[0] as i32, v[1], v[2], a[3]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                if p == "/proc/self/exe" {
                    let exe = self.apps[i].as_ref().unwrap().path.clone();
                    let bytes = &exe.as_bytes()[..exe.len().min(len as usize)];
                    let count = bytes.len() as i32;
                    return Done(if self.wr(i, buf, bytes) { count } else { EFAULT });
                }
                if let Err(e) = self.path_allowed(i, &p, false) { return Done(e); } // policy (manifest.rs)
                Done(if self.fs.stat(&p).is_some() { EINVAL } else { ENOENT })
            }
            92 | 93 | 193 | 194 => {
                let len = match n {
                    92 | 93 => u64::from(a[1]),
                    _ => u64::from(a[2]) | u64::from(a[3]) << 32,
                };
                let path = if n == 93 || n == 194 {
                    let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                    let f = f.borrow();
                    match &f.kind { Kind::File { path } => path.clone(), _ => return Done(EINVAL) }
                } else {
                    match self.at(i, AT_FDCWD, v[0]) { Ok(p) => p, Err(e) => return Done(e) }
                };
                // Violet Shield: truncate by path never went through open().
                if matches!(n, 92 | 193) { if let Some(o) = self.gate_change(i, &path) { return o; } }
                if matches!(n, 92 | 193) { if let Err(e) = self.path_allowed(i, &path, true) { return Done(e); } } // policy
                // ...and by descriptor only if it was opened for writing (the rules are asked again).
                if matches!(n, 93 | 194) {
                    let flags = self.file(i, a[0] as i32).map_or(0, |f| f.borrow().flags);
                    if let Some(e) = self.fd_write_error(i, &path, flags) { return Done(e); }
                }
                let now = (self.clock(0) / 1_000_000_000) as u32;
                match self.fs.writable(&path, false, false, now) {
                    Some(data) => Done(super::vma::resize_file(data, len, true).err().unwrap_or(0)),
                    None => Done(ENOENT),
                }
            }
            // fsync, fdatasync, sync: /data is written back before they return
            // 0, and they say so when it could not be (data::fsync). fsync
            // waits for the file (or directory) of its descriptor only.
            118 | 148 | 36 => {
                let scope = if n == 36 {
                    None
                } else {
                    let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                    let f = f.borrow();
                    match &f.kind { Kind::File { path } | Kind::Dir { path, .. } => Some(path.clone()), _ => return Done(0) }
                };
                let now = sys::now_ns();
                match super::data::fsync(&mut self.fs, scope.as_deref(), i, now) {
                    // Stored, but the app has used more than its share: answer late.
                    (0, wait) if wait > 0 => Block(Wait::Sleep, Some(now + wait)),
                    (r, _) => Done(r),
                }
            }
            373 | 15 | 94 | 333 | 325 | 95 | 207 | 212 | 269 | 348 | 412 | 143 | 352 => Done(0),
            60 => Done(0o022),
            122 => {
                let mut u = [0u8; 390];
                for (k, field) in ["Linux", "violet", "6.1.0-violetos", "#1 VioletOS open runtime", "armv7l", "(none)"].iter().enumerate() {
                    u[k * 65..k * 65 + field.len()].copy_from_slice(field.as_bytes());
                }
                Done(if self.wr(i, v[0], &u) { 0 } else { EFAULT })
            }
            191 | 369 => {
                let (resource, old) = if n == 191 { (a[0], v[1]) } else { (a[1], v[3]) };
                if old != 0 {
                    let (cur, max): (u64, u64) = match resource {
                        3 => (8 << 20, u64::MAX),
                        7 => (1024, 4096),
                        _ => (u64::MAX, u64::MAX),
                    };
                    if n == 191 {
                        let mut b = [0u8; 8];
                        b[..4].copy_from_slice(&(cur.min(u32::MAX as u64) as u32).to_le_bytes());
                        b[4..].copy_from_slice(&(max.min(u32::MAX as u64) as u32).to_le_bytes());
                        self.wr(i, old, &b);
                    } else {
                        let mut b = [0u8; 16];
                        b[..8].copy_from_slice(&cur.to_le_bytes());
                        b[8..].copy_from_slice(&max.to_le_bytes());
                        self.wr(i, old, &b);
                    }
                }
                Done(0)
            }
            75 => Done(0),
            384 => {
                // getrandom: at most 256 KiB a call (a short count, as Linux
                // gives for large requests), and EIO rather than predictable
                // bytes if the generator was never seeded.
                let len = a[1].min(256 << 10);
                // All of it must be writable before any of it is written.
                if !self.apps[i].as_ref().unwrap().space.writable(v[0], len as usize) {
                    return Done(EFAULT);
                }
                let mut chunk = [0u8; 256];
                let mut done = 0;
                while done < len {
                    let n = (len - done).min(256) as usize;
                    if !self.entropy.fill(&mut chunk[..n]) {
                        return Done(EIO);
                    }
                    if !self.wr(i, v[0] + Va::from(done), &chunk[..n]) {
                        return Done(EFAULT);
                    }
                    done += n as u32;
                }
                Done(len as i32)
            }
            // ---- waiting ----
            168 | 336 | 414 => {
                let timeout = match n {
                    168 => (a[2] as i32 >= 0).then(|| u64::from(a[2]) * 1_000_000),
                    _ => if v[2] == 0 { None } else { self.timespec(i, v[2], n == 414) },
                };
                self.poll(i, v[0], a[1], timeout)
            }
            142 | 335 | 413 => Done(ENOSYS),
            250 | 357 => Done(self.install(i, OpenFile::new(Kind::Epoll { interest: Vec::new(), reported: Vec::new() }, 2), 0)),
            251 => self.epoll_ctl(i, a[0] as i32, a[1], a[2] as i32, v[3]),
            252 | 346 => {
                let max = a[2];
                if max == 0 {
                    return Done(EINVAL);
                }
                let got = self.epoll_collect(i, a[0] as i32, v[1], max);
                if got != 0 || a[3] == 0 {
                    return Done(got);
                }
                let deadline = (a[3] as i32 > 0).then(|| sys::now_ns() + u64::from(a[3]) * 1_000_000);
                Block(Wait::Epoll { epfd: a[0] as i32, events: v[1], max }, deadline)
            }
            351 | 356 => Done(self.install(i, OpenFile::new(Kind::EventFd { count: u64::from(a[0]) }, 2 | a[1] & O_NONBLOCK), 0)),
            42 | 359 => {
                let buffer = Rc::new(RefCell::new(alloc::collections::VecDeque::new()));
                let open = Rc::new(RefCell::new(1u32));
                let flags = if n == 359 { a[1] } else { 0 };
                let r = self.install(i, OpenFile::new(Kind::Pipe { buffer: buffer.clone(), write: false, peer_open: open.clone() }, flags & O_NONBLOCK), 0);
                let w = self.install(i, OpenFile::new(Kind::Pipe { buffer, write: true, peer_open: open }, 1 | flags & O_NONBLOCK), 0);
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&r.to_le_bytes());
                b[4..].copy_from_slice(&w.to_le_bytes());
                Done(if self.wr(i, v[0], &b) { 0 } else { EFAULT })
            }
            350 | 355 | 360 | 385 => Done(ENOSYS),
            // ---- sockets: TCP through Radio, DNS answered here (sockets.rs) ----
            // The net capability (policy.rs) is checked where a socket is made; local ones need none.
            // While the user is asked (trusted_ui.rs), socket() waits for the answer and runs again.
            281 if a[0] != 1 && !self.may(i, super::policy::Cap::Net) => match self.consent_wait(i, super::policy::Cap::Net) {
                Some(w) => Block(w, None),
                None => Done(EACCES),
            },
            281..=297 | 366 => self.socket_call(i, n, a, v),
            374 | 365 => Done(ENOSYS),
            _ => {
                let app = self.apps[i].as_ref().unwrap();
                crate::log!("{}: unsupported system call {}", app.name, n);
                Done(ENOSYS)
            }
        }
    }

    fn exit_thread(&mut self, i: usize, slot: usize) -> Outcome {
        let (clear, tcb) = {
            let t = &self.apps[i].as_ref().unwrap().threads[slot];
            (t.clear_tid, t.tcb)
        };
        if clear != 0 {
            self.wr32(i, clear, 0);
            self.futex_wake(i, clear, 1);
        }
        sys::tcb_suspend(tcb);
        let app = self.apps[i].as_mut().unwrap();
        app.threads[slot].alive = false;
        if !app.threads.iter().any(|t| t.alive) {
            app.exit_code.get_or_insert(0);
            self.reap(i);
        }
        Gone
    }

    fn clone(&mut self, i: usize, slot: usize, a: [u32; 7], v: [Va; 7], m: &[u32; 16]) -> Outcome {
        // The stack, TLS and both tid pointers are app addresses: whole.
        let (flags, stack, ptid, ctid) = (a[0], v[1], v[2], v[4]);
        #[cfg(target_arch = "aarch64")]
        let tls = v[3];
        // A new process: musl's posix_spawn and Go's os/exec use CLONE_VM|CLONE_VFORK;
        // without CLONE_VM it is fork (CLONE_VFORK alone does not make the parent wait).
        if flags & CLONE_VM != 0 && flags & CLONE_VFORK != 0 {
            return self.vfork(i, slot, flags & 0xff, stack, m);
        }
        if flags & CLONE_VM == 0 {
            return self.fork(i, slot, flags & 0xff, flags, ptid, ctid, m);
        }
        #[cfg(target_arch = "aarch64")]
        let child = {
            // The parent's whole context, its call registers as the svc saw
            // them (the 64-bit fault words), x0 = 0 in the child, which
            // resumes after the svc on its own stack; CLONE_SETTLS sets tpidr_el0.
            let parent = self.apps[i].as_ref().unwrap().threads[slot].tcb;
            let Ok(mut c) = sys::tcb_read_context(parent) else { return Done(EAGAIN) };
            let (abi, w) = (super::arch::NATIVE, sys::wide());
            for k in 1..8 {
                c.words[abi.context_arg0 + k] = w[k];
            }
            c.words[abi.context_arg0] = 0;
            c.words[abi.context_pc] = w[MR_FAULT_IP] + 4;
            c.words[abi.context_sp] = if stack != 0 { stack } else { w[sys::MR_SP] };
            // (x30 is the parent's, from its context: message word 10 is ELR_EL1 here.)
            if flags & CLONE_SETTLS != 0 {
                c.words[abi.context_tls] = tls;
            }
            let tid = self.next_tid();
            match self.spawn64(i, tid, c) {
                Ok(c) => (c, tid),
                Err(_) => return Done(EAGAIN),
            }
        };
        #[cfg(not(target_arch = "aarch64"))]
        let child = {
        let parent = self.apps[i].as_ref().unwrap().threads[slot].tcb;
        let Ok(mut regs) = sys::tcb_read(parent) else { return Done(EAGAIN) };
        // The fault message holds r0-r7 as the svc saw them.
        regs.r0 = 0;
        regs.r1 = m[1];
        regs.r2 = m[2];
        regs.r3 = m[3];
        regs.r4 = m[4];
        regs.r5 = m[5];
        regs.r6 = m[6];
        regs.r7 = m[7];
        regs.r14 = m[sys::MR_LR];
        // (AArch32: the 32-bit words are the whole registers.)
        regs.sp = if stack != 0 { a[1] } else { m[sys::MR_SP] };
        if flags & CLONE_SETTLS != 0 {
            regs.tpidruro = a[3];
        }
        let thumb = (m[MR_CPSR] >> 5) & 1;
        let target = m[MR_FAULT_IP].wrapping_add(4) | thumb;
        let tid = self.next_tid();
        match self.spawn(i, tid, regs, target) {
            Ok(c) => (c, tid),
            Err(_) => return Done(EAGAIN),
        }
        };
        let (child, tid) = child;
        if flags & CLONE_PARENT_SETTID != 0 {
            self.wr32(i, ptid, tid);
        }
        if flags & CLONE_CHILD_SETTID != 0 {
            self.wr32(i, ctid, tid);
        }
        if flags & CLONE_CHILD_CLEARTID != 0 {
            self.apps[i].as_mut().unwrap().threads[child].clear_tid = ctid;
        }
        // A new thread starts with its creator's signal mask.
        let app = self.apps[i].as_mut().unwrap();
        app.threads[child].sig.blocked = app.threads[slot].sig.blocked;
        Done(tid as i32)
    }

    fn futex(&mut self, i: usize, a: [u32; 7], v: [Va; 7], time64: bool) -> Outcome {
        // uaddr, uaddr2 and (for a wait) the timeout are addresses: whole.
        let (addr, op, val, timeout) = (v[0], a[1] & 0x7f, a[2], v[3]);
        match op {
            0 | 9 => {
                let Some(current) = self.rd32(i, addr) else { return Done(EFAULT) };
                if current != val {
                    return Done(EAGAIN);
                }
                let deadline = if timeout == 0 {
                    None
                } else {
                    let Some(t) = self.timespec(i, timeout, time64) else { return Done(EFAULT) };
                    Some(if op == 9 {
                        let clock = if a[1] & 256 != 0 { 0 } else { 1 };
                        sys::now_ns() + t.saturating_sub(self.clock(clock))
                    } else {
                        sys::now_ns() + t
                    })
                };
                Block(Wait::Futex { app: i, addr }, deadline)
            }
            1 | 10 => Done(self.futex_wake(i, addr, val as usize) as i32),
            // Requeue: waking every waiter is a permitted spurious wakeup.
            3 | 4 => {
                let woken = self.futex_wake(i, addr, usize::MAX) + self.futex_wake(i, v[4], usize::MAX);
                Done(woken.min(val as usize) as i32)
            }
            5 => Done(self.futex_wake(i, addr, val as usize) as i32 + self.futex_wake(i, v[4], a[3] as usize) as i32),
            _ => Done(ENOSYS),
        }
    }

    pub fn futex_wake(&mut self, i: usize, addr: Va, count: usize) -> usize {
        let waiting: Vec<usize> = self.apps[i]
            .as_ref()
            .map(|app| {
                app.threads
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| matches!(t.wait, Some(Wait::Futex { addr: w, .. }) if w == addr))
                    .map(|(s, _)| s)
                    .take(count)
                    .collect()
            })
            .unwrap_or_default();
        for &s in &waiting {
            self.wake(i, s, 0);
        }
        waiting.len()
    }

    fn next_tid(&mut self) -> u32 {
        let t = self.next_pid_bump();
        t
    }

    fn open(&mut self, i: usize, dirfd: i32, path_ptr: Va, flags: u32) -> Outcome {
        let path = match self.at(i, dirfd, path_ptr) {
            Ok(p) => p,
            Err(e) => return Done(e),
        };
        let mode: AccessMode = super::shield::open_mode(flags, self.fs.stat(&path).is_some());
        // Violet Shield sees the same private copy of the path that is opened below,
        // and judges the open by what it does (O_TRUNC and O_CREAT write; O_RDWR also reads).
        match self.gate_open(i, &path, flags) {
            super::Gate::Allow => {}
            super::Gate::Deny => return Done(EACCES),
            super::Gate::Frozen => return Block(Wait::Frozen, None),
            super::Gate::Killed => return Gone,
        }
        // The capability policy (manifest.rs): after the Shield, so its
        // telemetry still sees the attempt; files by namespace, devices by capability.
        if let Err(e) = self.open_allowed(i, &path, mode == AccessMode::Write) {
            // The user is being asked (trusted_ui.rs): wait for the answer, then open again.
            let asking = super::policy::device_cap(&path).ok().flatten().and_then(|c| self.consent_wait(i, c));
            return match asking {
                Some(_) if flags & O_NONBLOCK != 0 => Done(EAGAIN),
                Some(w) if e == EACCES => Block(w, None),
                _ => Done(e),
            };
        }
        let kind = match path.as_str() {
            "/dev/violet/screen" => {
                if let Err(e) = self.surface(i) {
                    return Done(e);
                }
                Kind::Screen
            }
            "/dev/violet/events" => Kind::Events,
            "/dev/violet/sys" => Kind::Sys { pending: Vec::new() },
            "/dev/violet/audio" => {
                if !super::device::audio_present(self.audio) {
                    return Done(ENODEV);
                }
                Kind::Audio { channels: 2, pending: Vec::new() }
            }
            "/dev/violet/camera" => {
                if !super::device::camera::present(self.camera) {
                    return Done(ENODEV);
                }
                // Never under system UI (trusted_ui.rs): the first read after it goes starts it.
                if self.focused && self.foreground == Some(i) && !super::trusted_ui::covers() {
                    super::device::camera::want(true);
                }
                // Only frames that complete after the open.
                Kind::Camera { seen: super::device::camera::frames() }
            }
            "/dev/violet/mic" => {
                if !super::device::audio_present(self.audio) {
                    return Done(ENODEV);
                }
                Kind::Mic { seen: super::device::mic::open() }
            }
            "/dev/violet/hci" => {
                // Shared (hcimux.rs): core apps from the boot image may send any command.
                let pid = self.apps[i].as_ref().map_or(0, |a| a.pid);
                let subject = self.subject(i);
                let system = subject.role & super::shield::ROLE_CORE != 0;
                // Places are counted per app, not per process (hcimux.rs `open`).
                let app = &subject.name[..(subject.name_len as usize).min(subject.name.len())];
                match super::hci::open(pid, app, system) {
                    Some(client) => Kind::Hci { client },
                    None => return Done(EBUSY),
                }
            }
            "/dev/null" => Kind::Null,
            "/dev/urandom" | "/dev/random" => Kind::Random,
            "/dev/tty" | "/dev/stdout" | "/dev/stderr" | "/dev/console" => Kind::Log,
            _ => {
                let now = (self.clock(0) / 1_000_000_000) as u32;
                match self.fs.stat(&path) {
                    Some(s) if flags & O_CREAT != 0 && flags & O_EXCL != 0 => {
                        let _ = s;
                        return Done(EEXIST);
                    }
                    Some(s) if s.mode & S_IFMT == S_IFDIR => {
                        if flags & O_ACCMODE != 0 {
                            return Done(EISDIR);
                        }
                        Kind::Dir { entries: self.visible(i, &path, self.fs.list(&path)), path, pos: 0 }
                    }
                    Some(_) if flags & O_DIRECTORY != 0 => return Done(ENOTDIR),
                    Some(s) if s.mode & S_IFMT == S_IFCHR => Kind::Null,
                    existing => {
                        let wants_write = flags & O_ACCMODE != 0 || flags & O_TRUNC != 0;
                        if existing.is_none() && flags & O_CREAT == 0 {
                            return Done(ENOENT);
                        }
                        if wants_write || existing.is_none() {
                            if self.fs.writable(&path, flags & O_CREAT != 0, flags & O_TRUNC != 0, now).is_none() {
                                return Done(ENOENT);
                            }
                        }
                        Kind::File { path }
                    }
                }
            }
        };
        Done(self.install(i, OpenFile::new(kind, flags), 0))
    }

    fn read(&mut self, i: usize, fd: i32, buf: Va, len: u32) -> Outcome {
        let r = self.read_now(i, fd, buf, len);
        if r != EAGAIN {
            return Done(r);
        }
        let nonblock = self.file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if nonblock {
            return Done(EAGAIN);
        }
        Block(Wait::Read { fd, buf, len }, self.recv_deadline(i, fd))
    }

    /// When a blocking read of fd gives up (its SO_RCVTIMEO), if ever.
    pub fn recv_deadline(&self, i: usize, fd: i32) -> Option<u64> {
        let t = self.file(i, fd)?.borrow().recv_timeout;
        (t > 0).then(|| sys::now_ns() + t)
    }

    pub(crate) fn write(&mut self, i: usize, fd: i32, buf: Va, len: u32, at: Option<u64>) -> i32 {
        let Some(f) = self.file(i, fd) else { return EBADF };
        if matches!(f.borrow().kind, Kind::Audio { .. }) {
            let r = self.write_audio(i, fd, buf, len);
            return if r == 0 { EAGAIN } else { r };
        }
        if matches!(f.borrow().kind, Kind::Tcp { .. } | Kind::Udp { .. }) {
            // Violet Shield: every write to a socket comes through here (write, writev,
            // pwrite64, a send that was waiting), so this is where a quarantine cuts it.
            if self.quarantined(i) { return EACCES; }
            return self.socket_send(i, fd, buf, len, None);
        }
        if matches!(f.borrow().kind, Kind::Hci { .. }) && self.quarantined(i) { return EACCES; }
        // A pipe keeps what its reader has not taken on the runtime heap:
        // bounded per pipe and per writer (lifecycle.rs `pipe_room`), and
        // only what fits is copied. No reader left: EPIPE.
        let len = match &f.borrow().kind {
            Kind::Pipe { buffer, write: true, .. } if len > 0 => {
                if Rc::strong_count(buffer) < 2 {
                    return EPIPE;
                }
                let total = super::pipe_bytes(self.apps[i].as_ref().unwrap());
                match super::lifecycle::pipe_room(buffer.borrow().len(), total, len as usize) {
                    0 => return EAGAIN,
                    n => n as u32,
                }
            }
            // Commands are lines: a batch longer than RW_SLICE is refused
            // whole, never cut inside a line and run as two commands.
            Kind::Sys { .. } if len > super::device::RW_SLICE => return EINVAL,
            // One call moves at most RW_SLICE: the count comes back short and
            // the app calls again, behind whatever other apps have asked for.
            _ => len.min(super::device::RW_SLICE),
        };
        let Some(data) = self.rd(i, buf, len as usize) else {
            // A readable buffer the heap (or the pool) could not take: ENOMEM, not EFAULT.
            let readable = self.apps[i].as_ref().map_or(false, |a| super::vma::permits(&a.space.vmas, buf, len as usize, 7));
            return if readable { ENOMEM } else { EFAULT };
        };
        let mut f = f.borrow_mut();
        let flags = f.flags;
        match &mut f.kind {
            Kind::Log => {
                Log::write(&data);
                len as i32
            }
            Kind::Null | Kind::Random => len as i32,
            Kind::Hci { client } => {
                use super::hci::mux::Refusal;
                if !super::hci::up() {
                    ENETDOWN
                } else {
                    match super::hci::send(*client, &data) {
                        Ok(()) => len as i32,
                        Err(Refusal::Invalid) => EINVAL,
                        Err(Refusal::Denied) => EPERM,
                        Err(Refusal::Full) => EAGAIN,
                    }
                }
            }
            Kind::File { path } => {
                let path = path.clone();
                // Violet Shield: only a descriptor opened for writing writes, and the
                // built-in rules are asked again (they saw only what open() asked for).
                if let Some(e) = self.fd_write_error(i, &path, flags) { return e; }
                let now = (self.clock(0) / 1_000_000_000) as u32;
                let Some(content) = self.fs.writable(&path, false, false, now) else { return EIO };
                let start = if flags & O_APPEND != 0 { content.len() as u64 } else { at.unwrap_or(f.offset) };
                // The offset is the app's: bounded and fallible (vma.rs), never a wrap or an abort.
                if let Err(e) = super::vma::resize_file(content, start.saturating_add(data.len() as u64), false) { return e; }
                let start = start as usize;
                content[start..start + data.len()].copy_from_slice(&data);
                if at.is_none() {
                    f.offset = (start + data.len()) as u64;
                }
                len as i32
            }
            Kind::Screen => {
                if data.len() < 24 {
                    return EINVAL;
                }
                let word = |k: usize| u32::from_le_bytes(data[k * 4..k * 4 + 4].try_into().unwrap());
                let rect = PresentRect { x: word(0), y: word(1), width: word(2), height: word(3), buffer_index: word(4), flags: word(5) };
                drop(f);
                self.present(i, rect);
                len as i32
            }
            Kind::Sys { .. } => {
                let text = String::from_utf8_lossy(&data).into_owned();
                drop(f);
                let mut answer = String::new();
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    answer.push_str(&self.system(i, line));
                }
                if let Some(f) = self.file(i, fd) {
                    if let Kind::Sys { pending } = &mut f.borrow_mut().kind {
                        pending.extend_from_slice(answer.as_bytes());
                    }
                }
                len as i32
            }
            Kind::EventFd { count } => {
                if data.len() < 8 {
                    return EINVAL;
                }
                *count = count.saturating_add(u64::from_le_bytes(data[..8].try_into().unwrap()));
                8
            }
            Kind::Pipe { buffer, write: true, .. } => {
                buffer.borrow_mut().extend(data.iter().copied());
                len as i32
            }
            _ => EBADF,
        }
    }

    fn fcntl(&mut self, i: usize, fd: i32, cmd: u32, arg: u32, _ptr: Va) -> Outcome {
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        match cmd {
            0 | 1030 => Done(self.install_ref(i, f, arg as usize)),
            // F_GETFD, F_SETFD: FD_CLOEXEC (closed at execve).
            1 => Done(self.cloexec(i, fd) as i32),
            2 => {
                self.set_cloexec(i, fd, arg & 1 != 0);
                Done(0)
            }
            3 => Done((f.borrow().flags | 0o400000) as i32),
            4 => {
                let mut f = f.borrow_mut();
                f.flags = (f.flags & !(O_NONBLOCK | O_APPEND)) | (arg & (O_NONBLOCK | O_APPEND));
                Done(0)
            }
            5 | 6 | 7 | 12 | 13 | 14 | 36 | 37 | 38 => Done(0),
            _ => Done(EINVAL),
        }
    }
    fn install_ref(&mut self, i: usize, f: FileRef, from: usize) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        while app.fds.len() <= from {
            app.fds.push(None);
        }
        let pos = (from..app.fds.len()).find(|&k| app.fds[k].is_none()).unwrap_or(app.fds.len());
        if pos == app.fds.len() {
            app.fds.push(None);
        }
        app.fds[pos] = Some(f);
        pos as i32
    }

    /// `arg` is the third argument as a 32-bit word, `ptr` as an address.
    fn ioctl(&mut self, i: usize, fd: i32, cmd: u32, arg: u32, ptr: Va) -> Outcome {
        let _ = arg;
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        let is_screen = matches!(f.borrow().kind, Kind::Screen);
        match cmd {
            0x5601 if is_screen => {
                let info = [WIDTH as u32, HEIGHT as u32, STRIDE as u32, PIXEL_FORMAT_BGRA8888, 2, BUFFER_BYTES as u32];
                let mut b = [0u8; 24];
                for (k, w) in info.iter().enumerate() {
                    b[k * 4..k * 4 + 4].copy_from_slice(&w.to_le_bytes());
                }
                Done(if self.wr(i, ptr, &b) { 0 } else { EFAULT })
            }
            // SCREEN_IOC_WAIT_VSYNC: until the next frame starts; VsyncInfo (16 bytes) to arg if not 0.
            0x5604 if is_screen => {
                let deadline = super::sys::now_ns() + 100_000_000;
                Block(Wait::Vsync { after: self.beam.frame, ptr }, Some(deadline))
            }
            // AUDIO_IOC_CONFIG: 48 kHz s16, one or two channels.
            0x5610 => {
                let Some(b) = self.rd(i, ptr, 16) else { return Done(EFAULT) };
                let word = |k: usize| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
                let (rate, channels, format) = (word(0), word(1), word(2));
                if rate != 48000 || !(1..=2).contains(&channels) || format != AUDIO_FMT_S16LE {
                    return Done(EINVAL);
                }
                match &mut f.borrow_mut().kind {
                    Kind::Audio { channels: c, pending } => {
                        *c = channels;
                        pending.clear();
                        Done(0)
                    }
                    _ => Done(ENOTTY),
                }
            }
            0x5421 => {
                let on = self.rd32(i, ptr).unwrap_or(0) != 0;
                let mut f = f.borrow_mut();
                if on { f.flags |= O_NONBLOCK } else { f.flags &= !O_NONBLOCK }
                Done(0)
            }
            0x541b => {
                let n = match &f.borrow().kind {
                    Kind::Pipe { buffer, .. } => buffer.borrow().len() as u32,
                    _ => 0,
                };
                self.wr32(i, ptr, n);
                Done(0)
            }
            _ => Done(ENOTTY),
        }
    }

    /// `addr` and `len` whole (a 64-bit app reserves tens of GiB here).
    fn mmap(&mut self, i: usize, addr: Va, len: Va, prot: u32, flags: u32, fd: i32, offset: u32) -> Outcome {
        let layout = self.apps[i].as_ref().unwrap().space.layout;
        // At most half the 32-bit address space, or the whole 64-bit one.
        let most = if layout.user_top > 1 << 32 { layout.user_top } else { 0x8000_0000 };
        if len == 0 || len > most {
            return Done(EINVAL);
        }
        if prot & 6 == 6 {
            match self.gate(i, &Action::MapWriteExec { addr: telemetry(addr), len: telemetry(len) }) {
                super::Gate::Allow => {}
                super::Gate::Deny => return Done(EACCES),
                super::Gate::Frozen => return Block(Wait::Frozen, None),
                super::Gate::Killed => return Gone,
            }
        }
        let Some(len) = round_up(len, 4096) else { return Done(EINVAL) };
        // An app holding all the pool pages one app may gets no more memory.
        if self.apps[i].as_ref().unwrap().space.capped() {
            return Done(ENOMEM);
        }
        let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        let screen = flags & MAP_ANONYMOUS == 0
            && self.file(i, fd).map_or(false, |f| matches!(f.borrow().kind, Kind::Screen));
        let app = self.apps[i].as_mut().unwrap();
        // A hint is taken above the low addresses (above 4 GiB for a 64-bit app,
        // whose regions are all there) and below the mmap area's top.
        let hint_floor: Va = if layout.user_top > 1 << 32 { 1 << 32 } else { 0x1000_0000 };
        let start = if fixed {
            if addr & 0xfff != 0 || addr.checked_add(len).map_or(true, |e| e > layout.user_top) {
                return Done(EINVAL);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && app.space.overlaps(addr, addr + len) {
                return Done(EEXIST);
            }
            addr
        } else if addr != 0 && addr & 0xffff == 0 && addr >= hint_floor
            && addr.checked_add(len).map_or(false, |e| e <= layout.mmap_top)
            && !app.space.overlaps(addr, addr + len)
        {
            addr
        } else {
            match app.space.free_range(len) {
                Some(a) => a,
                None => return Done(ENOMEM),
            }
        };
        // The W^X policy (vma.rs): mmap never gives executable memory, puts
        // nothing in a 64 KiB page that holds code, and leaves the trampoline page alone.
        if app.space.refuses(start, start + len, prot & 7) {
            return Done(EACCES);
        }
        if fixed {
            app.space.remove(&mut self.k, &mut self.pool, start, start + len);
        }
        app.space.add(start, start + len, prot & 7);
        if screen {
            let mapped = if start & 0xffff != 0 || offset as usize % sys::PAGE != 0 { Err(EINVAL) } else { self.map_surface(i, start, len, offset) };
            if let Err(e) = mapped {
                // No region left behind: it would be anonymous memory.
                self.apps[i].as_mut().unwrap().space.remove(&mut self.k, &mut self.pool, start, start + len);
                return Done(e);
            }
            return address_answer(start);
        }
        if flags & MAP_ANONYMOUS == 0 {
            let Some(f) = self.file(i, fd) else { return Done(EBADF) };
            let path = match &f.borrow().kind {
                Kind::File { path } => path.clone(),
                _ => return Done(ENODEV),
            };
            // A mapping reads the file: not through a write-only descriptor (as on Linux).
            if !super::shield::fd_reads(f.borrow().flags) { return Done(EACCES); }
            let _ = MAP_SHARED; // Shared file mappings are not written back.
            let data = self.fs.contents(&path).map(|c| {
                let s = (offset as usize).min(c.len());
                let e = usize::try_from(len).map_or(c.len(), |l| s.saturating_add(l).min(c.len()));
                c[s..e].to_vec()
            });
            if let Some(data) = data {
                let app = self.apps[i].as_mut().unwrap();
                // The runtime writes the file in, even if the mapping is read-only.
                let mut ok = app.space.poke(&mut self.k, &mut self.pool, start, &data);
                // Out of pages: close a background app and try again.
                while !ok && self.foreground == Some(i) && self.pool.available() == 0 && self.reclaim(None) {
                    ok = self.apps[i].as_mut().unwrap().space.poke(&mut self.k, &mut self.pool, start, &data);
                }
                if !ok {
                    return Done(ENOMEM);
                }
            }
        }
        address_answer(start)
    }

    fn put_stat(&mut self, i: usize, st: fs::Stat, buf: Va) -> i32 {
        let mut b = [0u8; 104];
        b[..8].copy_from_slice(&1u64.to_le_bytes());
        b[12..16].copy_from_slice(&(st.inode as u32).to_le_bytes());
        b[16..20].copy_from_slice(&st.mode.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[48..56].copy_from_slice(&st.size.to_le_bytes());
        b[56..60].copy_from_slice(&4096u32.to_le_bytes());
        b[64..72].copy_from_slice(&((st.size + 511) / 512).to_le_bytes());
        for at in [72usize, 80, 88] {
            b[at..at + 4].copy_from_slice(&st.mtime.to_le_bytes());
        }
        b[96..104].copy_from_slice(&st.inode.to_le_bytes());
        if self.wr(i, buf, &b) { 0 } else { EFAULT }
    }
    fn stat_path(&mut self, i: usize, path: &str, buf: Va) -> i32 {
        if let Err(e) = self.path_allowed(i, path, false) {
            return e; // policy (manifest.rs): another app's files do not exist
        }
        match self.fs.stat(path) {
            Some(st) => self.put_stat(i, st, buf),
            None => ENOENT,
        }
    }
    fn fstat(&mut self, i: usize, fd: i32, buf: Va) -> i32 {
        let Some(f) = self.file(i, fd) else { return EBADF };
        let st = match &f.borrow().kind {
            Kind::File { path } | Kind::Dir { path, .. } => self.fs.stat(path),
            _ => Some(fs::Stat { mode: S_IFCHR | 0o666, size: 0, mtime: 0, inode: fd as u64 + 1 }),
        };
        match st {
            Some(st) => self.put_stat(i, st, buf),
            None => ENOENT,
        }
    }

    fn getdents(&mut self, i: usize, fd: i32, buf: Va, len: u32) -> Outcome {
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        let mut out = Vec::new();
        {
            let mut f = f.borrow_mut();
            let Kind::Dir { entries, pos, .. } = &mut f.kind else { return Done(ENOTDIR) };
            // "." and ".." first, then the entries.
            while *pos < entries.len() + 2 {
                let (name, dir) = match *pos {
                    0 => (String::from("."), true),
                    1 => (String::from(".."), true),
                    k => entries[k - 2].clone(),
                };
                let reclen = (19 + name.len() + 1 + 7) & !7;
                if out.len() + reclen > len as usize {
                    break;
                }
                let start = out.len();
                out.resize(start + reclen, 0);
                let ino = (*pos as u64) + 1;
                out[start..start + 8].copy_from_slice(&ino.to_le_bytes());
                out[start + 8..start + 16].copy_from_slice(&((*pos as u64) + 1).to_le_bytes());
                out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
                out[start + 18] = if dir { 4 } else { 8 };
                out[start + 19..start + 19 + name.len()].copy_from_slice(name.as_bytes());
                *pos += 1;
            }
            if out.is_empty() && *pos < entries.len() + 2 {
                return Done(EINVAL);
            }
        }
        let n = out.len() as i32;
        Done(if self.wr(i, buf, &out) { n } else { EFAULT })
    }

    fn poll(&mut self, i: usize, ptr: Va, nfds: u32, timeout: Option<u64>) -> Outcome {
        let mut fds = Vec::new();
        for k in 0..nfds.min(1024) {
            let Some(at) = ptr.checked_add(8 * Va::from(k)) else { return Done(EFAULT) };
            let Some(b) = self.rd(i, at, 8) else { return Done(EFAULT) };
            let fd = i32::from_le_bytes(b[..4].try_into().unwrap());
            let events = i16::from_le_bytes(b[4..6].try_into().unwrap());
            fds.push((fd, events));
        }
        let wait = Wait::Poll { fds, ptr, time64: false };
        if let Some(n) = self.ready(i, &wait) {
            return Done(n);
        }
        // Clear revents for a timeout of zero or a later wake.
        if let Wait::Poll { fds, .. } = &wait {
            for k in 0..fds.len() {
                self.wr(i, ptr + 8 * k as Va + 6, &[0, 0]);
            }
        }
        match timeout {
            Some(0) => Done(0),
            Some(t) => Block(wait, Some(sys::now_ns() + t)),
            None => Block(wait, None),
        }
    }

    fn epoll_ctl(&mut self, i: usize, epfd: i32, op: u32, fd: i32, event: Va) -> Outcome {
        let (mask, data) = if op == 2 {
            (0, 0)
        } else {
            let Some(b) = self.rd(i, event, 16) else { return Done(EFAULT) };
            (u32::from_le_bytes(b[..4].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap()))
        };
        if self.file(i, fd).is_none() {
            return Done(EBADF);
        }
        let Some(f) = self.file(i, epfd) else { return Done(EBADF) };
        let mut f = f.borrow_mut();
        let Kind::Epoll { interest, reported } = &mut f.kind else { return Done(EINVAL) };
        // Adding, changing or removing an entry re-arms its edge.
        reported.retain(|r| r.0 != fd);
        let existing = interest.iter().position(|e| e.0 == fd);
        match (op, existing) {
            (1, Some(_)) => Done(EEXIST),
            (1, None) => {
                interest.push((fd, mask, data));
                Done(0)
            }
            (2, Some(k)) => {
                interest.remove(k);
                Done(0)
            }
            (3, Some(k)) => {
                interest[k] = (fd, mask, data);
                Done(0)
            }
            _ => Done(ENOENT),
        }
    }
}

// ---- signals and processes ----
//
// The rules (masks, dispositions, the frame layout, timers, wait statuses)
// are in signal.rs; this is their delivery with seL4. A thread takes a
// signal at three points: when one of its system calls is answered (the
// reply carries the handler's registers), when it is blocked in a call (the
// saved reply does, with EINTR or SA_RESTART), and, for a thread running in
// user code, after the runtime has drained its fault queue (`signal_pass`):
// it is suspended, read, and written to start at the gate. A fault with a
// handler is delivered by a register write. Handlers enter and leave through
// the signal gate in the trampoline page (signal.rs); Thumb code resumes
// through the thread's trampoline, as threads start.

/// A descriptor to close at execve: its number and the open file it held
/// when FD_CLOEXEC was set. A number since reused for another file no longer
/// matches, so the record cannot close it.
pub type CloseOnExec = (u32, alloc::rc::Weak<RefCell<OpenFile>>);

/// The process a vfork child is while it runs in its parent's memory. Its
/// fields are swapped with the parent app's for the length of each of its
/// system calls (`vfork_swap`).
pub struct Vfork {
    pub pid: u32,
    pub fds: Vec<Option<FileRef>>,
    pub cwd: String,
    pub actions: alloc::boxed::Box<[signal::Action; 64]>,
    pub cloexec: Vec<CloseOnExec>,
    pub children: Vec<signal::Child>,
    pub ppid: u32,
    /// Signals sent to the child's pid, and its timers.
    pub pending: Vec<Info>,
    pub timers: Vec<signal::Timer>,
    /// The parent's thread, waiting until the child calls execve or exits.
    pub parent_slot: usize,
    /// Its fields are in the App now (during one of its calls), the App's here.
    pub swapped: bool,
}

/// How a stopped thread gets its new registers.
enum Via {
    /// The reply to the fault being handled now.
    Reply,
    /// A reply saved while the thread waited.
    Saved(u32),
    /// A register write (the thread is suspended, or blocked on a fault reply).
    Write(sys::Regs),
}

/// Pages fork keeps free in the pool beyond the copy (4 MiB).
const FORK_SPARE: usize = 64;

fn ctx_of(r: &sys::Regs) -> signal::Context {
    signal::Context {
        r: [r.r0, r.r1, r.r2, r.r3, r.r4, r.r5, r.r6, r.r7, r.r8, r.r9, r.r10, r.r11, r.r12],
        sp: r.sp,
        lr: r.r14,
        pc: r.pc,
        cpsr: r.cpsr,
    }
}

impl Runtime {
    /// Swap a vfork child's process identity with its parent app's (a
    /// second swap undoes it). False if the thread is not a vfork child.
    fn vfork_swap(&mut self, i: usize, slot: usize) -> bool {
        let Some(app) = self.apps[i].as_mut() else { return false };
        let super::App { pid, fds, cwd, sig, threads, .. } = app;
        let Some(v) = threads.get_mut(slot).and_then(|t| t.sig.vfork.as_mut()) else { return false };
        core::mem::swap(pid, &mut v.pid);
        core::mem::swap(fds, &mut v.fds);
        core::mem::swap(cwd, &mut v.cwd);
        core::mem::swap(&mut sig.actions, &mut v.actions);
        core::mem::swap(&mut sig.cloexec, &mut v.cloexec);
        core::mem::swap(&mut sig.children, &mut v.children);
        core::mem::swap(&mut sig.ppid, &mut v.ppid);
        core::mem::swap(&mut sig.pending, &mut v.pending);
        core::mem::swap(&mut sig.timers, &mut v.timers);
        v.swapped = !v.swapped;
        true
    }
    /// Undo a swap that is still in place (the child may have ended meanwhile).
    fn vfork_unswap(&mut self, i: usize, slot: usize) {
        if self.apps[i].as_ref().and_then(|a| a.threads.get(slot)).and_then(|t| t.sig.vfork.as_ref()).is_some_and(|v| v.swapped) {
            self.vfork_swap(i, slot);
        }
    }
    /// The vfork child of app `i` whose identity is swapped in now, if any.
    fn swapped_slot(&self, i: usize) -> Option<usize> {
        self.apps[i].as_ref()?.threads.iter().position(|t| t.sig.vfork.as_ref().is_some_and(|v| v.swapped))
    }
    /// The pid and parent pid of process (i, s) (app `i`, or the vfork
    /// child in thread `s`), wherever vfork_swap has put them.
    fn ident(&self, i: usize, s: Option<usize>) -> (u32, u32) {
        let Some(app) = self.apps[i].as_ref() else { return (0, 0) };
        let v_at = |s: usize| app.threads.get(s).and_then(|t| t.sig.vfork.as_deref());
        match s.and_then(v_at) {
            Some(v) if v.swapped => (app.pid, app.sig.ppid),
            Some(v) => (v.pid, v.ppid),
            // The app's own process (any of its other threads).
            None => match self.swapped_slot(i).and_then(v_at) {
                Some(v) => (v.pid, v.ppid),
                None => (app.pid, app.sig.ppid),
            },
        }
    }
    /// Violet Shield froze another process of app `i`'s owner: one that
    /// forked it, or one it forked (freezes are per process; forks share an owner).
    fn family_frozen(&self, i: usize) -> bool {
        let Some(me) = self.apps[i].as_ref() else { return false };
        self.apps.iter().enumerate().any(|(k, a)| k != i && a.as_ref().is_some_and(|a| a.frozen.is_some() && a.owner == me.owner))
    }
    /// End every other process of app `i`'s owner (a Violet Shield KillApp
    /// verdict on one of them; guard.rs calls this): forked children do not
    /// outlive the verdict.
    pub fn end_family(&mut self, i: usize, code: i32) {
        let Some(owner) = self.apps[i].as_ref().map(|a| a.owner.clone()).filter(|o| !o.is_empty()) else { return };
        for k in 0..super::MAX_APPS {
            if let Some(a) = self.apps[k].as_mut().filter(|a| k != i && a.owner == owner) {
                a.exit_code = Some(code);
                self.reap(k);
            }
        }
    }
    /// The app slot of app `i`'s first ancestor still running (itself if
    /// none): the lifecycle and the socket quota treat a process tree as one app.
    pub fn root_of(&self, i: usize) -> usize {
        let mut r = i;
        for _ in 0..super::MAX_APPS {
            let Some(ppid) = self.apps[r].as_ref().map(|a| a.sig.ppid).filter(|&p| p != 0) else { break };
            match self.apps.iter().position(|a| a.as_ref().is_some_and(|a| a.pid == ppid)) {
                Some(p) if p != r => r = p,
                _ => break,
            }
        }
        r
    }

    // ---- close-on-exec ----

    fn cloexec(&self, i: usize, fd: i32) -> bool {
        let Some(app) = self.apps[i].as_ref() else { return false };
        let Some(Some(f)) = app.fds.get(fd as usize) else { return false };
        app.sig.cloexec.iter().any(|(n, w)| *n == fd as u32 && core::ptr::eq(w.as_ptr(), Rc::as_ptr(f)))
    }
    fn set_cloexec(&mut self, i: usize, fd: i32, on: bool) {
        let Some(app) = self.apps[i].as_mut() else { return };
        app.sig.cloexec.retain(|(n, w)| *n != fd as u32 && w.strong_count() > 0);
        if let (true, Some(Some(f))) = (on, app.fds.get(fd as usize)) {
            app.sig.cloexec.push((fd as u32, Rc::downgrade(f)));
        }
    }
    /// FD_CLOEXEC for a descriptor a call just made: O_CLOEXEC and its
    /// SOCK_/EFD_/EPOLL_ twins set it, dup and dup2 clear it.
    fn note_cloexec(&mut self, i: usize, n: u32, m: &[u32; 16], o: Outcome) -> Outcome {
        let Done(fd) = o else { return o };
        if fd < 0 || self.apps[i].is_none() {
            return o;
        }
        let v = vargs(m);
        let flag = match n {
            5 => Some(m[1]),
            322 | 358 | 366 => Some(m[if n == 366 { 3 } else { 2 }]),
            281 | 356 => Some(m[1]),
            357 => Some(m[0]),
            41 | 63 | 285 | 250 | 351 => Some(0),
            55 | 221 if m[1] == 0 || m[1] == 1030 => Some(if m[1] == 1030 { O_CLOEXEC } else { 0 }),
            // openat2: the flags are in struct open_how (their low word).
            437 => self.rd32(i, v[2]),
            42 | 359 | 288 => {
                // Both ends of a pipe (pipe2's flags are its second argument)
                // or of a socket pair (SOCK_CLOEXEC in its type; the ends at its fourth).
                let (flags, sv) = match n { 359 => (m[1], v[0]), 288 => (m[1], v[3]), _ => (0, v[0]) };
                for k in 0..2 {
                    if let Some(end) = self.rd32(i, sv + 4 * k) {
                        self.set_cloexec(i, end as i32, flags & O_CLOEXEC != 0);
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(flag) = flag {
            self.set_cloexec(i, fd, flag & O_CLOEXEC != 0);
        }
        o
    }
    /// The descriptors that stay open across execve.
    fn close_on_exec(fds: &mut Vec<Option<FileRef>>, cloexec: &[CloseOnExec]) {
        for (n, w) in cloexec {
            if let Some(slot) = fds.get_mut(*n as usize) {
                if slot.as_ref().is_some_and(|f| core::ptr::eq(w.as_ptr(), Rc::as_ptr(f))) {
                    *slot = None;
                }
            }
        }
    }

    // ---- who is who ----

    /// The app and, for a vfork child, the thread that is process `pid`
    /// (looking through a swap in place: `ident`).
    fn find_pid(&self, pid: u32) -> Option<(usize, Option<usize>)> {
        for (i, app) in self.apps.iter().enumerate() {
            let Some(app) = app else { continue };
            if let Some(s) = (0..app.threads.len()).find(|&s| app.threads[s].alive && app.threads[s].sig.vfork.is_some() && self.ident(i, Some(s)).0 == pid) {
                return Some((i, Some(s)));
            }
            if self.ident(i, None).0 == pid {
                return Some((i, None));
            }
        }
        None
    }
    fn ppid_of(&self, pid: u32) -> u32 {
        self.find_pid(pid).map_or(0, |(i, s)| self.ident(i, s).1)
    }
    /// Whether `pid` is `ancestor` or one of its descendants.
    fn descends(&self, pid: u32, ancestor: u32) -> bool {
        let mut chain = Vec::new();
        let mut p = pid;
        for _ in 0..super::MAX_APPS * 2 {
            p = self.ppid_of(p);
            if p == 0 {
                break;
            }
            chain.push(p);
        }
        signal::may_signal(ancestor, pid, &chain)
    }
    /// The pid whose socket quota app `i` uses (slots.rs): the first
    /// ancestor still running, so a process tree shares one app's share.
    pub fn quota_owner(&self, i: usize) -> u32 {
        self.apps[self.root_of(i)].as_ref().map_or(u32::MAX, |a| a.pid)
    }

    // ---- raising ----

    /// Raise `info` in app `i`: at thread `slot`, or at the process (taken by
    /// the first thread that does not block it). A signal whose action ends
    /// the process ends it now unless every candidate thread blocks it; one
    /// that is ignored is dropped. A thread waiting in sigtimedwait for it
    /// takes it; one blocked in a call is interrupted at once.
    pub fn raise(&mut self, i: usize, slot: Option<usize>, info: Info) {
        // A held app (Shield-frozen, lifecycle-parked) keeps its signals
        // pending, SIGKILL apart: nothing of it runs or ends until it is released.
        let held = info.signo != signal::SIGKILL && self.held(i);
        let Some(app) = self.apps[i].as_mut() else { return };
        let sig = info.signo;
        if let Some(s) = slot {
            if app.threads.get(s).map_or(true, |t| !t.alive) {
                return;
            }
            if app.threads[s].sig.vfork.is_some() {
                signal::queue(&mut app.threads[s].sig.pending, info);
                // In its own call (identity swapped in) it decides when the
                // call ends; otherwise now, as its own process.
                if !held && !app.threads[s].sig.vfork.as_ref().unwrap().swapped {
                    self.vfork_signal(i, s);
                }
                return;
            }
        }
        if held {
            match slot {
                Some(s) => signal::queue(&mut app.threads[s].sig.pending, info),
                None => signal::queue(&mut app.sig.pending, info),
            };
            return;
        }
        let candidates: Vec<usize> = match slot {
            Some(s) => alloc::vec![s],
            None => (0..app.threads.len()).filter(|&s| app.threads[s].alive && app.threads[s].sig.vfork.is_none()).collect(),
        };
        let open = candidates.iter().any(|&s| app.threads[s].sig.blocked & signal::bit(sig) == 0);
        // sigtimedwait takes a signal before any action does.
        let taker = candidates.iter().copied().find(|&s| {
            let t = &app.threads[s];
            matches!(t.wait, Some(Wait::Signal)) && matches!(t.sig.waiting, signal::WaitFor::Set { set, .. } if set & signal::bit(sig) != 0)
        });
        if let Some(s) = taker {
            if let signal::WaitFor::Set { info: ptr, .. } = app.threads[s].sig.waiting {
                if ptr != 0 {
                    self.wr(i, ptr, &app_siginfo(&info));
                }
            }
            return self.wake(i, s, sig as i32);
        }
        match signal::disposition(&app.sig.action(sig), sig) {
            signal::Disposition::Ignore | signal::Disposition::Stop if open => return,
            signal::Disposition::Terminate { core } if open || sig == signal::SIGKILL => {
                return self.kill_app(i, sig, core);
            }
            _ => {}
        }
        match slot {
            Some(s) => signal::queue(&mut app.threads[s].sig.pending, info),
            None => signal::queue(&mut app.sig.pending, info),
        };
        for s in candidates {
            if !self.held(i) && self.apps[i].as_ref().is_some_and(|a| a.threads[s].reply.is_some()) && self.deliverable(i, s) {
                self.interrupt(i, s);
            }
        }
    }

    fn kill_app(&mut self, i: usize, sig: u32, core: bool) {
        let Some(app) = self.apps[i].as_mut() else { return };
        crate::log!("{} (pid {}) killed by signal {}", app.name, app.pid, sig);
        app.exit_code = Some(128 + sig as i32);
        app.sig.term = signal::signaled(sig, core);
        self.reap(i);
    }
    /// A signal ends the process thread `slot` is: its app, or the vfork child it runs.
    pub(super) fn terminate(&mut self, i: usize, slot: usize, sig: u32, core: bool) {
        if self.apps[i].as_ref().is_some_and(|a| a.threads[slot].sig.vfork.is_some()) {
            // (Called during the child's own call or fault: its identity is swapped in.)
            self.vfork_exit(i, slot, signal::signaled(sig, core));
        } else {
            self.kill_app(i, sig, core);
        }
    }
    /// Vfork child `s`, not in a call of its own, has a new signal: with its
    /// identity swapped in, a call it is blocked in is interrupted (SIGKILL
    /// and other fatal signals end it there), and while it runs a fatal one
    /// ends it now; a handler for a running child waits for `signal_pass`.
    fn vfork_signal(&mut self, i: usize, s: usize) {
        if !self.vfork_swap(i, s) {
            return;
        }
        if self.apps[i].as_ref().is_some_and(|a| a.threads[s].reply.is_some()) {
            self.interrupt(i, s);
        } else if self.deliverable(i, s) {
            let _ = self.next_handled(i, s);
        }
        // SIGKILL ends it even in a wait that cannot be interrupted.
        let kill = self.apps[i].as_ref().and_then(|a| a.threads.get(s)).is_some_and(|t| t.sig.vfork.is_some() && signal::set_of(&t.sig.pending) & signal::bit(signal::SIGKILL) != 0);
        if kill {
            self.vfork_exit(i, s, signal::signaled(signal::SIGKILL, false));
            return;
        }
        self.vfork_unswap(i, s);
    }

    /// kill, tkill, tgkill, rt_(tg)sigqueueinfo. An app may signal itself
    /// and its descendants (pid 0 and -1 are both), nothing else.
    fn kill(&mut self, i: usize, pid: i32, tid: Option<u32>, sig: u32, info: Option<Info>) -> Outcome {
        if sig > signal::NSIG {
            return Done(EINVAL);
        }
        let me = self.apps[i].as_ref().unwrap().pid;
        let mut targets: Vec<(usize, Option<usize>)> = Vec::new();
        if let Some(tid) = tid {
            // tkill names a thread only; tgkill a thread in process `pid`.
            for (k, app) in self.apps.iter().enumerate() {
                let Some(app) = app else { continue };
                if let Some(s) = app.threads.iter().position(|t| t.alive && t.tid == tid) {
                    let process = self.ident(k, Some(s)).0;
                    if pid <= 0 || process == pid as u32 {
                        targets.push((k, Some(s)));
                    }
                }
            }
        } else if pid > 0 {
            targets.extend(self.find_pid(pid as u32));
        } else if pid == 0 || pid == -1 || pid.unsigned_abs() == me {
            // 0 (and -pgid): the caller and its children, all in its group;
            // -1: everything it may signal but itself.
            if pid != -1 {
                // The caller's own process: a vfork child calling is its thread.
                targets.push((i, self.swapped_slot(i)));
            }
            for (k, app) in self.apps.iter().enumerate() {
                if let Some(app) = app {
                    if k != i && self.descends(app.pid, me) {
                        targets.push((k, None));
                    }
                }
            }
        }
        if targets.is_empty() {
            return Done(ESRCH);
        }
        let pid_of = |rt: &Runtime, (k, s): (usize, Option<usize>)| rt.ident(k, s).0;
        if targets.iter().any(|&t| !self.descends(pid_of(self, t), me)) {
            return Done(EPERM);
        }
        if let Some(info) = info {
            // Only the kernel sends codes of zero and up, except to oneself.
            if info.code >= 0 && targets.iter().any(|&t| pid_of(self, t) != me) {
                return Done(EPERM);
            }
        }
        if sig == 0 {
            return Done(0);
        }
        let code = if tid.is_some() { signal::SI_TKILL } else { signal::SI_USER };
        let info = info.unwrap_or(Info { signo: sig, code, a: me, b: 0, c: 0 });
        for (k, s) in targets {
            self.raise(k, s, Info { signo: sig, ..info });
        }
        if self.apps[i].as_ref().map_or(true, |a| !a.threads.iter().any(|t| t.alive)) {
            return Gone;
        }
        Done(0)
    }

    // ---- delivery ----

    /// An app whose threads are held (a Shield freeze, or parked by
    /// lifecycle.rs): its signals wait until it runs again.
    fn held(&self, i: usize) -> bool {
        self.apps[i].as_ref().map_or(true, |a| a.frozen.is_some() || a.life.state == super::lifecycle::State::Frozen)
    }
    /// Whether thread `slot` has a signal it does not block.
    fn deliverable(&self, i: usize, slot: usize) -> bool {
        let Some(app) = self.apps[i].as_ref() else { return false };
        let Some(t) = app.threads.get(slot) else { return false };
        t.alive && (signal::set_of(&t.sig.pending) | signal::set_of(&app.sig.pending)) & !t.sig.blocked != 0
    }
    /// The next signal the thread takes that runs a handler, left queued.
    /// Ignored ones are dropped on the way; one whose action ends the
    /// process ends it (None, and the thread is gone).
    fn next_handled(&mut self, i: usize, slot: usize) -> Option<Info> {
        loop {
            let app = self.apps[i].as_mut()?;
            let allowed = !app.threads.get(slot)?.sig.blocked;
            let t = &app.threads[slot].sig;
            let info = match signal::pick(&t.pending, allowed) {
                Some(k) => t.pending[k],
                None => app.sig.pending[signal::pick(&app.sig.pending, allowed)?],
            };
            match signal::disposition(&app.sig.action(info.signo), info.signo) {
                signal::Disposition::Handle => return Some(info),
                signal::Disposition::Terminate { core } => {
                    self.terminate(i, slot, info.signo, core);
                    return None;
                }
                _ => {
                    let t = &mut app.threads[slot].sig;
                    if signal::take(&mut t.pending, signal::bit(info.signo)).is_none() {
                        signal::take(&mut app.sig.pending, signal::bit(info.signo));
                    }
                }
            }
        }
    }

    /// The context of a thread stopped in a system call whose reply would set
    /// r0 to `r0` and resume at `pc`. r8-r12 are not in the fault message:
    /// the gate stores them in the frame itself.
    pub(crate) fn call_context(&self, _i: usize, _slot: usize, m: &[u32; 16], r0: u32, pc: u32) -> signal::Context {
        let mut r = [0u32; 13];
        r[..8].copy_from_slice(&m[..8]);
        r[0] = r0;
        signal::Context { r, sp: m[sys::MR_SP], lr: m[sys::MR_LR], pc, cpsr: m[MR_CPSR] }
    }

    /// Thread `slot`, stopped in call `ctx.r[7]` at `ctx` (pc after the
    /// svc, r0 its first argument), is interrupted by the signal it takes
    /// next: the context it resumes with, set to restart the call (SA_RESTART
    /// on a call that restarts) or to return EINTR. None if no handler is
    /// due after all (the thread may then be gone).
    fn interrupted(&mut self, i: usize, slot: usize, mut ctx: signal::Context) -> Option<signal::Context> {
        let info = self.next_handled(i, slot)?;
        let app = self.apps[i].as_ref()?;
        // pause, sigsuspend and sigtimedwait end with EINTR whatever SA_RESTART says.
        let waits = matches!(app.threads[slot].sig.waiting, signal::WaitFor::Suspend | signal::WaitFor::Set { .. });
        let flags = app.sig.action(info.signo).flags;
        if !waits && signal::restarts(ctx.r[7], flags) {
            ctx.pc = ctx.pc.wrapping_sub(if signal::thumb(ctx.cpsr) { 2 } else { 4 });
        } else {
            ctx.r[0] = EINTR as u32;
        }
        Some(ctx)
    }

    /// Whether a thread waiting in `w` may be interrupted (not one whose work
    /// is done or that must not run yet: a present, a Shield freeze, a
    /// connect under way, a vfork parent).
    fn interruptible(&self, i: usize, slot: usize, w: &Wait) -> bool {
        let vfork_parent = self.apps[i].as_ref().is_some_and(|a| matches!(a.threads[slot].sig.waiting, signal::WaitFor::Vfork { .. }));
        !vfork_parent && !matches!(w, Wait::Frozen | Wait::Held { .. } | Wait::Present { .. } | Wait::Connect { .. })
    }

    /// Resume a stopped thread at `ctx`, through the handler of the next
    /// signal it takes if there is one.
    fn resume(&mut self, i: usize, slot: usize, ctx: signal::Context) {
        self.resume_via(i, slot, ctx, Via::Reply, None, &signal::Trap::default())
    }
    fn resume_via(&mut self, i: usize, slot: usize, ctx: signal::Context, via: Via, forced: Option<Info>, trap: &signal::Trap) {
        let info = match forced {
            Some(info) => Some(info),
            None => match self.next_handled(i, slot) {
                Some(info) => {
                    let app = self.apps[i].as_mut().unwrap();
                    let bit = signal::bit(info.signo);
                    if signal::take(&mut app.threads[slot].sig.pending, bit).is_none() {
                        signal::take(&mut app.sig.pending, bit);
                    }
                    Some(info)
                }
                // Ended by a signal, or nothing to run.
                None if self.apps[i].as_ref().map_or(true, |a| !a.threads[slot].alive) => {
                    if let Via::Saved(r) = via {
                        sys::delete(r);
                        self.k.release(r);
                    }
                    return;
                }
                None => None,
            },
        };
        // AArch64: the handler starts through the A64 gate with a Linux
        // AArch64 frame (abi64.rs), the thread's registers written whole; a
        // call or saved reply is then not answered (the write resumes it).
        #[cfg(target_arch = "aarch64")]
        {
            let reply = match via { Via::Saved(r) => Some(r), _ => None };
            let started = match (info, &via) {
                (Some(info), Via::Reply | Via::Saved(_)) => {
                    Some(self.call_regs64(i, slot, &ctx, reply).and_then(|regs| self.deliver64(i, slot, &regs, &info, None)).is_some())
                }
                // A fault's handler: fault_signal (below) delivers it itself.
                _ => None,
            };
            if let Some(started) = started {
                if let Some(r) = reply {
                    sys::delete(r);
                    self.k.release(r);
                }
                if !started {
                    self.terminate(i, slot, signal::SIGSEGV, true);
                }
                return;
            }
        }
        let msg = match info {
            Some(info) => match self.frame(i, slot, &ctx, &info, trap) {
                Some(msg) => msg,
                None => {
                    // No room for the frame: the thread dies of SIGSEGV, as on Linux.
                    if let Via::Saved(r) = via {
                        sys::delete(r);
                        self.k.release(r);
                    }
                    return self.terminate(i, slot, signal::SIGSEGV, true);
                }
            },
            None => {
                // Back where it was; Thumb code by its trampoline (seL4 clears the T bit).
                let mut pc = ctx.pc;
                if signal::thumb(ctx.cpsr) {
                    let app = self.apps[i].as_mut().unwrap();
                    match app.space.stub(&mut self.k, &mut self.pool, slot, ctx.pc | 1).and_then(|s| u32::try_from(s).ok()) {
                        Some(stub) => pc = stub,
                        None => return self.terminate(i, slot, signal::SIGSEGV, false),
                    }
                }
                let r = &ctx.r;
                [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], pc, ctx.sp, ctx.lr, signal::user_cpsr(ctx.cpsr)]
            }
        };
        match via {
            Via::Reply => sys::reply(&msg),
            Via::Saved(r) => {
                sys::send(r, &msg);
                self.k.reply_done(r);
            }
            Via::Write(mut regs) => {
                (regs.r0, regs.r1, regs.r2, regs.r3, regs.r4, regs.r5, regs.r6, regs.r7) = (msg[0], msg[1], msg[2], msg[3], msg[4], msg[5], msg[6], msg[7]);
                (regs.pc, regs.sp, regs.r14, regs.cpsr) = (msg[8], msg[9], msg[10], msg[11]);
                let tcb = self.apps[i].as_ref().unwrap().threads[slot].tcb;
                if sys::tcb_write(tcb, true, &regs).is_err() {
                    self.terminate(i, slot, info.map_or(signal::SIGSEGV, |s| s.signo), false);
                }
            }
        }
    }

    /// Write the frame for `info` and set the handler's mask. The registers
    /// that start the handler at the gate: r0-r7, pc, sp, lr, cpsr.
    fn frame(&mut self, i: usize, slot: usize, ctx: &signal::Context, info: &Info, trap: &signal::Trap) -> Option<[u32; 12]> {
        let app = self.apps[i].as_ref()?;
        let a = app.sig.action(info.signo);
        let t = &app.threads[slot].sig;
        // (AArch32 frames only: the frame, gate and handler are 32-bit addresses.)
        let at = signal::place(ctx.sp.into(), &t.alt, a.flags & signal::SA_ONSTACK != 0)?;
        // sigsuspend: the handler returns to the mask from before the call.
        let saved_mask = t.suspend_mask.unwrap_or(t.blocked);
        let bytes = signal::encode(ctx, info, saved_mask, &t.alt, trap);
        let (gate, handler, at32) = (u32::try_from(app.space.gate()).ok()?, u32::try_from(a.handler).ok()?, u32::try_from(at).ok()?);
        if !self.wr(i, at, &bytes) {
            return None;
        }
        let app = self.apps[i].as_mut()?;
        let t = &mut app.threads[slot].sig;
        t.blocked = signal::handler_mask(t.blocked, &a, info.signo);
        t.suspend_mask = None;
        t.waiting = signal::WaitFor::None;
        if a.flags & signal::SA_RESETHAND != 0 {
            app.sig.set_action(info.signo, Default::default());
        }
        let r = &ctx.r;
        Some([info.signo, at32, at32 + signal::UC as u32, handler, r[4], r[5], r[6], r[7], gate, at32, ctx.lr, signal::user_cpsr(ctx.cpsr)])
    }

    /// A thread blocked in a call takes a signal: its saved reply carries
    /// the handler (or the restart).
    fn interrupt(&mut self, i: usize, slot: usize) {
        let Some(t) = self.apps[i].as_ref().and_then(|a| a.threads.get(slot)) else { return };
        let (Some(w), Some(cap)) = (t.wait.clone(), t.reply) else { return };
        if !self.interruptible(i, slot, &w) {
            return;
        }
        let (tcb, saved, deadline) = (t.tcb, t.saved, t.deadline);
        // sigtimedwait takes a signal from its set that was already pending.
        if let signal::WaitFor::Set { set, info: ptr } = t.sig.waiting {
            let app = self.apps[i].as_mut().unwrap();
            let got = signal::take(&mut app.threads[slot].sig.pending, set).or_else(|| signal::take(&mut app.sig.pending, set));
            if let Some(info) = got {
                if ptr != 0 {
                    self.wr(i, ptr, &app_siginfo(&info));
                }
                return self.wake(i, slot, info.signo as i32);
            }
        }
        let Ok(regs) = sys::tcb_read(tcb) else { return };
        let mut ctx = ctx_of(&regs);
        ctx.r[..8].copy_from_slice(&saved[..8]);
        ctx.pc = saved[8];
        let Some(ctx) = self.interrupted(i, slot, ctx) else { return };
        // nanosleep and clock_nanosleep (relative) report the time left.
        let nr = saved[7];
        if ctx.r[0] == EINTR as u32 && matches!(nr, 162 | 265 | 407) && (nr == 162 || saved[1] & 1 == 0) {
            // The pointer, whole: on AArch64 from the call's own registers
            // (nanosleep's x1, clock_nanosleep's x3; enter64 made both 407).
            #[cfg(target_arch = "aarch64")]
            let rem = {
                let w = sys::widened(Some(cap), &saved);
                if w[sys::MR_SYSCALL] == 101 { w[1] } else { w[3] }
            };
            #[cfg(not(target_arch = "aarch64"))]
            let rem = {
                let _ = cap;
                Va::from(if nr == 162 { saved[1] } else { saved[3] })
            };
            let left = deadline.map_or(0, |d| d.saturating_sub(sys::now_ns()));
            if rem != 0 {
                self.put_timespec(i, rem, left, nr == 407);
            }
        }
        let t = &mut self.apps[i].as_mut().unwrap().threads[slot];
        let reply = t.reply.take().unwrap();
        t.wait = None;
        t.deadline = None;
        self.resume_via(i, slot, ctx, Via::Saved(reply), None, &signal::Trap::default());
    }

    /// A thread running in user code takes a signal: suspended, read, and
    /// written to start the handler. Not while it is in an IT block or has
    /// GE flags set (a register write would lose them): later.
    fn preempt(&mut self, i: usize, slot: usize) {
        let app = self.apps[i].as_ref().unwrap();
        let (tcb, stubs) = (app.threads[slot].tcb, app.space.layout.stubs);
        sys::tcb_suspend(tcb);
        // Not on its way out of a trampoline either: a Thumb resume rewrites it.
        let regs = match sys::tcb_read(tcb) {
            Ok(r) if signal::async_ok(r.cpsr) && Va::from(r.pc).wrapping_sub(stubs) >= Va::from(signal::GATE_OFFSET) => r,
            _ => return sys::tcb_resume(tcb),
        };
        if self.next_handled(i, slot).is_none() {
            if self.apps[i].as_ref().is_some_and(|a| a.threads[slot].alive) {
                sys::tcb_resume(tcb);
            }
            return;
        }
        self.resume_via(i, slot, ctx_of(&regs), Via::Write(regs), None, &signal::Trap::default());
    }

    /// The run loop's turn (mod.rs): timers that expired raise their
    /// signals, and threads with a signal take it. Running threads are only
    /// interrupted when `drained` (no fault waits in the queue, so none is
    /// lost by suspending its thread), on one core, with the runtime above
    /// every app (the timer works; no MCS budget to run out); otherwise they
    /// take it at their next system call.
    pub fn signal_pass(&mut self, drained: bool) {
        let now = sys::now_ns();
        for i in 0..super::MAX_APPS {
            let Some(app) = self.apps[i].as_mut() else { continue };
            let mut fired = Vec::new();
            for t in app.sig.timers.iter_mut() {
                if t.expire(now) && !t.silent {
                    fired.push(*t);
                }
            }
            for t in fired {
                let slot = self.apps[i].as_ref().and_then(|a| a.threads.iter().position(|th| th.alive && t.tid != 0 && th.tid == t.tid));
                let code = if t.id == signal::ITIMER { 0x80 } else { signal::SI_TIMER };
                self.raise(i, slot, Info { signo: t.sig, code, a: t.id, b: t.overrun, c: t.value });
            }
        }
        let preempt = drained && !sys::mcs() && sys::cores() == 1 && sys::timer_ready();
        for i in 0..super::MAX_APPS {
            let threads = match self.apps[i].as_ref() {
                Some(app) if !self.held(i) => app.threads.len(),
                _ => continue,
            };
            let family = self.family_frozen(i);
            for slot in 0..threads {
                let Some(t) = self.apps[i].as_ref().and_then(|a| a.threads.get(slot)) else { break };
                if !t.alive {
                    continue;
                }
                // A call stopped while another process of its owner was
                // Shield-frozen: made again now none is.
                if !family && t.sig.held_call && matches!(t.wait, Some(Wait::Frozen)) {
                    self.call_again(i, slot);
                    continue;
                }
                // A vfork child takes signals as its own process.
                let vf = self.vfork_swap(i, slot);
                let t = &self.apps[i].as_ref().unwrap().threads[slot];
                if self.deliverable(i, slot) {
                    if t.reply.is_some() {
                        self.interrupt(i, slot);
                    } else if preempt && t.wait.is_none() {
                        self.preempt(i, slot);
                    }
                }
                if vf {
                    self.vfork_unswap(i, slot);
                }
            }
        }
    }

    /// Answer thread `slot`'s call back onto its svc instruction, so it
    /// makes the call again (it waited only because its app was held).
    fn call_again(&mut self, i: usize, slot: usize) {
        let Some(t) = self.apps[i].as_mut().and_then(|a| a.threads.get_mut(slot)) else { return };
        // (AArch64 has no Thumb, and no 32-bit register form to read.)
        let thumb = if super::vma::WIDE {
            false
        } else {
            let Ok(regs) = sys::tcb_read(t.tcb) else { return };
            signal::thumb(regs.cpsr)
        };
        let Some(reply) = t.reply.take() else { return };
        (t.wait, t.deadline, t.sig.held_call) = (None, None, false);
        let mut again = t.saved;
        again[8] = again[8].wrapping_sub(if thumb { 2 } else { 4 });
        sys::send(reply, &again);
        self.k.reply_done(reply);
    }

    /// When the run loop must look again: the next timer, or soon if a
    /// running thread still has a signal to take.
    pub fn signal_wake(&self, now: u64) -> Option<u64> {
        let preempt = !sys::mcs() && sys::cores() == 1 && sys::timer_ready();
        let mut wake = u64::MAX;
        for (i, app) in self.apps.iter().enumerate() {
            let Some(app) = app else { continue };
            // (Never sooner than signal::TIMER_MIN from now: a timer cannot keep the loop awake.)
            wake = wake.min(signal::soonest(&app.sig.timers, now).unwrap_or(u64::MAX));
            // A running thread that could not take its signal yet (IT block, GE flags): again in 1 ms.
            let due = |s: usize| app.threads[s].wait.is_none() && app.threads[s].sig.vfork.is_none() && self.deliverable(i, s);
            if preempt && !self.held(i) && (0..app.threads.len()).any(due) {
                wake = wake.min(now + 1_000_000);
            }
        }
        (wake != u64::MAX).then_some(wake)
    }

    /// A fault the region rules refuse (VM fault) or an undefined instruction
    /// (user exception), at thread `slot`: true if a handler takes it. False
    /// leaves the runtime's answer as before: the app is ended (and its wait
    /// status says which signal). A handler that keeps returning to the same
    /// fault is stopped after signal::FAULT_LIMIT.
    pub fn fault_signal(&mut self, i: usize, slot: usize, label: u32, addr: Va, access: Option<super::vma::Access>, fsr: u32) -> bool {
        let Some(app) = self.apps[i].as_ref() else { return false };
        let (sig, code) = match (label, access) {
            (sys::FAULT_VM, Some(a)) => match app.space.find(addr) {
                None => (signal::SIGSEGV, signal::SEGV_MAPERR),
                Some(v) if !a.allowed(v.prot) => (signal::SIGSEGV, signal::SEGV_ACCERR),
                // Allowed, yet the hardware refuses it: an unaligned access
                // (LDRD, LDM...), or a fault mapping a page did not cure.
                Some(_) if signal::alignment_fault(fsr) => (signal::SIGBUS, signal::BUS_ADRALN),
                Some(_) => (signal::SIGBUS, signal::BUS_ADRERR),
            },
            (sys::FAULT_USER_EXCEPTION, _) => (signal::SIGILL, signal::ILL_ILLOPC),
            _ => return false,
        };
        let swapped = self.vfork_swap(i, slot);
        let app = self.apps[i].as_mut().unwrap();
        let a = app.sig.action(sig);
        let t = &mut app.threads[slot];
        t.sig.faults += 1;
        let caught = a.handler > signal::SIG_IGN && t.sig.blocked & signal::bit(sig) == 0 && t.sig.faults <= signal::FAULT_LIMIT;
        // AArch64: the thread's whole context at the fault, and a Linux
        // AArch64 frame with the fault's address and ESR (abi64.rs).
        #[cfg(target_arch = "aarch64")]
        if caught {
            let tcb = t.tcb;
            let w = sys::wide();
            let regs = sys::tcb_read_context(tcb).ok().map(|c| super::abi64::regs_of(&c));
            let address = if label == sys::FAULT_VM { w[1] } else { regs.map_or(0, |r| r.pc) };
            let esr = if label == sys::FAULT_VM { w[3] } else { 0 };
            let info = Info { signo: sig, code, a: address as u32, b: 0, c: 0 };
            if regs.and_then(|r| self.deliver64(i, slot, &r, &info, Some((address, esr)))).is_none() {
                self.terminate(i, slot, signal::SIGSEGV, true);
            }
            if swapped && self.apps[i].as_ref().is_some_and(|a| a.threads[slot].sig.vfork.is_some()) {
                self.vfork_swap(i, slot);
            }
            return true;
        }
        let regs = if caught { sys::tcb_read(t.tcb).ok() } else { None };
        let Some(regs) = regs else {
            if swapped {
                // The vfork child dies, not its parent.
                self.vfork_exit(i, slot, signal::signaled(sig, true));
                return true;
            }
            app.sig.term = signal::signaled(sig, true);
            return false;
        };
        let mut ctx = ctx_of(&regs);
        if label == sys::FAULT_USER_EXCEPTION && signal::thumb(ctx.cpsr) {
            ctx.pc = ctx.pc.wrapping_add(2); // seL4 reports lr - 4 in Thumb too
        }
        // (AArch32: its fault addresses are 32-bit.)
        let fault_addr = if label == sys::FAULT_VM { addr as u32 } else { ctx.pc };
        let trap = signal::Trap { no: if label == sys::FAULT_VM { 14 } else { 6 }, error: fsr, addr: fault_addr };
        let info = Info { signo: sig, code, a: fault_addr, b: 0, c: 0 };
        self.resume_via(i, slot, ctx, Via::Write(regs), Some(info), &trap);
        if swapped && self.apps[i].as_ref().is_some_and(|a| a.threads[slot].sig.vfork.is_some()) {
            self.vfork_swap(i, slot);
        }
        true
    }

    // ---- signal system calls ----

    fn sigaction(&mut self, i: usize, sig: u32, act: Va, old: Va, size: u32) -> Outcome {
        if signal::bit(sig) == 0 || size != 8 {
            return Done(EINVAL);
        }
        // The kernel's struct sigaction in the app's ABI (../arch/compat.rs):
        // handler, flags, restorer, mask (8 bytes). The handler and restorer
        // are kept whole (an AArch64 app's lie above 4 GiB).
        let abi = super::arch::NATIVE;
        let new = if act != 0 {
            let Some(b) = self.rd(i, act, super::arch::compat::sigaction_bytes(abi)) else { return Done(EFAULT) };
            let Some(a) = super::arch::compat::sigaction(abi, &b) else { return Done(EFAULT) };
            Some(signal::Action { handler: a.handler, flags: a.flags as u32, restorer: a.restorer, mask: a.mask })
        } else {
            None
        };
        if new.is_some() && signal::bit(sig) & signal::UNBLOCKABLE != 0 {
            return Done(EINVAL);
        }
        if old != 0 {
            let a = self.apps[i].as_ref().unwrap().sig.action(sig);
            let wide = super::arch::compat::Sigaction { handler: a.handler, flags: a.flags.into(), restorer: a.restorer, mask: a.mask };
            let (b, n) = super::arch::compat::put_sigaction(abi, &wide);
            if !self.wr(i, old, &b[..n]) {
                return Done(EFAULT);
            }
        }
        if let Some(a) = new {
            let app = self.apps[i].as_mut().unwrap();
            app.sig.set_action(sig, a);
            if signal::disposition(&app.sig.action(sig), sig) == signal::Disposition::Ignore {
                for t in app.threads.iter_mut() {
                    signal::discard(&mut t.sig.pending, sig);
                }
            }
        }
        Done(0)
    }

    fn sigaltstack(&mut self, i: usize, slot: usize, new: Va, old: Va, sp: Va) -> Outcome {
        let alt = self.apps[i].as_ref().unwrap().threads[slot].sig.alt;
        let next = if new != 0 {
            // stack_t in the app's ABI: sp, flags (int), size.
            let abi = super::arch::NATIVE;
            let Some(b) = self.rd(i, new, 3 * abi.word_bytes()) else { return Done(EFAULT) };
            let Some((ss_sp, flags, size)) = super::arch::compat::stack_t(abi, &b) else { return Done(EFAULT) };
            match signal::alt_change(alt, sp, ss_sp, flags, size) {
                Ok(next) => Some(next),
                Err(e) => return Done(e),
            }
        } else {
            None
        };
        if old != 0 {
            let (b, n) = super::arch::compat::put_stack_t(super::arch::NATIVE, alt.sp, alt.report(sp), alt.size);
            if !self.wr(i, old, &b[..n]) {
                return Done(EFAULT);
            }
        }
        if let Some(next) = next {
            self.apps[i].as_mut().unwrap().threads[slot].sig.alt = next;
        }
        Done(0)
    }

    /// rt_sigtimedwait(set, info, timeout, size).
    fn sigtimedwait(&mut self, i: usize, slot: usize, a: [u32; 7], v: [Va; 7], time64: bool) -> Outcome {
        if a[3] != 8 {
            return Done(EINVAL);
        }
        let Some(b) = self.rd(i, v[0], 8) else { return Done(EFAULT) };
        let set = u64::from_le_bytes(b[..8].try_into().unwrap()) & !signal::UNBLOCKABLE;
        let app = self.apps[i].as_mut().unwrap();
        let got = signal::take(&mut app.threads[slot].sig.pending, set).or_else(|| signal::take(&mut app.sig.pending, set));
        if let Some(info) = got {
            if v[1] != 0 && !self.wr(i, v[1], &app_siginfo(&info)) {
                return Done(EFAULT);
            }
            return Done(info.signo as i32);
        }
        let deadline = if v[2] != 0 {
            let Some(t) = self.timespec(i, v[2], time64) else { return Done(EFAULT) };
            if t == 0 {
                return Done(EAGAIN);
            }
            Some(sys::now_ns() + t)
        } else {
            None
        };
        self.apps[i].as_mut().unwrap().threads[slot].sig.waiting = signal::WaitFor::Set { set, info: v[1] };
        Block(Wait::Signal, deadline)
    }

    /// rt_sigreturn, from the gate: the context in the frame at sp, as the
    /// handler left it, and the mask from before the handler.
    #[cfg_attr(target_arch = "aarch64", allow(dead_code))]
    fn sigreturn(&mut self, i: usize, slot: usize, m: &[u32; 16]) -> Outcome {
        let uc = self.rd(i, Va::from(m[sys::MR_SP].wrapping_add(signal::UC as u32)), (signal::FRAME as usize) - signal::UC);
        let Some((ctx, mask, alt)) = uc.as_deref().and_then(signal::decode) else {
            self.terminate(i, slot, signal::SIGSEGV, true);
            return Gone;
        };
        let t = &mut self.apps[i].as_mut().unwrap().threads[slot].sig;
        t.blocked = mask;
        // uc_stack, as Linux's restore_altstack: sigaltstack's rules at the
        // frame's sp, and a change they refuse is left unmade.
        if let Ok(next) = signal::alt_change(t.alt, m[sys::MR_SP].into(), alt.sp, alt.flags, alt.size) {
            t.alt = next;
        }
        self.resume(i, slot, ctx);
        Gone
    }

    // ---- timers ----

    fn itimer(&mut self, i: usize, set: bool, which: u32, new: Va, old: Va) -> Outcome {
        if which != 0 {
            return Done(EINVAL); // ITIMER_VIRTUAL and ITIMER_PROF need CPU-time accounting
        }
        let now = sys::now_ns();
        let (out, value) = if set { (old, new) } else { (new, 0) };
        let cur = self.apps[i].as_ref().unwrap().sig.timers.iter().find(|t| t.id == signal::ITIMER).copied().unwrap_or_default();
        let next = if set && value != 0 {
            let Some(b) = self.rd(i, value, 16) else { return Done(EFAULT) };
            let w = |k: usize| u64::from(u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap()));
            Some((w(0) * 1_000_000_000 + w(1).min(999_999) * 1000, w(2) * 1_000_000_000 + w(3).min(999_999) * 1000))
        } else {
            None
        };
        // struct itimerval: the interval, then the time left, as timevals.
        let mut b = [0u8; 16];
        for (k, ns) in [cur.every, cur.left(now)].iter().enumerate() {
            b[8 * k..8 * k + 4].copy_from_slice(&((ns / 1_000_000_000) as u32).to_le_bytes());
            b[8 * k + 4..8 * k + 8].copy_from_slice(&(((ns % 1_000_000_000) / 1000) as u32).to_le_bytes());
        }
        if out != 0 && !self.wr(i, out, &b) {
            return Done(EFAULT);
        }
        if let Some((every, after)) = next {
            let timers = &mut self.apps[i].as_mut().unwrap().sig.timers;
            timers.retain(|t| t.id != signal::ITIMER);
            if after != 0 {
                let (at, every) = (now + signal::timer_ns(after), signal::timer_ns(every));
                timers.push(signal::Timer { id: signal::ITIMER, at, every, sig: signal::SIGALRM, ..Default::default() });
            }
        }
        Done(0)
    }

    /// timer_create(clock, sigevent, id): signals, SIGEV_NONE and SIGEV_THREAD_ID.
    fn timer_create(&mut self, i: usize, clock: u32, sevp: Va, idp: Va) -> Outcome {
        if !matches!(clock, 0 | 1 | 4 | 7 | 8 | 9) {
            return Done(EINVAL); // CPU-time clocks need per-thread accounting
        }
        let Some(id) = signal::free_id(&self.apps[i].as_ref().unwrap().sig.timers) else { return Done(EAGAIN) };
        let mut t = signal::Timer { id, sig: signal::SIGALRM, value: id, clock, ..Default::default() };
        if sevp != 0 {
            // struct sigevent: value, signo, notify, thread id.
            let Some(b) = self.rd(i, sevp, 16) else { return Done(EFAULT) };
            let w = |k: usize| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
            (t.value, t.sig) = (w(0), w(1));
            match w(2) {
                0 => {}
                1 => t.silent = true,
                4 => {
                    let tid = w(3);
                    if !self.apps[i].as_ref().unwrap().threads.iter().any(|th| th.alive && th.tid == tid) {
                        return Done(EINVAL);
                    }
                    t.tid = tid;
                }
                _ => return Done(EINVAL), // SIGEV_THREAD is the C library's (it asks for SIGEV_THREAD_ID)
            }
            if !t.silent && signal::bit(t.sig) == 0 {
                return Done(EINVAL);
            }
        }
        if !self.wr32(i, idp, id) {
            return Done(EFAULT);
        }
        self.apps[i].as_mut().unwrap().sig.timers.push(t);
        Done(0)
    }

    fn timer_settime(&mut self, i: usize, id: u32, flags: u32, new: Va, old: Va, time64: bool) -> Outcome {
        let size: Va = if time64 { 16 } else { 8 };
        let Some(k) = self.apps[i].as_ref().unwrap().sig.timers.iter().position(|t| t.id == id && id != signal::ITIMER) else { return Done(EINVAL) };
        let (Some(every), Some(value)) = (self.timespec(i, new, time64), self.timespec(i, new.wrapping_add(size), time64)) else { return Done(EFAULT) };
        if old != 0 {
            let r = self.timer_gettime(i, id, old, time64);
            if !matches!(r, Done(0)) {
                return r;
            }
        }
        let now = sys::now_ns();
        // TIMER_ABSTIME: a time on the timer's own clock. Expiries and
        // periods are at least signal::TIMER_MIN away.
        let clock = self.apps[i].as_ref().unwrap().sig.timers[k].clock;
        let at = match (value, flags & 1 != 0) {
            (0, _) => 0,
            (v, true) => now + signal::timer_ns(v.saturating_sub(self.clock(clock)).max(1)),
            (v, false) => now + signal::timer_ns(v),
        };
        let t = &mut self.apps[i].as_mut().unwrap().sig.timers[k];
        (t.at, t.every, t.overrun) = (at, signal::timer_ns(every), 0);
        Done(0)
    }

    fn timer_gettime(&mut self, i: usize, id: u32, out: Va, time64: bool) -> Outcome {
        let Some(t) = self.apps[i].as_ref().unwrap().sig.timers.iter().find(|t| t.id == id && id != signal::ITIMER).copied() else { return Done(EINVAL) };
        let size: Va = if time64 { 16 } else { 8 };
        let ok = self.put_timespec(i, out, t.every, time64) && self.put_timespec(i, out.wrapping_add(size), t.left(sys::now_ns()), time64);
        Done(if ok { 0 } else { EFAULT })
    }

    // ---- processes ----

    /// fork, and clone without CLONE_VM: a new app slot with a copy of the
    /// caller's memory (procmgr.rs `fork_into`), its descriptors (shared open
    /// files, as on Linux), signal actions and mask, and one thread: the
    /// caller's, returning 0. It counts against the app limit and never closes
    /// another app to make room. AArch64: the same, through the A64 fork
    /// gate (../arch/signal64.rs FORK_HELPER, abi64.rs `fork_gate64`), the
    /// child starting from the caller's whole context.
    fn fork(&mut self, i: usize, slot: usize, exit_signal: u32, flags: u32, ptid: Va, ctid: Va, m: &[u32; 16]) -> Outcome {
        if self.apps[i].as_ref().unwrap().threads[slot].sig.vfork.is_some() {
            return Done(EAGAIN); // a vfork child has no memory of its own to copy
        }
        // Zombies cost the runtime's heap: reap some first.
        if !signal::room_for_child(&self.apps[i].as_ref().unwrap().sig.children) {
            return Done(EAGAIN);
        }
        let limit = self.app_limit.min(self.untypeds.len());
        let Some(j) = self.apps[..limit].iter().position(|a| a.is_none()) else { return Done(EAGAIN) };
        let (resident, _) = self.apps[i].as_ref().unwrap().space.usage();
        if self.pool.available() < resident + FORK_SPARE {
            return Done(ENOMEM);
        }
        // The runtime cannot read the caller's VFP (AArch64: SIMD)
        // registers (d8-d15 are kept across calls, FPSCR holds its modes):
        // the call goes round the fork gate first, which stores them where
        // the copy takes them. (A clone with a child stack forks here, without them.)
        let child_stack = if m[7] == 120 { vargs(m)[1] } else { 0 };
        if m[7] != signal::FORK_NR && child_stack == 0 {
            #[cfg(target_arch = "aarch64")]
            let gated = self.fork_gate64(i, slot, [Va::from(exit_signal), Va::from(flags), ptid, ctid], m);
            #[cfg(not(target_arch = "aarch64"))]
            let gated = self.fork_gate(i, slot, [exit_signal, flags, ptid as u32, ctid as u32], m);
            if let Some(o) = gated {
                return o;
            }
        }
        let parent = &self.apps[i].as_ref().unwrap().threads[slot];
        let (tcb, blocked, alt) = (parent.tcb, parent.sig.blocked, parent.sig.alt);
        // AArch64: the caller's whole context, its call registers as the svc
        // saw them, x0 = 0, after the svc (in the fork helper, for a gated
        // fork), on the child stack of a clone that names one.
        #[cfg(target_arch = "aarch64")]
        let start = {
            let Ok(mut c) = sys::tcb_read_context(tcb) else { return Done(EAGAIN) };
            let (abi, w) = (super::arch::NATIVE, sys::wide());
            for k in 1..8 {
                c.words[abi.context_arg0 + k] = w[k];
            }
            c.words[abi.context_arg0] = 0;
            c.words[abi.context_pc] = w[MR_FAULT_IP] + 4;
            c.words[abi.context_sp] = if child_stack != 0 { child_stack } else { w[sys::MR_SP] };
            c
        };
        #[cfg(not(target_arch = "aarch64"))]
        let (regs, target) = {
            let Ok(mut regs) = sys::tcb_read(tcb) else { return Done(EAGAIN) };
            (regs.r0, regs.r1, regs.r2, regs.r3, regs.r4, regs.r5, regs.r6, regs.r7) = (0, m[1], m[2], m[3], m[4], m[5], m[6], m[7]);
            regs.r14 = m[sys::MR_LR];
            regs.sp = if child_stack != 0 { m[1] } else { m[sys::MR_SP] };
            (regs, m[MR_FAULT_IP].wrapping_add(4) | (m[MR_CPSR] >> 5) & 1)
        };
        let mut space = match super::procmgr::Space::new(&mut self.k, self.untypeds[j]) {
            Ok(s) => s,
            Err(_) => {
                self.release_objects(j, Vec::new());
                return Done(ENOMEM);
            }
        };
        let k = &mut self.k;
        let made = space.object(k, sys::Object::CNode, super::APP_CNODE_BITS).map_err(|_| ENOMEM).and_then(|cnode| {
            self.apps[i].as_ref().unwrap().space.fork_into(&mut space, k, &mut self.pool).map(|()| cnode)
        });
        let cnode = match made {
            Ok(c) => c,
            Err(e) => {
                space.destroy(&mut self.k, &mut self.pool);
                self.release_objects(j, core::mem::take(&mut space.object_slots));
                return Done(e);
            }
        };
        let pid = self.next_pid_bump();
        let p = self.apps[i].as_ref().unwrap();
        let sig = p.sig.fork_copy(p.pid);
        self.apps[j] = Some(self.child_app(i, pid, space, cnode, sig));
        #[cfg(target_arch = "aarch64")]
        let started = self.spawn64(j, pid, start);
        #[cfg(not(target_arch = "aarch64"))]
        let started = self.spawn(j, pid, regs, target);
        let Ok(s) = started else {
            self.reap(j);
            return Done(EAGAIN);
        };
        let t = &mut self.apps[j].as_mut().unwrap().threads[s];
        (t.sig.blocked, t.sig.alt) = (blocked, alt);
        if flags & CLONE_CHILD_SETTID != 0 {
            self.wr32(j, ctid, pid);
        }
        if flags & CLONE_CHILD_CLEARTID != 0 {
            self.apps[j].as_mut().unwrap().threads[s].clear_tid = ctid;
        }
        if flags & CLONE_PARENT_SETTID != 0 {
            self.wr32(i, ptid, pid);
        }
        self.apps[i].as_mut().unwrap().sig.children.push(signal::Child { pid, status: None, exit_signal });
        Done(pid as i32)
    }

    /// Send the caller of fork through the fork gate: a signal frame with its
    /// context goes below its stack, and it resumes at the signal gate with
    /// the fork helper as the "handler". The gate stores r8-r12 and the VFP
    /// registers in the frame, the helper calls FORK_NR (`fork_done`), and
    /// parent and child each leave through the gate's rt_sigreturn with
    /// fork's result in the frame's r0. None if the frame does not fit.
    #[cfg_attr(target_arch = "aarch64", allow(dead_code))]
    fn fork_gate(&mut self, i: usize, slot: usize, req: [u32; 4], m: &[u32; 16]) -> Option<Outcome> {
        let ctx = self.call_context(i, slot, m, m[0], m[MR_FAULT_IP].wrapping_add(4));
        let app = self.apps[i].as_ref()?;
        let t = &app.threads[slot].sig;
        let at = signal::place(ctx.sp.into(), &t.alt, false)?;
        let bytes = signal::encode(&ctx, &Info::default(), t.blocked, &t.alt, &signal::Trap::default());
        let (gate, at32) = (u32::try_from(app.space.gate()).ok()?, u32::try_from(at).ok()?);
        if !self.wr(i, at, &bytes) {
            return None;
        }
        let helper = gate - signal::GATE_OFFSET + signal::FORK_HELPER;
        self.apps[i].as_mut()?.threads[slot].sig.forking = Some([at, req[0].into(), req[1].into(), req[2].into(), req[3].into()]);
        let r = &ctx.r;
        sys::reply(&[0, at32, at32 + signal::UC as u32, helper, r[4], r[5], r[6], r[7], gate, at32, ctx.lr, signal::user_cpsr(ctx.cpsr)]);
        Some(Gone)
    }
    /// FORK_NR, from the fork helper: the fork itself. The child is the
    /// helper's thread at this call, so it returns into the gate too; each
    /// side's frame gets its result (the child's 0, in the child's copy):
    /// r0 of an AArch32 frame, x0 (a whole 64-bit word) of an AArch64 one.
    fn fork_done(&mut self, i: usize, slot: usize, m: &[u32; 16]) -> Outcome {
        let Some([at, es, flags, ptid, ctid]) = self.apps[i].as_mut().unwrap().threads[slot].sig.forking.take() else { return Done(ENOSYS) };
        let Done(pid) = self.fork(i, slot, es as u32, flags as u32, ptid, ctid, m) else { return Done(EAGAIN) };
        let (r0, word) = if super::vma::WIDE { (super::arch::signal64::REGS, 8) } else { (signal::R0, 4) };
        let r0 = at + r0 as Va;
        self.wr(i, r0, &(pid as i64).to_le_bytes()[..word]);
        if pid > 0 {
            if let Some((j, None)) = self.find_pid(pid as u32) {
                self.wr(j, r0, &[0u8; 8][..word]);
            }
        }
        Done(0)
    }

    /// A new app, as app `p` is (program, descriptors, owner), with no threads.
    fn child_app(&self, p: usize, pid: u32, space: super::procmgr::Space, cnode: u32, sig: signal::Proc<CloseOnExec>) -> super::App {
        let p = self.apps[p].as_ref().unwrap();
        super::App {
            pid,
            name: p.name.clone(),
            path: p.path.clone(),
            space,
            cnode,
            threads: Vec::new(),
            fds: p.fds.clone(),
            cwd: p.cwd.clone(),
            events: Default::default(),
            surface: None,
            exit_code: None,
            digest: p.digest,
            new_hash: p.new_hash,
            owner: p.owner.clone(),
            frozen: None,
            used_ns: sys::now_ns(),
            life: Default::default(),
            sig,
        }
    }

    /// vfork, and clone with CLONE_VM|CLONE_VFORK (posix_spawn): the child is
    /// a thread in the parent's memory with its own process identity; the
    /// parent waits until it calls execve or exits.
    fn vfork(&mut self, i: usize, slot: usize, exit_signal: u32, stack: Va, m: &[u32; 16]) -> Outcome {
        if self.apps[i].as_ref().unwrap().threads[slot].sig.vfork.is_some() {
            return Done(EAGAIN);
        }
        let pid = self.next_pid_bump();
        let app = self.apps[i].as_ref().unwrap();
        let parent = &app.threads[slot];
        let (tcb, blocked) = (parent.tcb, parent.sig.blocked);
        // AArch64: the parent's whole context, its call registers as the svc
        // saw them, x0 = 0, after the svc on the child's stack (as clone).
        #[cfg(target_arch = "aarch64")]
        let start = {
            let Ok(mut c) = sys::tcb_read_context(tcb) else { return Done(EAGAIN) };
            let (abi, w) = (super::arch::NATIVE, sys::wide());
            for k in 1..8 {
                c.words[abi.context_arg0 + k] = w[k];
            }
            c.words[abi.context_arg0] = 0;
            c.words[abi.context_pc] = w[MR_FAULT_IP] + 4;
            c.words[abi.context_sp] = if stack != 0 { stack } else { w[sys::MR_SP] };
            let _ = m;
            c
        };
        #[cfg(not(target_arch = "aarch64"))]
        let (regs, target) = {
            let Ok(mut regs) = sys::tcb_read(tcb) else { return Done(EAGAIN) };
            (regs.r0, regs.r1, regs.r2, regs.r3, regs.r4, regs.r5, regs.r6, regs.r7) = (0, m[1], m[2], m[3], m[4], m[5], m[6], m[7]);
            regs.r14 = m[sys::MR_LR];
            // (AArch32: its stack is a 32-bit address.)
            regs.sp = if stack != 0 { stack as u32 } else { m[sys::MR_SP] };
            (regs, m[MR_FAULT_IP].wrapping_add(4) | (m[MR_CPSR] >> 5) & 1)
        };
        let v = Vfork {
            pid,
            fds: app.fds.clone(),
            cwd: app.cwd.clone(),
            actions: app.sig.actions.clone(),
            cloexec: app.sig.cloexec.clone(),
            children: Vec::new(),
            ppid: app.pid,
            pending: Vec::new(),
            timers: Vec::new(),
            parent_slot: slot,
            swapped: false,
        };
        if !signal::room_for_child(&app.sig.children) {
            return Done(EAGAIN);
        }
        #[cfg(target_arch = "aarch64")]
        let started = self.spawn64(i, pid, start);
        #[cfg(not(target_arch = "aarch64"))]
        let started = self.spawn(i, pid, regs, target);
        let Ok(s) = started else { return Done(EAGAIN) };
        let app = self.apps[i].as_mut().unwrap();
        app.threads[s].sig.blocked = blocked;
        app.threads[s].sig.vfork = Some(alloc::boxed::Box::new(v));
        app.sig.children.push(signal::Child { pid, status: None, exit_signal });
        app.threads[slot].sig.waiting = signal::WaitFor::Vfork { child: pid };
        Block(Wait::Signal, None)
    }

    /// The vfork child in thread `slot` (its identity swapped back out) is
    /// done with its parent's memory: its thread ends, its parent goes on.
    /// `status`: a wait status if it exited, None if it called execve.
    fn vfork_end(&mut self, i: usize, slot: usize, status: Option<u32>) -> Option<Vfork> {
        let app = self.apps[i].as_mut()?;
        let t = &mut app.threads[slot];
        let v = *t.sig.vfork.take()?;
        sys::tcb_suspend(t.tcb);
        t.alive = false;
        // Ended while blocked in a call (a signal): that call is never answered.
        if let Some(r) = t.reply.take() {
            sys::delete(r);
            self.k.release(r);
        }
        (t.wait, t.deadline) = (None, None);
        let parent = v.parent_slot;
        // (A held app's parent keeps the answer until it runs: mod.rs `wake`.)
        if matches!(app.threads[parent].sig.waiting, signal::WaitFor::Vfork { child } if child == v.pid) {
            app.threads[parent].sig.waiting = signal::WaitFor::None;
            self.wake(i, parent, v.pid as i32);
        }
        if let Some(status) = status {
            self.child_ended(i, v.pid, status);
        }
        Some(v)
    }
    fn vfork_exit(&mut self, i: usize, slot: usize, status: u32) -> Outcome {
        // The calling thread's identity is swapped in during its call: give it back first.
        self.vfork_swap(i, slot);
        self.vfork_end(i, slot, Some(status));
        Gone
    }

    /// An ended child's wait status reaches its parent `p`: a zombie for
    /// wait4 (none if the parent ignores SIGCHLD), waiting threads wake, and
    /// the child's exit signal is raised.
    fn child_ended(&mut self, p: usize, pid: u32, status: u32) {
        let Some(app) = self.apps[p].as_mut() else { return };
        let Some(k) = app.sig.children.iter().position(|c| c.pid == pid) else { return };
        app.sig.children[k].status = Some(status);
        let es = app.sig.children[k].exit_signal;
        let a = app.sig.action(signal::SIGCHLD);
        if es == signal::SIGCHLD && (a.handler == signal::SIG_IGN || a.flags & signal::SA_NOCLDWAIT != 0) {
            app.sig.children.remove(k);
        }
        let waiting: Vec<usize> = (0..app.threads.len())
            .filter(|&s| matches!(app.threads[s].wait, Some(Wait::Signal)) && matches!(app.threads[s].sig.waiting, signal::WaitFor::Child { .. }))
            .collect();
        for s in waiting {
            self.wait_again(p, s);
        }
        if es != 0 {
            let (code, st) = signal::child_code(status);
            self.raise(p, None, Info { signo: es, code, a: pid, b: 0, c: st });
        }
    }

    /// The reap hook (mod.rs): app `app` has ended. Its parent learns of it;
    /// its own children have no parent any more.
    pub fn proc_ended(&mut self, app: &super::App) {
        let status = match (app.sig.term, app.exit_code) {
            (0, Some(c)) => signal::exited(c),
            (0, None) => signal::signaled(signal::SIGKILL, false),
            (t, _) => t,
        };
        // Ended during a call of its vfork child: its own identity is in the child's record.
        let (pid, ppid, children) = match app.threads.iter().find_map(|t| t.sig.vfork.as_deref().filter(|v| v.swapped)) {
            Some(v) => (v.pid, v.ppid, &v.children),
            None => (app.pid, app.sig.ppid, &app.sig.children),
        };
        for c in children.iter().filter(|c| c.status.is_none()) {
            if let Some((k, None)) = self.find_pid(c.pid) {
                let s = self.swapped_slot(k);
                self.unswapped(k, s, |rt| rt.apps[k].as_mut().unwrap().sig.ppid = 0);
            }
        }
        if ppid != 0 {
            if let Some((p, None)) = self.find_pid(ppid) {
                // A parent whose vfork child is mid-call gets it in its own fields.
                let s = self.swapped_slot(p);
                self.unswapped(p, s, |rt| rt.child_ended(p, pid, status));
            }
        }
    }
    /// Run `f` with app `i`'s own identity in place (undoing vfork child
    /// `s`'s swap meanwhile, if it still runs after).
    fn unswapped(&mut self, i: usize, s: Option<usize>, f: impl FnOnce(&mut Self)) {
        if let Some(s) = s {
            self.vfork_swap(i, s);
        }
        f(self);
        if let Some(s) = s {
            if self.apps[i].as_ref().and_then(|a| a.threads.get(s)).and_then(|t| t.sig.vfork.as_ref()).is_some_and(|v| !v.swapped) {
                self.vfork_swap(i, s);
            }
        }
    }

    /// Finish a child wait if a child it matches has ended: the status, the
    /// zombie gone (unless WNOWAIT), and the call's result.
    fn wait_result(&mut self, i: usize, k: usize, status: Va, options: u32, rusage: Va, infop: Option<Va>) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        let c = app.sig.children[k];
        if infop.is_none() || options & 0x0100_0000 == 0 {
            app.sig.children.remove(k);
        }
        let st = c.status.unwrap_or(0);
        // The app ABI's struct rusage, all zero: a wait that blocked ends
        // here, past abi64's conversion of the call.
        if rusage != 0 {
            self.wr(i, rusage, &[0u8; super::arch::compat::RUSAGE64][..rusage_bytes()]);
        }
        match infop {
            Some(ptr) => {
                let (code, value) = signal::child_code(st);
                let info = Info { signo: signal::SIGCHLD, code, a: c.pid, b: 0, c: value };
                let ok = ptr == 0 || self.wr(i, ptr, &app_siginfo(&info));
                if ok { 0 } else { EFAULT }
            }
            None => {
                if status != 0 && !self.wr32(i, status, st) {
                    return EFAULT;
                }
                c.pid as i32
            }
        }
    }
    fn wait_again(&mut self, i: usize, s: usize) {
        let Some(app) = self.apps[i].as_ref() else { return };
        let signal::WaitFor::Child { pid, status, options, waitid, rusage } = app.threads[s].sig.waiting else { return };
        let infop = waitid.then_some(status);
        match signal::wait_pick(&app.sig.children, pid, app.pid) {
            Err(()) => self.wake(i, s, ECHILD),
            Ok(None) => {}
            Ok(Some(k)) => {
                let r = self.wait_result(i, k, if waitid { 0 } else { status }, options, rusage, infop);
                self.wake(i, s, r);
            }
        }
    }

    /// wait4(pid, status, options, rusage), and waitid with `infop`.
    fn wait4(&mut self, i: usize, slot: usize, pid: i32, status: Va, options: u32, rusage: Va, infop: Option<Va>) -> Outcome {
        let app = self.apps[i].as_ref().unwrap();
        match signal::wait_pick(&app.sig.children, pid, app.pid) {
            Err(()) => Done(ECHILD),
            Ok(Some(k)) => Done(self.wait_result(i, k, status, options, rusage, infop)),
            Ok(None) if options & 1 != 0 => {
                // WNOHANG; waitid then reports si_pid 0.
                if let Some(ptr) = infop.filter(|&p| p != 0) {
                    self.wr(i, ptr, &[0u8; 128]);
                }
                Done(0)
            }
            Ok(None) => {
                let waiting = signal::WaitFor::Child { pid, status: infop.unwrap_or(status), options, waitid: infop.is_some(), rusage };
                self.apps[i].as_mut().unwrap().threads[slot].sig.waiting = waiting;
                Block(Wait::Signal, None)
            }
        }
    }

    /// argv or envp: a NULL-terminated array of strings (NULL: none).
    fn strings(&mut self, i: usize, mut ptr: Va) -> Result<Vec<String>, i32> {
        let mut out = Vec::new();
        let mut bytes = 0;
        while ptr != 0 {
            let p = self.rdword(i, ptr).ok_or(EFAULT)?;
            if p == 0 {
                break;
            }
            let app = self.apps[i].as_mut().unwrap();
            let s = app.space.read_string(&mut self.k, &mut self.pool, p, signal::ARG_BYTES).ok_or(EFAULT)?;
            bytes += s.len() + 1;
            out.push(s);
            if out.len() > signal::ARG_STRINGS || bytes > signal::ARG_BYTES {
                return Err(E2BIG);
            }
            ptr = ptr.checked_add(if super::vma::WIDE { 8 } else { 4 }).ok_or(EFAULT)?;
        }
        Ok(out)
    }

    /// musl's dynamic loader, only as the system image has it: a copy an app
    /// wrote there is never run as other programs' loader.
    pub fn loader(&self) -> Option<&'static [u8]> {
        let path = super::procmgr::elf::LOADER;
        if self.fs.from_layer(path) {
            return None;
        }
        let c = self.fs.contents(path)?;
        // The file system outlives every program it loads (as in launch).
        Some(unsafe { core::slice::from_raw_parts(c.as_ptr(), c.len()) })
    }

    /// A new address space in app slot `j` with `image` loaded: the space,
    /// its CNode, where it starts. On failure slot j's kernel memory is freed.
    fn load_into(&mut self, j: usize, image: &[u8], ld: Option<&[u8]>, argv: &[String], envp: &[String], random: [u8; 16], layout: super::procmgr::Layout) -> Result<(super::procmgr::Space, u32, super::procmgr::Loaded), &'static str> {
        let mut space = match super::procmgr::Space::new(&mut self.k, self.untypeds[j]) {
            Ok(s) => s,
            Err(_) => {
                self.release_objects(j, Vec::new());
                return Err("no kernel memory");
            }
        };
        space.cap = self.pool.app_cap(); // one app's share of the pool (pool.rs)
        let k = &mut self.k;
        let cnode = space.object(k, sys::Object::CNode, super::APP_CNODE_BITS).map_err(|_| "no kernel memory");
        let loaded = cnode.and_then(|_| super::procmgr::load(&mut space, k, &mut self.pool, image, ld, argv, envp, random, layout));
        let stubs = loaded.and_then(|l| {
            space.add(layout.stubs, layout.stubs + super::procmgr::WINDOW, super::procmgr::PROT_READ | super::procmgr::PROT_EXEC);
            #[cfg(target_arch = "aarch64")]
            if !super::abi64::install_gate(&mut space, k, &mut self.pool) {
                return Err("out of memory");
            }
            space.resolve(k, &mut self.pool, layout.stubs).map(|_| l).ok_or("out of memory")
        });
        match (cnode, stubs) {
            (Ok(c), Ok(l)) => Ok((space, c, l)),
            (_, Err(e)) | (Err(e), _) => {
                space.destroy(&mut self.k, &mut self.pool);
                self.release_objects(j, core::mem::take(&mut space.object_slots));
                Err(e)
            }
        }
    }

    /// Tear app `i` down without ending its process (execve): its threads,
    /// memory and kernel objects go; the App comes back for its new image.
    fn retire(&mut self, i: usize) -> super::App {
        let mut app = self.apps[i].take().unwrap();
        for t in app.threads.iter_mut() {
            if t.alive {
                sys::tcb_suspend(t.tcb);
            }
            if let Some(r) = t.reply.take() {
                sys::delete(r);
                self.k.release(r);
            }
            t.alive = false;
        }
        if let Some(surface) = app.surface.take() {
            for p in surface.pages {
                self.pool.give_back(p);
            }
        }
        app.space.destroy(&mut self.k, &mut self.pool);
        self.release_objects(i, core::mem::take(&mut app.space.object_slots));
        app
    }

    /// execve: the caller's process runs `path` from the start, with fresh
    /// memory at a fresh random layout, argv and envp as given, descriptors
    /// without FD_CLOEXEC, caught signals reset. Everything that can fail is
    /// checked first (the file, "#!", the ELF headers, its loader, Violet
    /// Shield, entropy), so a failed execve returns with the caller intact.
    /// A plain execve rebuilds the caller's own app slot; a vfork child's
    /// takes a free one (the parent's memory stays the parent's).
    fn execve(&mut self, i: usize, slot: usize, path: String, argv_ptr: Va, envp_ptr: Va) -> Outcome {
        let argv = match self.strings(i, argv_ptr) { Ok(v) => v, Err(e) => return Done(e) };
        let envp = match self.strings(i, envp_ptr) { Ok(v) => v, Err(e) => return Done(e) };
        if let Err(e) = self.path_allowed(i, &path, false) {
            return Done(e);
        }
        match self.fs.stat(&path) {
            None => return Done(ENOENT),
            Some(st) if st.mode & S_IFMT == S_IFDIR => return Done(EACCES),
            _ => {}
        }
        let Some(file) = self.fs.contents(&path) else { return Done(EACCES) };
        let digest = super::sentinel::sha256(file);
        let new_hash = self.fs.from_layer(&path);
        let (mut program, mut args) = (path.clone(), argv);
        match signal::shebang(file) {
            None => {}
            Some(Err(())) => return Done(ENOEXEC),
            Some(Ok((interp, arg))) => {
                let mut v = alloc::vec![String::from(interp)];
                v.extend(arg.map(String::from));
                v.push(path.clone());
                v.extend(args.into_iter().skip(1));
                (program, args) = (String::from(interp), v);
            }
        }
        if args.is_empty() {
            args.push(program.clone());
        }
        let mut digest = digest;
        let mut new_hash = new_hash;
        if program != path {
            // The interpreter is a path the caller names too: the same
            // checks as any (a refused or private one is not there).
            program = fs::normalize(&self.apps[i].as_ref().unwrap().cwd, &program);
            args[0] = program.clone();
            if super::trusted_ui::private_path(&program) {
                return Done(ENOENT);
            }
            if let Err(e) = self.path_allowed(i, &program, false) {
                return Done(e);
            }
            match self.fs.stat(&program) {
                None => return Done(ENOENT),
                Some(st) if st.mode & S_IFMT == S_IFDIR => return Done(EACCES),
                _ => {}
            }
            // An interpreter written since boot is what runs: Sentinel and
            // the Shield see it, not the script (an image one keeps the
            // script's hash, as launch does).
            if self.fs.from_layer(&program) {
                new_hash = true;
                digest = self.fs.contents(&program).map_or(digest, super::sentinel::sha256);
            }
        }
        let Some(image) = self.fs.contents(&program) else { return Done(ENOENT) };
        let image: &'static [u8] = unsafe { core::slice::from_raw_parts(image.as_ptr(), image.len()) };
        let Ok(plan) = super::procmgr::elf::plan(image) else { return Done(ENOEXEC) };
        let ld = if plan.interp {
            match self.loader() { Some(l) => Some(l), None => return Done(ENOENT) }
        } else {
            None
        };
        // Violet Shield sees every program start, as for launch: the program
        // that runs (and the script), never the caller's argv[0].
        match self.gate(i, &Action::SpawnApp { path: &signal::exec_line(&program, &args) }) {
            super::Gate::Allow => {}
            super::Gate::Deny => return Done(EACCES),
            super::Gate::Frozen => return Block(Wait::Frozen, None),
            super::Gate::Killed => return Gone,
        }
        let owner = self.shield_owner(&path, &digest, Some(i));
        // Another app's program started by this one is not that app (manifest.rs `exec_identity`).
        let (owner, new_hash) = self.exec_identity(i, &path, owner, new_hash);
        let mut drawn = [0u8; 40];
        if !self.entropy.fill(&mut drawn) && !(super::sys::entropy_collect(&mut self.entropy) && self.entropy.fill(&mut drawn)) {
            return Done(EAGAIN);
        }
        let mut random = [0u8; 16];
        random.copy_from_slice(&drawn[..16]);
        // A fresh random layout of the ABI: vma::Layout64 for an AArch64 app.
        let layout = super::procmgr::Layout::native_random(&drawn[16..]);
        // Everything the loader would refuse (segment places, code sharing a
        // 64 KiB page with data, the loader's room), refused while the caller
        // is still there: after this only running out of memory can fail.
        if let Err(e) = super::procmgr::elf::check(image, ld, &layout) {
            crate::log!("execve {}: {}", program, e);
            return Done(ENOEXEC);
        }
        let _lowered = sys::Lowered::new();
        let name = String::from(path.rsplit('/').next().unwrap_or(&path));
        let vfork = self.apps[i].as_ref().unwrap().threads[slot].sig.vfork.is_some();
        if vfork {
            // A slot of its own. Only an app that may start apps (as
            // "launch" needs) closes background apps for it; any other
            // gets EAGAIN, as fork does, and cannot end other apps.
            let limit = self.app_limit.min(self.untypeds.len());
            let system = self.holds(i, super::policy::Cap::System);
            while system && self.apps[..limit].iter().all(Option::is_some) && self.reclaim(Some(i)) {}
            let Some(j) = self.apps[..limit].iter().position(|a| a.is_none()) else { return Done(EAGAIN) };
            let (mut space, cnode, loaded) = match self.load_into(j, image, ld, &args, &envp, random, layout) {
                Ok(l) => l,
                Err(e) => {
                    crate::log!("execve {}: {}", program, e);
                    return Done(ENOMEM);
                }
            };
            space.join(&self.apps[i].as_ref().unwrap().space); // the parent's family's pages (procmgr.rs)
            // The point of no return: the child leaves its parent's memory.
            let t = &mut self.apps[i].as_mut().unwrap().threads[slot];
            let (blocked, pending) = (t.sig.blocked, core::mem::take(&mut t.sig.pending));
            self.vfork_swap(i, slot);
            let Some(v) = self.vfork_end(i, slot, None) else { return Gone };
            let mut fds = v.fds;
            Self::close_on_exec(&mut fds, &v.cloexec);
            let mut sig = signal::Proc::new(v.ppid);
            (sig.actions, sig.children, sig.pending) = (v.actions, v.children, v.pending);
            sig.exec_reset();
            let mut app = self.child_app(i, v.pid, space, cnode, sig);
            (app.name, app.path, app.fds, app.cwd, app.digest, app.new_hash, app.owner) = (name, path, fds, v.cwd, digest, new_hash, owner);
            self.apps[j] = Some(app);
            self.exec_start(j, v.pid, loaded, blocked, pending, &program);
            return Gone;
        }
        // In place: the caller's slot is rebuilt. A vfork child another
        // thread runs loses the memory it runs in: it ends, killed, and
        // stays a zombie for the new program's wait.
        let others: Vec<usize> = self.apps[i].as_ref().unwrap().threads.iter().enumerate().filter(|(_, t)| t.alive && t.sig.vfork.is_some()).map(|(s, _)| s).collect();
        for s in others {
            if let Some(p) = self.apps[i].as_ref().and_then(|a| a.threads[s].sig.vfork.as_ref()).map(|v| v.parent_slot) {
                self.apps[i].as_mut().unwrap().threads[p].sig.waiting = signal::WaitFor::None; // retired below
            }
            self.vfork_end(i, s, Some(signal::signaled(signal::SIGKILL, false)));
        }
        let Some(app) = self.apps[i].as_mut() else { return Gone };
        let t = &mut app.threads[slot];
        let (blocked, pending) = (t.sig.blocked, core::mem::take(&mut t.sig.pending));
        let mut app = self.retire(i);
        let pid = app.pid;
        match self.load_into(i, image, ld, &args, &envp, random, layout) {
            Ok((mut space, cnode, loaded)) => {
                space.join(&app.space); // still its family's pages (procmgr.rs)
                (app.space, app.cnode, app.threads) = (space, cnode, Vec::new());
                Self::close_on_exec(&mut app.fds, &app.sig.cloexec);
                app.sig.exec_reset();
                (app.name, app.path, app.digest, app.new_hash, app.owner, app.exit_code) = (name, path, digest, new_hash, owner, None);
                self.apps[i] = Some(app);
                self.exec_start(i, pid, loaded, blocked, pending, &program);
            }
            Err(e) => {
                // Past the point of no return (out of memory): the process ends with SIGSEGV.
                crate::log!("{} (pid {}): execve {}: {}", app.name, pid, program, e);
                app.exit_code = Some(139);
                app.sig.term = signal::signaled(signal::SIGSEGV, true);
                app.threads.clear();
                self.apps[i] = Some(app);
                self.reap(i);
            }
        }
        Gone
    }

    /// The first thread of an execve'd program, with the caller's mask and
    /// pending signals.
    fn exec_start(&mut self, j: usize, pid: u32, loaded: super::procmgr::Loaded, blocked: u64, pending: Vec<Info>, program: &str) {
        #[cfg(not(target_arch = "aarch64"))]
        let started = match (u32::try_from(loaded.sp), u32::try_from(loaded.entry)) {
            (Ok(sp), Ok(entry)) => self.spawn(j, pid, sys::Regs { sp, ..Default::default() }, entry),
            _ => Err(EINVAL),
        };
        #[cfg(target_arch = "aarch64")]
        let started = self.spawn64(j, pid, super::arch::NATIVE.start_context(loaded.entry, loaded.sp, 0, 0));
        match started {
            Ok(s) => {
                let app = self.apps[j].as_mut().unwrap();
                (app.threads[s].sig.blocked, app.threads[s].sig.pending) = (blocked, pending);
                let name = app.name.clone();
                self.sentinel.note_exec(pid, &name, &app_digest(self, j));
                crate::log!("pid {} execve {} ({} KiB free)", pid, program, self.pool.available() * sys::PAGE_KIB);
            }
            Err(_) => {
                self.apps[j].as_mut().unwrap().exit_code = Some(126);
                self.reap(j);
            }
        }
    }
}

/// A siginfo_t in the app's ABI: AArch64's union starts at 16, AArch32's at 12.
fn app_siginfo(info: &Info) -> [u8; 128] {
    #[cfg(target_arch = "aarch64")]
    return super::abi64::siginfo(info);
    #[cfg(not(target_arch = "aarch64"))]
    return signal::siginfo(info);
}

fn app_digest(rt: &Runtime, j: usize) -> [u8; 32] {
    rt.apps[j].as_ref().map_or([0; 32], |a| a.digest)
}

/// Bytes of the app ABI's struct rusage (AArch32 72, AArch64 144).
pub fn rusage_bytes() -> usize {
    if super::vma::WIDE { super::arch::compat::RUSAGE64 } else { super::arch::compat::RUSAGE32 }
}
