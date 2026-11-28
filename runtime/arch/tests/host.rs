//! Host tests for the arch module (make test-unit): both ABI tables, on any host.
#[path = "../mod.rs"]
mod arch;
use arch::*;

/// A model of how seL4 walks page tables on each port: a frame maps only when
/// every table level above it exists, a table installs at the outermost
/// missing level, and nothing maps twice.
struct Model {
    abi: &'static Abi,
    /// (span bits, va >> span) of each installed table.
    tables: Vec<(u8, Word)>,
    /// (va, bits, protection, device) of each mapped frame.
    frames: Vec<(Word, u8, Protection, bool)>,
    table_budget: usize,
    next: usize,
}
impl Model {
    fn new(abi: &'static Abi) -> Self {
        Model { abi, tables: Vec::new(), frames: Vec::new(), table_budget: usize::MAX, next: 100 }
    }
    fn has(&self, span: u8, va: Word) -> bool {
        self.tables.contains(&(span, va >> span))
    }
}
impl Kernel for Model {
    fn map_frame(&mut self, _frame: usize, _vspace: usize, va: Word, bits: u8, protection: Protection, device: bool) -> Result<(), MapError> {
        for &span in self.abi.table_spans.iter().filter(|&&s| s > bits) {
            if !self.has(span, va) {
                return Err(MapError::MissingTable);
            }
        }
        // A table one level down where a block would go, or an overlapping frame.
        if self.tables.iter().any(|&(span, index)| span == bits && index == va >> bits)
            || self.frames.iter().any(|&(v, b, ..)| v < va + (1 << bits) && va < v + (1 << b)) {
            return Err(MapError::Occupied);
        }
        self.frames.push((va, bits, protection, device));
        Ok(())
    }
    fn new_table(&mut self) -> Result<usize, MapError> {
        if self.table_budget == 0 {
            return Err(MapError::Exhausted);
        }
        self.table_budget -= 1;
        self.next += 1;
        Ok(self.next)
    }
    fn map_table(&mut self, _table: usize, _vspace: usize, va: Word) -> Result<(), MapError> {
        for &span in self.abi.table_spans {
            if !self.has(span, va) {
                self.tables.push((span, va >> span));
                return Ok(());
            }
        }
        Err(MapError::Occupied)
    }
}

#[test]
fn tables_per_frame_size_match_each_kernel() {
    // AArch32: small and large pages sit under one page table; sections and
    // supersections are entries of the page directory itself.
    assert_eq!([12, 16, 20, 24].map(|b| AARCH32.tables_for(b)), [1, 1, 0, 0]);
    // AArch64, 4 KiB granule, 48-bit: three levels above a page, two above a
    // 2 MiB block, one above a 1 GiB block.
    assert_eq!([12, 21, 30].map(|b| AARCH64.tables_for(b)), [3, 2, 1]);
}

#[test]
fn mapping_creates_exactly_the_missing_levels() {
    let mut k = Model::new(&AARCH64);
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0000, 12, Protection::ReadExecute, false), Ok(3));
    // The next page shares all three tables.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 3, 0x40_1000, 12, Protection::ReadOnly, false), Ok(0));
    // 2 MiB further on: one new last-level table.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 4, 0x60_0000, 12, Protection::ReadWrite, false), Ok(1));
    // Another gigabyte: two.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 5, 0x4000_0000, 12, Protection::ReadWrite, false), Ok(2));
    // A 2 MiB block in a region with its upper tables present needs none.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 6, 0x4020_0000, 21, Protection::ReadWrite, true), Ok(0));
    // Far away (another 512 GiB slot): all three again.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 7, 0x80_0000_0000, 12, Protection::ReadWrite, false), Ok(3));
    assert_eq!(k.tables.len(), 9);
    assert_eq!(k.frames.len(), 6);

    let mut k = Model::new(&AARCH32);
    assert_eq!(map_frame(&AARCH32, &mut k, 1, 2, 0x10_0000, 12, Protection::ReadExecute, false), Ok(1));
    assert_eq!(map_frame(&AARCH32, &mut k, 1, 3, 0x11_0000, 16, Protection::ReadWrite, false), Ok(0));
    assert_eq!(map_frame(&AARCH32, &mut k, 1, 4, 0x4900_0000, 20, Protection::ReadWrite, true), Ok(0));
    assert_eq!(k.tables.len(), 1);
}

#[test]
fn mapping_refuses_what_the_abi_cannot_express() {
    let mut k = Model::new(&AARCH64);
    // 64 KiB frames exist only on AArch32 (the Open Runtime's pool pages today).
    assert!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0000, 16, Protection::ReadWrite, false).is_err());
    // Misaligned, and outside user space.
    assert!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0800, 12, Protection::ReadWrite, false).is_err());
    assert!(map_frame(&AARCH64, &mut k, 1, 2, AARCH64.user_top, 12, Protection::ReadWrite, false).is_err());
    assert!(map_frame(&AARCH32, &mut Model::new(&AARCH32), 1, 2, 0xe000_0000, 12, Protection::ReadWrite, false).is_err());
    assert!(k.tables.is_empty() && k.frames.is_empty(), "a refused request must not reach the kernel");
    // A second mapping at the same address is the kernel's error, passed on, with no table leaked.
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0000, 12, Protection::ReadOnly, false), Ok(3));
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 9, 0x40_0000, 12, Protection::ReadOnly, false), Err(MapError::Occupied));
    assert_eq!(k.tables.len(), 3);
}

#[test]
fn mapping_stops_when_tables_run_out() {
    let mut k = Model::new(&AARCH64);
    k.table_budget = 2;
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0000, 12, Protection::ReadOnly, false), Err(MapError::Exhausted));
    assert!(k.frames.is_empty());
    // With memory back, the walk resumes from the levels already there.
    k.table_budget = 5;
    assert_eq!(map_frame(&AARCH64, &mut k, 1, 2, 0x40_0000, 12, Protection::ReadOnly, false), Ok(1));
}

fn message(values: &[(usize, Word)]) -> [Word; SYSCALL_WORDS] {
    let mut mrs = [0; SYSCALL_WORDS];
    for &(i, v) in values {
        mrs[i] = v;
    }
    mrs
}

