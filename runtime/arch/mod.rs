//! Everything that differs between the AArch32 and AArch64 ports, in one place.
//!
//! Pure `no_std` code with no kernel access: the two `Abi` tables, decoding of
//! the kernel's fault messages into Linux system calls, the thread register
//! layouts, the page-table walk that maps one frame, ELF32/ELF64 parsing and
//! the initial user stack. The kernel is reached only through the `Kernel`
//! trait, so the same code runs on the host against a model of seL4's page
//! tables (tests/host.rs) and on the target (targets/qemu64/root.rs).
//!
//! Both tables are always compiled, whatever the target: a test on a 64-bit
//! Mac checks the AArch32 one too. `NATIVE` is the table of the target being
//! built.

#[path = "linux.rs"]
pub mod linux;
#[path = "signal64.rs"]
pub mod signal64;
#[path = "compat.rs"]
pub mod compat;
#[path = "enter64.rs"]
pub mod enter64;

/// A machine word of the *described* architecture, wide enough for both.
pub type Word = u64;

/// What differs between the two user ABIs.
#[derive(Debug, PartialEq, Eq)]
pub struct Abi {
    pub name: &'static str,
    pub word_bits: u32,
    /// ELF e_ident[EI_CLASS] and e_machine of a program for this ABI.
    pub elf_class: u8,
    pub elf_machine: u16,
    /// One past the highest user address the kernel configuration allows.
    pub user_top: Word,
    /// Stack alignment at a function call and at process entry.
    pub stack_align: Word,
    /// Frame sizes the kernel offers, as address bits, smallest first.
    pub frame_bits: &'static [u8],
    /// Address bits spanned by one table at each level below the root,
    /// outermost first. A frame of `b` bits needs every table whose span is
    /// larger than `b`.
    pub table_spans: &'static [u8],
    /// Words in the kernel's user context (seL4_UserContext), and where the
    /// fields this code sets live in it.
    pub context_words: usize,
    pub context_pc: usize,
    pub context_sp: usize,
    pub context_status: usize,
    pub context_arg0: usize,
    /// The thread-pointer register an ordinary user thread reads its TLS from.
    pub context_tls: usize,
    /// The status register value of a fresh user thread.
    pub user_status: Word,
    /// Status-register bit that says the faulting instruction was 2 bytes long
    /// (Thumb), or 0 when every instruction is 4 bytes.
    pub status_short_instruction: Word,
    /// Whether the kernel's TLS register is written with a system call
    /// (AArch32: TPIDRURO through seL4_TCB_SetTLSBase) or by the thread itself
    /// (AArch64: `msr tpidr_el0`, saved and restored with the context).
    pub tls_by_syscall: bool,
    /// Linux `AT_PLATFORM` string and the page size reported in `AT_PAGESZ`.
    pub platform: &'static str,
    pub page_size: Word,
}

pub const AARCH32: Abi = Abi {
    name: "aarch32",
    word_bits: 32,
    elf_class: 1,
    elf_machine: 40,
    user_top: 0xe000_0000,
    stack_align: 8,
    frame_bits: &[12, 16, 20, 24],
    table_spans: &[20],
    // pc sp cpsr r0 r1 r8 r9 r10 r11 r12 r2 r3 r4 r5 r6 r7 r14 tpidrurw tpidruro
    context_words: 19,
    context_pc: 0,
    context_sp: 1,
    context_status: 2,
    context_arg0: 3,
    context_tls: 18,
    user_status: 0x150, // user mode, asynchronous aborts and FIQ masked
    status_short_instruction: 1 << 5,
    tls_by_syscall: true,
    platform: "v7l",
    page_size: 4096,
};

pub const AARCH64: Abi = Abi {
    name: "aarch64",
    word_bits: 64,
    elf_class: 2,
    elf_machine: 183,
    user_top: 0x0000_8000_0000_0000,
    stack_align: 16,
    frame_bits: &[12, 21, 30],
    table_spans: &[39, 30, 21],
    // pc sp spsr x0..x8 x16 x17 x18 x29 x30 x9..x15 x19..x28 tpidr_el0 tpidrro_el0
    context_words: 36,
    context_pc: 0,
    context_sp: 1,
    context_status: 2,
    context_arg0: 3,
    context_tls: 34,
    user_status: 0, // EL0t, interrupts enabled
    status_short_instruction: 0,
    tls_by_syscall: false,
    platform: "aarch64",
    page_size: 4096,
};

