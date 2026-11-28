//! Linux signals and process bookkeeping for open apps, as plain data and
//! functions with no kernel calls, so tests/abi_host.rs runs them on the host.
//! syscall.rs ("signals, processes") does the delivery with seL4.
//!
//! How a handler runs (README, "Signals"): the runtime writes a Linux ARM
//! rt_sigframe (siginfo, then a ucontext whose register space has the VFP
//! block musl and the kernel use) on the app's stack or its alternate stack,
//! and starts the thread at the signal gate, ARM code in the runtime's
//! trampoline page: it stores r8-r12 and d0-d31/FPSCR into the frame (the
//! runtime cannot reach another thread's VFP registers), calls the handler,
//! loads them back from the frame (so a handler that edits the context is
//! obeyed) and calls rt_sigreturn. The libc's sa_restorer is not used.
use super::vma::Va;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGSEGV: u32 = 11;
pub const SIGALRM: u32 = 14;
pub const SIGCHLD: u32 = 17;
pub const SIGSTOP: u32 = 19;
pub const NSIG: u32 = 64;

pub const SIG_DFL: Va = 0;
pub const SIG_IGN: Va = 1;
pub const SA_NOCLDWAIT: u32 = 2;
pub const SA_ONSTACK: u32 = 0x0800_0000;
pub const SA_RESTART: u32 = 0x1000_0000;
pub const SA_NODEFER: u32 = 0x4000_0000;
pub const SA_RESETHAND: u32 = 0x8000_0000;
pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;
pub const MINSIGSTKSZ: u32 = 2048;

pub const SI_USER: i32 = 0;
pub const SI_TIMER: i32 = -2;
pub const SI_TKILL: i32 = -6;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
pub const BUS_ADRALN: i32 = 1;
pub const BUS_ADRERR: i32 = 2;
pub const ILL_ILLOPC: i32 = 1;
pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_DUMPED: i32 = 3;

/// The bit of `sig` in a signal set (0 outside 1..=64).
pub fn bit(sig: u32) -> u64 {
    if (1..=NSIG).contains(&sig) { 1 << (sig - 1) } else { 0 }
}
pub const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));
/// Faults the hardware raises: taken before any other pending signal.
const SYNCHRONOUS: u64 = (1 << 3) | (1 << 4) | (1 << 6) | (1 << 7) | (1 << 10) | (1 << 30);

/// A signal action. The handler and restorer are app addresses, whole for
/// either ABI (an AArch64 handler lies above 4 GiB, vma::Layout64).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Action {
    pub handler: Va,
    pub flags: u32,
    pub restorer: Va,
    pub mask: u64,
}

/// One pending signal: what siginfo says (si_code, then three words whose
/// meaning depends on it: pid/uid, the fault address, timer id/overrun/value,
/// a child's pid/uid/status; all at siginfo offsets 12, 16 and 20).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Info {
    pub signo: u32,
    pub code: i32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AltStack {
    pub sp: Va,
    pub size: Va,
    pub flags: u32,
}
impl Default for AltStack {
    fn default() -> Self {
        AltStack { sp: 0, size: 0, flags: SS_DISABLE }
    }
}
impl AltStack {
    pub fn on(&self, sp: Va) -> bool {
        self.flags & SS_DISABLE == 0 && sp > self.sp && sp - self.sp <= self.size
    }
    /// ss_flags as sigaltstack and uc_stack report them.
    pub fn report(&self, sp: Va) -> u32 {
        if self.flags & SS_DISABLE != 0 { SS_DISABLE } else if self.on(sp) { SS_ONSTACK } else { 0 }
    }
}

