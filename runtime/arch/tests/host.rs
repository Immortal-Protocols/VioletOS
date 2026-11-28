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