#[cfg(target_pointer_width = "64")]
pub const NATIVE: &Abi = &AARCH64;
#[cfg(target_pointer_width = "32")]
pub const NATIVE: &Abi = &AARCH32;

impl Abi {
    pub fn word_bytes(&self) -> usize {
        self.word_bits as usize / 8
    }
    /// Is `[address, address + length)` a non-empty range of user addresses?
    pub fn user_range(&self, address: Word, length: Word) -> bool {
        length != 0 && address.checked_add(length).is_some_and(|end| end <= self.user_top)
    }
    /// How many page tables lie between the root and a frame of `bits`.
    pub fn tables_for(&self, bits: u8) -> usize {
        self.table_spans.iter().filter(|&&span| span > bits).count()
    }
    /// The fresh context of a thread entering at `pc` with stack `sp`.
    pub fn start_context(&self, pc: Word, sp: Word, arg0: Word, tls: Word) -> Context {
        let mut context = Context { words: [0; MAX_CONTEXT] };
        context.words[self.context_pc] = pc;
        context.words[self.context_sp] = sp & !(self.stack_align - 1);
        context.words[self.context_status] = self.user_status;
        context.words[self.context_arg0] = arg0;
        context.words[self.context_tls] = tls;
        context
    }
}

pub const MAX_CONTEXT: usize = 36;
/// seL4_UserContext for either architecture; the first `Abi::context_words` are used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub words: [Word; MAX_CONTEXT],
}

// ---- The generic timer ----------------------------------------------------

/// The virtual counter of the ARM generic timer, which both kernels let user
/// level read (KernelArmExportVCNTUser). The one place the two instruction
/// encodings live: CP15 on AArch32, a system register on AArch64.
#[cfg(all(target_os = "none", target_arch = "aarch64"))]
pub fn counter() -> u64 {
    let ticks: u64;
    unsafe { core::arch::asm!("isb", "mrs {t}, cntvct_el0", t = out(reg) ticks, options(nomem, nostack, preserves_flags)) };
    ticks
}
#[cfg(all(target_os = "none", target_arch = "arm"))]
pub fn counter() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("isb", "mrrc p15, 1, {lo}, {hi}, c14", lo = out(reg) lo, hi = out(reg) hi, options(nomem, nostack, preserves_flags)) };
    u64::from(hi) << 32 | u64::from(lo)
}
/// The counter's frequency in Hz, as the firmware (or QEMU) programmed it.
#[cfg(all(target_os = "none", target_arch = "aarch64"))]
pub fn counter_frequency() -> u64 {
    let hz: u64;
    unsafe { core::arch::asm!("mrs {f}, cntfrq_el0", f = out(reg) hz, options(nomem, nostack, preserves_flags)) };
    hz & 0xffff_ffff
}
#[cfg(all(target_os = "none", target_arch = "arm"))]
pub fn counter_frequency() -> u64 {
    let hz: u32;
    unsafe { core::arch::asm!("mrc p15, 0, {f}, c14, c0, 0", f = out(reg) hz, options(nomem, nostack, preserves_flags)) };
    u64::from(hz)
}

// ---- Fault messages -------------------------------------------------------

/// Fault labels (seL4_Fault_tag) of the non-MCS kernels both ports build.
pub const FAULT_CAP: Word = 1;
pub const FAULT_UNKNOWN_SYSCALL: Word = 2;
pub const FAULT_USER_EXCEPTION: Word = 3;
pub const FAULT_VM: Word = 5;

