//! Host tests for app paging (`make test-unit`): the real pool.rs (the page
//! pool), procmgr.rs (address spaces, faults, the loader), vma.rs and
//! lifecycle.rs's page map, over a stand-in for seL4 that keeps the kernel's
//! own rules: a slot holds one capability, a frame capability maps at one
//! address, only at its own alignment and only where a page table is, and a
//! mapping is never writable and executable. The stand-in also plays the
//! MMU (`sys::touch`): an "app" here reaches memory only through the
//! mappings the runtime made, with their rights, and faults otherwise.
//!
//! Built twice. As is: 4 KiB pages, the shipped setting. With `--cfg page64`:
//! 64 KiB pages, the earlier setting, which the same cfg selects in the
//! shipped pages.rs (included here, as sys.rs includes it); the same tests
//! pass, and the ones marked for it show what 64 KiB pages could not enforce. `measure_*` print what each costs:
//!     rustc --edition=2021 --test -O runtime/open/tests/paging_host.rs -o build/tests/scale/paging4
//!     rustc --edition=2021 --test -O --cfg page64 runtime/open/tests/paging_host.rs -o build/tests/scale/paging64
//!     build/tests/scale/paging4 measure --nocapture
//! (`--cfg bigpool` gives the machine 128 MiB, for PAGING_ELF's programs.)
//!
//! Each test has its own machine (thread-local): they run in parallel.
#![allow(dead_code)]
#![allow(unexpected_cfgs)]
extern crate alloc;

macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = alloc::format!($($arg)*);
    }};
}
pub(crate) use log;

#[path = "../pages.rs"]
mod pages;
use sys::Object;
#[path = "../vma.rs"]
mod vma;
#[path = "../lifecycle.rs"]
mod lifecycle;
#[path = "../pool.rs"]
mod pool;
#[path = "../signal.rs"]
mod signal;
#[path = "../../../system/supervisor/procmgr.rs"]
mod procmgr;

mod syscall {
    pub const ENOMEM: i32 = -12;
    pub const EACCES: i32 = -13;
    pub const EEXIST: i32 = -17;
}

/// The runtime's heap takes the first pool pages; nothing to do here.
mod heap {
    pub struct Heap;
    impl Heap {
        pub unsafe fn add(&self, _start: usize, _len: usize) {}
    }
    pub static HEAP: Heap = Heap;
}

/// seL4 as the Open Runtime uses it (sys.rs has the same names).
mod sys {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    // The shipped page size and CSpace size (sys.rs takes them from pages.rs
    // too); only the pool is smaller here.
    pub use super::pages::{slots_needed, PAGE, PAGE_BITS, PAGE_KIB, PAGE_OBJECT, SLOTS, SLOT_DYNAMIC};
    /// A 4 MiB and an 8 MiB pool untyped (the boards have 128 + 256 or 256 MiB);
    /// `--cfg bigpool`: 128 MiB, for measure_real_programs_at_load.
    #[cfg(not(bigpool))]
    pub const POOL_BITS: &[u32] = &[22, 23];
    #[cfg(bigpool)]
    pub const POOL_BITS: &[u32] = &[27];
    pub const POOL_MIB: usize = if cfg!(bigpool) { 128 } else { 12 };
    pub const POOL_PAGES: usize = (POOL_MIB << 20) / PAGE;
    pub const POOL_VA: usize = 0x2000_0000;
    pub const SLOT_PD: u32 = 4;
    pub const SLOT_KERNEL: u32 = 6;
    pub const SLOT_POOL: u32 = 16;
    pub const SLOT_REPLY: u32 = 31;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Object {
        Untyped,
        Tcb,
        Endpoint,
        CNode,
        PageDirectory,
        PageTable,
        LargePage,
        SmallPage,
        Notification,
        Reply,
        SchedContext,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Mapping {
        pub pd: u32,
        pub va: u64,
        pub writable: bool,
        pub exec: bool,
    }
    #[derive(Clone, Copy, Debug)]
    enum Cap {
        /// Physical offset in the pool, size, and where this capability maps it.
        Frame { phys: usize, bytes: usize, at: Option<Mapping> },
        Other,
    }

    pub struct Machine {
        caps: BTreeMap<u32, Cap>,
        /// Bytes carved from each pool untyped.
        carved: Vec<usize>,
        tables: BTreeSet<(u32, u64)>,
        /// (pd, va) -> the slot of the capability mapped there.
        mapped: BTreeMap<(u32, u64), u32>,
        memory: Vec<u8>,
        /// Kernel invocations made, by kind.
        pub retypes: u64,
        pub maps: u64,
        pub deletes: u64,
        pub unifies: u64,
        /// Make the next page_map calls into an app fail (out of kernel memory).
        pub fail_maps: u32,
    }
    impl Default for Machine {
        fn default() -> Machine {
            Machine {
                caps: BTreeMap::new(),
                carved: vec![0; POOL_BITS.len()],
                tables: BTreeSet::new(),
                mapped: BTreeMap::new(),
                memory: vec![0u8; POOL_PAGES * PAGE],
                retypes: 0,
                maps: 0,
                deletes: 0,
                unifies: 0,
                fail_maps: 0,
            }
        }
    }
    thread_local!(pub static M: RefCell<Machine> = RefCell::new(Machine::default()));
    fn with<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
        M.with(|m| f(&mut m.borrow_mut()))
    }

    /// Where the runtime sees the pool: this machine's memory.
    pub fn pool_base() -> usize {
        with(|m| m.memory.as_mut_ptr() as usize)
    }

