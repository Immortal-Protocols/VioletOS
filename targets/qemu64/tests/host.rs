//! Host tests for the qemu64 bootstrap task's logic (make test-unit): the
//! untyped-memory planning, the Linux calls, and each program's address space
//! and loading (space.rs), against in-memory fakes.
#[path = "../../../runtime/arch/mod.rs"]
mod arch;
#[path = "../bootstrap.rs"]
mod bootstrap;
#[path = "../linux.rs"]
mod linux;
#[path = "../space.rs"]
mod space;

use arch::{Protection, Syscall};
use bootstrap::Untyped;
use linux::*;
use std::collections::BTreeMap;

#[test]
fn fillers_reach_a_device_address_exactly() {
    // The device region QEMU virt's UART sits in starts just after the
    // interrupt controller's frames; the UART is at 0x0900_0000.
    let mut region = Untyped::new(7, 0x0812_0000, 27, true);
    region.paddr = 0x0812_0000;
    let mut fillers = Vec::new();
    while let Some(bits) = region.filler(0x0900_0000) {
        // Each filler lands exactly at the cursor: it is aligned to its own size.
        assert_eq!(region.cursor % (1 << bits), 0);
        let at = region.take(bits).unwrap();
        fillers.push((at, bits));
    }
    assert_eq!(region.cursor, 0x0900_0000);
    assert_eq!(fillers.first(), Some(&(0x0812_0000, 17)));
    // Powers of two, so a 14 MiB gap takes a handful of objects, not thousands of pages.
    assert!(fillers.len() <= 8, "{fillers:?}");
    assert_eq!(fillers.iter().map(|&(_, b)| 1u64 << b).sum::<u64>(), 0x0900_0000 - 0x0812_0000);
    assert_eq!(region.take(12), Some(0x0900_0000));
    // The address is behind the cursor now: it cannot be made a second time.
    assert!(!region.reaches(0x0900_0000, 12));
    assert_eq!(region.filler(0x0900_0000), None);
}

#[test]
fn regions_are_chosen_by_kind_reach_and_room() {
    let regions = [
        Untyped::new(1, 0x0000_0000, 27, true),
        Untyped::new(2, 0x0812_0000, 16, true),
        Untyped::new(3, 0x4024_0000, 18, false),
        Untyped::new(4, 0x4100_0000, 12, false),
    ];
    assert_eq!(bootstrap::device_for(&regions, 0x0812_1000, 12), Some(1));
    assert_eq!(bootstrap::device_for(&regions, 0x0813_0000, 12), None, "one past the region");
    assert_eq!(bootstrap::device_for(&regions, 0x0812_0800, 12), None, "not page aligned");
    assert_eq!(bootstrap::device_for(&regions, 0x4024_0000, 12), None, "ordinary memory is not a device");
    assert_eq!(bootstrap::memory_for(&regions, 12), Some(2));
    assert_eq!(bootstrap::memory_for(&regions, 20), None, "nothing that large");
    // A region too full for the object is skipped for the next one.
    let mut regions = regions;
    regions[2].cursor = regions[2].end() - 0x800;
    assert_eq!(bootstrap::memory_for(&regions, 12), Some(3));
    assert_eq!(bootstrap::memory_for(&regions, 11), Some(2));
    // Objects land aligned to their size, and never past the end.
    let mut region = Untyped::new(9, 0x1000, 13, false);
    assert_eq!(region.take(4), Some(0x1000));
    assert_eq!(region.take(12), Some(0x2000));
    assert_eq!(region.take(12), None);
    assert_eq!(region.cursor, 0x3000);
}

