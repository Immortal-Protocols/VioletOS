//! The Linux AArch64 signal frame (arch/arm64 `struct rt_sigframe`), the
//! signal gate that runs a handler and returns through `rt_sigreturn`, and the
//! fork helper beside it. Pure code: the Open Runtime writes the bytes into
//! the app's stack itself (runtime/open/signal.rs holds the AArch32 frame).
//!
//! Layout (byte offsets from the frame, which is where `sp` points while the
//! handler runs):
//!
//! | Offset | What |
//! | --- | --- |
//! | 0 | siginfo (128) |
//! | 128 | ucontext: uc_flags, uc_link, uc_stack (24), uc_sigmask (8 + 120 unused) |
//! | 304 | uc_mcontext: fault_address, x0..x30, sp, pc, pstate |
//! | 592 | __reserved (4096): fpsimd_context (528), esr_context (16, faults only), the null record |
//! | 4688 | the frame record {x29, x30} the handler's x29 points at |
//!
//! seL4's fault messages carry no SIMD state, so the runtime writes only the
//! fpsimd_context header; the gate stores q0-q31, FPSR and FPCR into it before
//! calling the handler and loads them back after, so a handler can read or
//! change them, as on Linux. The layout of the gate's stores is the frame's
//! (tests/host.rs decodes the instruction offsets and checks them).

pub const INFO: usize = 0;
pub const UC: usize = 128;
pub const UC_STACK: usize = UC + 16;
pub const UC_SIGMASK: usize = UC + 40;
/// uc_mcontext, 16-byte aligned after the 168 bytes above it.
pub const MCONTEXT: usize = UC + 176;
pub const FAULT_ADDRESS: usize = MCONTEXT;
pub const REGS: usize = MCONTEXT + 8;
pub const SP: usize = REGS + 31 * 8;
pub const PC: usize = SP + 8;
pub const PSTATE: usize = PC + 8;
/// sigcontext.__reserved: the list of context records.
pub const RESERVED: usize = MCONTEXT + 288;
pub const RESERVED_BYTES: usize = 4096;
pub const FPSIMD: usize = RESERVED;
pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
pub const FPSIMD_SIZE: u32 = 528;
pub const FPSR: usize = FPSIMD + 8;
pub const FPCR: usize = FPSIMD + 12;
pub const VREGS: usize = FPSIMD + 16;
pub const ESR_MAGIC: u32 = 0x4553_5201;
pub const ESR_SIZE: u32 = 16;
/// The whole rt_sigframe.
pub const FRAME: usize = RESERVED + RESERVED_BYTES;
/// The frame and the {fp, lr} record above it: what delivery takes from the stack.
pub const TOTAL: usize = FRAME + 16;

/// SIGKILL and SIGSTOP, which no mask holds.
pub const UNBLOCKABLE: u64 = 1 << 8 | 1 << 18;
/// pstate bits a frame may change on return: the condition flags only.
/// Everything else (exception level, interrupt masks, single step) stays what
/// the kernel gives an EL0 thread.
pub const PSTATE_USER: u64 = 0xf000_0000;
const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;

/// A thread's general registers as the frame keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Regs {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

