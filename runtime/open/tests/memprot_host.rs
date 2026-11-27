//! Host tests for open-app memory protection (`make test-unit`):
//! vma.rs (rules), procmgr.rs (address spaces, the ELF loader, copies to and
//! from app memory) and entropy.rs (the random generator).
//!
//! procmgr.rs talks to seL4 through `sys` and takes memory from `pool`; here
//! both are stand-ins that record what the app's hardware mapping of every
//! page would be. The stand-in `page_map` panics on a mapping that is both
//! writable and executable, so every test also checks that invariant.
#![allow(dead_code)]
extern crate alloc;

#[path = "../vma.rs"]
mod vma;
#[path = "../entropy.rs"]
mod entropy;
#[path = "../../../system/supervisor/procmgr.rs"]
mod procmgr;
// The signal gate's words, which procmgr writes into the trampoline page.
#[path = "../signal.rs"]
mod signal;

mod syscall {
    pub const ENOMEM: i32 = -12;
    pub const EACCES: i32 = -13;
    pub const EEXIST: i32 = -17;
}

mod sys {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    pub const PAGE_BITS: u32 = 16;
    pub const PAGE: usize = 1 << PAGE_BITS;
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Object {
        PageDirectory,
        PageTable,
        CNode,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Mapping {
        pub va: u32,
        pub writable: bool,
        pub exec: bool,
    }
    #[derive(Default)]
    pub struct Hardware {
        /// Frame capability -> how it is mapped in the app now.
        pub mapped: BTreeMap<u32, Mapping>,
        pub maps: u32,
        pub unifies: Vec<u32>,
        /// Frame capability -> the pool page it is a copy of.
        pub frames: BTreeMap<u32, u32>,
        /// Make the next in-place remap fail, to test the fallback.
        pub fail_remaps: u32,
    }
    thread_local!(pub static HW: RefCell<Hardware> = RefCell::new(Hardware::default()));

    pub fn asid_assign(_pd: u32) -> Result<(), i32> {
        Ok(())
    }
    pub fn pt_map(_pt: u32, _pd: u32, _va: u32) -> Result<(), i32> {
        Ok(())
    }
    pub fn copy(dest: u32, src: u32) -> Result<(), i32> {
        HW.with(|h| h.borrow_mut().frames.insert(dest, src));
        Ok(())
    }
    pub fn page_map(frame: u32, _pd: u32, va: u32, writable: bool, exec: bool) -> Result<(), i32> {
        assert!(!(writable && exec), "a page mapped writable and executable at {va:#x}");
        assert_eq!(va & 0xffff, 0);
        HW.with(|h| {
            let mut h = h.borrow_mut();
            if let Some(old) = h.mapped.get(&frame) {
                // seL4 changes rights in place only at the same address.
                assert_eq!(old.va, va, "a frame mapped at two addresses");
                if h.fail_remaps > 0 {
                    h.fail_remaps -= 1;
                    return Err(3);
                }
            }
            h.maps += 1;
            h.mapped.insert(frame, Mapping { va, writable, exec });
            Ok(())
        })
    }
    pub fn page_unmap(frame: u32) {
        HW.with(|h| h.borrow_mut().mapped.remove(&frame));
    }
    pub fn page_unify(frame: u32) {
        HW.with(|h| {
            let mut h = h.borrow_mut();
            assert!(h.mapped.contains_key(&frame), "seL4 refuses to flush a frame that is not mapped");
            h.unifies.push(frame);
        });
    }
    pub fn delete(slot: u32) {
        page_unmap(slot);
    }
    /// The app's mapping of the 64 KiB page at `va`: (writable, executable).
    pub fn at(va: u32) -> Option<(bool, bool)> {
        HW.with(|h| h.borrow().mapped.values().find(|m| m.va == va & !0xffff).map(|m| (m.writable, m.exec)))
    }
}

mod pool {
    use super::sys::{Object, PAGE};
    use std::cell::RefCell;
    pub const PAGES: usize = 96;
    thread_local!(static MEMORY: RefCell<Vec<u8>> = RefCell::new(vec![0u8; PAGES * PAGE]));
    pub fn page_va(index: u32) -> usize {
        MEMORY.with(|m| m.borrow_mut().as_mut_ptr() as usize) + index as usize * PAGE
    }
    pub struct Kernel {
        next: u32,
    }
    impl Kernel {
        pub fn new() -> Kernel {
            Kernel { next: 1000 }
        }
        pub fn slot(&mut self) -> u32 {
            self.next += 1;
            self.next
        }
        pub fn release(&mut self, _slot: u32) {}
        pub fn object(&mut self, _untyped: u32, _o: Object, _bits: u32) -> Result<u32, i32> {
            Ok(self.slot())
        }
    }
    pub struct Pool {
        free: Vec<u32>,
    }
    impl Pool {
        pub fn new() -> Pool {
            Pool { free: (0..PAGES as u32).rev().collect() }
        }
        pub fn cap(&self, index: u32) -> u32 {
            index
        }
        pub fn available(&self) -> usize {
            self.free.len()
        }
        pub fn take(&mut self) -> Option<u32> {
            let index = self.free.pop()?;
            unsafe { core::ptr::write_bytes(page_va(index) as *mut u8, 0, PAGE) };
            Some(index)
        }
        pub fn give_back(&mut self, index: u32) {
            self.free.push(index);
        }
    }
}

use pool::{Kernel, Pool};
use procmgr::{load, Space, PROT_EXEC, PROT_READ, PROT_WRITE};
use vma::{Access, Layout, Vma};

const R: u32 = PROT_READ;
const RW: u32 = PROT_READ | PROT_WRITE;
const RX: u32 = PROT_READ | PROT_EXEC;
const RWX: u32 = 7;
const EACCES: i32 = syscall::EACCES;

/// A 32-bit ARM ELF with the given PT_LOAD segments: (vaddr, file bytes, memsz, PF_* flags).
fn elf(kind: u16, entry: u32, segments: &[(u32, Vec<u8>, u32, u32)]) -> Vec<u8> {
    let phnum = segments.len();
    let mut image = vec![0u8; 52 + 32 * phnum];
    image[..4].copy_from_slice(b"\x7fELF");
    image[4] = 1;
    image[5] = 1;
    image[16..18].copy_from_slice(&kind.to_le_bytes());
    image[18..20].copy_from_slice(&40u16.to_le_bytes());
    image[24..28].copy_from_slice(&entry.to_le_bytes());
    image[28..32].copy_from_slice(&52u32.to_le_bytes());
    image[42..44].copy_from_slice(&32u16.to_le_bytes());
    image[44..46].copy_from_slice(&(phnum as u16).to_le_bytes());
    for (i, (vaddr, bytes, memsz, flags)) in segments.iter().enumerate() {
        let offset = image.len() as u32;
        image.extend_from_slice(bytes);
        let h = 52 + 32 * i;
        for (k, w) in [1, offset, *vaddr, *vaddr, bytes.len() as u32, *memsz, *flags, 0x10000].iter().enumerate() {
            image[h + 4 * k..h + 4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    image
}

/// Like the shipped apps: constants, code, data + bss, each in its own 64 KiB pages.
fn program(kind: u16) -> Vec<u8> {
    elf(
        kind,
        0x20d90,
        &[
            (0x10000, vec![0x11; 0xd8c], 0xd8c, 4),
            (0x20d90, vec![0x22; 0x8f60], 0x8f60, 5),
            (0x39cf0, vec![0x33; 0x3c], 0x12310, 6),
        ],
    )
}

struct World {
    k: Kernel,
    pool: Pool,
    space: Space,
}
fn world() -> World {
    let mut k = Kernel::new();
    let space = Space::new(&mut k, 1).unwrap();
    World { k, pool: Pool::new(), space }
}
fn started(kind: u16, layout: Layout) -> (World, procmgr::Loaded) {
    let mut w = world();
    let args = [String::from("/apps/test")];
    let loaded = load(&mut w.space, &mut w.k, &mut w.pool, &program(kind), None, &args, &[], [7; 16], layout).unwrap();
    (w, loaded)
}
/// A byte the test's "app" writes and maps from files; the loaded program has none.
const SHELLCODE: u8 = 0xcc;
const STUBS: u32 = vma::STUBS_BASE;

impl World {
    /// No page the hardware would run holds a byte the app chose: every
    /// executable page is the program's code as loaded, or trampolines.
    fn no_app_bytes_in_executable_pages(&self) {
        sys::HW.with(|h| {
            let h = h.borrow();
            for (frame, m) in h.mapped.iter().filter(|(_, m)| m.exec) {
                let page = unsafe { core::slice::from_raw_parts(pool::page_va(h.frames[frame]) as *const u8, 0x10000) };
                assert!(!page.contains(&SHELLCODE), "the executable page at {:#x} holds app-chosen bytes", m.va);
            }
        });
    }
    fn fault(&mut self, va: u32, access: Access) -> bool {
        self.space.fault(&mut self.k, &mut self.pool, va, access)
    }
    fn write(&mut self, va: u32, data: &[u8]) -> bool {
        self.space.write(&mut self.k, &mut self.pool, va, data)
    }
    fn read(&mut self, va: u32, len: usize) -> Option<Vec<u8>> {
        let mut v = vec![0u8; len];
        self.space.read(&mut self.k, &mut self.pool, va, &mut v).then_some(v)
    }
    /// What the regions say every present page should be, against the hardware.
    fn check(&self) {
        assert!(!vma::mixed(&self.space.vmas), "a 64 KiB page holds code and something else");
        let stubs = self.space.layout.stubs;
        if let Some(v) = vma::find(&self.space.vmas, stubs) {
            assert_eq!(*v, Vma { start: stubs, end: stubs + 0x10000, prot: RX }, "the trampoline page changed");
        }
        for pair in self.space.vmas.windows(2) {
            assert!(pair[0].start < pair[0].end && pair[0].end <= pair[1].start, "regions out of order");
        }
        sys::HW.with(|h| {
            for m in h.borrow().mapped.values() {
                let want = vma::hardware(vma::union(&self.space.vmas, m.va, m.va + 0x10000));
                let have = if m.writable { vma::HW_RW } else if m.exec { vma::HW_RX } else { vma::HW_R };
                assert_eq!(have, want, "page {:#x} is mapped {} but its regions call for {}", m.va, have, want);
            }
        });
    }
}

// ---- vma.rs ----

#[test]
fn hardware_mapping_is_never_writable_and_executable() {
    for prot in 0..8u32 {
        let hw = vma::hardware(prot);
        assert_eq!(hw == vma::HW_NONE, prot == 0);
        assert_eq!(hw == vma::HW_RW, prot & PROT_WRITE != 0);
        assert_eq!(hw == vma::HW_RX, prot & PROT_WRITE == 0 && prot & PROT_EXEC != 0);
    }
}

#[test]
fn fault_status_decodes_to_the_access() {
    // Translation fault on a read, permission fault on a write (WnR, bit 11),
    // and a prefetch abort whatever its status says.
    assert_eq!(vma::access(0, 0x007), Access::Read);
    assert_eq!(vma::access(0, 0x80f), Access::Write);
    assert_eq!(vma::access(0, 0x807), Access::Write);
    assert_eq!(vma::access(1, 0x00f), Access::Exec);
    assert_eq!(vma::access(1, 0x80f), Access::Exec);
    assert!(Access::Read.allowed(PROT_WRITE) && !Access::Read.allowed(0));
    assert!(Access::Write.allowed(RW) && !Access::Write.allowed(RX));
    assert!(Access::Exec.allowed(RX) && !Access::Exec.allowed(RW));
    assert!(Access::Exec.served(vma::HW_RX) && !Access::Exec.served(vma::HW_RW) && !Access::Exec.served(vma::HW_R));
    assert!(Access::Write.served(vma::HW_RW) && !Access::Write.served(vma::HW_R) && !Access::Read.served(vma::HW_NONE));
}

#[test]
fn region_queries() {
    let mut v = Vec::new();
    vma::insert(&mut v, 0x5000, 0x8000, RW);
    vma::insert(&mut v, 0x1000, 0x3000, R);
    vma::insert(&mut v, 0x3000, 0x5000, 0);
    assert_eq!(v.iter().map(|r| r.start).collect::<Vec<_>>(), [0x1000, 0x3000, 0x5000]);
    assert_eq!(vma::find(&v, 0x2fff).unwrap().prot, R);
    assert!(vma::find(&v, 0x8000).is_none() && vma::find(&v, 0xfff).is_none());
    assert_eq!(vma::union(&v, 0, 0x1000), 0);
    assert_eq!(vma::union(&v, 0x3000, 0x4000), vma::ANY);
    assert_eq!(vma::union(&v, 0x2000, 0x6000) & 7, RW);
    assert_eq!(vma::union(&v, 0x4000, 0x4000), 0);
    // Readable needs some access everywhere; writable needs write everywhere.
    assert!(vma::permits(&v, 0x1000, 0x2000, 7));
    assert!(!vma::permits(&v, 0x1000, 0x2001, 7), "runs into a region with no access");
    assert!(vma::permits(&v, 0x1000, 0x7000, vma::ANY));
    assert!(!vma::permits(&v, 0x1000, 0x7001, vma::ANY), "runs off the end");
    assert!(!vma::permits(&v, 0xfff, 2, vma::ANY), "starts before any region");
    assert!(vma::permits(&v, 0x5000, 0x3000, PROT_WRITE) && !vma::permits(&v, 0x4fff, 2, PROT_WRITE));
    assert!(vma::permits(&v, 0xdead_0000, 0, PROT_WRITE), "nothing to copy");
    assert!(!vma::permits(&v, 0xffff_f000, 0x2000, vma::ANY), "wraps");
    // Carving the middle of a region leaves both sides.
    vma::insert(&mut v, 0x6000, 0x7000, R);
    assert_eq!(v.len(), 5);
    assert_eq!((v[2], v[3], v[4]), (
        Vma { start: 0x5000, end: 0x6000, prot: RW },
        Vma { start: 0x6000, end: 0x7000, prot: R },
        Vma { start: 0x7000, end: 0x8000, prot: RW },
    ));
}

#[test]
fn wx_policy() {
    let mut v = Vec::new();
    vma::insert(&mut v, 0x20000, 0x29000, RX); // code, ending inside page 0x20000
    vma::insert(&mut v, 0x40000, 0x48000, RW);
    vma::insert(&mut v, 0x50000, 0x51000, R);
    // New mappings: never executable, with or without write.
    assert!(vma::refuses(&v, 0x100000, 0x110000, RWX, true));
    assert!(vma::refuses(&v, 0x100000, 0x110000, RX, true));
    assert!(vma::refuses(&v, 0x100000, 0x110000, PROT_EXEC, true));
    assert!(!vma::refuses(&v, 0x100000, 0x110000, RW, true));
    assert!(!vma::refuses(&v, 0x100000, 0x110000, 0, true));
    // Nothing may move into a 64 KiB page that holds code, whatever its
    // protection: the hardware would run it.
    for prot in [RW, R, 0, PROT_WRITE] {
        assert!(vma::refuses(&v, 0x29000, 0x2a000, prot, true), "beside the code, prot {prot}");
        assert!(vma::refuses(&v, 0x2f000, 0x31000, prot, true), "reaching into the code's page");
        assert!(vma::refuses(&v, 0x1f000, 0x21000, prot, true), "MAP_FIXED over the start of the code");
        assert!(vma::refuses(&v, 0x22000, 0x23000, prot, true), "MAP_FIXED over the middle of the code");
        assert!(vma::refuses(&v, 0x21000, 0x30000, prot, true), "MAP_FIXED that leaves 4 KiB of code");
        assert!(!vma::refuses(&v, 0x30000, 0x31000, prot, true), "the next page is free");
        assert!(!vma::refuses(&v, 0x20000, 0x30000, prot, true), "replacing all the code in the page");
        assert!(!vma::refuses(&v, 0x1f000, 0x29000, prot, true), "exactly the code and more");
    }
    assert!(vma::near_code(&v, 0x2f000, 0x2f004) && vma::near_code(&v, 0x1fffc, 0x20004));
    assert!(!vma::near_code(&v, 0x30000, 0x40000) && !vma::near_code(&v, 0x1f000, 0x20000));
    // mprotect: data never becomes code, code never becomes writable code.
    assert!(vma::refuses(&v, 0x40000, 0x48000, RX, false), "W -> X");
    assert!(vma::refuses(&v, 0x50000, 0x51000, RX, false), "R -> X");
    assert!(vma::refuses(&v, 0x20000, 0x29000, RWX, false));
    assert!(vma::refuses(&v, 0x20000, 0x51000, RX, false), "a range with parts that are not code");
    assert!(!vma::refuses(&v, 0x20000, 0x29000, RX, false), "code may stay code");
    assert!(vma::refuses(&v, 0x21000, 0x22000, R, false), "part of a code page may not stop being code");
    assert!(vma::refuses(&v, 0x21000, 0x22000, 0, false));
    assert!(vma::refuses(&v, 0x21000, 0x22000, RW, false), "part of a code page made writable");
    assert!(!vma::refuses(&v, 0x20000, 0x29000, R, false), "all the code in the page gives up execute");
    assert!(!vma::refuses(&v, 0x20000, 0x29000, RW, false), "all the code in the page gives up execute");
    assert!(!vma::refuses(&v, 0x40000, 0x48000, R, false));
    assert!(!vma::mixed(&v));
    for prot in [RW, R, 0] {
        let mut m = v.clone();
        vma::insert(&mut m, 0x2f000, 0x30000, prot);
        assert!(vma::mixed(&m), "code and a region with prot {prot} in page 0x20000");
    }
    vma::insert(&mut v, 0x2f000, 0x30000, RX);
    assert!(!vma::mixed(&v), "two pieces of code in one page");
    let one = vec![Vma { start: 0x1000, end: 0x2000, prot: RWX }];
    assert!(vma::mixed(&one));
}

fn random_bytes(seed: &mut u64) -> [u8; 24] {
    let mut b = [0u8; 24];
    for chunk in b.chunks_mut(8) {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        chunk.copy_from_slice(&seed.to_le_bytes());
    }
    b
}

#[test]
fn random_layouts_stay_apart_and_vary() {
    let mut seed = 0x9e3779b97f4a7c15u64;
    let (mut pies, mut mmaps, mut stacks, mut stubs) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for _ in 0..100_000 {
        let l = Layout::random(&random_bytes(&mut seed));
        for a in [l.pie_base, l.brk_gap, l.mmap_top, l.stack_top, l.stubs] {
            assert_eq!(a & 0xffff, 0);
        }
        assert_eq!(l.stack_pad & 15, 0);
        assert!(l.stack_pad < 4096);
        // A 512 MiB program still ends below the mmap area.
        assert!(l.pie_base >= 0x40_0000 && l.pie_base + (512 << 20) <= vma::MMAP_BASE);
        assert!(l.mmap_top > vma::MMAP_BASE + (1 << 30) && l.mmap_top <= vma::MMAP_TOP);
        let stack_bottom = l.stack_top - vma::STACK_SIZE;
        assert!(stack_bottom >= vma::MMAP_TOP && l.stack_top <= vma::STACK_TOP);
        assert!(l.stubs >= vma::STACK_TOP + 0x10000 && l.stubs + 0x10000 <= vma::USER_TOP);
        pies.insert(l.pie_base);
        mmaps.insert(l.mmap_top);
        stacks.insert(l.stack_top);
        stubs.insert(l.stubs);
    }
    assert_eq!((pies.len(), mmaps.len(), stacks.len(), stubs.len()), (4096, 4096, 2048, 224));
    // All zero or too few bytes fall back to the fixed layout, never out of bounds.
    assert_eq!(Layout::random(&[0; 24]), Layout::fixed());
    assert_eq!(Layout::random(&[]), Layout::fixed());
}

// ---- procmgr.rs: the loader ----

#[test]
fn loader_maps_segments_with_their_own_rights() {
    let (mut w, loaded) = started(2, Layout::fixed());
    assert_eq!(loaded.entry, 0x20d90);
    assert_eq!(sys::at(0x10000), Some((false, false)), "constants: read-only, not executable");
    assert_eq!(sys::at(0x20000), Some((false, true)), "code: read-execute");
    assert_eq!(sys::at(0x30000), Some((true, false)), "data: read-write, not executable");
    assert_eq!(sys::at(0x40000), Some((true, false)), "bss");
    let stack = Layout::fixed().stack_top - 0x10000;
    assert_eq!(sys::at(stack), Some((true, false)), "stack: read-write, not executable");
    assert!(loaded.sp > stack && loaded.sp < Layout::fixed().stack_top && loaded.sp & 15 == 0);
    // Contents arrived although the code and constants are not writable by the app.
    assert_eq!(w.read(0x20d90, 4).unwrap(), [0x22; 4]);
    assert_eq!(w.read(0x10000 + 0xd8b, 1).unwrap(), [0x11]);
    assert_eq!(w.read(0x39cf0 + 0x3c, 4).unwrap(), [0; 4], "bss is zero");
    // The code page was flushed for execution after it was written.
    sys::HW.with(|h| {
        let h = h.borrow();
        let code = h.mapped.iter().find(|(_, m)| m.va == 0x20000).map(|(f, _)| *f).unwrap();
        assert!(h.unifies.contains(&code));
    });
    // argc, argv[0], AT_RANDOM on the stack.
    let sp = loaded.sp;
    assert_eq!(w.read(sp, 4).unwrap(), 1u32.to_le_bytes());
    let mut at = sp + 4 * 4; // argc, argv[0], NULL, envp NULL
    let mut random = 0;
    loop {
        let pair = w.read(at, 8).unwrap();
        let (key, value) = (u32::from_le_bytes(pair[..4].try_into().unwrap()), u32::from_le_bytes(pair[4..].try_into().unwrap()));
        if key == 0 {
            break;
        }
        if key == 25 {
            random = value;
        }
        at += 8;
    }
    assert_eq!(w.read(random, 16).unwrap(), [7; 16]);
    w.check();
}

#[test]
fn layout_moves_a_pie_and_everything_else() {
    let mut seed = 42;
    let a = Layout::random(&random_bytes(&mut seed));
    let b = Layout::random(&random_bytes(&mut seed));
    assert!(a.pie_base != b.pie_base && a.stack_top != b.stack_top && a.mmap_top != b.mmap_top);
    for kind in [2u16, 3] {
        let (mut wa, la) = started(kind, a);
        let image_a = wa.read(if kind == 3 { a.pie_base } else { 0 } + 0x20d90, 64).unwrap();
        let brk_a = wa.space.brk_start;
        let mmap_a = wa.space.free_range(0x10000).unwrap();
        wa.check();
        sys::HW.with(|h| *h.borrow_mut() = Default::default());
        let (mut wb, lb) = started(kind, b);
        let image_b = wb.read(if kind == 3 { b.pie_base } else { 0 } + 0x20d90, 64).unwrap();
        assert_eq!(image_a, image_b);
        if kind == 3 {
            assert_eq!(la.entry - a.pie_base, 0x20d90);
            assert_eq!(lb.entry.wrapping_sub(la.entry), b.pie_base.wrapping_sub(a.pie_base));
            assert_eq!(sys::at(b.pie_base + 0x20000), Some((false, true)));
        } else {
            assert_eq!((la.entry, lb.entry), (0x20d90, 0x20d90), "a fixed-address program does not move");
        }
        assert_eq!(wb.space.brk_start.wrapping_sub(brk_a), (lb.entry - la.entry).wrapping_add(b.brk_gap).wrapping_sub(a.brk_gap));
        assert_eq!(mmap_a, a.mmap_top - 0x10000);
        assert_eq!(wb.space.free_range(0x10000).unwrap(), b.mmap_top - 0x10000);
        assert!(la.sp < a.stack_top && la.sp >= a.stack_top - 8192 && lb.sp < b.stack_top && lb.sp >= b.stack_top - 8192);
        assert_eq!(wb.space.layout, b);
        wb.check();
        sys::HW.with(|h| *h.borrow_mut() = Default::default());
    }
}

#[test]
fn loader_refuses_programs_that_break_wx() {
    let try_load = |image: &[u8]| {
        sys::HW.with(|h| *h.borrow_mut() = Default::default());
        let mut w = world();
        load(&mut w.space, &mut w.k, &mut w.pool, image, None, &[], &[], [0; 16], Layout::fixed()).map(|_| ()).err()
    };
    let rwx = elf(2, 0x10000, &[(0x10000, vec![0; 64], 64, 7)]);
    assert_eq!(try_load(&rwx), Some("a segment is writable and executable"));
    // Code and data 4 KiB apart: one 64 KiB hardware page cannot separate them.
    let close = elf(2, 0x10000, &[(0x10000, vec![0; 64], 64, 5), (0x11000, vec![0; 64], 64, 6)]);
    assert_eq!(try_load(&close), Some("code shares a 64 KiB page with data"));
    // Sharing one 4 KiB page as well.
    let closer = elf(2, 0x10000, &[(0x10000, vec![0; 64], 64, 5), (0x10800, vec![0; 64], 64, 6)]);
    assert_eq!(try_load(&closer), Some("code shares a 64 KiB page with data"));
    // Constants in a page of code would be executable too: refused, whether
    // they share a 4 KiB page with it (either order) or only the 64 KiB one.
    let why = Some("code shares a 64 KiB page with data");
    assert_eq!(try_load(&elf(2, 0x10800, &[(0x10000, vec![1; 0x800], 0x800, 4), (0x10800, vec![2; 64], 64, 5)])), why);
    assert_eq!(try_load(&elf(2, 0x10000, &[(0x10000, vec![2; 0x800], 0x800, 5), (0x10800, vec![1; 64], 64, 4)])), why);
    assert_eq!(try_load(&elf(2, 0x10000, &[(0x10000, vec![2; 64], 64, 5), (0x1f000, vec![1; 64], 64, 4)])), why);
    assert_eq!(try_load(&elf(2, 0x1f000, &[(0x10000, vec![1; 64], 64, 4), (0x1f000, vec![2; 64], 64, 5)])), why);
    // Each in its own 64 KiB page, and data segments sharing a 4 KiB page, load.
    let apart = elf(2, 0x20000, &[(0x10000, vec![1; 64], 64, 4), (0x20000, vec![2; 64], 64, 5), (0x30000, vec![3; 0x800], 0x800, 4), (0x30800, vec![4; 64], 64, 6)]);
    assert_eq!(try_load(&apart), None);
    assert_eq!(sys::at(0x10000), Some((false, false)));
    assert_eq!(sys::at(0x20000), Some((false, true)));
    assert_eq!(sys::at(0x30000), Some((true, false)));
}

#[test]
fn loader_survives_hostile_headers() {
    let try_load = |image: &[u8]| {
        let mut w = world();
        load(&mut w.space, &mut w.k, &mut w.pool, image, None, &[], &[], [0; 16], Layout::fixed()).map(|_| ()).err()
    };
    let good = elf(2, 0x10000, &[(0x10000, vec![0; 64], 64, 5)]);
    assert_eq!(try_load(&good), None);
    let patch = |at: usize, v: u32| {
        let mut image = good.clone();
        image[at..at + 4].copy_from_slice(&v.to_le_bytes());
        image
    };
    // p_offset + p_filesz wraps a 32-bit sum (it used to pass the check and panic).
    assert_eq!(try_load(&patch(52 + 4, 0xffff_fff0)), Some("bad segment"));
    assert_eq!(try_load(&patch(52 + 16, 0xffff_ffff)), Some("bad segment"));
    assert_eq!(try_load(&patch(52 + 20, 8)), Some("bad segment"), "file size above memory size");
    assert_eq!(try_load(&patch(52 + 8, 0xffff_0000)), Some("segment too high"));
    assert_eq!(try_load(&patch(52 + 20, 0xffff_0000)), Some("segment too high"));
    assert_eq!(try_load(&patch(28, 0xffff_fff0)), Some("bad program headers"));
    let mut many = good.clone();
    many[44..46].copy_from_slice(&0xffffu16.to_le_bytes());
    assert_eq!(try_load(&many), Some("bad program headers"));
    let mut pie_high = good.clone();
    pie_high[16] = 3;
    pie_high[52 + 8..52 + 12].copy_from_slice(&0xfff0_0000u32.to_le_bytes());
    assert_eq!(try_load(&pie_high), Some("bad segment address"));
    assert_eq!(try_load(&good[..40]), Some("not a 32-bit little-endian ELF file"));
}

// ---- procmgr.rs: faults ----

#[test]
fn faults_enforce_region_protection() {
    let (mut w, _) = started(2, Layout::fixed());
    let stack = Layout::fixed().stack_top - 0x20000;
    assert!(!w.fault(0x20d90, Access::Write), "a write to code stops the app");
    assert!(!w.fault(0x10010, Access::Write), "a write to constants stops the app");
    assert!(!w.fault(0x10010, Access::Exec), "constants are not code");
    assert!(!w.fault(0x39d00, Access::Exec), "data is not code");
    assert!(!w.fault(stack, Access::Exec), "the stack is not code");
    assert!(!w.fault(0x9000_0000, Access::Read), "no region");
    // First touch of a stack page: mapped read-write, Execute-Never.
    assert_eq!(sys::at(stack), None);
    assert!(w.fault(stack, Access::Write));
    assert_eq!(sys::at(stack), Some((true, false)));
    assert!(!w.fault(stack + 8, Access::Exec), "and still not code once it is mapped");
    // Constants end at 0x10d8c; the region ends at 0x11000. Past it, the same
    // mapped 64 KiB page has no region: not valid, although the hardware page is there.
    assert!(w.fault(0x10ffc, Access::Read));
    assert!(!w.fault(0x11000, Access::Read));
    assert!(!w.fault(0x1f000, Access::Read));
    // The page of code is mapped; a fetch fault there (a stale TLB entry) is served.
    assert!(w.fault(0x20d90, Access::Exec));
    assert_eq!(sys::at(0x20000), Some((false, true)));
    w.check();
}

#[test]
fn a_fault_that_cannot_make_progress_ends() {
    let (mut w, _) = started(2, Layout::fixed());
    // The region allows the read and the page is mapped; if the hardware
    // keeps faulting anyway the app is stopped instead of spinning forever.
    let mut served = 0;
    while w.fault(0x10010, Access::Read) {
        served += 1;
        assert!(served < 100_000);
    }
    assert!(served >= 1000);
    // Other faults in between start the count again.
    let (mut w, _) = started(2, Layout::fixed());
    for _ in 0..3000 {
        assert!(w.fault(0x10010, Access::Read));
        assert!(w.fault(0x39d00, Access::Write));
    }
}

#[test]
fn a_failed_remap_never_leaves_old_rights() {
    let mut w = world();
    w.space.add(0x100000, 0x110000, RW);
    assert!(w.write(0x100000, b"data"));
    assert_eq!(sys::at(0x100000), Some((true, false)));
    // The in-place change to read-only fails once: the page must not stay writable.
    sys::HW.with(|h| h.borrow_mut().fail_remaps = 1);
    assert_eq!(w.space.protect(0x100000, 0x110000, R), Ok(()));
    assert_eq!(sys::at(0x100000), None, "unmapped, not left writable");
    assert!(!w.fault(0x100000, Access::Write));
    // The next read maps it read-only, with its contents.
    assert!(w.fault(0x100000, Access::Read));
    assert_eq!(sys::at(0x100000), Some((false, false)));
    assert_eq!(w.read(0x100000, 4).unwrap(), b"data");
}

// ---- procmgr.rs: copies for system calls ----

#[test]
fn system_call_copies_respect_app_permissions() {
    let (mut w, _) = started(2, Layout::fixed());
    // read(fd, code, n) and friends: the runtime will not write where the app cannot.
    assert!(!w.write(0x20d90, b"\xff\xff\xff\xff"));
    assert_eq!(w.read(0x20d90, 4).unwrap(), [0x22; 4], "code unchanged");
    assert!(!w.write(0x10000, b"x"), "constants");
    assert!(w.write(0x39d00, b"ok"), "data");
    assert_eq!(w.read(0x39d00, 2).unwrap(), b"ok");
    // A mapped 64 KiB page, but outside every region.
    assert!(!w.write(0x1f000, b"x") && w.read(0x1f000, 1).is_none());
    assert!(!w.write(0x9000_0000, b"x") && w.read(0x9000_0000, 1).is_none());
    // A buffer that runs from data into nothing: refused whole, nothing written.
    let end_of_bss = 0x4c000;
    assert!(w.write(end_of_bss - 4, b"abcd"));
    assert!(!w.write(end_of_bss - 4, b"WXYZ!"));
    assert_eq!(w.read(end_of_bss - 4, 4).unwrap(), b"abcd");
    assert!(w.read(end_of_bss - 4, 5).is_none());
    // No access at all: neither read nor written, but its contents are kept.
    w.space.add(0x5000_0000, 0x5001_0000, RW);
    assert!(w.write(0x5000_0000, b"secret"));
    assert_eq!(w.space.protect(0x5000_0000, 0x5001_0000, 0), Ok(()));
    assert_eq!(sys::at(0x5000_0000), None, "PROT_NONE is not mapped");
    assert!(w.read(0x5000_0000, 6).is_none() && !w.write(0x5000_0000, b"x"));
    assert!(!w.fault(0x5000_0000, Access::Read));
    assert_eq!(w.space.protect(0x5000_0000, 0x5001_0000, RW), Ok(()));
    assert_eq!(sys::at(0x5000_0000), Some((true, false)));
    assert_eq!(w.read(0x5000_0000, 6).unwrap(), b"secret");
    // The runtime's own writes (the loader, file contents for mmap) go to
    // read-only regions, but still only inside regions.
    w.space.add(0x5100_0000, 0x5100_1000, R);
    assert!(w.space.poke(&mut w.k, &mut w.pool, 0x5100_0000, b"file"));
    assert_eq!(sys::at(0x5100_0000), Some((false, false)));
    assert_eq!(w.read(0x5100_0000, 4).unwrap(), b"file");
    assert!(!w.space.poke(&mut w.k, &mut w.pool, 0x5100_0ffe, b"abc"), "runs past the region");
    assert!(w.space.resolve(&mut w.k, &mut w.pool, 0x5200_0000).is_none());
    // Strings stop at the end of readable memory.
    assert!(w.write(end_of_bss - 3, b"abc"));
    assert!(w.space.read_string(&mut w.k, &mut w.pool, end_of_bss - 3, 64).is_none());
    assert!(w.write(end_of_bss - 3, b"ab\0"));
    assert_eq!(w.space.read_string(&mut w.k, &mut w.pool, end_of_bss - 3, 64).unwrap(), "ab");
    w.check();
}

// ---- procmgr.rs: mprotect, mmap, brk ----

#[test]
fn mprotect_changes_the_hardware_mapping() {
    let (mut w, _) = started(2, Layout::fixed());
    w.space.add(0x6000_0000, 0x6002_0000, RW);
    assert!(w.write(0x6000_0000, b"jit?") && w.write(0x6001_0000, b"more"));
    let maps = sys::HW.with(|h| h.borrow().mapped.len());
    // Data cannot become code, in one step or two.
    assert_eq!(w.space.protect(0x6000_0000, 0x6002_0000, RWX), Err(EACCES));
    assert_eq!(w.space.protect(0x6000_0000, 0x6002_0000, RX), Err(EACCES));
    assert_eq!(w.space.protect(0x6000_0000, 0x6002_0000, R), Ok(()));
    assert_eq!(w.space.protect(0x6000_0000, 0x6002_0000, RX), Err(EACCES));
    assert_eq!(sys::at(0x6000_0000), Some((false, false)));
    assert_eq!(sys::at(0x6001_0000), Some((false, false)));
    assert_eq!(sys::HW.with(|h| h.borrow().mapped.len()), maps, "changed in place, same frames");
    assert!(!w.fault(0x6000_0000, Access::Write) && !w.fault(0x6000_0000, Access::Exec));
    assert!(!w.write(0x6000_0000, b"x"));
    assert_eq!(w.read(0x6000_0000, 4).unwrap(), b"jit?");
    // Part of a 64 KiB page read-only, part read-write: the hardware page is
    // read-write (it cannot split), the runtime's copies still tell them apart.
    assert_eq!(w.space.protect(0x6000_1000, 0x6000_2000, RW), Ok(()));
    assert_eq!(sys::at(0x6000_0000), Some((true, false)));
    assert!(w.write(0x6000_1000, b"y") && !w.write(0x6000_0000, b"x") && !w.write(0x6000_2000, b"x"));
    assert!(!w.fault(0x6000_0000, Access::Write), "a write fault outside the writable part");
    // Code may stay code, or stop being code; it cannot be made writable in part.
    assert_eq!(w.space.protect(0x20000, 0x2a000, RX), Ok(()));
    assert_eq!(sys::at(0x20000), Some((false, true)));
    assert_eq!(w.space.protect(0x21000, 0x22000, RW), Err(EACCES));
    assert_eq!(w.space.protect(0x21000, 0x22000, R), Err(EACCES), "a read-only hole in a page of code");
    assert_eq!(w.space.protect(0x21000, 0x22000, 0), Err(EACCES));
    assert_eq!(w.space.protect(0x20000, 0x2a000, RWX), Err(EACCES));
    assert_eq!(sys::at(0x20000), Some((false, true)));
    assert_eq!(w.space.protect(0x20000, 0x2a000, R), Ok(()));
    assert_eq!(sys::at(0x20000), Some((false, false)), "Execute-Never once no region in it is code");
    assert_eq!(w.space.protect(0x20000, 0x2a000, RX), Err(EACCES), "and it does not come back");
    assert_eq!(w.space.protect(0x7000_0000, 0x7001_0000, R), Err(syscall::ENOMEM), "nothing there");
    w.check();
}

#[test]
fn mmap_and_brk_keep_writable_memory_away_from_code() {
    let (mut w, _) = started(2, Layout::fixed());
    assert!(w.space.refuses(0x6000_0000, 0x6001_0000, RWX));
    assert!(w.space.refuses(0x6000_0000, 0x6001_0000, RX));
    assert!(!w.space.refuses(0x6000_0000, 0x6001_0000, RW));
    // MAP_FIXED of writable memory into the tail of the code's 64 KiB page.
    assert!(w.space.refuses(0x2a000, 0x2b000, RW));
    assert!(w.space.refuses(0x2a000, 0x2b000, R) && w.space.refuses(0x2a000, 0x2b000, 0));
    // The heap grows from the end of bss; here it is far from code.
    let start = w.space.brk_start;
    assert_eq!(start, 0x4c000);
    assert_eq!(w.space.set_brk(&mut w.k, &mut w.pool, start + 0x30000), start + 0x30000);
    assert!(w.write(start + 0x2fff0, b"heap"));
    assert_eq!(sys::at(start + 0x2fff0), Some((true, false)));
    assert_eq!(w.space.set_brk(&mut w.k, &mut w.pool, start), start);
    assert_eq!(sys::at(0x70000), None, "freed");
    assert!(w.read(start + 0x2fff0, 4).is_none());
    // A program whose code is the last thing in it: the heap may not start in that page.
    sys::HW.with(|h| *h.borrow_mut() = Default::default());
    let mut w = world();
    let image = elf(2, 0x10000, &[(0x10000, vec![0; 0x100], 0x100, 5)]);
    load(&mut w.space, &mut w.k, &mut w.pool, &image, None, &[], &[], [0; 16], Layout::fixed()).unwrap();
    let start = w.space.brk_start;
    assert_eq!(start, 0x11000);
    assert_eq!(w.space.set_brk(&mut w.k, &mut w.pool, start + 0x1000), start, "refused: brk does not move");
    assert_eq!(sys::at(0x10000), Some((false, true)));
    w.check();
}

#[test]
fn unmapping_part_of_a_page_drops_rights_with_it() {
    let mut w = world();
    w.space.add(0x6000_0000, 0x6000_1000, R);
    w.space.add(0x6000_1000, 0x6000_2000, RW);
    assert!(w.write(0x6000_1000, b"x"));
    assert_eq!(sys::at(0x6000_0000), Some((true, false)));
    w.space.remove(&mut w.k, &mut w.pool, 0x6000_1000, 0x6000_2000);
    assert_eq!(sys::at(0x6000_0000), Some((false, false)), "only the read-only region is left");
    let free = w.pool.available();
    w.space.remove(&mut w.k, &mut w.pool, 0x6000_0000, 0x6000_1000);
    assert_eq!(sys::at(0x6000_0000), None);
    assert_eq!(w.pool.available(), free + 1);
    w.check();
}

/// Random mmap/mprotect/munmap/brk/fault/copy sequences, as an app could
/// issue them (each goes through the policy as the system calls do). After
/// every step: no page is writable and executable (the stand-in page_map
/// panics), every mapped page has exactly the rights its regions call for,
/// and no 64 KiB page holds both writable and executable regions.
#[test]
fn random_operations_keep_every_invariant() {
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut next = move |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 20) as u32) % n
    };
    let (mut refused, mut executable_faults, mut file_maps, mut threads) = (0, 0, 0, 0);
    // Code that loses execute never gets it back, so start over now and then.
    for round in 0..200 {
    sys::HW.with(|h| *h.borrow_mut() = Default::default());
    let (mut w, _) = started(2, Layout::fixed());
    w.space.add(STUBS, STUBS + 0x10000, RX);
    for step in 0..100 {
        // Addresses over the program (0x10000..0x50000), a small mmap arena,
        // and around the trampoline page.
        let base = match next(5) {
            0 | 1 => 0x10000,
            2 | 3 => 0x6000_0000,
            _ => w.space.layout.stubs - 0x20000,
        };
        let start = base + next(64) * 0x1000;
        let end = start + (1 + next(24)) * 0x1000;
        let prot = next(8);
        match next(8) {
            0 => {
                // mmap with MAP_FIXED, half the time of a file the app wrote.
                if w.space.refuses(start, end, prot) {
                    refused += 1;
                } else {
                    w.space.remove(&mut w.k, &mut w.pool, start, end);
                    w.space.add(start, end, prot);
                    if next(2) == 0 {
                        assert!(w.space.poke(&mut w.k, &mut w.pool, start, &vec![SHELLCODE; (end - start) as usize]));
                        file_maps += 1;
                    }
                }
            }
            7 => {
                // clone: the trampoline is written, in the runtime's page only.
                let slot = threads % 8192;
                let stub = w.space.stub(&mut w.k, &mut w.pool, slot, 0x20d91).expect("the trampoline page is the runtime's");
                assert_eq!(stub, w.space.layout.stubs + 8 * slot as u32);
                threads += 1;
            }
            1 | 2 => {
                if w.space.protect(start, end, prot) == Err(EACCES) {
                    refused += 1;
                }
            }
            3 => {
                // munmap, as the system call does it.
                if w.space.pinned(start, end) {
                    refused += 1;
                } else {
                    w.space.remove(&mut w.k, &mut w.pool, start, end);
                }
            }
            4 => {
                let access = [Access::Read, Access::Write, Access::Exec][next(3) as usize];
                let ok = w.fault(start, access);
                let region = w.space.find(start).map(|v| v.prot);
                assert_eq!(ok, region.is_some_and(|p| access.allowed(p)), "step {step}: fault {access:?} at {start:#x}");
                if ok {
                    let (writable, exec) = sys::at(start).unwrap();
                    assert!(access != Access::Write || writable);
                    assert!(access != Access::Exec || exec);
                    executable_faults += (access == Access::Exec) as u32;
                }
            }
            5 => {
                let ok = w.write(start, &[step as u8; 3]);
                assert_eq!(ok, vma::permits(&w.space.vmas, start, 3, PROT_WRITE));
            }
            _ => {
                let want = w.space.brk_start + next(32) * 0x1000;
                w.space.set_brk(&mut w.k, &mut w.pool, want);
            }
        }
        w.check();
        w.no_app_bytes_in_executable_pages();
    }
    w.space.destroy(&mut w.k, &mut w.pool);
    assert_eq!(w.pool.available(), pool::PAGES, "round {round}: every page came back");
    assert!(sys::HW.with(|h| h.borrow().mapped.is_empty()));
    }
    assert!(refused > 500, "the policy was exercised ({refused} refusals)");
    assert!(executable_faults > 5, "code was run ({executable_faults} fetch faults served)");
    assert!(file_maps > 200 && threads > 1000, "file mappings ({file_maps}) and threads ({threads}) were exercised");
}

/// The bypass the first version of the policy allowed: a read-only file
/// mapping placed with MAP_FIXED in a 64 KiB page that holds code is filled
/// by the runtime and the hardware runs it, with no fault to check anything.
#[test]
fn nothing_an_app_maps_lands_in_an_executable_page() {
    let (mut w, _) = started(2, Layout::fixed());
    w.space.add(STUBS, STUBS + 0x10000, RX);
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 0, 0x20d91), Some(STUBS));
    // The code is 0x20000..0x2a000; its page runs to 0x30000. Every way in:
    // beside the code, over part of it, over the trampoline page, any protection.
    let attempts = [
        (0x2a000, 0x2b000), (0x2f000, 0x30000), (0x2f000, 0x31000), (0x1f000, 0x21000), (0x22000, 0x23000),
        (0x21000, 0x30000), (STUBS + 0x1000, STUBS + 0x2000), (STUBS, STUBS + 0x10000), (STUBS - 0x1000, STUBS + 0x1000),
        (STUBS + 0xf000, STUBS + 0x11000), (vma::STUBS_BASE, vma::USER_TOP),
    ];
    for (start, end) in attempts {
        for prot in 0..8 {
            assert!(w.space.refuses(start, end, prot), "mmap({start:#x}..{end:#x}, prot {prot}) must be refused");
        }
        // Even if a caller forgot the policy, file contents do not go there.
        assert!(!w.space.poke(&mut w.k, &mut w.pool, start, &[SHELLCODE; 16]), "poke at {start:#x}");
    }
    assert!(!w.space.poke(&mut w.k, &mut w.pool, 0x20d90, &[SHELLCODE; 4]), "over the code itself");
    assert!(!w.space.poke(&mut w.k, &mut w.pool, STUBS + 64, &[SHELLCODE; 8]));
    assert_eq!(w.read(0x20d90, 4).unwrap(), [0x22; 4]);
    w.no_app_bytes_in_executable_pages();
    // Replacing every bit of code in a page is allowed, and the new page is
    // then data: read-only, Execute-Never, never code again.
    assert!(!w.space.refuses(0x20000, 0x30000, R));
    w.space.remove(&mut w.k, &mut w.pool, 0x20000, 0x30000);
    w.space.add(0x20000, 0x30000, R);
    assert!(w.space.poke(&mut w.k, &mut w.pool, 0x20000, &[SHELLCODE; 64]));
    assert_eq!(sys::at(0x20000), Some((false, false)));
    assert!(!w.fault(0x20000, Access::Exec));
    assert_eq!(w.space.protect(0x20000, 0x30000, RX), Err(EACCES));
    w.check();
    w.no_app_bytes_in_executable_pages();
}