    pub fn mcs() -> bool {
        false
    }
    pub fn retype(untyped: u32, o: Object, _bits: u32, slot: u32) -> Result<(), i32> {
        with(|m| {
            if m.caps.contains_key(&slot) {
                return Err(8); // seL4_DeleteFirst
            }
            m.retypes += 1;
            let bytes = match o {
                Object::SmallPage => 1 << 12,
                Object::LargePage => 1 << 16,
                _ => {
                    assert!(untyped < SLOT_POOL || untyped >= SLOT_POOL + POOL_BITS.len() as u32, "the pool is device memory: frames only");
                    m.caps.insert(slot, Cap::Other);
                    return Ok(());
                }
            };
            let u = untyped.checked_sub(SLOT_POOL).filter(|&u| (u as usize) < POOL_BITS.len()).expect("frames come from the pool") as usize;
            if m.carved[u] + bytes > 1 << POOL_BITS[u] {
                return Err(10); // seL4_NotEnoughMemory
            }
            let base: usize = POOL_BITS[..u].iter().map(|b| 1usize << b).sum();
            m.caps.insert(slot, Cap::Frame { phys: base + m.carved[u], bytes, at: None });
            m.carved[u] += bytes;
            Ok(())
        })
    }
    pub fn copy(dest: u32, src: u32) -> Result<(), i32> {
        with(|m| {
            if m.caps.contains_key(&dest) {
                return Err(8);
            }
            let cap = match m.caps.get(&src) {
                Some(Cap::Frame { phys, bytes, .. }) => Cap::Frame { phys: *phys, bytes: *bytes, at: None },
                Some(Cap::Other) => Cap::Other,
                None => return Err(6), // seL4_FailedLookup
            };
            m.caps.insert(dest, cap);
            Ok(())
        })
    }
    pub fn move_cap(dest: u32, src: u32) -> Result<(), i32> {
        with(|m| {
            let cap = m.caps.remove(&src).ok_or(6)?;
            m.caps.insert(dest, cap);
            Ok(())
        })
    }
    pub fn pt_map(_pt: u32, pd: u32, va: u64) -> Result<(), i32> {
        with(|m| if m.tables.insert((pd, va >> 20)) { Ok(()) } else { Err(8) })
    }
    pub fn asid_assign(_pd: u32) -> Result<(), i32> {
        Ok(())
    }
    pub fn page_map(frame: u32, pd: u32, va: u64, writable: bool, exec: bool) -> Result<(), i32> {
        assert!(!(writable && exec), "a page mapped writable and executable at {va:#x}");
        with(|m| {
            let Some(Cap::Frame { phys, bytes, at }) = m.caps.get(&frame).copied() else { return Err(3) };
            assert_eq!(va as usize % bytes, 0, "a {bytes}-byte frame mapped at {va:#x}");
            if !m.tables.contains(&(pd, va >> 20)) {
                return Err(6);
            }
            if pd == SLOT_PD {
                assert_eq!(va as usize, POOL_VA + phys, "the runtime's map of the pool is linear");
                assert!(writable && !exec, "the runtime's own view: writable, never executable");
            } else if m.fail_maps > 0 {
                m.fail_maps -= 1;
                return Err(10);
            }
            match at {
                // seL4 changes the rights of a mapped frame only where it is mapped.
                Some(old) if (old.pd, old.va) != (pd, va) => return Err(2),
                Some(_) => {}
                None if m.mapped.contains_key(&(pd, va)) => return Err(8),
                None => {}
            }
            m.maps += 1;
            m.mapped.insert((pd, va), frame);
            m.caps.insert(frame, Cap::Frame { phys, bytes, at: Some(Mapping { pd, va, writable, exec }) });
            Ok(())
        })
    }
    pub fn page_unmap(frame: u32) {
        with(|m| {
            if let Some(Cap::Frame { phys, bytes, at: Some(old) }) = m.caps.get(&frame).copied() {
                m.mapped.remove(&(old.pd, old.va));
                m.caps.insert(frame, Cap::Frame { phys, bytes, at: None });
            }
        })
    }
    pub fn page_unify(frame: u32) {
        with(|m| {
            assert!(matches!(m.caps.get(&frame), Some(Cap::Frame { at: Some(_), .. })), "seL4 refuses to flush a frame that is not mapped");
            m.unifies += 1;
        })
    }
    pub fn delete(slot: u32) {
        page_unmap(slot);
        with(|m| {
            m.deletes += 1;
            m.caps.remove(&slot);
        })
    }

    // ---- what a test may ask the machine ----

    /// The MMU: where an access by code running in `pd` lands, if the
    /// mapping there allows it. None is a fault.
    pub fn touch(pd: u32, va: u64, access: super::vma::Access) -> Option<*mut u8> {
        use super::vma::Access::*;
        with(|m| {
            let page = va & !(PAGE as u64 - 1);
            let slot = *m.mapped.get(&(pd, page))?;
            let Some(Cap::Frame { phys, at: Some(at), .. }) = m.caps.get(&slot).copied() else { return None };
            let ok = match access {
                Read => true,
                Write => at.writable,
                Exec => at.exec,
            };
            ok.then(|| unsafe { m.memory.as_mut_ptr().add(phys + (va - page) as usize) })
        })
    }
    /// Every mapping in `pd`.
    pub fn mappings(pd: u32) -> Vec<Mapping> {
        with(|m| {
            m.mapped
                .values()
                .filter_map(|slot| match m.caps.get(slot) {
                    Some(Cap::Frame { at: Some(at), .. }) if at.pd == pd => Some(*at),
                    _ => None,
                })
                .collect()
        })
    }
    /// Capabilities in slots now.
    pub fn caps() -> usize {
        with(|m| m.caps.len())
    }
    pub fn counters() -> (u64, u64, u64) {
        with(|m| (m.retypes, m.maps, m.deletes))
    }
    pub fn fail_maps(n: u32) {
        with(|m| m.fail_maps = n)
    }
    /// A new machine for this thread (a test that builds several worlds).
    pub fn reset() {
        with(|m| *m = Machine::default())
    }
}

use pool::{Kernel, Pool};
use procmgr::{load, Space, PROT_EXEC, PROT_READ, PROT_WRITE, WINDOW};
use vma::{Access, Layout};

const R: u32 = PROT_READ;
const RW: u32 = PROT_READ | PROT_WRITE;
const RX: u32 = PROT_READ | PROT_EXEC;
const PAGE: u64 = sys::PAGE as u64;
const KIB: usize = sys::PAGE_KIB;
/// The runtime's heap in these tests: 1 MiB of the pool.
const HEAP_PAGES: usize = (1 << 20) / sys::PAGE;
const APP_PAGES: usize = sys::POOL_PAGES - HEAP_PAGES;

/// One running system: the runtime's kernel bookkeeping, the pool, and apps.
struct World {
    k: Kernel,
    pool: Pool,
    /// Page faults the runtime was asked to resolve.
    faults: u32,
}
/// An app: its address space, and "its code" touching memory through the MMU.
struct App {
    space: Space,
}

fn world() -> World {
    sys::reset();
    let mut k = Kernel::new();
    let (pool, heap_bytes) = Pool::new(&mut k, HEAP_PAGES).unwrap();
    assert_eq!(heap_bytes, HEAP_PAGES * sys::PAGE);
    World { k, pool, faults: 0 }
}

