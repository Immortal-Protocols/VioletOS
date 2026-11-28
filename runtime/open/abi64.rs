//! AArch64 programs' system calls at the edge of the one dispatcher
//! (syscall.rs): each call is translated into the AArch32 call of the same
//! meaning (../arch/enter64.rs) before the dispatcher sees it, and the
//! structures it wrote are made into the AArch64 ones after. The registers
//! the thread gets back are its own 64-bit ones (sys.rs `calls`).
use super::arch::enter64::{self, After, Entry};
use super::syscall::Outcome;
use super::{sys, Runtime};

// The conversion due after the call being dispatched (one at a time).
static mut AFTER: After = After::None;

impl Runtime {
    /// The 32-bit fault message the dispatcher takes for an AArch64 thread's
    /// call, or None if it was answered here (a call with no AArch32 meaning:
    /// ENOSYS; mmap's unaligned offset, lseek's offset beyond an int...).
    pub fn enter64(&mut self, i: usize, r: &sys::Received) -> Option<[u32; 16]> {
        let wide = sys::wide();
        let mut args = [0u64; 6];
        args.copy_from_slice(&wide[..6]);
        let number = wide[sys::MR_SYSCALL];
        match enter64::translate(number, args) {
            Entry::Answer(e) => {
                if e == enter64::ENOSYS {
                    if let Some(app) = self.apps[i].as_ref() {
                        let why = if enter64::unconverted(number) { "its AArch64 structures are not converted yet" } else { "unknown" };
                        crate::log!("{}: unsupported AArch64 system call {} ({})", app.name, number, why);
                    }
                }
                let mut saved = [0u32; 9];
                saved[..8].copy_from_slice(&r.mrs[..8]);
                saved[0] = e as u32;
                saved[8] = r.mrs[sys::MR_FAULT_IP].wrapping_add(4);
                sys::reply(&saved);
                None
            }
            Entry::Call { number, args, after } => {
                let mut mrs = r.mrs;
                mrs[..6].copy_from_slice(&args);
                mrs[7] = number;
                sys::show(&mrs);
                unsafe { *core::ptr::addr_of_mut!(AFTER) = after };
                Some(mrs)
            }
        }
    }

    /// The call has run: what it wrote in the AArch32 layout is rewritten in
    /// the AArch64 one, in the app's own buffer (which is the AArch64
    /// structure's size, so larger than what was written). Any result that
    /// is not an error counts: wait4 answers a pid, times a tick count.
    pub fn after64(&mut self, i: usize, o: Outcome) -> Outcome {
        let after = unsafe { core::mem::replace(&mut *core::ptr::addr_of_mut!(AFTER), After::None) };
        let (Outcome::Done(r), Some((at, from, to))) = (&o, enter64::sizes(after)) else { return o };
        if !enter64::converts(*r) {
            return o;
        }
        let Some(app) = self.apps[i].as_mut() else { return o };
        let mut b = alloc::vec![0u8; from];
        if !app.space.read(&mut self.k, &mut self.pool, at, &mut b) {
            return Outcome::Done(super::syscall::EFAULT);
        }
        let d = enter64::convert(after, &b);
        if !app.space.write(&mut self.k, &mut self.pool, at, &d[..to]) {
            return Outcome::Done(super::syscall::EFAULT);
        }
        o
    }
}

// ---- signals ----
//
// A handler runs as on Linux AArch64 (../arch/signal64.rs): the frame
// (siginfo, ucontext with every general register, the fpsimd record the gate
// fills) below the thread's stack or on its alternate one, and the thread
// restarted at the A64 signal gate in the runtime's trampoline page, which
// saves the SIMD registers, calls the handler and makes rt_sigreturn. The
// thread's registers are written whole (tcb_write_context), so it resumes
// from a system call, a saved reply or a fault the same way.

use super::arch::{signal64, Context};
use super::signal::{self, Info};
use super::procmgr::{Space, WINDOW};
use super::pool::{Kernel, Pool};

/// seL4_UserContext (runtime/arch Context) as the frame's registers.
pub fn regs_of(c: &Context) -> signal64::Regs {
    let w = &c.words;
    let mut r = signal64::Regs { pc: w[0], sp: w[1], pstate: w[2], ..Default::default() };
    r.x[..9].copy_from_slice(&w[3..12]); // x0..x8
    (r.x[16], r.x[17], r.x[18], r.x[29], r.x[30]) = (w[12], w[13], w[14], w[15], w[16]);
    r.x[9..16].copy_from_slice(&w[17..24]); // x9..x15
    r.x[19..29].copy_from_slice(&w[24..34]); // x19..x28
    r
}
pub fn put_regs(c: &mut Context, r: &signal64::Regs) {
    let w = &mut c.words;
    (w[0], w[1], w[2]) = (r.pc, r.sp, r.pstate);
    w[3..12].copy_from_slice(&r.x[..9]);
    (w[12], w[13], w[14], w[15], w[16]) = (r.x[16], r.x[17], r.x[18], r.x[29], r.x[30]);
    w[17..24].copy_from_slice(&r.x[9..16]);
    w[24..34].copy_from_slice(&r.x[19..29]);
}

/// Write the A64 signal gate (and fork helper after it) into the app's
/// trampoline page, which the app has read-execute and cannot change.
pub fn install_gate(space: &mut Space, k: &mut Kernel, pool: &mut Pool) -> bool {
    let stubs = space.layout.stubs;
    let Some(at) = space.resolve(k, pool, stubs + signal64::GATE_OFFSET as u32) else { return false };
    for (n, word) in signal64::GATE.iter().enumerate() {
        unsafe { ((at + 4 * n) as *mut u32).write_volatile(*word) };
    }
    space.sync_code(stubs, stubs + WINDOW);
    true
}