/// sigaltstack, in the AArch64 layout (ss_sp, ss_flags, ss_size).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AltStack {
    pub sp: u64,
    pub flags: u32,
    pub size: u64,
}
impl AltStack {
    pub fn on(&self, sp: u64) -> bool {
        self.flags & SS_DISABLE == 0 && sp.wrapping_sub(self.sp) < self.size
    }
    /// ss_flags as sigaltstack and uc_stack report them for a thread at `sp`.
    pub fn report(&self, sp: u64) -> u32 {
        if self.flags & SS_DISABLE != 0 { SS_DISABLE } else if self.on(sp) { SS_ONSTACK } else { 0 }
    }
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn get64(b: &[u8], at: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(w)
}
fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A 64-bit siginfo_t: si_signo, si_errno, si_code, then the union at 16
/// (8-aligned on AArch64, unlike AArch32's 12), filled with `fields` as
/// 32-bit words (si_pid, si_uid, si_status, ...).
pub fn siginfo(signo: u32, code: i32, fields: &[u32]) -> [u8; 128] {
    let mut b = [0u8; 128];
    put32(&mut b, 0, signo);
    put32(&mut b, 8, code as u32);
    for (k, v) in fields.iter().take(28).enumerate() {
        put32(&mut b, 16 + 4 * k, *v);
    }
    b
}
/// The siginfo of a fault: si_addr is a 64-bit pointer at 16.
pub fn siginfo_fault(signo: u32, code: i32, address: u64) -> [u8; 128] {
    let mut b = siginfo(signo, code, &[]);
    put64(&mut b, 16, address);
    b
}

/// Where a frame for a thread at `sp` goes: below `sp`, or at the top of the
/// alternate stack for an SA_ONSTACK handler not already on it, 16-byte
/// aligned with the frame record above it (as arch/arm64 get_sigframe).
/// None when it does not fit (Linux then kills the thread with SIGSEGV).
pub fn place(sp: u64, alt: &AltStack, onstack: bool) -> Option<u64> {
    let switch = onstack && alt.flags & SS_DISABLE == 0 && !alt.on(sp);
    let top = if switch { alt.sp.checked_add(alt.size)? } else { sp };
    let record = top.checked_sub(16)? & !15;
    let frame = record.checked_sub(FRAME as u64)? & !15;
    if (switch || alt.on(sp)) && frame < alt.sp {
        return None;
    }
    Some(frame)
}

/// Fill `frame` (TOTAL bytes, written at `place`'s address) for a signal
/// interrupting `regs`. `esr` is the ESR_EL1 of a fault (it adds the
/// esr_context record Linux gives a fault's handler). The SIMD registers are
/// left for the gate.
pub fn encode(frame: &mut [u8], regs: &Regs, info: &[u8; 128], mask: u64, alt: &AltStack, fault_address: u64, esr: Option<u64>) -> Option<()> {
    let frame = frame.get_mut(..TOTAL)?;
    frame.fill(0);
    frame[INFO..INFO + 128].copy_from_slice(info);
    put64(frame, UC_STACK, alt.sp);
    put32(frame, UC_STACK + 8, alt.report(regs.sp));
    put64(frame, UC_STACK + 16, alt.size);
    put64(frame, UC_SIGMASK, mask & !UNBLOCKABLE);
    put64(frame, FAULT_ADDRESS, fault_address);
    for (k, v) in regs.x.iter().enumerate() {
        put64(frame, REGS + 8 * k, *v);
    }
    put64(frame, SP, regs.sp);
    put64(frame, PC, regs.pc);
    put64(frame, PSTATE, regs.pstate);
    put32(frame, FPSIMD, FPSIMD_MAGIC);
    put32(frame, FPSIMD + 4, FPSIMD_SIZE);
    let mut end = FPSIMD + FPSIMD_SIZE as usize;
    if let Some(esr) = esr {
        put32(frame, end, ESR_MAGIC);
        put32(frame, end + 4, ESR_SIZE);
        put64(frame, end + 8, esr);
        end += ESR_SIZE as usize;
    }
    // The null record ends the list (already zero).
    let _ = end;
    // The frame record: the interrupted frame pointer and link register.
    put64(frame, FRAME, regs.x[29]);
    put64(frame, FRAME + 8, regs.x[30]);
    Some(())
}

/// What `rt_sigreturn` restores from a frame the app may have changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restored {
    pub regs: Regs,
    pub mask: u64,
    pub alt: AltStack,
}

/// Read a frame back for `rt_sigreturn`, checking it as arch/arm64
/// restore_sigframe does: the pc must be a 4-byte aligned user address, sp a
/// user address, and the record list must hold the fpsimd_context and end
/// with the null record inside __reserved. `interrupted_pstate` is the
/// thread's own (the frame may change only its condition flags).
/// None: a bad frame, which Linux answers with SIGSEGV.
pub fn decode(frame: &[u8], user_top: u64, interrupted_pstate: u64) -> Option<Restored> {
    let frame = frame.get(..FRAME)?;
    let mut regs = Regs::default();
    for k in 0..31 {
        regs.x[k] = get64(frame, REGS + 8 * k);
    }
    regs.sp = get64(frame, SP);
    regs.pc = get64(frame, PC);
    if regs.pc % 4 != 0 || regs.pc >= user_top || regs.sp >= user_top {
        return None;
    }
    regs.pstate = (interrupted_pstate & !PSTATE_USER) | (get64(frame, PSTATE) & PSTATE_USER);
    // The context records.
    let mut at = RESERVED;
    let mut fpsimd = false;
    loop {
        if at + 8 > RESERVED + RESERVED_BYTES {
            return None;
        }
        let (magic, size) = (get32(frame, at), get32(frame, at + 4) as usize);
        match magic {
            0 if size == 0 => break,
            FPSIMD_MAGIC if size == FPSIMD_SIZE as usize && !fpsimd => fpsimd = true,
            ESR_MAGIC if size == ESR_SIZE as usize => {}
            _ => return None,
        }
        at += size;
    }
    if !fpsimd {
        return None;
    }
    let mask = get64(frame, UC_SIGMASK) & !UNBLOCKABLE;
    let alt = AltStack { sp: get64(frame, UC_STACK), flags: get32(frame, UC_STACK + 8), size: get64(frame, UC_STACK + 16) };
    Some(Restored { regs, mask, alt })
}