/// The unknown-syscall fault message is laid out alike on both: eight
/// argument registers, then FaultIP, SP, LR, the status register and the
/// number the kernel dispatched on.
pub const SYSCALL_WORDS: usize = 13;
/// Words of that message the kernel copies back into the thread on reply.
pub const SYSCALL_REPLY_WORDS: usize = 12;
const MR_IP: usize = 8;
const MR_SP: usize = 9;
const MR_STATUS: usize = 11;
const MR_NUMBER: usize = 12;

/// A Linux system call, as a thread made it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Syscall {
    /// The number in the ABI's own numbering (linux::number() names it).
    pub number: Word,
    pub args: [Word; 6],
    /// Address of the trapping instruction, and the stack pointer.
    pub pc: Word,
    pub sp: Word,
}

/// Decode an unknown-syscall fault. `None` when the message is not one a user
/// thread of this ABI can have produced (a number that is not a Linux number).
///
/// AArch32: the number is r7, which is also what the kernel reports, because
/// seL4's own numbers are negative there. AArch64: Linux passes it in x8 and
/// leaves x7 alone; kernel/sel4/aarch64/linux-svc.patch makes the kernel
/// dispatch `svc #0` on x8, so the reported number is x8.
pub fn decode_syscall(abi: &Abi, mrs: &[Word; SYSCALL_WORDS]) -> Option<Syscall> {
    let mask = if abi.word_bits == 32 { 0xffff_ffff } else { Word::MAX };
    let number = mrs[MR_NUMBER] & mask;
    // Linux numbers are small and positive on both. Anything else is not a
    // system call this runtime knows how to answer.
    if number >= 0x1_0000 && !(abi.word_bits == 32 && (0x0f_0000..0x0f_0010).contains(&number)) {
        return None;
    }
    let mut args = [0; 6];
    for (i, a) in args.iter_mut().enumerate() {
        *a = mrs[i] & mask;
    }
    Some(Syscall { number, args, pc: mrs[MR_IP] & mask, sp: mrs[MR_SP] & mask })
}

/// Turn the fault message into the reply that returns `value` from the call:
/// the result register is set and the thread resumes after the instruction.
/// Every other register in the message is left as the thread had it.
pub fn syscall_return(abi: &Abi, mrs: &mut [Word; SYSCALL_WORDS], value: i64) {
    let mask = if abi.word_bits == 32 { 0xffff_ffff } else { Word::MAX };
    mrs[0] = value as Word & mask;
    let short = abi.status_short_instruction != 0 && mrs[MR_STATUS] & abi.status_short_instruction != 0;
    mrs[MR_IP] = mrs[MR_IP].wrapping_add(if short { 2 } else { 4 }) & mask;
}

/// The Open Runtime's AArch32 fork-helper call (signal.rs FORK_NR), which
/// the AArch64 helper's own number (signal64::FORK_NR) stands for.
pub const FORK_NR32: u32 = 0xf_0000;

/// The call a thread of `abi` made, in the one numbering the Open Runtime's
/// dispatcher, policy and Shield hooks know (AArch32's), and whether its
/// arguments need the other ABI's structure layouts (compat.rs).
///
/// AArch32 numbers pass unchanged. An AArch64 number is translated through
/// linux.rs, never passed on raw: 63 is AArch64's read but AArch32's dup2.
/// An AArch64 number the table lacks is None (ENOSYS), and so is every
/// AArch32-only call (open, fork, the 0xf0000 private range) made by number
/// from an AArch64 program.
pub fn internal_call(abi: &Abi, number: Word) -> Option<(u32, linux::Layout)> {
    if abi.word_bits == 32 {
        return u32::try_from(number).ok().map(|n| (n, linux::Layout::Same));
    }
    if number == signal64::FORK_NR {
        return Some((FORK_NR32, linux::Layout::Same));
    }
    linux::aarch64(number).map(|call| (u32::from(call.aarch32), call.layout))
}

/// A virtual-memory fault: where, at which instruction, and what the thread was doing.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct VmFault {
    pub pc: Word,
    pub address: Word,
    pub access: Access,
}
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Access {
    Read,
    Write,
    Execute,
}