impl World {
    fn app(&mut self, untyped: u32) -> App {
        App { space: Space::new(&mut self.k, untyped).unwrap() }
    }
    /// What the app's code does on an access: the MMU, a fault to the
    /// runtime if the mapping does not allow it, and the access again.
    /// None: the runtime refused the fault (the app would be stopped).
    fn access(&mut self, app: &mut App, va: u64, access: Access) -> Option<*mut u8> {
        for _ in 0..3 {
            if let Some(p) = sys::touch(app.space.pd, va, access) {
                return Some(p);
            }
            self.faults += 1;
            if !app.space.fault(&mut self.k, &mut self.pool, va, access) {
                return None;
            }
        }
        panic!("the fault at {va:#x} was resolved but the access still faults");
    }
    fn store(&mut self, app: &mut App, va: u64, byte: u8) -> bool {
        self.access(app, va, Access::Write).map(|p| unsafe { p.write(byte) }).is_some()
    }
    fn fetch(&mut self, app: &mut App, va: u64) -> Option<u8> {
        self.access(app, va, Access::Read).map(|p| unsafe { p.read() })
    }
    /// mmap(len, prot) as syscall.rs does it: a free range, the policy, the region.
    fn mmap(&mut self, app: &mut App, len: u64, prot: u32) -> u64 {
        let len = (len + 4095) & !4095;
        let at = app.space.free_range(len).unwrap();
        assert!(!app.space.refuses(at, at + len, prot));
        app.space.add(at, at + len, prot);
        at
    }
    fn munmap(&mut self, app: &mut App, start: u64, len: u64) {
        app.space.remove(&mut self.k, &mut self.pool, start, start + len);
    }
    fn end(&mut self, mut app: App) {
        app.space.destroy(&mut self.k, &mut self.pool);
    }
    fn free_kib(&self) -> usize {
        self.pool.available() * KIB
    }
    /// Every page is the heap's, free, or owned by exactly one of `apps`.
    fn accounted(&self, apps: &[&App]) {
        let owned: usize = apps.iter().map(|a| a.space.usage().0).sum();
        assert_eq!(self.pool.available() + owned, self.pool.app_pages, "pool pages lost or counted twice");
        assert_eq!(self.pool.refused, 0);
    }
}

/// Every hardware mapping of `app` has exactly the rights of the one region
/// that covers its page (4 KiB pages), or of the regions in it (64 KiB).
fn mappings_follow_regions(app: &App) {
    for m in sys::mappings(app.space.pd) {
        let prot = vma::union(&app.space.vmas, m.va, m.va + PAGE);
        let hw = vma::hardware(prot);
        assert_eq!((m.writable, m.exec), (hw == vma::HW_RW, hw == vma::HW_RX), "page {:#x} (regions {:#x})", m.va, prot);
        assert_ne!(hw, vma::HW_NONE, "page {:#x} is mapped with no region allowing access", m.va);
        #[cfg(not(page64))]
        {
            let v = app.space.find(m.va).expect("a mapped page outside every region");
            assert!(v.start <= m.va && v.end >= m.va + PAGE, "a 4 KiB page lies in one region");
        }
    }
}

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
/// Laid out like the shipped apps: constants, code, data + bss, 64 KiB apart.
fn program() -> Vec<u8> {
    elf(
        2,
        0x20d90,
        &[
            (0x10000, vec![0x11; 0xd8c], 0xd8c, 4),
            (0x20d90, vec![0x22; 0x8f60], 0x8f60, 5),
            (0x39cf0, vec![0x33; 0x3c], 0x12310, 6),
        ],
    )
}
fn started(w: &mut World, untyped: u32, image: &[u8]) -> (App, procmgr::Loaded) {
    let mut app = w.app(untyped);
    let args = [String::from("/apps/test")];
    let layout = Layout::fixed();
    let loaded = load(&mut app.space, &mut w.k, &mut w.pool, image, None, &args, &[], [7; 16], layout).unwrap();
    // As mod.rs `launch` does: the trampoline region, then the first thread's trampoline.
    app.space.add(layout.stubs, layout.stubs + WINDOW, RX);
    app.space.stub(&mut w.k, &mut w.pool, 0, loaded.entry as u32).unwrap();
    (app, loaded)
}

// ---- the pool ----

#[test]
fn the_pool_cuts_all_its_memory_into_pages_mapped_in_a_line() {
    let w = world();
    assert_eq!(w.pool.total, sys::POOL_PAGES);
    assert_eq!(w.pool.app_pages, APP_PAGES);
    assert_eq!(w.pool.available(), APP_PAGES);
    // One frame per page, each mapped once in the runtime (the stand-in
    // checks every one is at POOL_VA + its offset, writable, not executable).
    let mine = sys::mappings(sys::SLOT_PD);
    assert_eq!(mine.len(), sys::POOL_PAGES);
    assert!(mine.iter().all(|m| m.writable && !m.exec));
    let (retypes, maps, _) = sys::counters();
    assert_eq!(maps as usize, sys::POOL_PAGES);
    // Frames, and a page table per MiB.
    assert_eq!(retypes as usize, sys::POOL_PAGES + sys::POOL_MIB);
    // The capability of page i is first + i: pool.cap finds it without a table.
    assert_eq!(w.pool.cap(5) - w.pool.cap(0), 5);
}

#[test]
fn pages_come_zeroed_and_go_back_once() {
    let mut w = world();
    let a = w.pool.take().unwrap();
    unsafe { core::ptr::write_bytes(pool::page_va(a) as *mut u8, 0xee, sys::PAGE) };
    w.pool.give_back(a);
    assert_eq!(w.pool.available(), APP_PAGES);
    // Given back twice, or a page of the heap: refused, not on the free list twice.
    w.pool.give_back(a);
    w.pool.give_back(0);
    assert_eq!((w.pool.available(), w.pool.refused), (APP_PAGES, 2));
    let mut seen = std::collections::BTreeSet::new();
    while let Some(p) = w.pool.take() {
        assert!(seen.insert(p), "page {p} handed out twice");
        assert!(p as usize >= HEAP_PAGES, "a heap page handed to an app");
        let bytes = unsafe { core::slice::from_raw_parts(pool::page_va(p) as *const u8, sys::PAGE) };
        assert!(bytes.iter().all(|&b| b == 0), "page {p} not zeroed");
    }
    assert_eq!(seen.len(), APP_PAGES);
}

#[test]
fn the_image_takes_the_top_of_the_pool_for_good() {
    let mut w = world();
    let count = (1 << 20) / sys::PAGE; // a 1 MiB unpacked image
    let base = w.pool.take_top(count).unwrap();
    assert_eq!(base, pool::page_va((sys::POOL_PAGES - count) as u32));
    assert_eq!((w.pool.available(), w.pool.app_pages), (APP_PAGES - count, APP_PAGES - count));
    // Its pages can never be given to an app.
    w.pool.give_back(sys::POOL_PAGES as u32 - 1);
    assert_eq!(w.pool.refused, 1);
    // One app's share follows what is left.
    assert_eq!(w.pool.app_cap() as usize, w.pool.app_pages - w.pool.app_pages / 8);
}

// ---- rights, page by page ----