#[test]
fn aarch64_syscall_number_is_x8_whatever_x7_holds() {
    // write(1, buf, 5) with x7 = -7, seL4's own Yield number: the patched
    // kernel reports x8 (64), and the reply leaves x1..x7 untouched.
    let mut mrs = message(&[(0, 1), (1, 0x41_0000), (2, 5), (7, (-7i64) as Word), (8, 0x40_0120), (9, 0x7fff_0000), (11, 0), (12, 64)]);
    let call = decode_syscall(&AARCH64, &mrs).unwrap();
    assert_eq!(call, Syscall { number: 64, args: [1, 0x41_0000, 5, 0, 0, 0], pc: 0x40_0120, sp: 0x7fff_0000 });
    assert_eq!(linux::aarch64(call.number).unwrap().name, "write");
    syscall_return(&AARCH64, &mut mrs, 5);
    assert_eq!((mrs[0], mrs[8], mrs[7]), (5, 0x40_0124, (-7i64) as Word));
    // An error is the negative errno, full width.
    syscall_return(&AARCH64, &mut mrs, -38);
    assert_eq!(mrs[0], (-38i64) as Word);
    // A number that is no Linux call (a native seL4 number that reached the
    // handler some other way) is refused rather than dispatched.
    assert_eq!(decode_syscall(&AARCH64, &message(&[(12, (-3i64) as Word)])), None);
    // What the patched kernel reports for an x8 of 2^31 or more (linux-svc.patch):
    // no Linux number either, so the handler answers ENOSYS.
    assert_eq!(decode_syscall(&AARCH64, &message(&[(12, 0x1_0000)])), None);
    assert!(decode_syscall(&AARCH64, &message(&[(12, 0xffff)])).is_some());
}

#[test]
fn aarch32_syscall_resumes_after_arm_and_thumb_instructions() {
    let mut arm = message(&[(0, 1), (7, 4), (8, 0x1_0000), (11, 0x10), (12, 4)]);
    assert_eq!(decode_syscall(&AARCH32, &arm).unwrap().number, 4);
    assert_eq!(linux::aarch32(4).unwrap().name, "write");
    syscall_return(&AARCH32, &mut arm, -9);
    assert_eq!((arm[0], arm[8]), (0xffff_fff7, 0x1_0004));
    let mut thumb = message(&[(8, 0x1_0000), (11, 0x30), (12, 4)]);
    syscall_return(&AARCH32, &mut thumb, 0);
    assert_eq!(thumb[8], 0x1_0002);
    // The ARM private calls (set_tls is 0xf0005) are Linux calls too.
    assert_eq!(decode_syscall(&AARCH32, &message(&[(12, 0xf0005)])).unwrap().number, 0xf0005);
    assert_eq!(decode_syscall(&AARCH32, &message(&[(12, 0xffff_fffd)])), None);
}

#[test]
fn vm_fault_access_comes_from_each_fault_status_register() {
    // AArch64 ESR_EL1: data abort from EL0 (EC 0x24), permission fault level 3, WnR set.
    let esr_write = (0x24 << 26) | (1 << 6) | 0x0f;
    assert_eq!(decode_vm_fault(&AARCH64, &[0x40_0100, 0x40_0000, 0, esr_write]).access, Access::Write);
    assert_eq!(decode_vm_fault(&AARCH64, &[0x40_0100, 0x9000, 0, esr_write & !(1 << 6)]).access, Access::Read);
    assert_eq!(decode_vm_fault(&AARCH64, &[0x9000, 0x9000, 1, 0x20 << 26]).access, Access::Execute);
    // AArch32 DFSR: WnR is bit 11; ESR's bit 6 means nothing there.
    assert_eq!(decode_vm_fault(&AARCH32, &[0x1_0000, 0x2_0000, 0, 0x80f]).access, Access::Write);
    assert_eq!(decode_vm_fault(&AARCH32, &[0x1_0000, 0x2_0000, 0, 0x04f]).access, Access::Read);
}

#[test]
fn fresh_contexts_follow_each_register_layout() {
    let c = AARCH64.start_context(0x40_0000, 0x7fff_fff8, 7, 0x1234);
    // pc, sp (16-byte aligned), spsr = EL0t, x0, and tpidr_el0 at word 34 of 36.
    assert_eq!((c.words[0], c.words[1], c.words[2], c.words[3], c.words[34]), (0x40_0000, 0x7fff_fff0, 0, 7, 0x1234));
    assert_eq!(AARCH64.context_words * 8, 288, "sizeof(seL4_UserContext) on AArch64");
    let c = AARCH32.start_context(0x1_0000, 0x13_fffc, 7, 0x1234);
    assert_eq!((c.words[1], c.words[2], c.words[18]), (0x13_fff8, 0x150, 0x1234));
    assert_eq!(AARCH32.context_words * 4, 76, "sizeof(seL4_UserContext) on AArch32");
    assert!(AARCH32.tls_by_syscall && !AARCH64.tls_by_syscall);
}

/// A minimal ET_EXEC with the given segments: (va, flags, file bytes, memory size).
fn elf(abi: &Abi, kind: u16, entry: Word, segments: &[(Word, u32, &[u8], Word)]) -> Vec<u8> {
    let wide = abi.elf_class == 2;
    let (ehsize, phsize) = if wide { (64, 56) } else { (52, 32) };
    let mut image = vec![0u8; ehsize + phsize * segments.len()];
    image[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', abi.elf_class, 1, 1]);
    image[16..18].copy_from_slice(&kind.to_le_bytes());
    image[18..20].copy_from_slice(&abi.elf_machine.to_le_bytes());
    let w = |image: &mut Vec<u8>, at: usize, v: Word, n: usize| image[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
    if wide {
        w(&mut image, 24, entry, 8);
        w(&mut image, 32, ehsize as Word, 8);
        w(&mut image, 54, phsize as Word, 2);
        w(&mut image, 56, segments.len() as Word, 2);
    } else {
        w(&mut image, 24, entry, 4);
        w(&mut image, 28, ehsize as Word, 4);
        w(&mut image, 42, phsize as Word, 2);
        w(&mut image, 44, segments.len() as Word, 2);
    }
    for (i, (va, flags, bytes, memory)) in segments.iter().enumerate() {
        let at = ehsize + i * phsize;
        let offset = image.len() as Word;
        w(&mut image, at, 1, 4);
        if wide {
            w(&mut image, at + 4, Word::from(*flags), 4);
            w(&mut image, at + 8, offset, 8);
            w(&mut image, at + 16, *va, 8);
            w(&mut image, at + 32, bytes.len() as Word, 8);
            w(&mut image, at + 40, *memory, 8);
        } else {
            w(&mut image, at + 4, offset, 4);
            w(&mut image, at + 8, *va, 4);
            w(&mut image, at + 16, bytes.len() as Word, 4);
            w(&mut image, at + 20, *memory, 4);
            w(&mut image, at + 24, Word::from(*flags), 4);
        }
        image.extend_from_slice(bytes);
    }
    image
}

#[test]
fn both_elf_classes_parse_to_the_same_segments() {
    for abi in [&AARCH32, &AARCH64] {
        let image = elf(abi, 2, 0x40_0010, &[(0x40_0000, 5, b"code", 4), (0x41_0ff0, 6, &[7u8; 0x20], 0x3000)]);
        let parsed = Elf::parse(abi, &image).unwrap();
        assert_eq!(parsed.entry, 0x40_0010);
        let segments: Vec<_> = parsed.segments().collect();
        assert_eq!(segments.len(), 2);
        assert_eq!((segments[0].va, segments[0].protection), (0x40_0000, Protection::ReadExecute));
        assert_eq!((segments[1].memory_bytes, segments[1].protection), (0x3000, Protection::ReadWrite));
        // A data segment that starts 16 bytes before a page boundary: the
        // first page takes 16 file bytes at its end, the second the other 16
        // at its start, the third is all zero.
        assert_eq!(parsed.page_bytes(&segments[1], 0x41_0000), Some((0xff0, &[7u8; 16][..])));
        assert_eq!(parsed.page_bytes(&segments[1], 0x41_1000), Some((0, &[7u8; 16][..])));
        assert_eq!(parsed.page_bytes(&segments[1], 0x41_2000), None);
    }
}

#[test]
fn elf_loader_refuses_the_wrong_program() {
    let good = elf(&AARCH64, 2, 0x40_0000, &[(0x40_0000, 5, b"code", 4)]);
    // A 64-bit program is not an AArch32 one and the reverse: no silent truncation.
    assert_eq!(Elf::parse(&AARCH32, &good).err(), Some(ElfError::WrongFormat));
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH32, 2, 0x1_0000, &[(0x1_0000, 5, b"code", 4)])).err(), Some(ElfError::WrongFormat));
    // Writable and executable at once (W^X).
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, 0x40_0000, &[(0x40_0000, 7, b"code", 4)])).err(), Some(ElfError::WritableCode));
    // Position-independent (ET_DYN): not loaded by this path.
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 3, 0x40_0000, &[(0x40_0000, 5, b"code", 4)])).err(), Some(ElfError::NotStatic));
    // A segment in the kernel's half of the address space, and an entry outside user space.
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, 0x40_0000, &[(0xffff_ff80_0000_0000, 5, b"code", 4)])).err(), Some(ElfError::BadSegment));
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, AARCH64.user_top, &[(0x40_0000, 5, b"code", 4)])).err(), Some(ElfError::BadSegment));
    // More file bytes than memory, and a file cut short.
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, 0x40_0000, &[(0x40_0000, 5, b"code", 2)])).err(), Some(ElfError::BadSegment));
    assert_eq!(Elf::parse(&AARCH64, &good[..good.len() - 1]).err(), Some(ElfError::BadSegment));
    assert_eq!(Elf::parse(&AARCH64, &good[..40]).err(), Some(ElfError::Truncated));
    assert!(Elf::parse(&AARCH64, &good).is_ok());
}