/// A 64-bit siginfo for `info` (the union at 16, not AArch32's 12).
pub fn siginfo(info: &Info) -> [u8; 128] {
    signal64::siginfo(info.signo, info.code, &[info.a, info.b, info.c])
}

impl Runtime {
    /// The registers of thread `slot` as a call left them, answered as the
    /// 32-bit `ctx` says (x0, pc), its other call registers its own 64-bit
    /// ones (`reply`: the saved reply it waits on, or the call being served).
    pub fn call_regs64(&self, i: usize, slot: usize, ctx: &signal::Context, reply: Option<u32>) -> Option<signal64::Regs> {
        let tcb = self.apps[i].as_ref()?.threads.get(slot)?.tcb;
        let mut r = regs_of(&sys::tcb_read_context(tcb).ok()?);
        let w = sys::widened(reply, &[ctx.r[0], ctx.r[1], ctx.r[2], ctx.r[3], ctx.r[4], ctx.r[5], ctx.r[6], ctx.r[7], ctx.pc, ctx.sp, ctx.lr, ctx.cpsr]);
        r.x[..8].copy_from_slice(&w[..8]);
        // (Message word 10 is ELR_EL1 on AArch64, not x30: x30 is the context's.)
        (r.pc, r.sp, r.pstate) = (w[8], w[9], w[11]);
        Some(r)
    }

    /// Start `info`'s handler on thread `slot`, stopped at `regs` (`fault`:
    /// the address and ESR of the fault it takes, for siginfo and the
    /// esr_context record). None: no room for the frame, or the thread is
    /// gone; the caller ends it with SIGSEGV, as Linux does.
    pub fn deliver64(&mut self, i: usize, slot: usize, regs: &signal64::Regs, info: &Info, fault: Option<(u64, u64)>) -> Option<()> {
        let app = self.apps[i].as_ref()?;
        let a = app.sig.action(info.signo);
        let t = &app.threads[slot];
        let tcb = t.tcb;
        let alt = signal64::AltStack { sp: t.sig.alt.sp.into(), flags: t.sig.alt.flags, size: t.sig.alt.size.into() };
        let at = signal64::place(regs.sp, &alt, a.flags & signal::SA_ONSTACK != 0)?;
        let at32 = u32::try_from(at).ok()?;
        let mask = t.sig.suspend_mask.unwrap_or(t.sig.blocked);
        let info_bytes = match fault {
            Some((address, _)) => signal64::siginfo_fault(info.signo, info.code, address),
            None => siginfo(info),
        };
        let mut frame = alloc::vec![0u8; signal64::TOTAL];
        signal64::encode(&mut frame, regs, &info_bytes, mask, &alt, fault.map_or(0, |f| f.0), fault.map(|f| f.1))?;
        let gate = u64::from(app.space.layout.stubs) + signal64::GATE_OFFSET;
        let app = self.apps[i].as_mut()?;
        if !app.space.write(&mut self.k, &mut self.pool, at32, &frame) {
            return None;
        }
        let t = &mut app.threads[slot].sig;
        t.blocked = signal::handler_mask(t.blocked, &a, info.signo);
        t.suspend_mask = None;
        t.waiting = signal::WaitFor::None;
        if a.flags & signal::SA_RESETHAND != 0 {
            app.sig.set_action(info.signo, Default::default());
        }
        let entry = signal64::gate_entry(regs, info.signo, at, a.handler.into(), gate);
        let mut c = sys::tcb_read_context(tcb).ok()?;
        put_regs(&mut c, &entry);
        sys::tcb_write_context(tcb, true, &c).ok()
    }

    /// rt_sigreturn: the registers, mask and alternate stack from the frame
    /// at the thread's stack (the gate calls it with sp at the frame),
    /// checked (signal64::decode); a bad frame ends it with SIGSEGV.
    pub fn sigreturn64(&mut self, i: usize, slot: usize) -> Outcome {
        let w = sys::wide();
        let Some(tcb) = self.apps[i].as_ref().map(|a| a.threads[slot].tcb) else { return Outcome::Gone };
        let mut frame = alloc::vec![0u8; signal64::FRAME];
        let read = u32::try_from(w[sys::MR_SP]).ok().is_some_and(|sp| {
            let app = self.apps[i].as_mut().unwrap();
            app.space.read(&mut self.k, &mut self.pool, sp, &mut frame)
        });
        let restored = if read { signal64::decode(&frame, u64::from(super::vma::USER_TOP), w[sys::MR_CPSR]) } else { None };
        let started = restored.and_then(|r| {
            let mut c = sys::tcb_read_context(tcb).ok()?;
            put_regs(&mut c, &r.regs);
            let t = &mut self.apps[i].as_mut()?.threads[slot].sig;
            t.blocked = r.mask & !signal::UNBLOCKABLE;
            // uc_stack, as Linux's restore_altstack: sigaltstack's rules at
            // the frame's sp, and a change they refuse is left unmade.
            let sp = w[sys::MR_SP] as u32;
            if let Ok(alt) = signal::alt_change(t.alt, sp, r.alt.sp, r.alt.flags, r.alt.size) {
                t.alt = alt;
            }
            sys::tcb_write_context(tcb, true, &c).ok()
        });
        if started.is_none() {
            self.terminate(i, slot, signal::SIGSEGV, true);
        }
        Outcome::Gone
    }
}