#[test]
fn a_guard_page_inside_a_mapping_stops_the_app() {
    let mut w = world();
    let mut a = w.app(900);
    // A thread stack as musl makes it: one mapping, its lowest page the guard.
    let at = w.mmap(&mut a, 0x22000, RW);
    a.space.protect(at, at + 0x1000, 0).unwrap();
    assert!(w.store(&mut a, at + 0x1000, 1), "the stack above the guard");
    assert!(w.store(&mut a, at + 0x21fff, 2));
    #[cfg(not(page64))]
    {
        assert!(!w.store(&mut a, at + 0xfff, 3), "a write to the guard page must fault and be refused");
        assert_eq!(w.fetch(&mut a, at), None, "and a read");
        assert!(sys::touch(a.space.pd, at, Access::Read).is_none(), "the guard has no hardware mapping");
    }
    #[cfg(page64)]
    {
        // What 64 KiB pages could not do: the guard shares a hardware page
        // with the stack above it, so the overflow goes through unseen.
        assert!(sys::touch(a.space.pd, at + 0xfff, Access::Write).is_some());
    }
    mappings_follow_regions(&a);
    w.accounted(&[&a]);
}

#[test]
fn mprotect_of_one_page_is_enforced_for_that_page_only() {
    let mut w = world();
    let mut a = w.app(900);
    let at = w.mmap(&mut a, 0x10000, RW);
    for page in 0..16 {
        assert!(w.store(&mut a, at + page * 0x1000, page as u8 + 1));
    }
    // Read-only: page 5 (relro, a JIT's sealed data).
    a.space.protect(at + 0x5000, at + 0x6000, R).unwrap();
    mappings_follow_regions(&a);
    assert_eq!(w.fetch(&mut a, at + 0x5000), Some(6), "still readable, contents kept");
    assert!(w.store(&mut a, at + 0x4fff, 9) && w.store(&mut a, at + 0x6000, 9), "its neighbours stay writable");
    #[cfg(not(page64))]
    assert!(!w.store(&mut a, at + 0x5000, 9), "a write to the read-only page must be refused");
    #[cfg(page64)]
    assert!(sys::touch(a.space.pd, at + 0x5000, Access::Write).is_some(), "64 KiB pages: still writable in hardware");
    // No access at all: page 9.
    a.space.protect(at + 0x9000, at + 0xa000, 0).unwrap();
    #[cfg(not(page64))]
    {
        assert_eq!(w.fetch(&mut a, at + 0x9000), None);
        assert!(sys::touch(a.space.pd, at + 0x9000, Access::Read).is_none());
    }
    // And back: writable again, contents kept (the page was never freed).
    a.space.protect(at + 0x9000, at + 0xa000, RW).unwrap();
    assert_eq!(w.fetch(&mut a, at + 0x9000), Some(10));
    assert!(w.store(&mut a, at + 0x9000, 1));
    // Never executable, whatever it asks.
    assert_eq!(a.space.protect(at, at + 0x1000, RX), Err(syscall::EACCES));
    assert!(a.space.refuses(at, at + 0x1000, RX));
    assert!(sys::touch(a.space.pd, at, Access::Exec).is_none());
    mappings_follow_regions(&a);
    w.accounted(&[&a]);
}

#[test]
fn munmap_gives_back_exactly_the_pages_it_covers() {
    let mut w = world();
    let mut a = w.app(900);
    let at = w.mmap(&mut a, 0x10000, RW);
    for page in 0..16 {
        assert!(w.store(&mut a, at + page * 0x1000, 0x40 + page as u8));
    }
    let before = w.pool.available();
    w.munmap(&mut a, at + 0x4000, 0x2000);
    #[cfg(not(page64))]
    assert_eq!(w.pool.available(), before + 2, "two 4 KiB pages return");
    #[cfg(page64)]
    assert_eq!(w.pool.available(), before, "64 KiB pages: nothing returns until the whole page is unmapped");
    assert_eq!(w.fetch(&mut a, at + 0x3000), Some(0x43), "the rest keeps its contents");
    assert_eq!(w.fetch(&mut a, at + 0x6000), Some(0x46));
    #[cfg(not(page64))]
    assert_eq!(w.fetch(&mut a, at + 0x4000), None, "the hole faults");
    // Mapped again, the hole is zero: nothing of the old contents shows.
    a.space.add(at + 0x4000, at + 0x6000, RW);
    #[cfg(not(page64))]
    assert_eq!(w.fetch(&mut a, at + 0x4000), Some(0));
    w.munmap(&mut a, at, 0x10000);
    assert_eq!(a.space.usage().0, 0);
    assert_eq!(w.pool.available(), APP_PAGES);
    w.accounted(&[&a]);
}

#[test]
fn a_page_is_never_writable_and_executable_and_code_is_never_app_written() {
    let mut w = world();
    let (mut a, loaded) = started(&mut w, 900, &program());
    assert_eq!(loaded.entry, 0x20d90);
    // Each segment has its own rights, page by page.
    assert!(sys::touch(a.space.pd, 0x10000, Access::Read).is_some());
    assert!(sys::touch(a.space.pd, 0x10000, Access::Write).is_none(), "constants are read-only");
    assert!(sys::touch(a.space.pd, 0x10000, Access::Exec).is_none(), "and not code");
    assert!(sys::touch(a.space.pd, 0x20d90, Access::Exec).is_some());
    assert!(sys::touch(a.space.pd, 0x20d90, Access::Write).is_none(), "code is not writable");
    assert!(!w.store(&mut a, 0x20d90, 0xcc), "and a write fault there is refused");
    assert!(w.store(&mut a, 0x39cf0, 1), "data is writable");
    assert!(sys::touch(a.space.pd, 0x39cf0, Access::Exec).is_none(), "and never runs");
    assert_eq!(w.fetch(&mut a, 0x20d90), Some(0x22));
    // The stack: writable, never executable.
    let sp = loaded.sp;
    assert!(sys::touch(a.space.pd, sp, Access::Write).is_some() && sys::touch(a.space.pd, sp, Access::Exec).is_none());
    // The policy is unchanged: no executable memory from mmap or mprotect,
    // nothing next to code, and system calls cannot write code either.
    assert!(a.space.refuses(0x5000_0000, 0x5000_1000, RX));
    assert!(a.space.refuses(0x2a000, 0x2b000, RW), "nothing in a 64 KiB window that holds code");
    assert_eq!(a.space.protect(0x20000, 0x21000, RW), Err(syscall::EACCES));
    assert!(!a.space.write(&mut w.k, &mut w.pool, 0x20d90, &[0xcc]));
    assert!(!a.space.poke(&mut w.k, &mut w.pool, 0x2a000, &[0xcc]));
    // Every executable page is the program's code as loaded, or trampolines.
    for m in sys::mappings(a.space.pd).iter().filter(|m| m.exec) {
        let v = a.space.find(m.va).unwrap();
        assert!(v.prot & PROT_EXEC != 0 && v.prot & PROT_WRITE == 0);
        assert!(!m.writable);
    }
    mappings_follow_regions(&a);
    w.accounted(&[&a]);
}