#[test]
fn empty_and_overlapping_segments_are_checked_too() {
    let code = (0x40_0000, 5, &b"code"[..], 4);
    // An empty PT_LOAD in the kernel's half, or where rounding up wraps, is refused like any other.
    for va in [0xffff_ffff_ffff_f001, 0xffff_ff80_0000_0000, AARCH64.user_top] {
        assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, 0x40_0000, &[code, (va, 4, b"", 0)])).err(), Some(ElfError::BadSegment), "{va:#x}");
    }
    // An empty one in user space is accepted and never handed to a loader.
    let image = elf(&AARCH64, 2, 0x40_0000, &[code, (0x50_0000, 4, b"", 0)]);
    let parsed = Elf::parse(&AARCH64, &image).unwrap();
    assert_eq!(parsed.segments().count(), 1);
    // A program of nothing but empty segments has nothing to run.
    assert_eq!(Elf::parse(&AARCH64, &elf(&AARCH64, 2, 0x40_0000, &[(0x40_0000, 5, b"", 0)])).err(), Some(ElfError::BadSegment));
    // Two segments on one page, with different protections: refused, so no
    // loader merges them into a page that is writable and executable.
    for abi in [&AARCH32, &AARCH64] {
        let shared = elf(abi, 2, 0x40_0000, &[(0x40_0000, 5, b"code", 0x10), (0x40_0800, 6, b"data", 0x10)]);
        assert_eq!(Elf::parse(abi, &shared).err(), Some(ElfError::BadSegment));
        let nested = elf(abi, 2, 0x40_0000, &[(0x40_0000, 5, b"code", 0x3000), (0x40_1000, 4, b"rodata", 0x10)]);
        assert_eq!(Elf::parse(abi, &nested).err(), Some(ElfError::BadSegment));
        // Adjacent pages are fine.
        let adjacent = elf(abi, 2, 0x40_0000, &[(0x40_0000, 5, b"code", 0x1000), (0x40_1000, 6, b"data", 0x10)]);
        assert!(Elf::parse(abi, &adjacent).is_ok());
    }
    // The page rounding a loader uses is checked arithmetic.
    let top = Segment { va: Word::MAX - 10, offset: 0, file_bytes: 0, memory_bytes: 5, protection: Protection::ReadOnly };
    assert_eq!(top.page_span(4096), None);
    let s = Segment { va: 0x41_0ff0, offset: 0, file_bytes: 0, memory_bytes: 0x20, protection: Protection::ReadOnly };
    assert_eq!(s.page_span(4096), Some((0x41_0000, 0x41_2000)));
}

#[test]
fn initial_stack_uses_each_word_size_and_alignment() {
    for abi in [&AARCH32, &AARCH64] {
        let mut page = vec![0u8; 4096];
        let top = 0x7fff_0000;
        let sp = build_stack(abi, &mut page, top, &[b"hello", b"x"], &[(AT_PAGESZ, 4096), (AT_ENTRY, 0x40_0000)]).unwrap();
        assert_eq!(sp % abi.stack_align, 0);
        let word = abi.word_bytes();
        let read = |i: usize| {
            let at = (sp - (top - 4096)) as usize + i * word;
            let mut bytes = [0u8; 8];
            bytes[..word].copy_from_slice(&page[at..at + word]);
            Word::from_le_bytes(bytes)
        };
        assert_eq!(read(0), 2, "argc");
        let argv0 = (read(1) - (top - 4096)) as usize;
        assert_eq!(&page[argv0..argv0 + 6], b"hello\0");
        assert_eq!((read(3), read(4)), (0, 0), "argv and envp terminators");
        assert_eq!((read(5), read(6), read(7), read(8)), (AT_PAGESZ, 4096, AT_ENTRY, 0x40_0000));
        assert_eq!((read(9), read(10)), (AT_NULL, 0));
    }
    // Too many or too long arguments are refused, not truncated.
    assert_eq!(build_stack(&AARCH64, &mut [0u8; 64], 0x1000, &[&[b'a'; 60]], &[]), None);
}

#[test]
fn linux_tables_are_consistent() {
    use arch::linux::*;
    assert!(CALLS.windows(2).all(|w| w[0].aarch64 < w[1].aarch64), "sorted, no duplicate AArch64 number");
    // Values every Linux program depends on.
    for (name, n64, n32) in [("read", 63, 3), ("write", 64, 4), ("openat", 56, 322), ("close", 57, 6), ("exit_group", 94, 248),
                             ("brk", 214, 45), ("munmap", 215, 91), ("mprotect", 226, 125), ("getrandom", 278, 384),
                             ("set_tid_address", 96, 256), ("ioctl", 29, 54)] {
        let call = aarch64(n64).unwrap();
        assert_eq!((call.name, call.aarch32), (name, n32));
        assert_eq!(aarch32(n32.into()).unwrap().name, name);
    }
    // 64-bit time: AArch64's clock_gettime is AArch32's clock_gettime64, not its 32-bit one (263).
    assert_eq!((aarch64(113).unwrap().aarch32_name, aarch64(113).unwrap().aarch32), ("clock_gettime64", 403));
    assert_eq!(aarch64(98).unwrap().aarch32, 422);
    // mmap takes a byte offset where mmap2 takes pages; stat and iovec change size.
    for n in [222, 80, 66, 134, 220] {
        assert_eq!(aarch64(n).unwrap().layout, Layout::Words);
    }
    // The legacy calls AArch64 never had are simply absent.
    assert!(CALLS.iter().all(|c| !["open", "stat", "pipe", "dup2", "fork", "poll", "epoll_wait"].contains(&c.name)));
    assert!(aarch64(1000).is_none());
}