/// Decode a VM fault message (IP, address, prefetch flag, fault status).
pub fn decode_vm_fault(abi: &Abi, mrs: &[Word; 4]) -> VmFault {
    let access = if mrs[2] != 0 {
        Access::Execute
    } else if abi.word_bits == 32 {
        // DFSR: WnR is bit 11.
        if mrs[3] & (1 << 11) != 0 { Access::Write } else { Access::Read }
    } else {
        // ESR_EL1 of a data abort from EL0: WnR is bit 6.
        if mrs[3] & (1 << 6) != 0 { Access::Write } else { Access::Read }
    };
    VmFault { pc: mrs[0], address: mrs[1], access }
}

// ---- Address spaces -------------------------------------------------------

/// What a mapping lets the thread do. Never write and execute together.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Protection {
    ReadOnly,
    ReadWrite,
    ReadExecute,
}

/// The kernel's answer to a mapping request, as far as the walk cares.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MapError {
    /// A page table between the root and the frame is missing (seL4_FailedLookup).
    MissingTable,
    /// Something is already mapped there (seL4_DeleteFirst).
    Occupied,
    /// No memory or capability slot left for a table.
    Exhausted,
    /// Any other kernel error, with its code.
    Kernel(u32),
}

/// The kernel operations the walk needs. Capabilities are opaque numbers.
pub trait Kernel {
    /// Map `frame` (of `bits` address bits) at `va` in `vspace`.
    fn map_frame(&mut self, frame: usize, vspace: usize, va: Word, bits: u8, protection: Protection, device: bool) -> Result<(), MapError>;
    /// A fresh, empty page table.
    fn new_table(&mut self) -> Result<usize, MapError>;
    /// Install `table` at the outermost level still missing on the way to `va`.
    fn map_table(&mut self, table: usize, vspace: usize, va: Word) -> Result<(), MapError>;
}

/// Map one frame, creating the page tables the address still lacks.
///
/// The kernel says when a level is missing; each retry adds exactly one, so
/// the loop is bounded by the ABI's table depth. Returns how many tables it
/// created. Refuses (without touching the kernel) a frame size the ABI does
/// not have, a misaligned address, and any range outside user space.
pub fn map_frame<K: Kernel>(abi: &Abi, kernel: &mut K, vspace: usize, frame: usize, va: Word, bits: u8,
                            protection: Protection, device: bool) -> Result<usize, MapError> {
    if !abi.frame_bits.contains(&bits) || va & ((1 << bits) - 1) != 0 || !abi.user_range(va, 1 << bits) {
        return Err(MapError::Kernel(u32::MAX));
    }
    let mut created = 0;
    loop {
        match kernel.map_frame(frame, vspace, va, bits, protection, device) {
            Err(MapError::MissingTable) if created < abi.tables_for(bits) => {
                let table = kernel.new_table()?;
                kernel.map_table(table, vspace, va)?;
                created += 1;
            }
            other => return other.map(|()| created),
        }
    }
}

// ---- Programs -------------------------------------------------------------

/// One loadable segment of a program.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Segment {
    pub va: Word,
    pub offset: Word,
    pub file_bytes: Word,
    pub memory_bytes: Word,
    pub protection: Protection,
}