/// A program's memory as a map of pages, with the protection each was given.
#[derive(Default)]
struct Pages {
    pages: BTreeMap<u64, (Vec<u8>, Protection)>,
    budget: Option<u64>,
}
impl Memory for Pages {
    fn read(&self, va: u64, out: &mut [u8]) -> bool {
        for (i, byte) in out.iter_mut().enumerate() {
            let at = va + i as u64;
            match self.pages.get(&(at & !(PAGE - 1))) {
                Some((data, _)) => *byte = data[(at % PAGE) as usize],
                None => return false,
            }
        }
        true
    }
    fn write(&mut self, va: u64, data: &[u8]) -> bool {
        let writable = |at: u64| matches!(self.pages.get(&(at & !(PAGE - 1))), Some((_, Protection::ReadWrite)));
        if !(0..data.len() as u64).all(|i| writable(va + i)) {
            return false;
        }
        for (i, byte) in data.iter().enumerate() {
            let at = va + i as u64;
            self.pages.get_mut(&(at & !(PAGE - 1))).unwrap().0[(at % PAGE) as usize] = *byte;
        }
        true
    }
    fn map(&mut self, va: u64, pages: u64, protection: Protection) -> bool {
        if self.budget.is_some_and(|b| b < pages) || (0..pages).any(|i| self.pages.contains_key(&(va + i * PAGE))) {
            return false;
        }
        if let Some(b) = self.budget.as_mut() {
            *b -= pages;
        }
        for i in 0..pages {
            self.pages.insert(va + i * PAGE, (vec![0; PAGE as usize], protection));
        }
        true
    }
    fn unmap(&mut self, va: u64, pages: u64) {
        for i in 0..pages {
            self.pages.remove(&(va + i * PAGE));
        }
    }
    fn protect(&mut self, va: u64, pages: u64, protection: Protection) -> bool {
        if (0..pages).any(|i| !self.pages.contains_key(&(va + i * PAGE))) {
            return false;
        }
        for i in 0..pages {
            self.pages.get_mut(&(va + i * PAGE)).unwrap().1 = protection;
        }
        true
    }
}
#[derive(Default)]
struct Text(Vec<u8>);
impl Console for Text {
    fn write(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

fn number(name: &str) -> u64 {
    arch::linux::CALLS.iter().find(|c| c.name == name).unwrap().aarch64.into()
}
fn call(name: &str, args: [u64; 6]) -> Syscall {
    Syscall { number: number(name), args, pc: 0x40_0000, sp: 0x7fff_0000 }
}
const DATA: u64 = 0x41_0000;
fn setup() -> (Process, Pages, Text) {
    let mut memory = Pages::default();
    assert!(memory.map(DATA, 2, Protection::ReadWrite));
    assert!(memory.map(0x40_0000, 1, Protection::ReadExecute));
    (Process::new(5, 0x41_2000, 0x1_0000_0000, 0x1_0001_0000), memory, Text::default())
}

#[test]
fn write_and_writev_copy_from_the_program_or_fail_with_efault() {
    let (mut p, mut m, mut out) = setup();
    assert!(m.write(DATA + 4090, b"across a page"));
    assert_eq!(p.syscall(&call("write", [1, DATA + 4090, 13, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(13));
    assert_eq!(out.0, b"across a page");
    // 64-bit iovec: {base, len} pairs of 8-byte words.
    let mut iov = Vec::new();
    for (base, len) in [(DATA + 4090, 7u64), (DATA + 4097, 6)] {
        iov.extend_from_slice(&base.to_le_bytes());
        iov.extend_from_slice(&len.to_le_bytes());
    }
    assert!(m.write(DATA + 100, &iov));
    out.0.clear();
    assert_eq!(p.syscall(&call("writev", [2, DATA + 100, 2, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(13));
    assert_eq!(out.0, b"across a page");
    // An unmapped buffer, a buffer that runs off the mapping, a wild iovec pointer.
    assert_eq!(p.syscall(&call("write", [1, 0xdead_0000, 4, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EFAULT));
    out.0.clear();
    assert_eq!(p.syscall(&call("write", [1, DATA + 2 * PAGE - 3, 10, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(3), "a short write, as Linux");
    assert_eq!(p.syscall(&call("write", [1, u64::MAX - 1, 10, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EFAULT), "no wrap-around");
    assert_eq!(p.syscall(&call("writev", [1, 0x10, 1, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EFAULT));
    assert_eq!(p.syscall(&call("writev", [1, DATA, 17, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EINVAL));
    // Only the console's descriptors exist.
    assert_eq!(p.syscall(&call("write", [0, DATA, 1, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EBADF));
    assert_eq!(p.syscall(&call("write", [3, DATA, 1, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EBADF));
}

#[test]
fn brk_grows_shrinks_and_reports_failure_by_not_moving() {
    let (mut p, mut m, mut out) = setup();
    let start = match p.syscall(&call("brk", [0; 6]), &mut m, &mut out, 0) {
        Outcome::Return(v) => v as u64,
        other => panic!("{other:?}"),
    };
    assert_eq!(start, 0x41_2000);
    assert_eq!(p.syscall(&call("brk", [start + 5000, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return((start + 5000) as i64));
    assert!(m.pages.contains_key(&start) && m.pages.contains_key(&(start + PAGE)) && !m.pages.contains_key(&(start + 2 * PAGE)));
    assert_eq!(m.pages[&start].1, Protection::ReadWrite);
    // Shrinking gives the pages back.
    assert_eq!(p.syscall(&call("brk", [start + 10, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return((start + 10) as i64));
    assert!(m.pages.contains_key(&start) && !m.pages.contains_key(&(start + PAGE)));
    // Below the start, beyond the limit, or with no memory left: the break stays.
    assert_eq!(p.syscall(&call("brk", [start - 1, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return((start + 10) as i64));
    assert_eq!(p.syscall(&call("brk", [start + (64 << 20), 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return((start + 10) as i64));
    m.budget = Some(1);
    assert_eq!(p.syscall(&call("brk", [start + 3 * PAGE, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return((start + 10) as i64));
    assert!(!m.pages.contains_key(&(start + PAGE)), "a failed growth leaves nothing mapped");
}

#[test]
fn mmap_is_anonymous_private_and_never_writable_and_executable() {
    let (mut p, mut m, mut out) = setup();
    let anon = |length: u64, prot: u64, flags: u64| call("mmap", [0, length, prot, flags, u64::MAX, 0]);
    assert_eq!(p.syscall(&anon(PAGE + 1, 3, 0x22), &mut m, &mut out, 0), Outcome::Return(0x1_0000_0000));
    assert_eq!(m.pages[&0x1_0000_1000].1, Protection::ReadWrite);
    assert_eq!(p.syscall(&anon(PAGE, 5, 0x22), &mut m, &mut out, 0), Outcome::Return(0x1_0000_2000), "the next one follows");
    assert_eq!(m.pages[&0x1_0000_2000].1, Protection::ReadExecute);
    // W^X, file mappings, fixed addresses, shared mappings, zero length.
    for (length, prot, flags) in [(PAGE, 7, 0x22), (PAGE, 6, 0x22), (PAGE, 3, 0x02), (PAGE, 3, 0x32), (PAGE, 3, 0x21), (0, 3, 0x22)] {
        assert_eq!(p.syscall(&anon(length, prot, flags), &mut m, &mut out, 0), Outcome::Return(EINVAL), "{length} {prot} {flags:#x}");
    }
    // The range is finite: 16 pages here, 3 used.
    assert_eq!(p.syscall(&anon(14 * PAGE, 3, 0x22), &mut m, &mut out, 0), Outcome::Return(ENOMEM));
    assert_eq!(p.syscall(&anon(u64::MAX - 5, 3, 0x22), &mut m, &mut out, 0), Outcome::Return(ENOMEM));
    // mprotect changes what is mapped, refuses W+X and what is not mapped.
    assert_eq!(p.syscall(&call("mprotect", [0x1_0000_0000, PAGE, 1, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(0));
    assert_eq!(m.pages[&0x1_0000_0000].1, Protection::ReadOnly);
    assert_eq!(p.syscall(&call("mprotect", [0x1_0000_0000, PAGE, 7, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EINVAL));
    assert_eq!(p.syscall(&call("mprotect", [0x1_0000_8000, PAGE, 1, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(ENOMEM));
    assert_eq!(p.syscall(&call("munmap", [0x1_0000_0000, 2 * PAGE, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(0));
    assert!(!m.pages.contains_key(&0x1_0000_0000) && !m.pages.contains_key(&0x1_0000_1000) && m.pages.contains_key(&0x1_0000_2000));
    assert_eq!(p.syscall(&call("munmap", [0x1_0000_0001, PAGE, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EINVAL));
}

#[test]
fn clock_identity_exit_and_everything_else() {
    let (mut p, mut m, mut out) = setup();
    assert_eq!(p.syscall(&call("clock_gettime", [1, DATA, 0, 0, 0, 0]), &mut m, &mut out, 3_000_000_007), Outcome::Return(0));
    let mut ts = [0u8; 16];
    assert!(m.read(DATA, &mut ts));
    assert_eq!((u64::from_le_bytes(ts[..8].try_into().unwrap()), u64::from_le_bytes(ts[8..].try_into().unwrap())), (3, 7));
    // The wall clock has no source; a read-only or missing buffer is EFAULT.
    assert_eq!(p.syscall(&call("clock_gettime", [0, DATA, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EINVAL));
    assert_eq!(p.syscall(&call("clock_gettime", [1, 0x40_0000, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EFAULT));
    assert_eq!(p.syscall(&call("clock_gettime", [1, 0x9000, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EFAULT));
    assert_eq!(p.syscall(&call("getpid", [0; 6]), &mut m, &mut out, 0), Outcome::Return(5));
    assert_eq!(p.syscall(&call("set_tid_address", [DATA, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(5));
    assert_eq!(p.syscall(&call("ioctl", [1, 0x5413, DATA, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(ENOTTY));
    assert_eq!(p.syscall(&call("exit_group", [3, 0, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Exit(3));
    // Anything else is ENOSYS and named on the console: by name, or by number when unknown.
    out.0.clear();
    assert_eq!(p.syscall(&call("openat", [0; 6]), &mut m, &mut out, 0), Outcome::Return(ENOSYS));
    assert_eq!(p.syscall(&Syscall { number: 441, args: [0; 6], pc: 0, sp: 0 }, &mut m, &mut out, 0), Outcome::Return(ENOSYS));
    assert_eq!(out.0, b"qemu64: unhandled Linux call openat\nqemu64: unhandled Linux call 441\n");
}

#[test]
fn prot_none_is_never_readable() {
    let (mut p, mut m, mut out) = setup();
    let anon = |length: u64, prot: u64| call("mmap", [0, length, prot, 0x22, u64::MAX, 0]);
    // A PROT_NONE mapping reserves its range and maps nothing, so any access faults.
    assert_eq!(p.syscall(&anon(2 * PAGE, 0), &mut m, &mut out, 0), Outcome::Return(0x1_0000_0000));
    assert!(!m.pages.contains_key(&0x1_0000_0000) && !m.pages.contains_key(&0x1_0000_1000));
    assert_eq!(p.syscall(&anon(PAGE, 3), &mut m, &mut out, 0), Outcome::Return(0x1_0000_2000), "the next mapping goes after it");
    // mprotect to PROT_NONE is refused, never turned into a readable page.
    assert_eq!(p.syscall(&call("mprotect", [0x1_0000_2000, PAGE, 0, 0, 0, 0]), &mut m, &mut out, 0), Outcome::Return(EINVAL));
    assert_eq!(m.pages[&0x1_0000_2000].1, Protection::ReadWrite);
}

// ---- One program's address space (space.rs) against a model of the kernel ----

use space::{Frames, Layout, Space, ALIAS, MAX_PAGES, STACK_PAGES};

const OWN: usize = 1;
const PROGRAM: usize = 2;

#[derive(Clone)]
struct Cap {
    /// The frame this capability names, or None for a page table.
    frame: Option<usize>,
    mapped: Option<(usize, u64)>,
}

/// The kernel as the address space sees it: an arena of capability slots
/// and memory, frames with contents, and one table level per 2 MiB, enough
/// for seL4's rules that matter here: a frame capability maps at one place
/// only, nothing maps twice, and a missing table is reported.
struct Fake {
    caps: BTreeMap<usize, Cap>,
    next_slot: usize,
    slots: usize,
    objects: usize,
    memory: usize,
    contents: Vec<Vec<u8>>,
    maps: BTreeMap<(usize, u64), usize>,
    tables: std::collections::BTreeSet<(usize, u64)>,
    /// Fail the n-th mapping into the program (counting from 0), once.
    fail_program_map: Option<usize>,
    program_maps: usize,
    unified: Vec<usize>,
    resets: usize,
}
impl Fake {
    fn new(slots: usize, memory: usize) -> Fake {
        Fake { caps: BTreeMap::new(), next_slot: 100, slots, objects: 0, memory, contents: Vec::new(), maps: BTreeMap::new(),
               tables: Default::default(), fail_program_map: None, program_maps: 0, unified: Vec::new(), resets: 0 }
    }
    fn slot(&mut self) -> Result<usize, arch::MapError> {
        if self.next_slot - 100 >= self.slots {
            return Err(arch::MapError::Exhausted);
        }
        self.next_slot += 1;
        Ok(self.next_slot - 1)
    }
    fn object(&mut self, frame: Option<usize>) -> Result<usize, arch::MapError> {
        if self.objects == self.memory {
            return Err(arch::MapError::Exhausted);
        }
        let slot = self.slot()?;
        self.objects += 1;
        self.caps.insert(slot, Cap { frame, mapped: None });
        Ok(slot)
    }
    fn aliases(&self) -> usize {
        self.maps.keys().filter(|(vspace, va)| *vspace == OWN && *va >= ALIAS).count()
    }
    fn byte(&self, va: u64) -> Option<&u8> {
        let slot = self.maps.get(&(OWN, va & !(PAGE - 1)))?;
        self.contents[self.caps[slot].frame?].get((va % PAGE) as usize)
    }
}
impl arch::Kernel for Fake {
    fn map_frame(&mut self, frame: usize, vspace: usize, va: u64, _bits: u8, _p: Protection, _device: bool) -> Result<(), arch::MapError> {
        if vspace == PROGRAM {
            self.program_maps += 1;
            if self.fail_program_map == Some(self.program_maps - 1) {
                self.fail_program_map = None;
                return Err(arch::MapError::Exhausted);
            }
        }
        let cap = self.caps.get(&frame).expect("mapping a capability that does not exist").clone();
        assert!(cap.frame.is_some(), "mapping a table as a frame");
        if !self.tables.contains(&(vspace, va >> 21)) {
            return Err(arch::MapError::MissingTable);
        }
        match cap.mapped {
            Some(at) if at == (vspace, va) => return Ok(()), // a remap: new rights
            Some(_) => return Err(arch::MapError::Kernel(3)), // seL4: a mapped frame cap maps nowhere else
            None => {}
        }
        if self.maps.contains_key(&(vspace, va)) {
            return Err(arch::MapError::Occupied);
        }
        self.maps.insert((vspace, va), frame);
        self.caps.get_mut(&frame).unwrap().mapped = Some((vspace, va));
        Ok(())
    }
    fn new_table(&mut self) -> Result<usize, arch::MapError> {
        self.object(None)
    }
    fn map_table(&mut self, _table: usize, vspace: usize, va: u64) -> Result<(), arch::MapError> {
        if !self.tables.insert((vspace, va >> 21)) {
            return Err(arch::MapError::Occupied);
        }
        Ok(())
    }
}
impl Frames for Fake {
    fn frame(&mut self) -> Result<usize, arch::MapError> {
        let id = self.contents.len();
        let slot = self.object(Some(id))?;
        self.contents.push(vec![0; PAGE as usize]);
        Ok(slot)
    }
    fn copy(&mut self, frame: usize) -> Result<usize, arch::MapError> {
        let slot = self.slot()?;
        self.caps.insert(slot, Cap { frame: self.caps[&frame].frame, mapped: None });
        Ok(slot)
    }
    fn unmap(&mut self, frame: usize) {
        if let Some(at) = self.caps.get_mut(&frame).and_then(|c| c.mapped.take()) {
            self.maps.remove(&at);
        }
    }
    fn unify(&mut self, frame: usize) {
        assert!(self.caps[&frame].mapped.is_some(), "seL4 cleans only a mapped frame");
        self.unified.push(frame);
    }
    fn own(&self) -> usize {
        OWN
    }
    fn poke(&mut self, va: u64, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            let at = va + i as u64;
            let slot = self.maps[&(OWN, at & !(PAGE - 1))];
            let frame = self.caps[&slot].frame.unwrap();
            self.contents[frame][(at % PAGE) as usize] = *b;
        }
    }
    fn peek(&self, va: u64, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = *self.byte(va + i as u64).expect("reading an alias that is not mapped");
        }
    }
    fn zero(&mut self, va: u64, bytes: usize) {
        self.poke(va, &vec![0; bytes]);
    }
    fn reset(&mut self) {
        // seL4_CNode_Revoke on the arena's untyped: every capability made from
        // it is deleted, which unmaps it, and its memory and slots are free again.
        self.caps.clear();
        self.maps.clear();
        self.tables.clear();
        self.next_slot = 100;
        self.objects = 0;
        self.resets += 1;
    }
}

/// A static ELF64 whose first segment starts at file offset 0 and so carries
/// the headers, as linkers lay them out: (va, flags, file bytes, memory size).
/// `first_file_bytes`: how many file bytes the first segment covers, if not all of its own.
fn elf64(entry: u64, segments: &[(u64, u32, Vec<u8>, u64)], first_file_bytes: Option<u64>) -> Vec<u8> {
    let table = 64 + 56 * segments.len();
    let mut image = vec![0u8; table];
    image[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    image[16..18].copy_from_slice(&2u16.to_le_bytes());
    image[18..20].copy_from_slice(&183u16.to_le_bytes());
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes());
    image[54..56].copy_from_slice(&56u16.to_le_bytes());
    image[56..58].copy_from_slice(&(segments.len() as u16).to_le_bytes());
    for (i, (va, flags, bytes, memory)) in segments.iter().enumerate() {
        let at = 64 + i * 56;
        // The first segment begins at the file's start (headers included).
        let (offset, file, va, memory) = if i == 0 {
            let file = first_file_bytes.unwrap_or((table + bytes.len()) as u64);
            (0u64, file, *va, (*memory).max(file))
        } else {
            (image.len() as u64, bytes.len() as u64, *va, *memory)
        };
        image.extend_from_slice(bytes);
        let put = |image: &mut Vec<u8>, o: usize, v: u64, n: usize| image[at + o..at + o + n].copy_from_slice(&v.to_le_bytes()[..n]);
        put(&mut image, 0, 1, 4);
        put(&mut image, 4, u64::from(*flags), 4);
        put(&mut image, 8, offset, 8);
        put(&mut image, 16, va, 8);
        put(&mut image, 32, file, 8);
        put(&mut image, 40, memory, 8);
    }
    image
}

fn layout() -> Layout {
    Layout { stack_top: space::STACK_HIGH, mmap_base: space::MMAP_LOW, mmap_limit: space::MMAP_LOW + space::MMAP_BYTES, random: *b"0123456789abcdef" }
}
fn hello() -> Vec<u8> {
    elf64(0x40_0078, &[(0x40_0000, 5, b"\x1f\x20\x03\xd5".to_vec(), 0), (0x41_0000, 6, b"data".to_vec(), 0x2000)], None)
}
/// One program's life the way root.rs runs it: an address space, its image, and back.
fn run(kernel: &mut Fake, image: &[u8], args: &[&[u8]], layout: &Layout) -> Result<space::Start, &'static str> {
    let elf = arch::Elf::parse(arch::NATIVE, image).map_err(|_| "not a program")?;
    space::contained(kernel, |kernel| {
        let mut space = Space::new(kernel, PROGRAM);
        space::load(&mut space, &elf, args, layout)
    })
}

#[test]
fn a_program_that_fails_to_load_leaves_nothing_behind() {
    // Its second segment lies inside the stack's range: the segments map, then the stack does not.
    let clash = elf64(0x40_0078, &[(0x40_0000, 5, b"code".to_vec(), 0), (space::STACK_HIGH - 4 * PAGE, 6, b"x".to_vec(), 0x10)], None);
    let mut kernel = Fake::new(4096, 2048);
    assert_eq!(run(&mut kernel, &clash, &[b"one"], &layout()), Err("no memory for the stack"));
    assert_eq!((kernel.maps.len(), kernel.caps.len(), kernel.resets), (0, 0, 1), "every mapping and capability taken back");
    // Arguments that do not fit the stack page, after everything else mapped.
    let long = vec![b'a'; 5000];
    assert_eq!(run(&mut kernel, &hello(), &[&long], &layout()), Err("arguments too long"));
    assert_eq!((kernel.maps.len(), kernel.caps.len()), (0, 0));
    // More pages than one program may hold.
    let big = elf64(0x40_0078, &[(0x40_0000, 5, b"code".to_vec(), 0), (0x50_0000, 6, b"".to_vec(), (MAX_PAGES as u64 + 1) * PAGE)], None);
    assert_eq!(run(&mut kernel, &big, &[b"big"], &layout()), Err("cannot map the program"));
    assert_eq!((kernel.maps.len(), kernel.caps.len()), (0, 0));
    // So the next program at the same addresses loads.
    let start = run(&mut kernel, &hello(), &[b"hello"], &layout()).unwrap();
    assert_eq!(start.entry, 0x40_0078);
    // Without `contained`, the first failure's aliases block the next program,
    // which is the fault this test exists to catch.
    let mut leaky = Fake::new(4096, 2048);
    let elf = arch::Elf::parse(arch::NATIVE, &clash).unwrap();
    assert!(space::load(&mut Space::new(&mut leaky, PROGRAM), &elf, &[b"one"], &layout()).is_err());
    let second = hello();
    let elf = arch::Elf::parse(arch::NATIVE, &second).unwrap();
    assert_eq!(space::load(&mut Space::new(&mut leaky, PROGRAM), &elf, &[b"two"], &layout()), Err("cannot map the program"));
}

#[test]
fn programs_never_wear_down_the_kernel_for_later_ones() {
    // A small arena: every program gets all of it, and gives all of it back.
    let mut kernel = Fake::new(256, 96);
    for _ in 0..10 {
        let start = space::contained(&mut kernel, |kernel| {
            let image = hello();
            let elf = arch::Elf::parse(arch::NATIVE, &image).unwrap();
            let mut space = Space::new(kernel, PROGRAM);
            let start = space::load(&mut space, &elf, &[b"hello"], &layout()).unwrap();
            // brk(cur + 4096); brk(cur) in a loop: the same frame each time.
            let mut process = linux::Process::new(1, start.image_end, space::MMAP_LOW, space::MMAP_LOW + space::MMAP_BYTES);
            for _ in 0..500 {
                let grown = process.syscall(&call("brk", [start.image_end + PAGE, 0, 0, 0, 0, 0]), &mut space, &mut Text::default(), 0);
                assert_eq!(grown, Outcome::Return((start.image_end + PAGE) as i64));
                assert!(space.write(start.image_end, b"dirty"));
                process.syscall(&call("brk", [start.image_end, 0, 0, 0, 0, 0]), &mut space, &mut Text::default(), 0);
            }
            // mmap and munmap in a loop: fresh addresses, recycled frames.
            for _ in 0..200 {
                let Outcome::Return(at) = process.syscall(&call("mmap", [0, PAGE, 3, 0x22, u64::MAX, 0]), &mut space, &mut Text::default(), 0) else { panic!() };
                assert!(at > 0, "mmap failed: {at}");
                let mut byte = [9u8];
                assert!(space.read(at as u64, &mut byte) && byte == [0], "a recycled frame is zeroed");
                assert!(space.write(at as u64, b"x"));
                process.syscall(&call("munmap", [at as u64, PAGE, 0, 0, 0, 0]), &mut space, &mut Text::default(), 0);
            }
            let (mapped, held) = space.usage();
            assert!(held <= 2 + 2 + STACK_PAGES as usize + 1, "frames held: {held}, mapped {mapped}");
            start
        });
        assert_eq!(start.entry, 0x40_0078);
        assert_eq!((kernel.maps.len(), kernel.objects), (0, 0));
    }
}

#[test]
fn a_mapping_that_fails_halfway_is_undone() {
    let mut kernel = Fake::new(4096, 2048);
    let mut space = Space::new(&mut kernel, PROGRAM);
    // The alias is mapped here, then the program's own mapping fails.
    space.kernel.fail_program_map = Some(0);
    assert!(space.map_one(0x40_0000, Protection::ReadWrite).is_err());
    assert_eq!(space.kernel.aliases(), 0, "no alias left at ALIAS + va");
    assert_eq!(space.usage(), (0, 1), "the frame is kept as a spare");
    // The same address maps on the next try, with the spare frame.
    assert!(space.map_one(0x40_0000, Protection::ReadWrite).is_ok());
    assert_eq!(space.usage(), (1, 1));
    assert_eq!(space.kernel.aliases(), 1);
    // A spare reused for another address fails the same way, and is undone too.
    space.unmap(0x40_0000, 1);
    let maps = space.kernel.program_maps;
    space.kernel.fail_program_map = Some(maps);
    assert!(space.map_one(0x50_0000, Protection::ReadOnly).is_err());
    assert_eq!((space.kernel.aliases(), space.usage()), (0, (0, 1)));
    // Out of memory for the frame itself: nothing recorded, nothing mapped.
    space.kernel.memory = space.kernel.objects;
    space.kernel.next_slot = 100 + space.kernel.slots;
    assert!(space.map_one(0x48_0000, Protection::ReadWrite).is_ok(), "the spare still serves");
    assert_eq!(space.map_one(0x48_1000, Protection::ReadWrite), Err(arch::MapError::Exhausted));
    assert_eq!(space.usage(), (1, 1));
}

#[test]
fn code_is_made_visible_to_instruction_fetch() {
    let mut kernel = Fake::new(4096, 2048);
    let elf_image = hello();
    let elf = arch::Elf::parse(arch::NATIVE, &elf_image).unwrap();
    let mut space = Space::new(&mut kernel, PROGRAM);
    space::load(&mut space, &elf, &[b"hello"], &layout()).unwrap();
    // The code page (and only executable pages) went through the cache maintenance call.
    let code = space.kernel.maps[&(PROGRAM, 0x40_0000)];
    let data = space.kernel.maps[&(PROGRAM, 0x41_0000)];
    assert!(space.kernel.unified.contains(&code));
    assert!(!space.kernel.unified.contains(&data));
    // A page made executable later, and fresh executable memory, too.
    let before = space.kernel.unified.len();
    assert!(space.protect(0x41_0000, 1, Protection::ReadExecute));
    assert_eq!(space.kernel.unified.last(), Some(&data));
    assert!(space.map(0x7000_0000, 1, Protection::ReadExecute));
    assert_eq!(space.kernel.unified.len(), before + 2);
}

/// The initial stack's argc, argv strings and auxiliary vector, read back from the program.
fn auxv(space: &Space<'_, Fake>, sp: u64) -> (u64, Vec<(u64, u64)>) {
    let word = |at: u64| {
        let mut b = [0u8; 8];
        assert!(space.read(at, &mut b));
        u64::from_le_bytes(b)
    };
    let argc = word(sp);
    let mut at = sp + 8 * (argc + 2); // argv and its NULL
    while word(at) != 0 {
        at += 8; // the environment
    }
    at += 8;
    let mut pairs = Vec::new();
    loop {
        let (kind, value) = (word(at), word(at + 8));
        if kind == 0 {
            return (argc, pairs);
        }
        pairs.push((kind, value));
        at += 16;
    }
}

#[test]
fn the_initial_stack_carries_at_random_and_a_complete_header_table() {
    let mut kernel = Fake::new(4096, 2048);
    let layout = Layout::from_random(&[0x5a; 32]);
    let image = hello();
    let elf = arch::Elf::parse(arch::NATIVE, &image).unwrap();
    let mut space = Space::new(&mut kernel, PROGRAM);
    let start = space::load(&mut space, &elf, &[b"hello", b"x"], &layout).unwrap();
    let (argc, aux) = auxv(&space, start.sp);
    assert_eq!(argc, 2);
    let get = |kind| aux.iter().find(|(k, _)| *k == kind).map(|&(_, v)| v);
    // AT_RANDOM points at the layout's 16 bytes, at the top of the stack.
    let random = get(arch::AT_RANDOM).expect("AT_RANDOM");
    assert_eq!(random, layout.stack_top - 16);
    let mut bytes = [0u8; 16];
    assert!(space.read(random, &mut bytes));
    assert_eq!(bytes, layout.random);
    // The whole header table is in the first segment: AT_PHDR at its address.
    assert_eq!(get(arch::AT_PHDR), Some(0x40_0000 + 64));
    assert_eq!((get(arch::AT_PHENT), get(arch::AT_PHNUM)), (Some(56), Some(2)));
    drop(space);

    // A first segment that ends inside the table (after the first entry): no AT_PHDR,
    // so a C library never reads entries that are not mapped.
    let cut = elf64(0x40_0078, &[(0x40_0000, 5, Vec::new(), 0x1000), (0x41_0000, 6, b"data".to_vec(), 0x10)], Some(64 + 56));
    let mut kernel = Fake::new(4096, 2048);
    let elf = arch::Elf::parse(arch::NATIVE, &cut).unwrap();
    let mut space = Space::new(&mut kernel, PROGRAM);
    let start = space::load(&mut space, &elf, &[b"cut"], &layout).unwrap();
    let (_, aux) = auxv(&space, start.sp);
    assert!(aux.iter().all(|(k, _)| *k != arch::AT_PHDR && *k != arch::AT_PHNUM), "{aux:?}");
    assert!(aux.iter().any(|(k, _)| *k == arch::AT_RANDOM));
}

#[test]
fn each_program_gets_its_own_layout() {
    let a = Layout::from_random(&[0; 32]);
    assert_eq!((a.stack_top, a.mmap_base), (space::STACK_HIGH, space::MMAP_LOW));
    let mut bytes = [0xffu8; 32];
    let b = Layout::from_random(&bytes);
    assert!(b.stack_top % PAGE == 0 && b.stack_top > space::STACK_HIGH - space::STACK_SPREAD && b.stack_top <= space::STACK_HIGH);
    assert!(b.mmap_base % PAGE == 0 && b.mmap_base >= space::MMAP_LOW && b.mmap_base < space::MMAP_LOW + space::MMAP_SPREAD);
    assert_eq!(b.mmap_limit - b.mmap_base, space::MMAP_BYTES);
    assert!(b.mmap_limit <= ALIAS, "anonymous memory stays below the aliases");
    bytes[0] = 1;
    assert_ne!(Layout::from_random(&bytes).stack_top, b.stack_top);
    assert_eq!(b.random, [0xff; 16]);
}