// ---- the Open Runtime's AArch64 pieces: call translation, signal frame, structures ----

#[test]
fn aarch64_calls_translate_to_one_numbering() {
    use arch::linux::Layout;
    // AArch32 numbers pass unchanged, private ones included.
    assert_eq!(internal_call(&AARCH32, 3), Some((3, Layout::Same)));
    assert_eq!(internal_call(&AARCH32, FORK_NR32.into()), Some((FORK_NR32, Layout::Same)));
    // 63 is AArch64's read, never AArch32's dup2.
    assert_eq!(internal_call(&AARCH64, 63), Some((3, Layout::Same)));
    assert_eq!(internal_call(&AARCH64, 64), Some((4, Layout::Same)));
    assert_eq!(internal_call(&AARCH64, 139), Some((173, Layout::Words)), "rt_sigreturn");
    assert_eq!(internal_call(&AARCH64, 222), Some((192, Layout::Words)), "mmap is mmap2");
    assert_eq!(internal_call(&AARCH64, signal64::FORK_NR), Some((FORK_NR32, Layout::Same)));
    // AArch32-only numbers and the AArch32 private range are not reachable by number.
    for n in [5u64, 2, 0xf_0000, 0x1_0000, 0xffff_ffff, 1000, 0xf001] {
        assert_eq!(internal_call(&AARCH64, n), None, "{n:#x}");
    }
    // Every table entry translates, and none lands on another entry's AArch32 number.
    let mut seen = std::collections::HashSet::new();
    for call in arch::linux::CALLS {
        let (n, _) = internal_call(&AARCH64, call.aarch64.into()).unwrap();
        assert_eq!(n, u32::from(call.aarch32));
        assert!(seen.insert(n), "{} shares AArch32 number {n}", call.name);
    }
    // The fork helper's number gets through decode_syscall (below 0x10000) and is no Linux call.
    assert!(arch::linux::aarch64(signal64::FORK_NR).is_none());
    let mut mrs = [0; SYSCALL_WORDS];
    mrs[12] = signal64::FORK_NR;
    assert!(decode_syscall(&AARCH64, &mrs).is_some());
}

#[test]
fn aarch64_signal_frame_matches_linux() {
    use arch::signal64::*;
    // arch/arm64 offsets: ucontext after the 128-byte siginfo, sigcontext at
    // +176 into it, __reserved at 592 (PR #48's _Static_assert), fpsimd
    // 528 bytes, the whole frame 4688 bytes and 16-byte aligned.
    assert_eq!((UC, MCONTEXT, REGS, SP, PC, PSTATE, RESERVED), (128, 304, 312, 560, 568, 576, 592));
    assert_eq!((FPSR, FPCR, VREGS), (600, 604, 608));
    assert_eq!(FRAME, 4688);
    assert_eq!(FRAME % 16, 0);
    assert!(FPSIMD + FPSIMD_SIZE as usize + ESR_SIZE as usize + 8 <= RESERVED + RESERVED_BYTES);

    let mut regs = Regs::default();
    for k in 0..31 {
        regs.x[k] = 0x1000 + k as u64;
    }
    regs.sp = 0x7fff_ffff_e008;
    regs.pc = 0x55_5555_1234;
    regs.pstate = 0x6000_0000;
    let alt = AltStack { sp: 0, flags: SS_DISABLE, size: 0 };
    let frame = place(regs.sp, &alt, false).unwrap();
    assert_eq!(frame % 16, 0);
    assert!(frame + TOTAL as u64 <= regs.sp, "frame and record below sp");
    let info = siginfo_fault(11, 1, 0xdead_0000_beef);
    assert_eq!(u64::from_le_bytes(info[16..24].try_into().unwrap()), 0xdead_0000_beef, "si_addr at 16");
    let mut b = vec![0u8; TOTAL];
    encode(&mut b, &regs, &info, u64::MAX, &alt, 0xdead_0000_beef, Some(0x9200_0047)).unwrap();
    let w64 = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let w32 = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    assert_eq!(w64(REGS + 8 * 30), 0x1000 + 30);
    assert_eq!((w64(SP), w64(PC), w64(PSTATE)), (regs.sp, regs.pc, regs.pstate));
    assert_eq!(w64(UC_SIGMASK), u64::MAX & !UNBLOCKABLE, "SIGKILL and SIGSTOP never masked");
    assert_eq!((w32(FPSIMD), w32(FPSIMD + 4)), (FPSIMD_MAGIC, FPSIMD_SIZE));
    assert_eq!((w32(FPSIMD + 528), w32(FPSIMD + 532), w64(FPSIMD + 536)), (ESR_MAGIC, ESR_SIZE, 0x9200_0047));
    assert_eq!((w32(FPSIMD + 544), w32(FPSIMD + 548)), (0, 0), "the null record ends the list");
    assert_eq!((w64(FRAME), w64(FRAME + 8)), (regs.x[29], regs.x[30]), "frame record {{fp, lr}}");

    // Round trip; the frame may change only the condition flags of pstate.
    let back = decode(&b, AARCH64.user_top, 0x6000_0000).unwrap();
    assert_eq!(back.regs, regs);
    assert_eq!(back.mask, u64::MAX & !UNBLOCKABLE);
    let mut evil = b.clone();
    evil[PSTATE..PSTATE + 8].copy_from_slice(&0x3c5u64.to_le_bytes()); // EL1h, interrupts masked
    assert_eq!(decode(&evil, AARCH64.user_top, 0).unwrap().regs.pstate, 0, "no exception level or masks from a frame");
    // Bad frames: SIGSEGV, as Linux.
    let bad = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut f = b.clone();
        edit(&mut f);
        decode(&f, AARCH64.user_top, 0).is_none()
    };
    assert!(bad(&|f| f[PC..PC + 8].copy_from_slice(&0x1002u64.to_le_bytes())), "misaligned pc");
    assert!(bad(&|f| f[PC..PC + 8].copy_from_slice(&AARCH64.user_top.to_le_bytes())), "kernel pc");
    assert!(bad(&|f| f[SP..SP + 8].copy_from_slice(&u64::MAX.to_le_bytes())), "kernel sp");
    assert!(bad(&|f| f[FPSIMD..FPSIMD + 4].copy_from_slice(&0u32.to_le_bytes())), "fpsimd record gone");
    assert!(bad(&|f| f[FPSIMD + 4..FPSIMD + 8].copy_from_slice(&0u32.to_le_bytes())), "zero-size record");
    assert!(bad(&|f| f[FPSIMD + 528..FPSIMD + 532].copy_from_slice(&0x1234_5678u32.to_le_bytes())), "unknown record");
    assert!(bad(&|f| f.truncate(FRAME - 1)), "short");

    // The alternate stack: the frame goes at its top, and must fit it.
    let alt = AltStack { sp: 0x10_0000_0000, flags: 0, size: 0x4000 };
    let on = place(regs.sp, &alt, true).unwrap();
    assert!(on >= alt.sp && on + TOTAL as u64 <= alt.sp + alt.size);
    assert_eq!(place(regs.sp, &AltStack { size: 0x1000, ..alt }, true), None, "too small for a frame");
    assert_eq!(alt.report(on), 1, "SS_ONSTACK while on it");

    let entry = gate_entry(&regs, 11, frame, 0x55_0000_4000, 0x7f00_0000_ff00);
    assert_eq!((entry.x[0], entry.x[1], entry.x[2], entry.x[3]), (11, frame, frame + 128, 0x55_0000_4000));
    assert_eq!((entry.x[29], entry.sp, entry.pc), (frame + FRAME as u64, frame, 0x7f00_0000_ff00));
}