/// The registers a thread enters the gate with: x0 = signal, x1 = siginfo,
/// x2 = ucontext, x3 = handler, x29 = the frame record, sp = the frame,
/// pc = the gate. The rest are the interrupted thread's.
pub fn gate_entry(regs: &Regs, signo: u32, frame: u64, handler: u64, gate: u64) -> Regs {
    let mut r = *regs;
    r.x[0] = u64::from(signo);
    r.x[1] = frame + INFO as u64;
    r.x[2] = frame + UC as u64;
    r.x[3] = handler;
    r.x[29] = frame + FRAME as u64;
    r.sp = frame;
    r.pc = gate;
    r
}

/// The AArch64 signal gate, at GATE_OFFSET in the app's trampoline page
/// (read-execute, as on AArch32). Entered as `gate_entry` sets the registers.
/// x10-x12 are scratch: rt_sigreturn restores every register from the frame.
pub const GATE_OFFSET: u64 = 0xff00;
pub const GATE: [u32; 47] = [
    0x910983ea, // add x10, sp, #608        (VREGS)
    0xad000540, // stp q0, q1, [x10]
    0xad010d42, // stp q2, q3, [x10, #0x20]
    0xad021544, // stp q4, q5, [x10, #0x40]
    0xad031d46, // stp q6, q7, [x10, #0x60]
    0xad042548, // stp q8, q9, [x10, #0x80]
    0xad052d4a, // stp q10, q11, [x10, #0xa0]
    0xad06354c, // stp q12, q13, [x10, #0xc0]
    0xad073d4e, // stp q14, q15, [x10, #0xe0]
    0xad084550, // stp q16, q17, [x10, #0x100]
    0xad094d52, // stp q18, q19, [x10, #0x120]
    0xad0a5554, // stp q20, q21, [x10, #0x140]
    0xad0b5d56, // stp q22, q23, [x10, #0x160]
    0xad0c6558, // stp q24, q25, [x10, #0x180]
    0xad0d6d5a, // stp q26, q27, [x10, #0x1a0]
    0xad0e755c, // stp q28, q29, [x10, #0x1c0]
    0xad0f7d5e, // stp q30, q31, [x10, #0x1e0]
    0xd53b442b, // mrs x11, fpsr
    0xd53b440c, // mrs x12, fpcr
    0x293f314b, // stp w11, w12, [x10, #-8] (FPSR, FPCR)
    0xd63f0060, // blr x3
    0x910983ea, // add x10, sp, #608
    0x297f314b, // ldp w11, w12, [x10, #-8]
    0xd51b442b, // msr fpsr, x11
    0xd51b440c, // msr fpcr, x12
    0xad400540, // ldp q0, q1, [x10]
    0xad410d42, // ldp q2, q3, [x10, #0x20]
    0xad421544, // ldp q4, q5, [x10, #0x40]
    0xad431d46, // ldp q6, q7, [x10, #0x60]
    0xad442548, // ldp q8, q9, [x10, #0x80]
    0xad452d4a, // ldp q10, q11, [x10, #0xa0]
    0xad46354c, // ldp q12, q13, [x10, #0xc0]
    0xad473d4e, // ldp q14, q15, [x10, #0xe0]
    0xad484550, // ldp q16, q17, [x10, #0x100]
    0xad494d52, // ldp q18, q19, [x10, #0x120]
    0xad4a5554, // ldp q20, q21, [x10, #0x140]
    0xad4b5d56, // ldp q22, q23, [x10, #0x160]
    0xad4c6558, // ldp q24, q25, [x10, #0x180]
    0xad4d6d5a, // ldp q26, q27, [x10, #0x1a0]
    0xad4e755c, // ldp q28, q29, [x10, #0x1c0]
    0xad4f7d5e, // ldp q30, q31, [x10, #0x1e0]
    0xd2801168, // mov x8, #139             (rt_sigreturn)
    0xd4000001, // svc #0
    0xd4200000, // brk #0
    // The fork helper (FORK_HELPER), called by the gate like a handler, as
    // on AArch32: the frame then holds the caller's registers and SIMD state,
    // fork copies them into the child, and both return through the gate.
    0xd29e0008, // mov x8, #0xf000          (FORK_NR)
    0xd4000001, // svc #0
    0xd65f03c0, // ret
];
pub const FORK_HELPER: u64 = GATE_OFFSET + 44 * 4;
/// The private call the fork helper makes. AArch64 Linux numbers stay below
/// 0x1000 and the kernel patch turns anything from 2^31 up into 0x10000, so
/// a number in between is the runtime's own.
pub const FORK_NR: u64 = 0xf000;