#[test]
fn bss_and_untouched_memory_cost_nothing_until_touched() {
    let mut w = world();
    let (mut a, _) = started(&mut w, 900, &program());
    let after_load = a.space.usage().0;
    // The 0x12310-byte data segment holds 0x3c bytes of file: only the page
    // with them has memory; the bss gets pages as it is touched.
    #[cfg(not(page64))]
    assert!(sys::touch(a.space.pd, 0x3b000, Access::Read).is_none());
    assert_eq!(w.fetch(&mut a, 0x39cf0 + 0x3c), Some(0), "bss is zero");
    assert_eq!(w.fetch(&mut a, 0x4bfff), Some(0));
    assert!(a.space.usage().0 > after_load || cfg!(page64));
    // An 8 MiB stack region costs the pages the start-up vectors are on.
    let stack_pages = sys::mappings(a.space.pd).iter().filter(|m| m.va >= Layout::fixed().stack_top - vma::STACK_SIZE && m.va < Layout::fixed().stack_top).count();
    assert_eq!(stack_pages, 1);
    w.accounted(&[&a]);
}

#[test]
fn filling_a_buffer_faults_once_every_few_pages_and_a_lone_touch_gets_one_page() {
    let mut w = world();
    let mut a = w.app(900);
    let at = w.mmap(&mut a, 0x40000, RW);
    // 64 pages (256 KiB) written front to back, as memset or read() would.
    for n in 0..0x40000 / 0x1000 {
        assert!(w.store(&mut a, at + n * 0x1000, 0x5a));
    }
    assert_eq!(a.space.usage().0 * KIB, 256, "exactly the buffer");
    #[cfg(not(page64))]
    assert_eq!(w.faults, 1 + 63_u32.div_ceil(4), "the first page, then four pages a fault");
    #[cfg(page64)]
    assert_eq!(w.faults, 4);
    mappings_follow_regions(&a);
    // Pages that came ahead are zero and the region's own: nothing past its end.
    let next = w.mmap(&mut a, 0x3000, RW);
    assert!(w.store(&mut a, next, 1) && w.store(&mut a, next + 0x1000, 1));
    #[cfg(not(page64))]
    {
        assert_eq!(a.space.usage().0, 64 + 3, "the third page came with the second, the region ends there");
        assert_eq!(sys::touch(a.space.pd, next + 0x2000, Access::Read).map(|p| unsafe { p.read() }), Some(0));
        assert!(sys::touch(a.space.pd, next + 0x3000, Access::Read).is_none());
    }
    // Touches far apart get one page each.
    let sparse = w.mmap(&mut a, 0x100000, RW);
    let before = a.space.usage().0;
    for n in 0..8 {
        assert!(w.store(&mut a, sparse + n * 0x20000, 1));
    }
    assert_eq!(a.space.usage().0 - before, 8);
    // A guard page ahead is never brought in: it is another region.
    let stack = w.mmap(&mut a, 0x4000, RW);
    a.space.protect(stack + 0x2000, stack + 0x3000, 0).unwrap();
    assert!(w.store(&mut a, stack, 1) && w.store(&mut a, stack + 0x1000, 1));
    #[cfg(not(page64))]
    assert!(sys::touch(a.space.pd, stack + 0x2000, Access::Read).is_none() && sys::touch(a.space.pd, stack + 0x3000, Access::Read).is_none());
    // With little memory left (under 8 MiB free), a fault brings its own page only.
    let mut held = Vec::new();
    while w.pool.available() >= (8 << 20) / sys::PAGE {
        held.push(w.pool.take().unwrap());
    }
    let tight = w.mmap(&mut a, 0x10000, RW);
    let before = a.space.usage().0;
    assert!(w.store(&mut a, tight, 1) && w.store(&mut a, tight + 0x1000, 1));
    assert_eq!(a.space.usage().0 - before, if cfg!(page64) { 1 } else { 2 });
    for p in held {
        w.pool.give_back(p);
    }
    // At its cap nothing comes ahead either, and the fault itself still works.
    a.space.cap = a.space.usage().0 as u32 + 2;
    let capped = w.mmap(&mut a, 0x10000, RW);
    assert!(w.store(&mut a, capped, 1) && w.store(&mut a, capped + 0x1000, 1));
    #[cfg(not(page64))]
    assert!(a.space.capped());
    mappings_follow_regions(&a);
    w.accounted(&[&a]);
}

// ---- the trampoline region ----

#[test]
fn trampolines_and_the_gate_live_on_their_own_pages_of_a_pinned_region() {
    let mut w = world();
    let (mut a, loaded) = started(&mut w, 900, &program());
    let stubs = Layout::fixed().stubs;
    let word = |w: &mut World, a: &mut App, va: u64| -> u32 {
        let mut b = [0u8; 4];
        assert!(a.space.read(&mut w.k, &mut w.pool, va, &mut b));
        u32::from_le_bytes(b)
    };
    assert_eq!(word(&mut w, &mut a, stubs), 0xe51f_f004);
    assert_eq!(word(&mut w, &mut a, stubs + 4), loaded.entry as u32);
    // Thread 600's trampoline is 4800 bytes in: on the second 4 KiB page.
    let far = a.space.stub(&mut w.k, &mut w.pool, 600, 0x1234_5678).unwrap();
    assert_eq!(far, stubs + 4800);
    assert_eq!(word(&mut w, &mut a, far + 4), 0x1234_5678);
    // The signal gate, at the end of the region.
    let gate = a.space.gate();
    assert_eq!(gate, stubs + u64::from(signal::GATE_OFFSET));
    assert_eq!(word(&mut w, &mut a, gate), signal::GATE[0]);
    assert_eq!(word(&mut w, &mut a, stubs + u64::from(signal::FORK_HELPER)), signal::GATE[18]);
    // The app runs them and cannot write them.
    for va in [stubs, far, gate] {
        assert!(sys::touch(a.space.pd, va, Access::Exec).is_some(), "{va:#x} must be executable");
        assert!(sys::touch(a.space.pd, va, Access::Write).is_none());
        assert!(!w.store(&mut a, va, 0xcc));
        assert!(!a.space.write(&mut w.k, &mut w.pool, va, &[0xcc]));
    }
    // Only the pages in use have memory: three of sixteen with 4 KiB pages.
    let held = sys::mappings(a.space.pd).iter().filter(|m| m.va >= stubs && m.va < stubs + WINDOW).count();
    assert_eq!(held, if cfg!(page64) { 1 } else { 3 });
    // The whole region is the runtime's: no part can be unmapped, replaced or changed.
    for (s, e) in [(stubs, stubs + 0x1000), (stubs + 0x8000, stubs + 0x9000), (stubs + 0xf000, stubs + WINDOW), (stubs - 0x1000, stubs + 0x1000)] {
        assert!(a.space.pinned(s, e));
        assert!(a.space.refuses(s, e, RW));
        assert_eq!(a.space.protect(s, e, RW), Err(syscall::EACCES));
    }
    assert!(!a.space.pinned(stubs + WINDOW, stubs + WINDOW + 0x1000));
    let before = a.space.usage().0;
    w.munmap(&mut a, stubs - 0x10000, 0x30000);
    assert_eq!(a.space.usage().0, before, "munmap across the region frees none of it");
    assert_eq!(word(&mut w, &mut a, stubs), 0xe51f_f004);
    // A region that is not whole (an app got at it somehow): no thread starts.
    a.space.vmas.retain(|v| v.start != stubs);
    a.space.add(stubs, stubs + 0x1000, RX);
    assert_eq!(a.space.stub(&mut w.k, &mut w.pool, 1, 0), None);
}