#[test]
fn aarch64_gate_stores_where_the_frame_says() {
    use arch::signal64::*;
    // add x10, sp, #imm: the base of the SIMD stores is VREGS.
    let add_imm = |w: u32| (w >> 10) & 0xfff;
    assert_eq!(GATE[0] & 0xffc0_03ff, 0x9100_03ea, "add x10, sp, #imm");
    assert_eq!(add_imm(GATE[0]) as usize, VREGS);
    assert_eq!(GATE[21], GATE[0]);
    // stp/ldp qN, qN+1, [x10, #32*k] for k in 0..16: the 32 vector registers in order.
    for k in 0..16u32 {
        for (at, opc) in [(1 + k as usize, 0xad00_0000u32), (25 + k as usize, 0xad40_0000)] {
            let w = GATE[at];
            assert_eq!(w & 0xffc0_0000, opc);
            assert_eq!(w & 0x1f, 2 * k, "Rt");
            assert_eq!((w >> 10) & 0x1f, 2 * k + 1, "Rt2");
            assert_eq!((w >> 5) & 0x1f, 10, "base x10");
            assert_eq!((w >> 15) & 0x7f, 2 * k, "imm7 in 16-byte units");
        }
    }
    // stp/ldp w11, w12, [x10, #-8]: FPSR at VREGS - 8 (= FPSR), FPCR after it.
    for at in [19, 22] {
        let imm7 = (GATE[at] >> 15) & 0x7f;
        assert_eq!(imm7, 0x7e, "-2 words");
        assert_eq!(VREGS - 8, FPSR);
        assert_eq!(FPSR + 4, FPCR);
    }
    assert_eq!(GATE[20], 0xd63f_0060, "blr x3: the handler");
    // rt_sigreturn: mov x8, #139; svc #0; and a trap if it ever returns.
    assert_eq!(GATE[41], 0xd280_0008 | 139 << 5);
    assert_eq!((GATE[42], GATE[43]), (0xd400_0001, 0xd420_0000));
    // The fork helper: mov x8, #FORK_NR; svc #0; ret.
    assert_eq!(FORK_HELPER, GATE_OFFSET + 44 * 4);
    assert_eq!(GATE[44], 0xd280_0008 | (FORK_NR as u32) << 5);
    assert_eq!((GATE[45], GATE[46]), (0xd400_0001, 0xd65f_03c0));
    // It fits the trampoline page's last 256 bytes, as the AArch32 gate does.
    assert!(GATE_OFFSET as usize + 4 * GATE.len() <= 0x1_0000);
}