/// The trampoline page stays the runtime's: an app that unmaps or replaces
/// it would get its pool page freed and handed to another app while the
/// runtime kept writing thread trampolines into it.
#[test]
fn the_trampoline_page_cannot_be_freed_or_replaced() {
    let mut seed = 7;
    let layout = Layout::random(&random_bytes(&mut seed));
    let stubs = layout.stubs;
    let (mut w, _) = started(2, layout);
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 0, 0x20d91), None, "no region yet: nothing is written");
    w.space.add(stubs, stubs + 0x10000, RX);
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 0, 0x20d91), Some(stubs));
    assert_eq!(sys::at(stubs), Some((false, true)));
    assert_eq!(w.read(stubs, 8).unwrap(), [0x04, 0xf0, 0x1f, 0xe5, 0x91, 0x0d, 0x02, 0x00]);
    let (free, used) = (w.pool.available(), w.space.usage().0);
    assert!(used > 4);
    // munmap of every candidate page (the system call refuses; `remove`
    // itself also leaves the page), MAP_FIXED over it, mprotect.
    assert!(w.space.pinned(vma::STUBS_BASE, vma::USER_TOP) && w.space.pinned(stubs + 0xf000, stubs + 0x10000));
    assert!(!w.space.pinned(stubs - 0x1000, stubs) && !w.space.pinned(stubs + 0x10000, stubs + 0x11000));
    w.space.remove(&mut w.k, &mut w.pool, vma::STUBS_BASE, vma::USER_TOP);
    w.space.remove(&mut w.k, &mut w.pool, stubs + 0x1000, stubs + 0x2000);
    w.space.remove(&mut w.k, &mut w.pool, 0, u32::MAX);
    assert_eq!(w.pool.available() - free, used - 1, "the program's pages were freed, the trampoline page was not");
    assert_eq!(w.space.usage().0, 1);
    assert_eq!(sys::at(stubs), Some((false, true)));
    assert_eq!(w.space.vmas, [Vma { start: stubs, end: stubs + 0x10000, prot: RX }]);
    for prot in 0..8 {
        assert!(w.space.refuses(stubs, stubs + 0x10000, prot));
        assert_eq!(w.space.protect(stubs, stubs + 0x10000, prot), Err(EACCES));
        assert_eq!(w.space.protect(stubs + 0x1000, stubs + 0x2000, prot), Err(EACCES));
    }
    // System calls cannot write it either, and the next thread still works.
    assert!(!w.write(stubs, b"x") && !w.space.writable(stubs, 1));
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 1, 0x30001), Some(stubs + 8));
    assert_eq!(w.read(stubs + 8, 8).unwrap(), [0x04, 0xf0, 0x1f, 0xe5, 0x01, 0x00, 0x03, 0x00]);
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 8192, 0), None, "past the end of the page");
    // Were the region ever not the runtime's (the unchecked `add` is the only
    // way), no trampoline is written: the page another app may hold stays untouched.
    let mut other = world();
    other.space.layout = layout;
    for region in [(stubs, stubs + 0x10000, R), (stubs, stubs + 0x10000, RW), (stubs, stubs + 0x8000, RX), (stubs - 0x10000, stubs + 0x10000, RX)] {
        other.space.vmas.clear();
        other.space.vmas.push(Vma { start: region.0, end: region.1, prot: region.2 });
        let before = other.pool.available();
        assert_eq!(other.space.stub(&mut other.k, &mut other.pool, 3, 0xdead_beef), None, "{region:x?}");
        assert_eq!(other.pool.available(), before, "no page was even taken");
    }
    w.check();
}