// ---- one app's share, and running out ----

#[test]
fn an_app_at_its_cap_gets_no_more_pages_and_others_are_untouched() {
    let mut w = world();
    let mut a = w.app(900);
    let mut b = w.app(901);
    a.space.cap = 8;
    let at = w.mmap(&mut a, 0x100000, RW);
    for n in 0..8 {
        assert!(w.store(&mut a, at + n * WINDOW, 1));
    }
    assert!(a.space.capped());
    assert!(!w.store(&mut a, at + 8 * WINDOW, 1), "the ninth page is refused");
    assert!(w.store(&mut a, at, 2), "what it has keeps working");
    // A system call's copy into memory it has no page for fails cleanly too.
    assert!(!a.space.write(&mut w.k, &mut w.pool, at + 9 * WINDOW, &[1, 2, 3]));
    // brk does not grow at the cap.
    a.space.brk_start = 0x100_0000;
    a.space.brk = 0x100_0000;
    assert_eq!(a.space.set_brk(&mut w.k, &mut w.pool, 0x100_4000), 0x100_0000);
    // Another app is not affected.
    let bt = w.mmap(&mut b, 0x100000, RW);
    for n in 0..16 {
        assert!(w.store(&mut b, bt + n * WINDOW, 1));
    }
    // Giving memory back makes room again.
    w.munmap(&mut a, at, WINDOW);
    assert!(!a.space.capped());
    assert!(w.store(&mut a, at + 8 * WINDOW, 1));
    assert_eq!(a.space.set_brk(&mut w.k, &mut w.pool, 0x100_4000), 0x100_0000, "at the cap again: no growth");
    w.accounted(&[&a, &b]);
    w.end(a);
    w.end(b);
    assert_eq!(w.pool.available(), APP_PAGES);
}

#[test]
fn an_empty_pool_or_a_failed_mapping_fails_the_fault_and_leaks_nothing() {
    let mut w = world();
    let mut a = w.app(900);
    let at = w.mmap(&mut a, 0x10000, RW);
    // The kernel cannot map (out of its own memory): the page goes back.
    sys::fail_maps(1);
    assert!(!a.space.fault(&mut w.k, &mut w.pool, at, Access::Write));
    assert_eq!((w.pool.available(), a.space.usage().0), (APP_PAGES, 0));
    let caps = sys::caps();
    assert!(w.store(&mut a, at, 1));
    assert_eq!(sys::caps(), caps + 1, "one capability per mapped page");
    // Drain the pool: the next first touch fails, nothing else changes.
    let mut held = Vec::new();
    while let Some(p) = w.pool.take() {
        held.push(p);
    }
    assert!(!w.store(&mut a, at + 0x8000 + WINDOW, 1) || cfg!(page64));
    let big = w.mmap(&mut a, 0x10000, RW);
    assert!(!w.store(&mut a, big, 1));
    assert_eq!(w.fetch(&mut a, at), Some(1));
    for p in held {
        w.pool.give_back(p);
    }
    assert!(w.store(&mut a, big, 1));
    w.accounted(&[&a]);
    // Ending the app returns every page and every capability it had.
    let before = sys::caps();
    let owned = a.space.usage().0;
    w.end(a);
    assert_eq!(w.pool.available(), APP_PAGES);
    assert_eq!(sys::caps(), before - owned);
}

#[test]
fn running_out_of_capability_slots_fails_calls_and_never_stops_the_runtime() {
    let mut w = world();
    let mut a = w.app(900);
    let at = w.mmap(&mut a, 4 * WINDOW, RW);
    assert!(w.store(&mut a, at, 1));
    // Something (kernel objects past pages.rs OBJECT_SLOTS) takes every slot but the last.
    let last = sys::SLOTS - 1;
    let mut taken = std::collections::BTreeSet::new();
    loop {
        let s = w.k.slot();
        if s == last {
            break;
        }
        assert!(taken.insert(s) && s < last, "slot {s} handed out twice");
    }
    // The last slot serves one more mapping; the one after is refused like
    // any other failure to get memory, and nothing is lost.
    assert!(w.store(&mut a, at + WINDOW, 2));
    assert!(!w.store(&mut a, at + 2 * WINDOW, 3), "no slot left: the fault is refused");
    // ... as out of memory (mod.rs closes it so), not as a bad access.
    assert!(a.space.starved(at + 2 * WINDOW, Access::Write));
    assert!(!a.space.write(&mut w.k, &mut w.pool, at + 3 * WINDOW, &[1]));
    assert_eq!(w.fetch(&mut a, at + WINDOW), Some(2));
    w.accounted(&[&a]);
    // The page that used the last slot is unmapped: the slot serves the next.
    w.munmap(&mut a, at + WINDOW, WINDOW);
    assert!(w.store(&mut a, at + 2 * WINDOW, 3));
    // A returned slot is used before the last one, which is never queued twice.
    let some = *taken.iter().next().unwrap();
    w.k.release(some);
    w.k.release(last);
    w.k.release(last);
    assert_eq!(w.k.slot(), some);
    assert_eq!(w.k.slot(), last);
    w.accounted(&[&a]);
}

#[test]
fn a_screen_page_is_mapped_once_per_app_so_remapping_cannot_use_slots_up() {
    // The screen's pages are taken from the pool by the device and mapped
    // into the app not owned (device.rs `surface`, `map_surface`).
    let mut w = world();
    let mut a = w.app(900);
    let mut b = w.app(901);
    let screen: Vec<u32> = (0..16).map(|_| w.pool.take().unwrap()).collect();
    let first = w.mmap(&mut a, 16 * PAGE, RW);
    for (n, &p) in screen.iter().enumerate() {
        a.space.map(&mut w.k, &w.pool, first + n as u64 * PAGE, p, false).unwrap();
    }
    assert_eq!(a.space.usage().0, 0, "the screen's pages are not the app's");
    // The attack: map the screen again and again. Every attempt is refused
    // before a slot is spent.
    let caps = sys::caps();
    let again = w.mmap(&mut a, 16 * PAGE, RW);
    for _ in 0..1000 {
        for (n, &p) in screen.iter().enumerate() {
            assert_eq!(a.space.map(&mut w.k, &w.pool, again + n as u64 * PAGE, p, false), Err(syscall::ENOMEM));
        }
    }
    assert_eq!(sys::caps(), caps, "no capability slot spent on a refused mapping");
    // No other app can map them at all (frames.rs).
    let bt = w.mmap(&mut b, PAGE, RW);
    assert_eq!(b.space.map(&mut w.k, &w.pool, bt, screen[0], false), Err(syscall::EACCES));
    // Unmapped, a page can be mapped again (mmap, munmap, mmap).
    w.munmap(&mut a, first, 16 * PAGE);
    assert_eq!(sys::caps(), caps - 16, "its slots come back");
    a.space.map(&mut w.k, &w.pool, again, screen[0], false).unwrap();
    w.munmap(&mut a, again, 16 * PAGE);
    for p in screen {
        w.pool.give_back(p);
    }
    w.accounted(&[&a, &b]);
}