/// The alternate stack a thread at `sp` with `current` gets from a stack_t
/// of (ss_sp, flags, size), by sigaltstack's rules (Linux do_sigaltstack):
/// EPERM while on the current one, EINVAL for unknown flags, ENOMEM for a
/// stack under MINSIGSTKSZ, EFAULT for one that would wrap the address space.
/// sigaltstack answers the error; rt_sigreturn (restore_altstack) leaves the
/// stack as it was.
pub fn alt_change(current: AltStack, sp: Va, ss_sp: u64, flags: u32, size: u64) -> Result<AltStack, i32> {
    const EPERM: i32 = -1;
    const EFAULT: i32 = -14;
    const ENOMEM: i32 = -12;
    const EINVAL: i32 = -22;
    if flags & SS_DISABLE == 0 && ss_sp.checked_add(size).is_none() {
        return Err(EFAULT);
    }
    if current.on(sp) {
        return Err(EPERM);
    }
    if flags & !(SS_DISABLE | SS_ONSTACK | 0x8000_0000) != 0 {
        return Err(EINVAL);
    }
    if flags & SS_DISABLE != 0 {
        Ok(AltStack::default())
    } else if size < u64::from(MINSIGSTKSZ) {
        Err(ENOMEM)
    } else {
        Ok(AltStack { sp: ss_sp, size, flags: 0 })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disposition {
    Ignore,
    Terminate { core: bool },
    /// Job control is not modelled: a stop signal with its default action is discarded.
    Stop,
    Handle,
}

/// What `sig` does under `a`.
pub fn disposition(a: &Action, sig: u32) -> Disposition {
    if sig == SIGKILL {
        return Disposition::Terminate { core: false };
    }
    if sig == SIGSTOP {
        return Disposition::Stop;
    }
    match a.handler {
        SIG_IGN => Disposition::Ignore,
        SIG_DFL => match sig {
            17 | 18 | 23 | 28 => Disposition::Ignore,
            20 | 21 | 22 => Disposition::Stop,
            3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31 => Disposition::Terminate { core: true },
            _ => Disposition::Terminate { core: false },
        },
        _ => Disposition::Handle,
    }
}

/// Add `info` to a pending queue. Standard signals (below 32) are pending at
/// most once; real-time ones queue, up to 64 in all. False if dropped.
pub fn queue(q: &mut Vec<Info>, info: Info) -> bool {
    if bit(info.signo) == 0 || (info.signo < 32 && q.iter().any(|p| p.signo == info.signo)) || q.len() >= 64 {
        return false;
    }
    q.push(info);
    true
}
pub fn set_of(q: &[Info]) -> u64 {
    q.iter().fold(0, |s, p| s | bit(p.signo))
}
/// The signal in `q` and `allowed` to deliver next: faults first, then the
/// lowest number (and the oldest instance of it).
pub fn pick(q: &[Info], allowed: u64) -> Option<usize> {
    let ready = set_of(q) & allowed;
    if ready == 0 {
        return None;
    }
    let set = if ready & SYNCHRONOUS != 0 { ready & SYNCHRONOUS } else { ready };
    let sig = set.trailing_zeros() + 1;
    q.iter().position(|p| p.signo == sig)
}
pub fn take(q: &mut Vec<Info>, allowed: u64) -> Option<Info> {
    pick(q, allowed).map(|k| q.remove(k))
}
pub fn discard(q: &mut Vec<Info>, sig: u32) {
    q.retain(|p| p.signo != sig);
}

/// Per process: actions, process-directed pending signals, the parent,
/// children (running or zombie), timers, the signal that ended it, and the
/// descriptors to close at exec (`C` is the runtime's record of one).
pub struct Proc<C> {
    pub actions: Box<[Action; 64]>,
    pub pending: Vec<Info>,
    pub ppid: u32,
    pub children: Vec<Child>,
    pub timers: Vec<Timer>,
    /// The wait status of the signal that ended the process (0: none).
    pub term: u32,
    pub cloexec: Vec<C>,
}

impl<C: Clone> Proc<C> {
    pub fn new(ppid: u32) -> Self {
        Proc { actions: Box::new([Action::default(); 64]), pending: Vec::new(), ppid, children: Vec::new(), timers: Vec::new(), term: 0, cloexec: Vec::new() }
    }
    pub fn action(&self, sig: u32) -> Action {
        if bit(sig) == 0 { Action::default() } else { self.actions[sig as usize - 1] }
    }
    /// Set an action (callers refuse SIGKILL and SIGSTOP). A signal that is
    /// now ignored is discarded from the process queue (the caller does the
    /// threads' queues with `discard`).
    pub fn set_action(&mut self, sig: u32, mut a: Action) {
        if bit(sig) == 0 {
            return;
        }
        a.mask &= !UNBLOCKABLE;
        self.actions[sig as usize - 1] = a;
        if disposition(&a, sig) == Disposition::Ignore {
            discard(&mut self.pending, sig);
        }
    }
    /// What fork gives the child: the same actions and close-on-exec records,
    /// nothing pending, no children, no timers.
    pub fn fork_copy(&self, ppid: u32) -> Self {
        Proc { actions: self.actions.clone(), pending: Vec::new(), ppid, children: Vec::new(), timers: Vec::new(), term: 0, cloexec: self.cloexec.clone() }
    }
    /// What survives exec: caught signals go back to their default action,
    /// ignored ones stay ignored; POSIX timers are deleted (the interval
    /// timer stays); pending signals and children stay.
    pub fn exec_reset(&mut self) {
        for a in self.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = Action::default();
            }
        }
        self.timers.retain(|t| t.id == ITIMER);
        self.cloexec.clear();
    }
}