/// getrandom checks its whole buffer before writing any of it.
#[test]
fn writable_checks_the_whole_range() {
    let mut w = world();
    w.space.add(0x6000_0000, 0x6000_1000, RW);
    w.space.add(0x6000_1000, 0x6000_2000, R);
    assert!(w.space.writable(0x6000_0000, 4096) && w.space.writable(0x6000_0ff0, 16) && w.space.writable(0x1234, 0));
    assert!(!w.space.writable(0x6000_0000, 8192) && !w.space.writable(0x6000_0ff0, 17));
    assert!(!w.space.writable(0x6000_1000, 1) && !w.space.writable(0x5fff_ffff, 2) && !w.space.writable(0xffff_fff0, 32));
}

// ---- entropy.rs ----

/// A plain SHA-256 for the tests (the image uses the one it links).
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let t2 = s0.wrapping_add((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap()).collect()
}

#[test]
fn test_sha256_is_sha256() {
    assert_eq!(sha256(b"abc").to_vec(), unhex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    assert_eq!(sha256(b"").to_vec(), unhex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
}

/// NIST CAVP HMAC_DRBG, SHA-256, no reseed, no additional input, COUNT = 0:
/// instantiate with entropy || nonce, generate 128 bytes twice, check the second.
#[test]
fn drbg_matches_the_nist_known_answer() {
    let mut drbg = entropy::Drbg::new(sha256);
    drbg.seed(&unhex(concat!(
        "ca851911349384bffe89de1cbdc46e6831e44d34a4fb935ee285dd14b71a7488",
        "659ba96c601dc69fc902940805ec0ca8"
    )));
    let mut out = [0u8; 128];
    assert!(drbg.fill(&mut out));
    assert!(drbg.fill(&mut out));
    assert_eq!(
        out.to_vec(),
        unhex(concat!(
            "e528e9abf2dece54d47c7e75e5fe302149f817ea9fb4bee6f4199697d04d5b89",
            "d54fbb978a15b5c443c9ec21036d2460b6f73ebad0dc2aba6e624abf07745bc1",
            "07694bb7547bb0995f70de25d6b29e2d3011bb19d27676c07162c8b5ccde0668",
            "961df86803482cb37ed6d5c0bb8d50cf1f50d476aa0458bdaba806f48be9dcb8"
        ))
    );
}

#[test]
fn drbg_gives_nothing_until_seeded() {
    let mut drbg = entropy::Drbg::new(sha256);
    let mut out = [0xa5u8; 40];
    drbg.stir(123);
    assert!(!drbg.fill(&mut out) && !drbg.seeded());
    assert_eq!(out, [0xa5; 40], "untouched");
    drbg.seed(b"seed");
    assert!(drbg.fill(&mut out) && drbg.seeded() && !drbg.hardware);
    assert_ne!(out, [0xa5; 40]);
    drbg.reseed(&[9; 300]);
    assert!(drbg.hardware);
}

#[test]
fn drbg_streams() {
    let stream = |seed: &[u8], stir: Option<u32>, reseed: Option<&[u8]>| {
        let mut drbg = entropy::Drbg::new(sha256);
        drbg.seed(seed);
        let mut first = [0u8; 16];
        drbg.fill(&mut first);
        if let Some(t) = stir {
            drbg.stir(t);
        }
        if let Some(bytes) = reseed {
            drbg.reseed(bytes);
        }
        let mut out = vec![0u8; 4097];
        assert!(drbg.fill(&mut out));
        (first, out)
    };
    let (first, base) = stream(b"one", None, None);
    assert_eq!(stream(b"one", None, None).1, base, "the same seed and inputs give the same stream");
    assert_ne!(stream(b"two", None, None).1, base);
    assert_ne!(stream(b"one", Some(1), None).1, base, "a stirred timing changes the stream");
    assert_ne!(stream(b"one", Some(1), None).1, stream(b"one", Some(2), None).1);
    assert_ne!(stream(b"one", None, Some(b"hw")).1, base, "a reseed changes the stream");
    assert_ne!(stream(b"one", None, Some(&[1; 200])).1, stream(b"one", None, Some(&[2; 200])).1, "long input is hashed in");
    assert_ne!(&base[..16], &first[..], "requests do not repeat");
    // Every length is filled to the end, and bytes look spread out.
    let mut drbg = entropy::Drbg::new(sha256);
    drbg.seed(b"lengths");
    for len in [0usize, 1, 31, 32, 33, 64, 257, 4096] {
        let mut out = vec![0u8; len];
        assert!(drbg.fill(&mut out));
        if len >= 31 {
            assert!(out[len - 8..].iter().any(|&b| b != 0));
        }
    }
    let ones: u32 = base.iter().map(|b| b.count_ones()).sum();
    let bits = base.len() as u32 * 8;
    assert!(ones > bits * 47 / 100 && ones < bits * 53 / 100);
}

/// A stand-in for timing noise in the estimator tests (not a seed).
fn noise(i: u32) -> u32 {
    let x = i.wrapping_mul(0x9e37_79b9) ^ 0x5bd1_e995;
    (x ^ (x >> 15)).wrapping_mul(0x85eb_ca6b) >> 16
}

#[test]
fn jitter_estimate_counts_nothing_predictable() {
    let bits = |f: &dyn Fn(u32) -> u32| {
        let (mut j, mut t) = (entropy::Jitter::default(), 0u32);
        for i in 0..entropy::MAX_SAMPLES {
            t = t.wrapping_add(f(i));
            j.sample(t);
        }
        j.bits()
    };
    assert_eq!(bits(&|_| 0), 0, "a stuck timer");
    assert_eq!(bits(&|_| 13), 0, "a constant step");
    assert_eq!(bits(&|i| 13 + (i & 1) * 40), 0, "a step that alternates between two values");
    for period in 3..=8 {
        assert_eq!(bits(&|i| 100 + (i % period) * 37), 0, "a step that repeats every {period}");
    }
    // A fixed delay of 13.37 ticks read on a whole-tick counter: steps of 13
    // and 14 in a pattern that never repeats exactly, and no randomness.
    assert_eq!(bits(&|i| ((i + 1) as f64 * 13.37) as u32 - (i as f64 * 13.37) as u32), 0, "tick rounding");
    assert_eq!(bits(&|i| 13 + noise(i) % 2), 0, "one tick of noise is not told from rounding");
    assert_eq!(bits(&|i| 1000 + i / 64), 0, "a slow drift");
    assert!(bits(&|i| 1000 + (i % 4096)) < 8, "a ramp counts only where it wraps");
    assert!(bits(&|i| 13 + (i % 1000 == 0) as u32 * 500) < 20, "a rare spike counts once each");
    // Real variation is credited, and well under what it holds: 4 bits a
    // sample here, credited less than 0.2.
    let wide = bits(&|i| 100 + noise(i) % 16);
    assert!(wide >= 256 && wide < entropy::MAX_SAMPLES / 5, "{wide} bits");
    assert!(bits(&|i| 100 + noise(i) % 4096) <= entropy::MAX_SAMPLES / 2);
}

#[test]
fn seed_needs_enough_credited_jitter() {
    let seed = |sha: entropy::Sha256, f: &dyn Fn(u32) -> u32| {
        let mut drbg = entropy::Drbg::new(sha);
        let (mut t, mut i) = (0u32, 0u32);
        let (samples, bits) = entropy::gather(&mut drbg, &mut || {
            t = t.wrapping_add(f(i));
            i += 1;
            t
        });
        assert_eq!(samples, i, "every sample asked for is reported");
        let mut out = [0u8; 32];
        (drbg.fill(&mut out), samples, bits, out)
    };
    // What the first health check let through as a seed.
    for (name, f) in [
        ("stuck", &(|_| 0u32) as &dyn Fn(u32) -> u32),
        ("constant", &|_| 13),
        ("two-valued", &|i| 13 + (i & 1)),
        ("one in sixteen differs", &|i| 13 + (i % 16 == 0) as u32),
        ("tick rounding", &|i| ((i + 1) as f64 * 13.37) as u32 - (i as f64 * 13.37) as u32),
        ("one tick of noise", &|i| 13 + noise(i) % 2),
    ] {
        let (ok, samples, bits, out) = seed(sha256, f);
        assert!(!ok && bits < entropy::SEED_BITS, "{name}: must not seed ({bits} bits)");
        assert_eq!(samples, entropy::MAX_SAMPLES, "{name}: it kept trying to the limit");
        assert_eq!(out, [0; 32], "{name}: nothing given out");
    }
    // Enough jitter seeds, as soon as the credit is there and not before.
    let (ok, samples, bits, a) = seed(sha256, &|i| 100 + noise(i) % 64);
    assert!(ok && bits >= entropy::SEED_BITS && samples >= 2 * entropy::SEED_BITS && samples < 2000, "{samples} samples");
    let (_, _, _, b) = seed(sha256, &|i| 100 + noise(i + 1) % 64);
    assert_ne!(a, b, "different timings, different generator");
    let (ok, slow, _, _) = seed(sha256, &|i| 100 + noise(i) % 12);
    assert!(ok && slow > samples, "less jitter takes more samples ({slow})");
    // A hash that is not SHA-256 (a miscompiled or missing one) seeds nothing.
    let (ok, samples, _, _) = seed(|_| [0; 32], &|i| 100 + noise(i) % 64);
    assert!(!ok && samples == 0);
    // A later attempt can seed a generator the first could not.
    let mut drbg = entropy::Drbg::new(sha256);
    entropy::gather(&mut drbg, &mut || 5);
    assert!(!drbg.seeded());
    let (mut t, mut i) = (0u32, 0u32);
    entropy::gather(&mut drbg, &mut || {
        t = t.wrapping_add(100 + noise(i) % 64);
        i += 1;
        t
    });
    assert!(drbg.seeded() && !drbg.hardware);
}

// ---- the Linux ABI cluster: PT_INTERP, fork, the signal gate (procmgr.rs) ----

/// `elf` with PT_PHDR and a PT_INTERP naming `interp` placed after the headers.
fn dynamic_elf(kind: u16, entry: u32, segments: &[(u32, Vec<u8>, u32, u32)], interp: &str) -> Vec<u8> {
    let mut image = elf(kind, entry, segments);
    // Two more program headers go after the existing ones: move the segment data along.
    let phnum = segments.len();
    let extra = 64;
    let mut out = image[..52 + 32 * phnum].to_vec();
    out.extend(vec![0u8; extra]);
    out.extend_from_slice(&image[52 + 32 * phnum..]);
    for i in 0..phnum {
        let at = 52 + 32 * i + 4;
        let off = u32::from_le_bytes(out[at..at + 4].try_into().unwrap()) + extra as u32;
        out[at..at + 4].copy_from_slice(&off.to_le_bytes());
    }
    let path_at = out.len() as u32;
    out.extend_from_slice(interp.as_bytes());
    out.push(0);
    let n = interp.len() as u32 + 1;
    let headers = [[6, 52, 52, 52, 32 * (phnum as u32 + 2), 32 * (phnum as u32 + 2), 4, 4], [3, path_at, 0, 0, n, n, 4, 1]];
    for (k, h) in headers.iter().enumerate() {
        let at = 52 + 32 * (phnum + k);
        for (j, w) in h.iter().enumerate() {
            out[at + 4 * j..at + 4 * j + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    out[44..46].copy_from_slice(&((phnum + 2) as u16).to_le_bytes());
    image.clear();
    out
}
fn musl_like_loader(code_with_data: bool) -> Vec<u8> {
    let code_at = if code_with_data { 0x1000 } else { 0x10000 };
    elf(3, code_at + 0x100, &[(0, vec![0x41; 0x200], 0x200, 4), (code_at, vec![0x42; 0x400], 0x400, 5), (0x20000, vec![0x43; 0x40], 0x2000, 6)])
}
fn aux(w: &mut World, sp: u32) -> Vec<(u32, u32)> {
    let argc = w.read(sp, 4).map(|b| u32::from_le_bytes(b[..4].try_into().unwrap())).unwrap();
    let mut at = sp + 4 + 4 * (argc + 1);
    while w.read(at, 4).unwrap() != [0, 0, 0, 0] {
        at += 4;
    }
    at += 4;
    let mut out = Vec::new();
    loop {
        let b = w.read(at, 8).unwrap();
        let (k, v) = (u32::from_le_bytes(b[..4].try_into().unwrap()), u32::from_le_bytes(b[4..].try_into().unwrap()));
        if k == 0 {
            return out;
        }
        out.push((k, v));
        at += 8;
    }
}

#[test]
fn a_dynamic_program_starts_in_musls_loader() {
    let main = dynamic_elf(3, 0x10010, &[(0, vec![0x11; 0x100], 0x100, 4), (0x10000, vec![0x22; 0x100], 0x100, 5), (0x20000, vec![0x33; 0x10], 0x1000, 6)], "/lib/ld-musl-armhf.so.1");
    let ld = musl_like_loader(false);
    let layout = Layout::random(&[0x5a; 24]);
    let mut w = world();
    let args = [String::from("/apps/dyn")];
    let loaded = load(&mut w.space, &mut w.k, &mut w.pool, &main, Some(&ld), &args, &[], [1; 16], layout).unwrap();
    let a = aux(&mut w, loaded.sp);
    let get = |key| a.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap();
    let ld_base = get(7);
    assert!(ld_base >= vma::MMAP_BASE && ld_base < layout.mmap_top && ld_base & 0xffff == 0, "AT_BASE {ld_base:#x} in the mmap area");
    assert_eq!(loaded.entry, ld_base + 0x10100, "the thread starts in the loader");
    assert_eq!(get(9), layout.pie_base + 0x10010, "AT_ENTRY is the program's");
    assert_eq!(get(3), layout.pie_base + 52, "AT_PHDR is the program's");
    // Both images' code is read-execute, their data read-write, nothing both.
    assert_eq!(sys::at(ld_base + 0x10000), Some((false, true)));
    assert_eq!(sys::at(ld_base + 0x20000), Some((true, false)));
    assert_eq!(sys::at(layout.pie_base + 0x10000), Some((false, true)));
    assert_eq!(w.read(ld_base + 0x10000, 4).unwrap(), vec![0x42; 4]);
    assert!(!vma::mixed(&w.space.vmas));
    // The heap follows the program, not the loader.
    assert!(w.space.brk_start >= layout.pie_base + 0x21000 && w.space.brk_start < vma::MMAP_BASE);
}

#[test]
fn a_static_program_keeps_at_base_zero() {
    let (mut s, l) = started(3, Layout::random(&[0x5a; 24]));
    assert_eq!(aux(&mut s, l.sp).iter().find(|(k, _)| *k == 7).map(|(_, v)| *v), Some(0));
}

#[test]
fn a_dynamic_program_needs_a_proper_loader() {
    let main = dynamic_elf(3, 0x10010, &[(0, vec![0x11; 0x100], 0x100, 4), (0x10000, vec![0x22; 0x100], 0x100, 5)], "/lib/ld-musl-armhf.so.1");
    let try_load = |ld: Option<&[u8]>| {
        let mut w = world();
        load(&mut w.space, &mut w.k, &mut w.pool, &main, ld, &[], &[], [0; 16], Layout::fixed()).map(|_| ()).err()
    };
    assert!(try_load(None).unwrap().contains("dynamic loader"));
    assert_eq!(try_load(Some(&musl_like_loader(true))), Some("code shares a 64 KiB page with data"), "the loader obeys W^X too");
    assert_eq!(try_load(Some(&program(2))), Some("/lib/ld-musl-armhf.so.1 is not a dynamic loader"), "not position-independent");
    assert_eq!(try_load(Some(&main)), Some("/lib/ld-musl-armhf.so.1 is not a dynamic loader"), "a loader with an interpreter");
    let other = dynamic_elf(3, 0x10010, &[(0x10000, vec![0x22; 0x100], 0x100, 5)], "/lib/ld-linux-armhf.so.3");
    let mut w = world();
    assert!(load(&mut w.space, &mut w.k, &mut w.pool, &other, Some(&musl_like_loader(false)), &[], &[], [0; 16], Layout::fixed()).is_err());
}

#[test]
fn fork_copies_owned_memory_and_drops_device_pages() {
    let (mut w, _) = started(2, Layout::fixed());
    assert!(w.write(0x39cf0, b"parent"));
    // A device page (the screen surface): mapped, not the app's to copy.
    w.space.add(0x5000_0000, 0x5001_0000, RW);
    let device = w.pool.take().unwrap();
    w.space.map(&mut w.k, &w.pool, 0x5000_0000, device, false).unwrap();
    let mut child = Space::new(&mut w.k, 2).unwrap();
    let before = w.pool.available();
    w.space.fork_into(&mut child, &mut w.k, &mut w.pool).unwrap();
    let (owned, _) = w.space.usage();
    assert_eq!(before - w.pool.available(), owned, "one new page per page the parent owns");
    assert_eq!(child.brk, w.space.brk);
    assert_eq!(child.layout, w.space.layout);
    assert!(child.find(0x5000_0000).is_none(), "the device region is not the child's");
    assert_eq!(child.vmas.len() + 1, w.space.vmas.len());
    let mut got = [0u8; 6];
    assert!(child.read(&mut w.k, &mut w.pool, 0x39cf0, &mut got));
    assert_eq!(&got, b"parent");
    assert!(child.write(&mut w.k, &mut w.pool, 0x39cf0, b"child!"));
    assert_eq!(w.read(0x39cf0, 6).unwrap(), b"parent".to_vec(), "the parent keeps its own copy");
    // The child's code is executable and was made visible to instruction fetch.
    let text = sys::HW.with(|h| h.borrow().unifies.len());
    assert!(text > 0);
}

#[test]
fn fork_without_pool_room_fails_cleanly() {
    // Out of pool pages: an error, and the caller destroys the half-made child.
    let mut w2 = world();
    load(&mut w2.space, &mut w2.k, &mut w2.pool, &program(2), None, &[], &[], [0; 16], Layout::fixed()).unwrap();
    while w2.pool.available() > 1 {
        w2.pool.take();
    }
    let mut c2 = Space::new(&mut w2.k, 3).unwrap();
    assert!(w2.space.fork_into(&mut c2, &mut w2.k, &mut w2.pool).is_err());
    c2.destroy(&mut w2.k, &mut w2.pool);
}

#[test]
fn the_trampoline_page_holds_the_signal_gate() {
    let (mut w, _) = started(2, Layout::fixed());
    w.space.add(STUBS, STUBS + 0x10000, RX);
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, 3, 0x20d91), Some(STUBS + 24));
    assert_eq!(w.space.gate(), STUBS + signal::GATE_OFFSET);
    let words: Vec<u32> = (0..signal::GATE.len() as u32)
        .map(|k| {
            let mut b = [0u8; 4];
            w.space.read(&mut w.k, &mut w.pool, STUBS + signal::GATE_OFFSET + 4 * k, &mut b);
            u32::from_le_bytes(b)
        })
        .collect();
    assert_eq!(words, signal::GATE.to_vec());
    // No trampoline reaches the gate.
    assert_eq!(w.space.stub(&mut w.k, &mut w.pool, (signal::GATE_OFFSET / 8) as usize, 0x20d91), None);
    w.no_app_bytes_in_executable_pages();
}