#[test]
fn the_shipped_cspaces_hold_every_page_one_mapping_of_each_and_the_objects() {
    // Each board's numbers as the runtime is built with them (pages.rs).
    for (board, (&bits, pool)) in pages::BOARD_CNODE_BITS.iter().zip(pages::BOARD_POOL_BITS).enumerate() {
        let need = sys::slots_needed(pages::pool_pages(pool), pages::HEAP_PAGES);
        assert!(need <= 1 << bits, "board {board}: {need} slots needed, 2^{bits} made");
    }
    // The supervisor makes the CSpace those sizes (OPEN_BITS: emulator, then handset).
    let c = include_str!("../../../system/supervisor/supervisor.c");
    let open_bits: Vec<u32> = c
        .match_indices("enum { OPEN_BITS = ")
        .map(|(at, m)| c[at + m.len()..].split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(c.contains("#ifdef VIOLET_QEMU\nenum { OPEN_BITS = "), "the emulator's OPEN_BITS comes first");
    assert_eq!(open_bits, pages::BOARD_CNODE_BITS, "supervisor.c OPEN_BITS and pages.rs CNODE_BITS disagree");
    // 4 KiB pages, unless built for the earlier 64 KiB setting.
    assert_eq!(sys::PAGE_BITS, if cfg!(page64) { 16 } else { 12 });
}

#[test]
fn a_family_of_forked_processes_shares_one_cap() {
    let mut w = world();
    let mut a = w.app(900);
    a.space.cap = 16;
    let at = w.mmap(&mut a, 0x200000, RW);
    let touch = |w: &mut World, x: &mut App, n: u64| w.store(x, at + n * WINDOW as u64, 1);
    for n in 0..6 {
        assert!(touch(&mut w, &mut a, n));
    }
    let mut c = w.app(901);
    a.space.fork_into(&mut c.space, &mut w.k, &mut w.pool).unwrap();
    assert_eq!(c.space.usage().0, 6);
    // The child's own new pages count with its parent's: 4 more in all.
    for n in 6..10 {
        assert!(touch(&mut w, &mut c, n), "page {n}");
    }
    assert!(c.space.capped() && a.space.capped(), "16 pages held between them");
    assert!(!touch(&mut w, &mut c, 10) && !touch(&mut w, &mut a, 10), "neither gets a 17th");
    assert!(c.space.starved(at + 10 * WINDOW, Access::Write));
    // A fork whose copy would pass the cap is refused, and costs nothing.
    let mut d = w.app(902);
    assert_eq!(a.space.fork_into(&mut d.space, &mut w.k, &mut w.pool), Err(syscall::ENOMEM));
    assert_eq!(d.space.usage().0, 0);
    w.end(d);
    // A program exec'd in the family counts too (syscall.rs execve).
    w.end(c);
    assert!(!a.space.capped());
    let mut e = w.app(903);
    e.space.cap = 16;
    let et = w.mmap(&mut e, 0x200000, RW);
    for n in 0..4 {
        assert!(w.store(&mut e, et + n * WINDOW, 1));
    }
    e.space.join(&a.space);
    for n in 6..12 {
        assert!(touch(&mut w, &mut a, n), "page {n}");
    }
    assert!(a.space.capped() && e.space.capped(), "6 + 6 + 4 pages");
    assert!(!w.store(&mut e, et + 4 * WINDOW, 1));
    w.end(e);
    assert!(!a.space.capped());
    w.accounted(&[&a]);
    w.end(a);
    assert_eq!(w.pool.available(), APP_PAGES);
}

#[test]
fn only_a_fault_the_region_allows_and_memory_could_not_serve_is_out_of_memory() {
    let mut w = world();
    let mut a = w.app(900);
    a.space.cap = 1;
    let at = w.mmap(&mut a, 4 * WINDOW, RW);
    let ro = w.mmap(&mut a, WINDOW, R);
    assert!(w.store(&mut a, at, 1));
    assert!(!w.store(&mut a, at + WINDOW, 1));
    assert!(a.space.starved(at + WINDOW, Access::Write), "at its cap: out of memory");
    assert!(!a.space.starved(at, Access::Write), "a page is there: not for want of memory");
    assert!(!a.space.starved(ro, Access::Write), "the region refuses writes: a bad access");
    assert!(!a.space.starved(ro, Access::Exec));
    assert!(!a.space.starved(0x1000, Access::Read), "no region: a bad access");
    w.accounted(&[&a]);
}

// ---- fork ----

#[test]
fn fork_copies_every_page_and_shares_none() {
    let mut w = world();
    let (mut a, _) = started(&mut w, 900, &program());
    a.space.cap = 500;
    let at = w.mmap(&mut a, 0x8000, RW);
    assert!(w.store(&mut a, at + 0x3000, 0x5a));
    a.space.protect(at + 0x7000, at + 0x8000, 0).unwrap();
    let mut b = w.app(901);
    a.space.fork_into(&mut b.space, &mut w.k, &mut w.pool).unwrap();
    assert_eq!(b.space.usage().0, a.space.usage().0);
    assert_eq!(b.space.cap, 500, "the child keeps the parent's cap");
    assert_eq!(w.fetch(&mut b, at + 0x3000), Some(0x5a));
    assert!(w.store(&mut b, at + 0x3000, 0x77));
    assert_eq!(w.fetch(&mut a, at + 0x3000), Some(0x5a), "the parent does not see the child's write");
    // The child's code runs, and its regions keep their rights.
    assert!(sys::touch(b.space.pd, 0x20d90, Access::Exec).is_some());
    assert!(sys::touch(b.space.pd, 0x20d90, Access::Write).is_none());
    #[cfg(not(page64))]
    assert!(!w.store(&mut b, at + 0x7000, 1), "the guard page is a guard in the child too");
    mappings_follow_regions(&a);
    mappings_follow_regions(&b);
    w.accounted(&[&a, &b]);
    w.end(b);
    w.accounted(&[&a]);
}

// ---- random operations ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32 as u64
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn random_maps_protects_unmaps_and_touches_keep_every_invariant() {
    for seed in 1..=6u64 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
        let mut w = world();
        let (mut a, _) = started(&mut w, 900, &program());
        let mut b = w.app(901);
        a.space.cap = 400;
        // A model of what the app stored: address -> byte.
        let mut model: std::collections::BTreeMap<u64, u8> = Default::default();
        let base = 0x6000_0000u64;
        let span = 96u64; // 4 KiB slots the test plays in
        for step in 0..1500 {
            let s = rng.below(span);
            let n = 1 + rng.below(12).min(span - s - 1).min(11);
            let (start, end) = (base + s * 0x1000, base + (s + n) * 0x1000);
            match rng.below(10) {
                0..=2 => {
                    let prot = [RW, RW, R, 0][rng.below(4) as usize];
                    if !a.space.refuses(start, end, prot) && !a.space.capped() {
                        a.space.remove(&mut w.k, &mut w.pool, start, end);
                        a.space.add(start, end, prot);
                        model.retain(|&va, _| va < start || va >= end);
                    }
                }
                3 => {
                    w.munmap(&mut a, start, end - start);
                    #[cfg(not(page64))]
                    model.retain(|&va, _| va < start || va >= end);
                    #[cfg(page64)]
                    {
                        // A 64 KiB page lives while any region is in it; what
                        // the hole held is gone once nothing covers its page.
                        let vmas = a.space.vmas.clone();
                        model.retain(|&va, _| vma::union(&vmas, va & !0xffff, (va & !0xffff) + 0x10000) != 0);
                    }
                }
                4 | 5 => {
                    let prot = [RW, R, 0][rng.below(3) as usize];
                    let _ = a.space.protect(start, end, prot);
                }
                6..=8 => {
                    let va = start + rng.below(0x1000);
                    let byte = (step & 0xff) as u8 | 1;
                    let allowed = a.space.find(va).is_some_and(|v| v.prot & PROT_WRITE != 0);
                    let stored = w.store(&mut a, va, byte);
                    if stored {
                        model.insert(va, byte);
                    }
                    #[cfg(not(page64))]
                    assert_eq!(stored, allowed && (!a.space.capped() || stored), "step {step}: store at {va:#x}");
                    let _ = allowed;
                }
                _ => {
                    // The other app works alongside and is never disturbed.
                    let va = 0x7000_0000 + rng.below(64) * 0x1000;
                    if b.space.find(va).is_none() {
                        b.space.add(va & !0xffff, (va & !0xffff) + 0x10000, RW);
                    }
                    assert!(w.store(&mut b, va, 0xb0));
                }
            }
            if step % 50 == 0 {
                mappings_follow_regions(&a);
                mappings_follow_regions(&b);
                w.accounted(&[&a, &b]);
            }
        }
        // Everything the app stored and still has a readable region for reads back.
        #[cfg(not(page64))]
        for (&va, &byte) in &model {
            match a.space.find(va).map(|v| v.prot) {
                Some(p) if p & 7 != 0 => assert_eq!(w.fetch(&mut a, va), Some(byte), "seed {seed}: {va:#x}"),
                Some(_) => assert_eq!(w.fetch(&mut a, va), None, "seed {seed}: no access at {va:#x}"),
                None => panic!("the model kept an unmapped address"),
            }
        }
        mappings_follow_regions(&a);
        w.accounted(&[&a, &b]);
        w.end(a);
        w.end(b);
        assert_eq!(w.pool.available(), APP_PAGES, "seed {seed}: pages lost");
        assert_eq!(w.pool.refused, 0);
    }
}