/// What a thread in Wait::Signal waits for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitFor {
    None,
    /// pause or sigsuspend: any handled signal ends it with EINTR.
    Suspend,
    /// sigtimedwait: a signal in `set` is taken and its siginfo written to `info`.
    Set { set: u64, info: Va },
    /// wait4/waitid on children matching `pid` (as wait4's argument).
    /// (`status` and `rusage`: app addresses, 0 for none.)
    Child { pid: i32, status: Va, options: u32, waitid: bool, rusage: Va },
    /// The parent of a vfork child, until the child calls execve or exits.
    Vfork { child: u32 },
}

pub struct Thread<V> {
    pub blocked: u64,
    pub pending: Vec<Info>,
    pub alt: AltStack,
    pub waiting: WaitFor,
    /// sigsuspend: the mask to restore once a handler has run.
    pub suspend_mask: Option<u64>,
    /// Faults delivered to a handler with no system call between: a handler
    /// that returns without fixing the cause is stopped after FAULT_LIMIT.
    pub faults: u32,
    /// The process identity of a vfork child running in this thread.
    pub vfork: Option<Box<V>>,
    /// A fork on its way through the fork gate: the frame holding the
    /// caller's context and VFP registers, then fork's exit signal, clone
    /// flags, parent and child tid pointers.
    /// A fork through the gate: the frame, exit signal, flags, ptid, ctid.
    pub forking: Option<[Va; 5]>,
    /// Its call waits only because its app is held (parked, or another
    /// process of its owner is frozen): it is made again once released.
    pub held_call: bool,
}
pub const FAULT_LIMIT: u32 = 256;
/// Whether system call `nr` shows the program got past a fault: any but
/// rt_sigreturn, which ends every handler, so a handler that just returns
/// to the same fault keeps counting.
pub fn fault_reset(nr: u32) -> bool {
    nr != 173
}
/// Whether a data abort's fault status (DFSR) is an alignment fault, in the
/// short-descriptor format (FS 0b00001) or LPAE's (status 0b100001).
pub fn alignment_fault(fsr: u32) -> bool {
    if fsr & (1 << 9) != 0 { fsr & 0x3f == 0x21 } else { (fsr & 0xf) | (fsr >> 6 & 0x10) == 1 }
}
/// Children, running or zombie, a process may have: fork and vfork return
/// EAGAIN beyond this until it reaps some (each costs the runtime's heap).
pub const MAX_CHILDREN: usize = 64;
pub fn room_for_child(children: &[Child]) -> bool {
    children.len() < MAX_CHILDREN
}

impl<V> Thread<V> {
    /// A new thread inherits its creator's mask and nothing else.
    pub fn new(blocked: u64) -> Self {
        Thread { blocked: blocked & !UNBLOCKABLE, pending: Vec::new(), alt: AltStack::default(), waiting: WaitFor::None, suspend_mask: None, faults: 0, vfork: None, forking: None, held_call: false }
    }
}

/// sigprocmask: the new mask from `how` (block, unblock, set) and `set`.
pub fn mask_change(old: u64, how: u32, set: u64) -> Option<u64> {
    let new = match how {
        0 => old | set,
        1 => old & !set,
        2 => set,
        _ => return None,
    };
    Some(new & !UNBLOCKABLE)
}
/// The mask while a handler for `sig` runs.
pub fn handler_mask(blocked: u64, a: &Action, sig: u32) -> u64 {
    let own = if a.flags & SA_NODEFER != 0 { 0 } else { bit(sig) };
    (blocked | a.mask | own) & !UNBLOCKABLE
}

// ---- the signal frame (Linux arch/arm rt_sigframe) ----