impl Segment {
    /// The whole pages the segment occupies: `[first, last)`, `None` if
    /// rounding up overflows. Checked, so a loader never computes it alone.
    pub fn page_span(&self, page: Word) -> Option<(Word, Word)> {
        let end = self.va.checked_add(self.memory_bytes)?;
        let last = end.checked_add(page - 1)? & !(page - 1);
        Some((self.va & !(page - 1), last))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ElfError {
    Truncated,
    /// Not an ELF file, or one for another architecture or byte order.
    WrongFormat,
    /// Only statically linked, fixed-address executables (ET_EXEC) load here.
    NotStatic,
    /// A segment is both writable and executable.
    WritableCode,
    /// A segment lies outside the file or outside user space.
    BadSegment,
}

/// A parsed ELF32 or ELF64 executable: the entry point and its PT_LOAD segments.
pub struct Elf<'a> {
    abi: &'a Abi,
    image: &'a [u8],
    pub entry: Word,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

fn le(image: &[u8], offset: usize, bytes: usize) -> Option<Word> {
    let slice = image.get(offset..offset.checked_add(bytes)?)?;
    let mut value = 0;
    for (i, b) in slice.iter().enumerate() {
        value |= Word::from(*b) << (8 * i);
    }
    Some(value)
}

impl<'a> Elf<'a> {
    pub fn parse(abi: &'a Abi, image: &'a [u8]) -> Result<Self, ElfError> {
        let ident = image.get(..16).ok_or(ElfError::Truncated)?;
        if &ident[..4] != b"\x7fELF" || ident[4] != abi.elf_class || ident[5] != 1 {
            return Err(ElfError::WrongFormat);
        }
        let wide = abi.elf_class == 2;
        let field = |o32: usize, o64: usize, b32: usize, b64: usize| {
            le(image, if wide { o64 } else { o32 }, if wide { b64 } else { b32 }).ok_or(ElfError::Truncated)
        };
        if le(image, 18, 2).ok_or(ElfError::Truncated)? != Word::from(abi.elf_machine) {
            return Err(ElfError::WrongFormat);
        }
        if le(image, 16, 2).ok_or(ElfError::Truncated)? != 2 {
            return Err(ElfError::NotStatic);
        }
        let elf = Elf {
            abi,
            image,
            entry: field(24, 24, 4, 8)?,
            phoff: field(28, 32, 4, 8)? as usize,
            phentsize: field(42, 54, 2, 2)? as usize,
            phnum: field(44, 56, 2, 2)? as usize,
        };
        if elf.phentsize < if wide { 56 } else { 32 } || elf.phnum > 64 {
            return Err(ElfError::WrongFormat);
        }
        // Validate every segment now, so a loader never sees a bad one: each
        // lies in user space (an empty one too, though no loader is handed
        // it), and no two share a page, so a page-granular loader never has to
        // merge two protections (which could make a page writable and executable).
        let mut loads = 0;
        let mut spans = [(0 as Word, 0 as Word); 64];
        for i in 0..elf.phnum {
            if let Some(segment) = elf.segment(i)? {
                if !abi.user_range(segment.va, segment.memory_bytes.max(1)) {
                    return Err(ElfError::BadSegment);
                }
                if segment.memory_bytes == 0 {
                    continue;
                }
                let span = segment.page_span(abi.page_size).ok_or(ElfError::BadSegment)?;
                if spans[..loads].iter().any(|&(first, last)| span.0 < last && first < span.1) {
                    return Err(ElfError::BadSegment);
                }
                spans[loads] = span;
                loads += 1;
            } else if elf.kind(i)? == 3 {
                return Err(ElfError::NotStatic); // PT_INTERP
            }
        }
        if loads == 0 || !abi.user_range(elf.entry, 4) {
            return Err(ElfError::BadSegment);
        }
        Ok(elf)
    }
    fn kind(&self, index: usize) -> Result<Word, ElfError> {
        le(self.image, self.phoff + index * self.phentsize, 4).ok_or(ElfError::Truncated)
    }
    /// Segment `index` if it is a PT_LOAD, checked against the file.
    fn segment(&self, index: usize) -> Result<Option<Segment>, ElfError> {
        let base = self.phoff.checked_add(index * self.phentsize).ok_or(ElfError::Truncated)?;
        if self.kind(index)? != 1 {
            return Ok(None);
        }
        let wide = self.abi.elf_class == 2;
        let get = |o32: usize, o64: usize| {
            le(self.image, base + if wide { o64 } else { o32 }, if wide { 8 } else { 4 }).ok_or(ElfError::Truncated)
        };
        let flags = le(self.image, base + if wide { 4 } else { 24 }, 4).ok_or(ElfError::Truncated)?;
        let segment = Segment {
            offset: get(4, 8)?,
            va: get(8, 16)?,
            file_bytes: get(16, 32)?,
            memory_bytes: get(20, 40)?,
            protection: match (flags & 2 != 0, flags & 1 != 0) {
                (true, true) => return Err(ElfError::WritableCode),
                (true, false) => Protection::ReadWrite,
                (false, true) => Protection::ReadExecute,
                (false, false) => Protection::ReadOnly,
            },
        };
        let end = segment.offset.checked_add(segment.file_bytes).ok_or(ElfError::BadSegment)?;
        if segment.file_bytes > segment.memory_bytes || end > self.image.len() as Word {
            return Err(ElfError::BadSegment);
        }
        Ok(Some(segment))
    }
    /// The program header table: (file offset, bytes of one entry, entries).
    pub fn program_headers(&self) -> (Word, Word, Word) {
        (self.phoff as Word, self.phentsize as Word, self.phnum as Word)
    }
    /// The PT_LOAD segments that occupy memory, in file order. All were
    /// validated by `parse`; empty ones are left out.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        (0..self.phnum).filter_map(move |i| self.segment(i).ok().flatten()).filter(|s| s.memory_bytes != 0)
    }
    /// The bytes of the page at `page_va` (a multiple of the page size) that
    /// come from the file: `(offset in the page, bytes)`. The rest is zero.
    pub fn page_bytes(&self, segment: &Segment, page_va: Word) -> Option<(usize, &'a [u8])> {
        let page = self.abi.page_size;
        let start = page_va.max(segment.va);
        let end = (page_va + page).min(segment.va + segment.file_bytes);
        if start >= end {
            return None;
        }
        let offset = (segment.offset + (start - segment.va)) as usize;
        Some(((start - page_va) as usize, &self.image[offset..offset + (end - start) as usize]))
    }
}

/// Auxiliary vector types the start code of a static C library reads.
pub const AT_NULL: Word = 0;
pub const AT_PHDR: Word = 3;
pub const AT_PHENT: Word = 4;
pub const AT_PHNUM: Word = 5;
pub const AT_PAGESZ: Word = 6;
pub const AT_ENTRY: Word = 9;
pub const AT_RANDOM: Word = 25;

/// Write a process's initial stack into `page`, the memory that will sit just
/// below `top` (so `page[page.len()-1]` is the byte at `top - 1`): the
/// argument strings, then, aligned for the ABI, argc, argv, an empty
/// environment and the auxiliary vector, each in the ABI's word size.
/// Returns the stack pointer, or `None` when it does not fit.
pub fn build_stack(abi: &Abi, page: &mut [u8], top: Word, args: &[&[u8]], aux: &[(Word, Word)]) -> Option<Word> {
    let word = abi.word_bytes();
    let base = top.checked_sub(page.len() as Word)?;
    let mut cursor = page.len();
    const MAX_ARGS: usize = 16;
    if args.len() > MAX_ARGS {
        return None;
    }
    let mut pointers = [0 as Word; MAX_ARGS];
    for (i, arg) in args.iter().enumerate() {
        cursor = cursor.checked_sub(arg.len() + 1)?;
        page[cursor..cursor + arg.len()].copy_from_slice(arg);
        page[cursor + arg.len()] = 0;
        pointers[i] = base + cursor as Word;
    }
    // argc, argv..., 0, envp 0, aux pairs..., AT_NULL pair.
    let words = 1 + args.len() + 1 + 1 + 2 * (aux.len() + 1);
    cursor = cursor.checked_sub(words * word)?;
    // `base` must itself be aligned (it is a page boundary in practice).
    if base & (abi.stack_align - 1) != 0 {
        return None;
    }
    cursor &= !(abi.stack_align as usize - 1);
    let sp = base + cursor as Word;
    let mut at = cursor;
    let mut put = |value: Word, page: &mut [u8]| {
        page[at..at + word].copy_from_slice(&value.to_le_bytes()[..word]);
        at += word;
    };
    put(args.len() as Word, page);
    for pointer in &pointers[..args.len()] {
        put(*pointer, page);
    }
    put(0, page);
    put(0, page);
    for (kind, value) in aux {
        put(*kind, page);
        put(*value, page);
    }
    put(AT_NULL, page);
    put(0, page);
    Some(sp)
}