#[test]
fn structures_convert_between_the_abis() {
    use arch::compat::*;
    // struct stat64 (AArch32) as the runtime writes it, to AArch64's struct stat.
    let mut s = [0u8; STAT32];
    s[..8].copy_from_slice(&1u64.to_le_bytes());
    s[16..20].copy_from_slice(&0o100644u32.to_le_bytes());
    s[20..24].copy_from_slice(&1u32.to_le_bytes());
    s[48..56].copy_from_slice(&0x1_2345_6789u64.to_le_bytes());
    s[56..60].copy_from_slice(&4096u32.to_le_bytes());
    s[64..72].copy_from_slice(&9u64.to_le_bytes());
    s[80..84].copy_from_slice(&1_795_000_000u32.to_le_bytes());
    s[84..88].copy_from_slice(&5u32.to_le_bytes());
    s[96..104].copy_from_slice(&0xabcd_0000_0042u64.to_le_bytes());
    let d = stat_to_aarch64(&s);
    let q = |at: usize| u64::from_le_bytes(d[at..at + 8].try_into().unwrap());
    let l = |at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap());
    assert_eq!((q(0), q(8), l(16), l(20)), (1, 0xabcd_0000_0042, 0o100644, 1));
    assert_eq!((q(48), l(56), q(64)), (0x1_2345_6789, 4096, 9));
    assert_eq!((q(88), q(96)), (1_795_000_000, 5), "st_mtime, st_mtime_nsec");
    // timeval, rlimit: word-sized fields; RLIM_INFINITY is all ones in each.
    assert_eq!(timeval(&AARCH32, 7, 8).1, 8);
    let (t, n) = timeval(&AARCH64, -1, 8);
    assert_eq!((n, &t[..8], t[8]), (16, &[0xff; 8][..], 8));
    let (r, n) = rlimit(&AARCH32, u64::MAX, 1 << 40);
    assert_eq!((n, &r[..8]), (8, &[0xff; 8][..]));
    assert_eq!(read_rlimit(&AARCH32, &r).unwrap(), (u64::MAX, u64::MAX));
    let (r, n) = rlimit(&AARCH64, u64::MAX, 1 << 40);
    assert_eq!((n, read_rlimit(&AARCH64, &r).unwrap()), (16, (u64::MAX, 1 << 40)));
    let mut ru = [0u8; RUSAGE32];
    ru[4..8].copy_from_slice(&(-3i32).to_le_bytes());
    assert_eq!(i64::from_le_bytes(rusage_to_aarch64(&ru)[8..16].try_into().unwrap()), -3);
    // iovec and msghdr hold 64-bit pointers on AArch64.
    let mut iov = Vec::new();
    for (base, len) in [(0x7f12_3456_0000u64, 10u64), (0x55_0000_1000, 20)] {
        iov.extend_from_slice(&base.to_le_bytes());
        iov.extend_from_slice(&len.to_le_bytes());
    }
    assert_eq!(iovec_bytes(&AARCH64), 16);
    assert_eq!(iovec(&AARCH64, &iov, 1), Some((0x55_0000_1000, 20)));
    assert_eq!(iovec(&AARCH64, &iov, 2), None);
    assert_eq!(iovec(&AARCH32, &iov[..8], 0), Some((0x3456_0000, 0x7f12)));
    let mut m = vec![0u8; 56];
    m[0..8].copy_from_slice(&0x7f00_0000_0010u64.to_le_bytes());
    m[8..12].copy_from_slice(&16u32.to_le_bytes());
    m[16..24].copy_from_slice(&0x7f00_0000_0100u64.to_le_bytes());
    m[24..32].copy_from_slice(&2u64.to_le_bytes());
    m[32..40].copy_from_slice(&0x7f00_0000_0200u64.to_le_bytes());
    m[40..48].copy_from_slice(&24u64.to_le_bytes());
    let h = msghdr(&AARCH64, &m).unwrap();
    assert_eq!((h.name, h.namelen, h.iov, h.iovlen, h.control, h.controllen), (0x7f00_0000_0010, 16, 0x7f00_0000_0100, 2, 0x7f00_0000_0200, 24));
    msghdr_outputs(&AARCH64, &mut m, 8, 20, 0x40);
    assert_eq!(msghdr(&AARCH64, &m).unwrap().controllen, 20);
    assert_eq!((msghdr_bytes(&AARCH32), msghdr_bytes(&AARCH64)), (28, 56));
    // cmsghdr: 12-byte header on AArch32, 16 on AArch64 (SCM_RIGHTS with one fd).
    assert_eq!((cmsg_len(&AARCH32, 4), cmsg_space(&AARCH32, 4)), (16, 16));
    assert_eq!((cmsg_len(&AARCH64, 4), cmsg_space(&AARCH64, 4)), (20, 24));
    let mut c = vec![0u8; 24];
    c[..8].copy_from_slice(&20u64.to_le_bytes());
    c[8..12].copy_from_slice(&1u32.to_le_bytes()); // SOL_SOCKET
    c[12..16].copy_from_slice(&1u32.to_le_bytes()); // SCM_RIGHTS
    assert_eq!(cmsg(&AARCH64, &c, 0), Some((20, 1, 1, 16)));
    c[..8].copy_from_slice(&100u64.to_le_bytes());
    assert_eq!(cmsg(&AARCH64, &c, 0), None, "longer than the control buffer");
    // rt_sigaction and sigaltstack.
    let a = Sigaction { handler: 0x55_0000_4000, flags: 0x0400_0004, restorer: 0, mask: 1 << 9 };
    for abi in [&AARCH32, &AARCH64] {
        let (b, n) = put_sigaction(abi, &Sigaction { handler: a.handler & if abi.word_bits == 32 { 0xffff_ffff } else { u64::MAX }, ..a });
        assert_eq!(n, if abi.word_bits == 32 { 20 } else { 32 });
        assert_eq!(sigaction(abi, &b[..n]).unwrap().mask, 1 << 9);
    }
    let (b, n) = put_stack_t(&AARCH64, 0x7f00_0000_0000, 0, 0x8000);
    assert_eq!((n, stack_t(&AARCH64, &b).unwrap()), (24, (0x7f00_0000_0000, 0, 0x8000)));
    // mmap: AArch64 bytes become mmap2's pages; an unaligned offset is EINVAL.
    assert_eq!(mmap_pages(&AARCH64, 0x3000), Some(3));
    assert_eq!(mmap_pages(&AARCH64, 0x3001), None);
    assert_eq!(mmap_pages(&AARCH32, 3), Some(3));
}

/// A call's 32-bit words only (its `wide` registers are tested apart).
fn words(e: arch::enter64::Entry) -> arch::enter64::Entry {
    use arch::enter64::Entry;
    match e {
        Entry::Call { number, args, after, .. } => Entry::Call { number, args, wide: [0; 6], after },
        e => e,
    }
}

#[test]
fn the_dispatcher_gets_every_address_whole() {
    use arch::enter64::*;
    let wide = |n, a| match translate(n, a) {
        Entry::Call { wide, after, .. } => (wide, after),
        e => panic!("{e:?}"),
    };
    // read(3, a buffer above 4 GiB, 16): the 32-bit word is NOWHERE, the
    // address the dispatcher reads app memory at is the register itself.
    let high = 0x1234_5678_9000u64;
    assert_eq!(wide(63, [3, high, 16, 0, 0, 0]).0, [3, high, 16, 0, 0, 0]);
    // mmap's address and a length of 64 GiB (a JavaScript engine's reservation).
    assert_eq!(wide(222, [high, 64 << 30, 0, 0x22, u64::MAX, 0]).0[..2], [high, 64 << 30]);
    // Moved with the call where it is rewritten: nanosleep's two pointers
    // are clock_nanosleep's third and fourth, statfs's buffer its third.
    assert_eq!(wide(101, [high, high + 16, 0, 0, 0, 0]).0, [1, 0, high, high + 16, 0, 0]);
    assert_eq!(wide(43, [high, high + 0x100, 0, 0, 0, 0]), ([high, 84, high + 0x100, 0, 0, 0], After::Statfs(high + 0x100)));
    assert_eq!(wide(80, [3, high, 0, 0, 0, 0]).1, After::Stat(high));
    assert_eq!(wide(67, [3, high, 64, 0x1_2345_6789, 0, 0]).0, [3, high, 64, 0, 0x2345_6789, 1]);
    assert_eq!(wide(163, [7, high, 0, 0, 0, 0]).0, [0, 7, 0, high, 0, 0]);
    // A sign-extended address stays one no app has (above user space).
    assert_eq!(wide(64, [1, 0xffff_ffff_bef0_0000, 16, 0, 0, 0]).0[1], 0xffff_ffff_bef0_0000);
}