// ---- what it costs ----

/// What musl's pthread_create maps for a thread with the default sizes: a
/// 128 KiB stack over an 8 KiB guard, and TLS and the thread's own block
/// (about 4 KiB) at the top. A new thread touches the top of its stack.
fn thread_stack(w: &mut World, a: &mut App) {
    let at = w.mmap(a, 0x22000 + 0x1000, RW);
    a.space.protect(at, at + 0x2000, 0).unwrap();
    assert!(w.store(a, at + 0x22fff, 1));
    assert!(w.store(a, at + 0x21ff0, 1));
}

fn report(what: &str, pages: usize) {
    println!("measure[{} KiB pages] {:<44} {:>6} KiB", KIB, what, pages * KIB);
}

#[test]
fn measure_small_mappings_and_thread_stacks() {
    let mut w = world();
    let (mut a, _) = started(&mut w, 900, &program());
    let start = a.space.usage().0;
    report("the test program loaded, one thread", start);
    for _ in 0..16 {
        thread_stack(&mut w, &mut a);
    }
    let stacks = a.space.usage().0 - start;
    report("16 more threads (stack, guard, TLS touched)", stacks);
    let before = a.space.usage().0;
    for _ in 0..64 {
        let at = w.mmap(&mut a, 0x2000, RW);
        assert!(w.store(&mut a, at, 1) && w.store(&mut a, at + 0x1000, 1));
    }
    let small = a.space.usage().0 - before;
    report("64 mappings of 8 KiB, both pages written", small);
    let before = a.space.usage().0;
    // A 4 MiB arena touched every 256 KiB (an allocator's sparse first use).
    let at = w.mmap(&mut a, 4 << 20, RW);
    for n in 0..16 {
        assert!(w.store(&mut a, at + n * (256 << 10), 1));
    }
    let sparse = a.space.usage().0 - before;
    report("a 4 MiB arena touched in 16 places", sparse);
    #[cfg(not(page64))]
    {
        assert_eq!(stacks * KIB, 16 * 8, "two 4 KiB pages a thread");
        assert_eq!(small * KIB, 64 * 8);
        assert_eq!(sparse * KIB, 16 * 4);
    }
    #[cfg(page64)]
    {
        assert_eq!(stacks * KIB, 16 * 64);
        assert_eq!(small * KIB, 64 * 64);
        assert_eq!(sparse * KIB, 16 * 64);
    }
    w.accounted(&[&a]);
}

/// Real programs, if PAGING_ELF names some (':'-separated paths, as built
/// into build/sel4/qemu/initfs-build): what each holds once loaded, before
/// it has run an instruction. Without the variable this measures nothing.
#[test]
fn measure_real_programs_at_load() {
    let Ok(list) = std::env::var("PAGING_ELF") else { return };
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let Ok(image) = std::fs::read(path) else {
            println!("measure: cannot read {path}");
            continue;
        };
        let mut w = world();
        let mut a = w.app(900);
        let args = [String::from(path)];
        match load(&mut a.space, &mut w.k, &mut w.pool, &image, None, &args, &[], [7; 16], Layout::fixed()) {
            Ok(_) => {
                let name = path.rsplit('/').next().unwrap_or(path);
                report(&format!("{name} loaded ({} KiB file)", image.len() >> 10), a.space.usage().0);
                mappings_follow_regions(&a);
                w.accounted(&[&a]);
            }
            Err(e) => println!("measure: {path}: {e}"),
        }
        // (procmgr's frame ledger is this thread's: let go before the next world.)
        w.end(a);
        assert_eq!(w.pool.available(), w.pool.app_pages);
    }
}