/// siginfo (128 bytes), ucontext (744), retcode (16).
pub const FRAME: u32 = 888;
pub const UC: usize = 128;
/// uc_mcontext (struct sigcontext): trap_no, error_code, oldmask, r0..r10,
/// fp, ip, sp, lr, pc, cpsr, fault_address.
pub const MCONTEXT: usize = UC + 20;
pub const R0: usize = MCONTEXT + 12;
/// Where the gate stores r8-r12.
pub const R8: usize = R0 + 32;
pub const SIGMASK: usize = UC + 104;
/// uc_regspace: struct vfp_sigframe (magic, size, d0-d31, fpscr, fpexc,
/// fpinst, fpinst2), then a zero word that ends the list.
pub const VFP: usize = UC + 232;
pub const VFP_MAGIC: u32 = 0x5646_5001;
pub const VFP_SIZE: u32 = 288;

/// A thread's registers as a signal frame keeps them.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Context {
    pub r: [u32; 13],
    pub sp: u32,
    pub lr: u32,
    pub pc: u32,
    pub cpsr: u32,
}

/// Fault details for sigcontext: trap_no, error_code (the fault status), the address.
#[derive(Clone, Copy, Default)]
pub struct Trap {
    pub no: u32,
    pub error: u32,
    pub addr: u32,
}

fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn get(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A siginfo_t (sigtimedwait, waitid and the frame).
pub fn siginfo(info: &Info) -> [u8; 128] {
    let mut b = [0u8; 128];
    for (k, v) in [info.signo, 0, info.code as u32, info.a, info.b, info.c].iter().enumerate() {
        put(&mut b, 4 * k, *v);
    }
    b
}

/// The frame bytes; `alt` fills uc_stack. The VFP registers (and r8-r12
/// when they are not known) are left for the gate to store.
pub fn encode(ctx: &Context, info: &Info, mask: u64, alt: &AltStack, trap: &Trap) -> Vec<u8> {
    let mut b = alloc::vec![0u8; FRAME as usize];
    b[..128].copy_from_slice(&siginfo(info));
    // (An AArch32 frame: its app's addresses are 32-bit, so these are whole.)
    put(&mut b, UC + 8, alt.sp as u32);
    put(&mut b, UC + 12, alt.report(ctx.sp.into()));
    put(&mut b, UC + 16, alt.size.min(u64::from(u32::MAX)) as u32);
    put(&mut b, MCONTEXT, trap.no);
    put(&mut b, MCONTEXT + 4, trap.error);
    put(&mut b, MCONTEXT + 8, mask as u32);
    for (k, v) in ctx.r.iter().chain([ctx.sp, ctx.lr, ctx.pc, ctx.cpsr, trap.addr].iter()).enumerate() {
        put(&mut b, R0 + 4 * k, *v);
    }
    put(&mut b, SIGMASK, mask as u32);
    put(&mut b, SIGMASK + 4, (mask >> 32) as u32);
    put(&mut b, VFP, VFP_MAGIC);
    put(&mut b, VFP + 4, VFP_SIZE);
    b
}

/// Read back a ucontext (`uc` is the 744 bytes at frame + 128): the
/// registers, the signal mask and uc_stack.
pub fn decode(uc: &[u8]) -> Option<(Context, u64, AltStack)> {
    if uc.len() < SIGMASK + 8 - UC {
        return None;
    }
    let w = |at: usize| get(uc, at - UC);
    let mut ctx = Context::default();
    for k in 0..13 {
        ctx.r[k] = w(R0 + 4 * k);
    }
    ctx.sp = w(R0 + 52);
    ctx.lr = w(R0 + 56);
    ctx.pc = w(R0 + 60);
    ctx.cpsr = w(R0 + 64);
    let mask = (u64::from(w(SIGMASK)) | u64::from(w(SIGMASK + 4)) << 32) & !UNBLOCKABLE;
    let alt = AltStack { sp: w(UC + 8).into(), flags: w(UC + 12), size: w(UC + 16).into() };
    Some((ctx, mask, alt))
}

/// Where the frame goes: below `sp`, or at the top of the alternate stack
/// for an SA_ONSTACK handler when the thread is not already on it. None if it
/// does not fit there (the thread is then ended with SIGSEGV, as on Linux).
pub fn place(sp: Va, alt: &AltStack, onstack: bool) -> Option<Va> {
    let switch = onstack && alt.flags & SS_DISABLE == 0 && !alt.on(sp);
    let top = if switch { alt.sp.checked_add(alt.size)? } else { sp };
    let frame = top.checked_sub(FRAME.into())? & !7;
    if (switch || alt.on(sp)) && frame < alt.sp {
        return None;
    }
    Some(frame)
}

/// The CPSR a written context gets: the flags, in user mode (seL4 clears the
/// rest, Thumb included; Thumb code resumes through a trampoline).
pub fn user_cpsr(cpsr: u32) -> u32 {
    (cpsr & 0xf800_0000) | 0x10
}
pub fn thumb(cpsr: u32) -> bool {
    cpsr & 0x20 != 0
}
/// Whether a thread stopped at an arbitrary instruction can take a signal
/// now: a register write would lose the GE flags and a Thumb IT block's
/// state, so with either set the runtime tries again on a later pass.
pub fn async_ok(cpsr: u32) -> bool {
    cpsr & 0x000f_0000 == 0 && (!thumb(cpsr) || cpsr & 0x0600_fc00 == 0)
}

/// The signal gate, ARM code at GATE_OFFSET in the trampoline page. Entered
/// with r0 = signal, r1 = siginfo, r2 = ucontext, r3 = handler, sp = frame.
/// (tests/abi_host.rs checks these words against the frame offsets above.)
pub const GATE_OFFSET: u32 = 0xff00;
pub const GATE: [u32; 21] = [
    0xe28d40c0, // add r4, sp, #192        (R8)
    0xe8841f00, // stm r4, {r8-r12}
    0xe28d4e17, // add r4, sp, #368        (VFP + 8)
    0xeca40b20, // vstmia r4!, {d0-d15}
    0xece40b20, // vstmia r4!, {d16-d31}
    0xeef15a10, // vmrs r5, fpscr
    0xe5845000, // str r5, [r4]
    0xe12fff33, // blx r3
    0xe28d4e17, // add r4, sp, #368
    0xecb40b20, // vldmia r4!, {d0-d15}
    0xecf40b20, // vldmia r4!, {d16-d31}
    0xe5945000, // ldr r5, [r4]
    0xeee15a10, // vmsr fpscr, r5
    0xe28d40c0, // add r4, sp, #192
    0xe8941f00, // ldm r4, {r8-r12}
    0xe3a070ad, // mov r7, #173            (rt_sigreturn)
    0xef000000, // svc #0
    0xe7f000f0, // udf #0
    // The fork helper (FORK_HELPER), called by the gate like a handler: the
    // frame below sp then holds the caller's context and VFP registers, and
    // fork copies them into the child; both return through the gate.
    0xe3a0780f, // mov r7, #0xf0000        (FORK_NR)
    0xef000000, // svc #0
    0xe12fff1e, // bx lr
];
/// The fork helper's address in the trampoline page, and the private call
/// it makes (ARM's __ARM_NR_BASE, which Linux never gives a meaning).
pub const FORK_HELPER: u32 = GATE_OFFSET + 18 * 4;
pub const FORK_NR: u32 = 0xf0000;

// ---- interrupted system calls ----

/// Whether a blocking call interrupted by a handler with SA_RESTART is
/// started again (the rest return EINTR, as Linux's poll, select, epoll,
/// sleeps, connect and the signal waits do whatever the flag).
pub fn restarts(nr: u32, flags: u32) -> bool {
    flags & SA_RESTART != 0
        && matches!(nr, 3 | 4 | 145 | 146 | 114 | 280 | 240 | 422 | 54 | 285 | 289..=292 | 296 | 297 | 366)
}

// ---- timers ----

/// Timer id of the interval timer (setitimer ITIMER_REAL, alarm).
pub const ITIMER: u32 = u32::MAX;
/// The shortest timer period and first expiry (1 ms): a shorter one would
/// keep the runtime, which runs above every app, from ever sleeping.
pub const TIMER_MIN: u64 = 1_000_000;
/// A requested expiry or period, at least TIMER_MIN (0 stays 0: disarmed, or one-shot).
pub fn timer_ns(v: u64) -> u64 {
    if v == 0 { 0 } else { v.max(TIMER_MIN) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Timer {
    pub id: u32,
    /// Next expiry (monotonic ns); 0 is disarmed.
    pub at: u64,
    pub every: u64,
    pub sig: u32,
    /// A thread to signal (SIGEV_THREAD_ID), or 0 for the process.
    pub tid: u32,
    pub value: u32,
    pub overrun: u32,
    /// sigev_notify is SIGEV_NONE: it runs but signals nothing.
    pub silent: bool,
    /// The clock timer_create named (TIMER_ABSTIME values are on it).
    pub clock: u32,
}

impl Timer {
    /// At `now`: whether it fired. A periodic timer moves to its next expiry
    /// after `now` and counts the periods it skipped as overruns.
    pub fn expire(&mut self, now: u64) -> bool {
        if self.at == 0 || now < self.at {
            return false;
        }
        if self.every == 0 {
            self.at = 0;
            self.overrun = 0;
        } else {
            // (Arming clamps the period; this keeps a bad one from spinning too.)
            let every = self.every.max(TIMER_MIN);
            let missed = (now - self.at) / every;
            self.overrun = missed.min(i32::MAX as u64) as u32;
            self.at += (missed + 1) * every;
        }
        true
    }
    pub fn left(&self, now: u64) -> u64 {
        if self.at == 0 { 0 } else { self.at.saturating_sub(now).max(1) }
    }
}
/// The next expiry, never sooner than TIMER_MIN after `now`.
pub fn soonest(timers: &[Timer], now: u64) -> Option<u64> {
    timers.iter().filter(|t| t.at != 0).map(|t| t.at.max(now + TIMER_MIN)).min()
}
/// The smallest free timer id.
pub fn free_id(timers: &[Timer]) -> Option<u32> {
    (0..32u32).find(|id| !timers.iter().any(|t| t.id == *id))
}

// ---- children and exit status ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Child {
    pub pid: u32,
    /// Its wait status once it has ended (a zombie).
    pub status: Option<u32>,
    /// The signal its end sends the parent (clone's low byte; fork: SIGCHLD).
    pub exit_signal: u32,
}

pub fn exited(code: i32) -> u32 {
    ((code as u32) & 0xff) << 8
}
pub fn signaled(sig: u32, core: bool) -> u32 {
    (sig & 0x7f) | if core { 0x80 } else { 0 }
}
/// SIGCHLD's si_code and si_status for a wait status.
pub fn child_code(status: u32) -> (i32, u32) {
    if status & 0x7f == 0 {
        (CLD_EXITED, (status >> 8) & 0xff)
    } else {
        (if status & 0x80 != 0 { CLD_DUMPED } else { CLD_KILLED }, status & 0x7f)
    }
}

/// wait4's choice among `children` for argument `pid` (process groups are
/// not modelled: every child is in the caller's group, whose id is its pid).
/// Err: no child matches (ECHILD). Ok(None): matching children are all running.
pub fn wait_pick(children: &[Child], pid: i32, me: u32) -> Result<Option<usize>, ()> {
    let matches = |c: &Child| match pid {
        -1 | 0 => true,
        p if p > 0 => c.pid == p as u32,
        p => p.unsigned_abs() == me,
    };
    let mut any = false;
    for (k, c) in children.iter().enumerate() {
        if matches(c) {
            any = true;
            if c.status.is_some() {
                return Ok(Some(k));
            }
        }
    }
    if any { Ok(None) } else { Err(()) }
}

/// The kill rule: a process may signal itself and its descendants only.
/// `chain` is the target's ancestry, parent first.
pub fn may_signal(sender: u32, target: u32, chain: &[u32]) -> bool {
    target == sender || chain.contains(&sender)
}

// ---- execve ----

pub const ARG_STRINGS: usize = 4096;
pub const ARG_BYTES: usize = 256 * 1024;

/// The program line Violet Shield sees for an execve: the program that
/// runs (for a script, its interpreter, then the script), then the
/// arguments after argv[0], which is the caller's to choose and never
/// stands for the program.
pub fn exec_line(program: &str, args: &[alloc::string::String]) -> alloc::string::String {
    let mut line = alloc::string::String::from(program);
    for a in args.iter().skip(1) {
        line.push(' ');
        line.push_str(a);
    }
    line
}

/// A "#!" line: the interpreter and its one optional argument, as Linux
/// splits it. Err if the line is too long or names nothing.
pub fn shebang(image: &[u8]) -> Option<Result<(&str, Option<&str>), ()>> {
    if !image.starts_with(b"#!") {
        return None;
    }
    let end = image.iter().take(256).position(|&b| b == b'\n');
    let Some(end) = end.or((image.len() <= 256).then_some(image.len())) else { return Some(Err(())) };
    let Ok(line) = core::str::from_utf8(&image[2..end]) else { return Some(Err(())) };
    let line = line.trim_matches(|c| c == ' ' || c == '\t' || c == '\r');
    let (interp, arg) = match line.find([' ', '\t']) {
        Some(k) => (&line[..k], Some(line[k..].trim_matches(|c| c == ' ' || c == '\t')).filter(|a| !a.is_empty())),
        None => (line, None),
    };
    if interp.is_empty() { Some(Err(())) } else { Some(Ok((interp, arg))) }
}