#[test]
fn aarch64_calls_enter_the_one_dispatcher_as_their_aarch32_meaning() {
    use arch::enter64::*;
    let call = |number, args| Entry::Call { number, args, wide: [0; 6], after: After::None };
    let translate = |n, a| words(translate(n, a));
    // Same layout: the number translated, the words narrowed. AT_FDCWD and
    // an ioctl request with bit 31 come sign-extended and stay what they were.
    assert_eq!(translate(56, [(-100i64) as u64, 0x1000, 2, 0, 0, 0]), call(322, [(-100i32) as u32, 0x1000, 2, 0, 0, 0]));
    assert_eq!(translate(29, [3, 0xffff_ffff_8004_5601, 0x2000, 0, 0, 0]), call(54, [3, 0x8004_5601, 0x2000, 0, 0, 0]));
    // A word above 4 GiB that is not a sign-extended int names no app
    // memory: NOWHERE, never cut to the low address it would alias.
    assert_eq!(translate(63, [3, 1 << 32 | 0x1000, 16, 0, 0, 0]), call(3, [3, NOWHERE, 16, 0, 0, 0]));
    assert_eq!(translate(63, [3, 0xffff_fffe_0000_1000, 16, 0, 0, 0]), call(3, [3, NOWHERE, 16, 0, 0, 0]));
    // Registers a call does not read may hold anything (musl's write passes
    // three): the call still runs.
    assert_eq!(translate(64, [5, 0x1000, 4, 0xffff_ffe0, 0x455a_ff5b_5e5e_4cff, 0x6872_2e72_6f6f_602e]),
               call(4, [5, 0x1000, 4, 0xffff_ffe0, NOWHERE, NOWHERE]));
    assert!(NOWHERE >= 0xc000_0000, "above every app region");
    // No AArch32 meaning, or an AArch32-only number: ENOSYS.
    for n in [5u64, 2, 1000, 0xf_0000] {
        assert_eq!(translate(n, [0; 6]), Entry::Answer(ENOSYS), "{n:#x}");
    }
    // 64-bit values become AArch32's register pairs.
    assert_eq!(translate(62, [3, 0x1234, 0, 0, 0, 0]), call(19, [3, 0x1234, 0, 0, 0, 0]));
    assert_eq!(translate(62, [3, (-8i64) as u64, 2, 0, 0, 0]), call(19, [3, (-8i32) as u32, 2, 0, 0, 0]));
    assert_eq!(translate(62, [3, 1 << 33, 0, 0, 0, 0]), Entry::Answer(EOVERFLOW), "an offset lseek's int cannot answer");
    assert_eq!(translate(67, [3, 0x1000, 64, 0x1_2345_6789, 0, 0]), call(180, [3, 0x1000, 64, 0, 0x2345_6789, 1]));
    assert_eq!(translate(46, [3, 0x1_0000_0010, 0, 0, 0, 0]), call(194, [3, 0, 0x10, 1, 0, 0]));
    assert_eq!(translate(223, [3, 0x10, 0x20, 4, 0, 0]), call(270, [3, 4, 0x10, 0, 0x20, 0]));
    // mmap's byte offset is mmap2's pages, and must be page aligned.
    assert_eq!(translate(222, [0, 0x2000, 3, 0x22, (-1i64) as u64, 0x3000]), call(192, [0, 0x2000, 3, 0x22, u32::MAX, 3]));
    assert_eq!(translate(222, [0, 0x2000, 3, 2, 4, 0x3001]), Entry::Answer(EINVAL));
    // Calls AArch32 makes another way.
    assert_eq!(translate(101, [0x100, 0x200, 0, 0, 0, 0]), call(407, [1, 0, 0x100, 0x200, 0, 0]), "nanosleep");
    assert_eq!(translate(163, [7, 0x100, 0, 0, 0, 0]), call(369, [0, 7, 0, 0x100, 0, 0]), "getrlimit");
    assert_eq!(translate(164, [7, 0x100, 0, 0, 0, 0]), call(369, [0, 7, 0x100, 0, 0, 0]), "setrlimit");
    assert_eq!(translate(169, [0, 0, 0, 0, 0, 0]), Entry::Answer(0), "gettimeofday without a timeval");
    // Structures the dispatcher fills in are converted after it.
    assert_eq!(translate(80, [3, 0x500, 0, 0, 0, 0]), Entry::Call { number: 197, args: [3, 0x500, 0, 0, 0, 0], wide: [0; 6], after: After::Stat(0x500) });
    assert_eq!(translate(169, [0x600, 0, 0, 0, 0, 0]), Entry::Call { number: 403, args: [0, 0x600, 0, 0, 0, 0], wide: [0; 6], after: After::Timeval(0x600) });
    assert_eq!(translate(43, [0x10, 0x700, 0, 0, 0, 0]), Entry::Call { number: 266, args: [0x10, 84, 0x700, 0, 0, 0], wide: [0; 6], after: After::Statfs(0x700) });
    // Pointer-holding structures go to the dispatcher, which reads them in
    // the app's layout; everything else with another layout is ENOSYS, named.
    for n in WIDE_IN_DISPATCH {
        assert!(matches!(translate(*n, [0; 6]), Entry::Call { .. }), "{n}");
    }
    assert!(unconverted(243), "recvmmsg");
    assert!(!unconverted(80) && !unconverted(63));
    // Every AArch64 call either reaches the dispatcher or is answered.
    for c in arch::linux::CALLS {
        let e = translate(u64::from(c.aarch64), [0; 6]);
        assert!(matches!(e, Entry::Call { .. } | Entry::Answer(_)), "{}", c.name);
    }
}

#[test]
fn a_sign_extended_address_is_no_app_address() {
    use arch::enter64::*;
    let call = |number, args| Entry::Call { number, args, wide: [0; 6], after: After::None };
    let translate = |n, a| words(translate(n, a));
    // Linux: a kernel-half pointer is EFAULT. Here it must not alias the
    // app's own stack at 0xbef0_0000 (write, mprotect, munmap, mmap FIXED).
    let kernel_half = 0xffff_ffff_bef0_0000u64;
    assert_eq!(translate(64, [1, kernel_half, 16, 0, 0, 0]), call(4, [1, NOWHERE, 16, 0, 0, 0]));
    assert_eq!(translate(226, [kernel_half, 0x1000, 7, 0, 0, 0]), call(125, [NOWHERE, 0x1000, 7, 0, 0, 0]));
    assert_eq!(translate(215, [kernel_half, 0x1000, 0, 0, 0, 0]), call(91, [NOWHERE, 0x1000, 0, 0, 0, 0]));
    assert_eq!(translate(222, [kernel_half, 0x1000, 3, 0x32, (-1i64) as u64, 0]), call(192, [NOWHERE, 0x1000, 3, 0x32, u32::MAX, 0]));
    // A sign-extended size is no size either.
    assert_eq!(translate(63, [3, 0x1000, 0xffff_ffff_8000_0000, 0, 0, 0]), call(3, [3, 0x1000, NOWHERE, 0, 0, 0]));
    // Int arguments keep their sign: fd -1 and AT_FDCWD, pid -1, ioctl's request.
    assert_eq!(translate(260, [(-1i64) as u64, 0x100, 0, 0, 0, 0]), Entry::Call { number: 114, args: [u32::MAX, 0x100, 0, 0, 0, 0], wide: [0; 6], after: After::None });
    assert_eq!(translate(56, [(-100i64) as u64, 0x1000, 0, 0, 0, 0]), call(322, [(-100i32) as u32, 0x1000, 0, 0, 0, 0]));
    // Every argument the table names exists in the call.
    for c in arch::linux::CALLS {
        assert!(memory_args(u64::from(c.aarch64)) < 1 << 6, "{}", c.name);
    }
}

#[test]
fn x0_of_an_answered_call_is_its_result() {
    use arch::enter64::*;
    let svc = 0x40_1000u32;
    let mut wide = [0u64; 16];
    let mut shown = [0u32; 16];
    // read(0xfffffff7, buf, n) with x0 zero-extended: EBADF is -9, though it
    // equals the narrowed fd the dispatcher was shown.
    (wide[0], wide[1], wide[2], wide[8]) = (0xffff_fff7, 0x1000, 16, u64::from(svc));
    (shown[0], shown[1], shown[2], shown[8]) = (0xffff_fff7, 0x1000, 16, svc);
    let answer = [(-9i32) as u32, 0x1000, 16, 0, 0, 0, 0, 0, svc + 4];
    let mut out = [0u64; 16];
    assert_eq!(widen(true, &wide, &shown, &answer, None, &mut out), 9);
    assert_eq!(out[0] as i64, -9, "an error, as musl reads it");
    assert_eq!((out[1], out[2], out[8]), (0x1000, 16, u64::from(svc + 4)));
    // Any errno result equal to the call's x0 (uname(0xfffffff2): EFAULT).
    (wide[0], shown[0]) = (0xffff_fff2, 0xffff_fff2);
    let answer = [(-14i32) as u32, 0x1000, 16, 0, 0, 0, 0, 0, svc + 4];
    widen(true, &wide, &shown, &answer, None, &mut out);
    assert_eq!(out[0] as i64, -14);
    // A result is zero-extended, a register the call left is the thread's own.
    wide[3] = 0xdead_beef_0000_0001;
    shown[3] = 1;
    let answer = [0x8000_0000, 0x1000, 16, 1, 0, 0, 0, 0, svc + 4];
    widen(true, &wide, &shown, &answer, None, &mut out);
    assert_eq!((out[0], out[3]), (0x8000_0000, 0xdead_beef_0000_0001));
    // A call made again (back on the svc): x0 as the thread had it.
    (wide[0], shown[0]) = ((-100i64) as u64, (-100i32) as u32);
    let again = [shown[0], 0x1000, 16, 1, 0, 0, 0, 0, svc];
    widen(true, &wide, &shown, &again, None, &mut out);
    assert_eq!((out[0], out[8]), ((-100i64) as u64, u64::from(svc)));
    // nanosleep shown as clock_nanosleep(1, ...): made again, x0 is the pointer.
    (wide[0], shown[0]) = (0x2000, 1);
    let again = [1, 0, 0x2000, 0, 0, 0, 0, 0, svc];
    widen(true, &wide, &shown, &again, None, &mut out);
    assert_eq!(out[0], 0x2000);
}

#[test]
fn code_and_addresses_above_4_gib_come_back_whole() {
    use arch::enter64::*;
    // A program above 4 GiB (vma::Layout64): the svc at a 47-bit address.
    let svc = 0x1000_2345_6788u64;
    let mut wide = [0u64; 16];
    let mut shown = [0u32; 16];
    (wide[0], wide[1], wide[8], wide[9]) = (0, 0x2000, svc, 0x7ff0_0000_1000);
    (shown[0], shown[1], shown[8], shown[9]) = (0, 0x2000, svc as u32, 0x0000_1000);
    let mut out = [0u64; 16];
    // Answered: past the svc, from the whole address; sp as the thread had it.
    widen(true, &wide, &shown, &[0, 0x2000, 0, 0, 0, 0, 0, 0, svc as u32 + 4, shown[9]], None, &mut out);
    assert_eq!((out[8], out[9]), (svc + 4, 0x7ff0_0000_1000));
    // Back onto the svc, and an svc whose low word wraps past it.
    widen(true, &wide, &shown, &[0, 0x2000, 0, 0, 0, 0, 0, 0, svc as u32], None, &mut out);
    assert_eq!(out[8], svc);
    let edge = 0x1000_ffff_fffcu64;
    (wide[8], shown[8]) = (edge, edge as u32);
    widen(true, &wide, &shown, &[0, 0, 0, 0, 0, 0, 0, 0, (edge as u32).wrapping_add(4)], None, &mut out);
    assert_eq!(out[8], edge + 4, "never cut to the low word");
    // mmap answered an address above 4 GiB: x0 is all of it.
    let mapped = 0x6fff_f000_0000u64;
    widen(true, &wide, &shown, &[mapped as u32, 0, 0, 0, 0, 0, 0, 0, (edge as u32).wrapping_add(4)], Some((mapped as u32, mapped)), &mut out);
    assert_eq!(out[0], mapped);
    // A result the runtime changed since (an error, a restart): not the address.
    widen(true, &wide, &shown, &[(-4i32) as u32, 0, 0, 0, 0, 0, 0, 0, (edge as u32).wrapping_add(4)], Some((mapped as u32, mapped)), &mut out);
    assert_eq!(out[0] as i64, -4);
}

#[test]
fn structures_the_dispatcher_wrote_become_the_aarch64_ones() {
    use arch::enter64::*;
    // A time64 timespec becomes a timeval.
    let mut ts = [0u8; 16];
    ts[..8].copy_from_slice(&1_700_000_000i64.to_le_bytes());
    ts[8..12].copy_from_slice(&123_456_789u32.to_le_bytes());
    let tv = convert(After::Timeval(0), &ts);
    assert_eq!(i64::from_le_bytes(tv[..8].try_into().unwrap()), 1_700_000_000);
    assert_eq!(i64::from_le_bytes(tv[8..16].try_into().unwrap()), 123_456);
    // sysinfo: longs widen, procs and mem_unit keep their places.
    let mut si = [0u8; 64];
    si[..4].copy_from_slice(&77u32.to_le_bytes());
    si[16..20].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    si[20..24].copy_from_slice(&0x1000u32.to_le_bytes());
    si[40..42].copy_from_slice(&5u16.to_le_bytes());
    si[52..56].copy_from_slice(&1u32.to_le_bytes());
    let w = convert(After::Sysinfo(0), &si);
    let word = |at: usize| u64::from_le_bytes(w[at..at + 8].try_into().unwrap());
    assert_eq!((word(0), word(32), word(40)), (77, 0x8000_0000, 0x1000), "uptime, totalram (not sign-extended), freeram");
    assert_eq!(u16::from_le_bytes([w[80], w[81]]), 5);
    assert_eq!(u32::from_le_bytes(w[104..108].try_into().unwrap()), 1);
    assert_eq!(sizes(After::Sysinfo(0)), Some((0, 64, 112)));
    // statfs64 to statfs.
    let mut sf = [0u8; 84];
    sf[..4].copy_from_slice(&0xef53u32.to_le_bytes());
    sf[4..8].copy_from_slice(&4096u32.to_le_bytes());
    sf[8..16].copy_from_slice(&1000u64.to_le_bytes());
    sf[56..60].copy_from_slice(&255u32.to_le_bytes());
    let f = convert(After::Statfs(0), &sf);
    let word = |at: usize| u64::from_le_bytes(f[at..at + 8].try_into().unwrap());
    assert_eq!((word(0), word(8), word(16), word(64)), (0xef53, 4096, 1000, 255));
    // The largest fits.
    for a in [After::Stat(0), After::Rusage(0), After::Statfs(0), After::Tms(0), After::Sysinfo(0), After::Timeval(0)] {
        assert!(sizes(a).unwrap().2 <= MAX_AFTER);
    }
    // A fault above 4 GiB is looked up whole, never as a low address.
    assert_eq!(fault_address(0x1000), 0x1000);
    assert_eq!(fault_address(1 << 32 | 0x1000), 1 << 32 | 0x1000);
}
