//! Process and memory manager for open apps, used by the Open Runtime.
//!
//! Each app has a page directory, regions (VMAs) that say which addresses are
//! valid and how they may be used, and pages mapped on first touch from the
//! pool. Pages are sys::PAGE (4 KiB, the unit regions are made of, so every
//! page has exactly its region's rights: a guard page or an mprotect of one
//! page is enforced by the hardware, and a mapping costs the pages it
//! touches, not 64 KiB each). The hardware mapping of a page follows its
//! regions: read-only, read-write or read-execute, never writable and
//! executable, and Execute-Never unless it holds code (the rules are in
//! ../../runtime/open/vma.rs; they keep code and anything else at least a
//! 64 KiB WINDOW apart, which is stricter than the hardware now needs).
//! A page that holds code holds nothing else, and only the loader fills it.
//! One app holds at most `Space::cap` pages of the pool.
//! The runtime's copies to and from app memory check the same regions.
//! The trampoline page belongs to the runtime: the app cannot unmap, replace
//! or change it, so its pool page never returns to the pool while the app lives.
//! The ELF loader handles static executables (ET_EXEC), static PIE (ET_DYN
//! without an interpreter) and programs linked against musl's dynamic loader
//! (PT_INTERP naming /lib/ld-musl-armhf.so.1 only, which is then loaded too,
//! at a random address in the mmap area, under the same W^X rules), places
//! them by a random layout, and builds the Linux initial stack with its
//! auxiliary vector. The header checks are in ../../runtime/open/elf.rs.
//! The trampoline page also holds the signal gate (../../runtime/open/signal.rs).
//! `fork_into` copies an address space for fork(2).
#[path = "../../runtime/open/elf.rs"]
pub mod elf;
// Which address space holds each pool page: asked before every mapping.
#[path = "frames.rs"]
pub mod frames;
use super::pool::{page_va, Kernel, Pool};
use super::signal::{GATE, GATE_OFFSET};
use super::sys::{self, Object, PAGE, PAGE_BITS};
use super::syscall::{EACCES, EEXIST, ENOMEM};
use super::vma::{self, HW_NONE, HW_RW, HW_RX};
#[allow(unused_imports)]
pub use super::vma::{Access, Layout, Va, Vma, MMAP_BASE, MMAP_TOP, PROT_EXEC, PROT_READ, PROT_WRITE, STACK_SIZE, USER_TOP};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

struct Page {
    pool: u32,
    cap: u32,
    /// Pages a device owns (the screen) go back to it, not to the pool.
    owned: bool,
    /// How the app's mapping is set up now (vma::HW_*).
    hw: u8,
}

/// Addresses are whole (vma::Va, 64 bits for either ABI); pages are keyed by
/// page number (address >> PAGE_BITS), page tables by level and index.
pub struct Space {
    pub pd: u32,
    pub untyped: u32,
    tables: BTreeMap<u64, u32>,
    pages: BTreeMap<u64, Page>,
    pub vmas: Vec<Vma>,
    pub brk_start: Va,
    pub brk: Va,
    /// Where this program's parts are (random per start; set by `load`),
    /// and the bounds of its ABI's address space.
    pub layout: Layout,
    /// The last fault that changed nothing, and how often it repeated.
    spin: (u64, u32),
    /// Capability slots holding this app's kernel objects (freed by revoking `untyped`).
    pub object_slots: Vec<u32>,
    /// Pool pages it owns now, and the most it may (the runtime sets it from
    /// the pool's size; fork hands it on): at the cap it gets no new page.
    owned: u32,
    pub cap: u32,
    /// Pool pages its whole family owns: this space, the children it forked
    /// and the programs they exec'd share one count, and `cap` bounds that,
    /// so forking cannot multiply one app's share.
    family: Rc<Cell<u32>>,
}

/// The 64 KiB unit of app address-space layout: mmap placement, the
/// trampoline region, and the windows the W^X policy keeps code alone in
/// (vma.rs). It was also the hardware page; pages are sys::PAGE now.
pub const WINDOW: Va = 0x1_0000;

pub struct Loaded {
    pub entry: Va,
    pub sp: Va,
}

/// Pages a fault brings in when first touches come in a row (4 KiB pages
/// only: one 64 KiB page was already sixteen of them), and the free pool
/// (8 MiB) below which a fault brings in just its own page.
const AHEAD: u64 = if PAGE_BITS == 12 { 4 } else { 1 };
const AHEAD_FLOOR: usize = (8 << 20) / PAGE;

/// A fault that leaves the mapping as it was may be a stale TLB entry or a
/// second thread arriving at a page the first just got. This many in a row
/// at one place is an access the hardware keeps refusing: the app is stopped.
const SPIN_LIMIT: u32 = 1024;

/// The frame ledger (frames.rs). The runtime's is empty until boot says
/// which pages are the apps' (`ledger().limit`): until then nothing maps.
#[cfg(not(test))]
pub fn ledger() -> &'static mut frames::Frames {
    static mut LEDGER: frames::Frames = frames::Frames::new(0, 0);
    // One process, one thread, and no caller keeps the reference.
    unsafe { &mut *core::ptr::addr_of_mut!(LEDGER) }
}
/// Host tests run in parallel threads, each with its own stand-in pool: a
/// ledger for each, open to every page unless the test limits it.
#[cfg(test)]
pub fn ledger() -> &'static mut frames::Frames {
    std::thread_local!(static LEDGER: core::cell::Cell<*mut frames::Frames> = const { core::cell::Cell::new(core::ptr::null_mut()) });
    LEDGER.with(|l| {
        if l.get().is_null() {
            l.set(alloc::boxed::Box::leak(alloc::boxed::Box::new(frames::Frames::new(0, u32::MAX))));
        }
        unsafe { &mut *l.get() }
    })
}

/// A page from the pool that no address space holds (frames.rs). One the
/// pool hands out while a space still holds it is passed over, before
/// anything here writes to it: it stays out of the pool until its holder
/// lets go and gives it back. (The pool itself zeroes a page as it hands it
/// out, so such a page is zeroed under its holder; `give` is what keeps a
/// held page off the free list in the first place.)
fn fresh(pool: &mut Pool) -> Option<u32> {
    loop {
        let index = pool.take()?;
        if ledger().owner(index) == 0 {
            return Some(index);
        }
    }
}
/// Give a page back to the pool, unless an address space (this one, through
/// another mapping, or any other) still holds it: then it stays out.
fn give(pool: &mut Pool, index: u32) {
    if ledger().owner(index) == 0 {
        pool.give_back(index);
    }
}

fn page_of(va: Va) -> u64 {
    va >> PAGE_BITS
}
fn align_down(v: Va, a: Va) -> Va {
    v & !(a - 1)
}
fn align_up(v: Va, a: Va) -> Va {
    v.wrapping_add(a - 1) & !(a - 1)
}

/// The hardware mapping the regions in page `p` call for.
fn want(vmas: &[Vma], p: u64) -> u8 {
    let start = p << PAGE_BITS;
    vma::hardware(vma::union(vmas, start, start.saturating_add(PAGE as Va)))
}

/// Give the app's mapping of page `p` the rights `hw`: unmapped, read-only,
/// read-write or read-execute, and Execute-Never unless it is the last.
/// Mapping a page that is mapped changes its rights in place. If that fails
/// the page ends up unmapped (`page.hw` says so), never with the old rights.
fn set(pd: u32, p: u64, page: &mut Page, hw: u8) {
    if hw != HW_NONE {
        let va = p << PAGE_BITS;
        let (w, x) = (hw == HW_RW, hw == HW_RX);
        if sys::page_map(page.cap, pd, va, w, x).is_ok() {
            page.hw = hw;
            if x {
                sys::page_unify(page.cap);
            }
            return;
        }
    }
    if page.hw != HW_NONE || hw != HW_NONE {
        sys::page_unmap(page.cap);
    }
    page.hw = HW_NONE;
}

impl Space {
    pub fn new(k: &mut Kernel, untyped: u32) -> Result<Space, i32> {
        let pd = k.object(untyped, Object::PageDirectory, 0)?;
        sys::asid_assign(pd)?;
        Ok(Space {
            pd,
            untyped,
            tables: BTreeMap::new(),
            pages: BTreeMap::new(),
            vmas: Vec::new(),
            brk_start: 0,
            brk: 0,
            layout: Layout::native(),
            spin: (0, 0),
            object_slots: alloc::vec![pd],
            owned: 0,
            cap: u32::MAX,
            family: Rc::new(Cell::new(0)),
        })
    }

    pub fn object(&mut self, k: &mut Kernel, o: Object, bits: u32) -> Result<u32, i32> {
        let slot = k.object(self.untyped, o, bits)?;
        self.object_slots.push(slot);
        Ok(slot)
    }

    fn table(&mut self, k: &mut Kernel, va: Va) -> Result<(), i32> {
        if vma::WIDE {
            return self.tables64(k, va);
        }
        let mib = va >> 20;
        if self.tables.contains_key(&mib) {
            return Ok(());
        }
        let pt = self.object(k, Object::PageTable, 0)?;
        sys::pt_map(pt, self.pd, mib << 20)?;
        self.tables.insert(mib, pt);
        Ok(())
    }
    /// AArch64 (4 KiB granule, 48-bit address space): below the root, one
    /// table per 512 GiB, one per GiB and one per 2 MiB. Made top down, each
    /// lands at the level the kernel's walk stops at. Keys: the level in the
    /// top bits (3, 2, 1), then the index of the range the table covers.
    fn tables64(&mut self, k: &mut Kernel, va: Va) -> Result<(), i32> {
        for key in [3 << 60 | va >> 39, 2 << 60 | va >> 30, 1 << 60 | va >> 21] {
            if !self.tables.contains_key(&key) {
                let pt = self.object(k, Object::PageTable, 0)?;
                sys::pt_map(pt, self.pd, va)?;
                self.tables.insert(key, pt);
            }
        }
        Ok(())
    }

    /// Map pool page `index` at the page holding `va`, with the rights
    /// the regions there call for (none: the page is kept but not mapped).
    /// Checked here whatever the caller passed: `va` is an app address, and
    /// `index` is one of the apps' pages that no other address space holds
    /// (frames.rs), so no page is ever mapped in two apps.
    pub fn map(&mut self, k: &mut Kernel, pool: &Pool, va: Va, index: u32, owned: bool) -> Result<(), i32> {
        // One mapping per page per space: a page this space holds is not
        // mapped again (the screen's pages are the only ones anyone asks
        // twice for). With the ledger's one space per page, the slots all
        // mappings take are at most the apps' pages (pool.rs).
        if ledger().owner(index) == self.pd {
            return Err(ENOMEM);
        }
        if va >= self.layout.user_top || ledger().claim(self.pd, index).is_err() {
            return Err(EACCES);
        }
        let mapped = self.map_claimed(k, pool, va, index, owned);
        if mapped.is_err() {
            ledger().release(self.pd, index);
        }
        mapped
    }
    fn map_claimed(&mut self, k: &mut Kernel, pool: &Pool, va: Va, index: u32, owned: bool) -> Result<(), i32> {
        let p = page_of(va);
        if self.pages.contains_key(&p) {
            return Err(EEXIST);
        }
        self.table(k, va)?;
        let cap = k.slot();
        if let Err(e) = sys::copy(cap, pool.cap(index)) {
            k.release(cap);
            return Err(e);
        }
        let mut page = Page { pool: index, cap, owned, hw: HW_NONE };
        let hw = want(&self.vmas, p);
        set(self.pd, p, &mut page, hw);
        if page.hw != hw {
            sys::delete(cap);
            k.release(cap);
            return Err(ENOMEM);
        }
        self.pages.insert(p, page);
        self.owned += owned as u32;
        self.family.set(self.family.get() + owned as u32);
        Ok(())
    }

    fn unmap_page(&mut self, k: &mut Kernel, pool: &mut Pool, p: u64) {
        if let Some(page) = self.pages.remove(&p) {
            sys::delete(page.cap);
            ledger().release(self.pd, page.pool);
            k.release(page.cap);
            if page.owned {
                self.owned -= 1;
                self.family.set(self.family.get() - 1);
                give(pool, page.pool);
            }
        }
    }

    /// Bring the mappings of the pages in [start, end) in line with the regions.
    fn sync(&mut self, start: Va, end: Va) {
        if start >= end {
            return;
        }
        let (pd, vmas) = (self.pd, &self.vmas);
        for (&p, page) in self.pages.range_mut(page_of(start)..=page_of(end - 1)) {
            let hw = want(vmas, p);
            if page.hw != hw {
                set(pd, p, page, hw);
            }
        }
    }

    /// Pool pages owned by this space, and page tables made.
    pub fn usage(&self) -> (usize, usize) {
        (self.owned as usize, self.tables.len())
    }
    /// At its cap (its family's pages, see `family`): it gets no new page
    /// (mmap and brk answer ENOMEM, a touch of memory it has no page for
    /// ends it as out of memory).
    pub fn capped(&self) -> bool {
        self.family.get() >= self.cap
    }
    /// A fault at `va` that failed for want of memory, not of rights: a
    /// region there allows `access` and no page is mapped (the pool, the
    /// cap, kernel memory or capability slots ran out). The runtime closes
    /// the app as out of memory, not as a bad access.
    /// (mod.rs asks `holds` after checking the region itself.)
    pub fn starved(&self, va: Va, access: Access) -> bool {
        self.find(va).is_some_and(|v| access.allowed(v.prot)) && !self.holds(va)
    }
    /// Whether a page is mapped at `va`.
    pub fn holds(&self, va: Va) -> bool {
        self.pages.contains_key(&page_of(va))
    }
    /// execve: this new space, loaded for a program `old`'s process now
    /// runs, counts against `old`'s family from here on.
    pub fn join(&mut self, old: &Space) {
        old.family.set(old.family.get() + self.owned);
        self.family = old.family.clone();
    }

    pub fn find(&self, va: Va) -> Option<&Vma> {
        vma::find(&self.vmas, va)
    }

    /// The runtime's own view of the page holding `va`, taking a zeroed page
    /// from the pool if there is none yet. No protection check: callers check.
    fn page(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va) -> Option<usize> {
        let p = page_of(va);
        if !self.pages.contains_key(&p) {
            if self.capped() {
                return None;
            }
            let index = fresh(pool)?;
            if self.map(k, pool, va, index, true).is_err() {
                give(pool, index);
                return None;
            }
            self.spin.1 = 0;
        }
        let page = self.pages.get(&p)?;
        Some(page_va(page.pool) + (va as usize & (PAGE - 1)))
    }

    /// Whether [start, end) touches the trampoline region (one WINDOW),
    /// which only the runtime may map, unmap or change (munmap, mmap and
    /// mprotect refuse).
    pub fn pinned(&self, start: Va, end: Va) -> bool {
        let stubs = self.layout.stubs;
        start < end && start < stubs.saturating_add(WINDOW) && end > stubs
    }

    /// Write thread `slot`'s trampoline (`ldr pc, [pc, #-4]`, then `target`)
    /// and return its address in the app. The page is looked up on every
    /// call, and only the runtime's whole read-execute region will do: None
    /// (nothing written) if it is missing or altered, or memory ran out.
    pub fn stub(&mut self, k: &mut Kernel, pool: &mut Pool, slot: usize, target: u32) -> Option<Va> {
        let stubs = self.layout.stubs;
        let whole = Vma { start: stubs, end: stubs.checked_add(WINDOW)?, prot: PROT_READ | PROT_EXEC };
        if slot >= (GATE_OFFSET / 8) as usize || self.find(stubs) != Some(&whole) {
            return None;
        }
        // The region is WINDOW long; only the pages in use have memory: the
        // one with this trampoline (8 bytes, 8-aligned: never across pages)
        // and the last, with the gate.
        let at = self.page(k, pool, stubs + 8 * slot as Va)?;
        let gate = self.page(k, pool, stubs + Va::from(GATE_OFFSET))?;
        unsafe {
            (at as *mut u32).write_volatile(0xe51f_f004);
            ((at + 4) as *mut u32).write_volatile(target);
            // The signal gate, after the trampolines (rewritten with each: it costs little).
            for (n, word) in GATE.iter().enumerate() {
                ((gate + 4 * n) as *mut u32).write_volatile(*word);
            }
        }
        self.sync_code(stubs, whole.end);
        Some(stubs + 8 * slot as Va)
    }

    /// Where the signal gate is in the app.
    pub fn gate(&self) -> Va {
        self.layout.stubs + Va::from(GATE_OFFSET)
    }

    /// fork(2): give `child` (a new, empty space) a copy of this one: its
    /// regions, layout and break, and a copy of every page it owns, the
    /// trampoline page included. Pages a device owns (the screen surface)
    /// are not copied and their regions are dropped. Nothing is shared, so
    /// nothing needs copy-on-write. On error the caller destroys `child`.
    pub fn fork_into(&self, child: &mut Space, k: &mut Kernel, pool: &mut Pool) -> Result<(), i32> {
        // The child's pages count with this family's: fork fails (ENOMEM)
        // where the copy would take the family past its cap.
        if self.family.get() + self.owned > self.cap {
            return Err(ENOMEM);
        }
        child.family = self.family.clone();
        child.layout = self.layout;
        child.cap = self.cap;
        child.brk_start = self.brk_start;
        child.brk = self.brk;
        child.vmas = self.vmas.clone();
        for (&p, _) in self.pages.iter().filter(|(_, page)| !page.owned) {
            vma::carve(&mut child.vmas, p << PAGE_BITS, (p << PAGE_BITS).saturating_add(PAGE as Va));
        }
        for (&p, page) in self.pages.iter().filter(|(_, page)| page.owned) {
            // Checked before the copy: the parent's bytes never land in a page another space holds.
            let index = fresh(pool).ok_or(ENOMEM)?;
            unsafe { core::ptr::copy_nonoverlapping(page_va(page.pool) as *const u8, page_va(index) as *mut u8, PAGE) };
            if let Err(e) = child.map(k, pool, p << PAGE_BITS, index, true) {
                give(pool, index);
                return Err(e);
            }
        }
        for v in self.vmas.iter().filter(|v| v.prot & PROT_EXEC != 0) {
            child.sync_code(v.start, v.end);
        }
        Ok(())
    }

    /// The runtime's view of an app address inside a region, for the
    /// runtime's own writes (the loader's bss). Not for app-given addresses.
    pub fn resolve(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va) -> Option<usize> {
        self.find(va)?;
        self.page(k, pool, va)
    }

    /// Resolve a page fault: true if the region at `va` allows `access` and
    /// the page is now mapped to serve it. False stops the app: an access the
    /// region does not allow (a write to read-only memory, running data), no
    /// region, or no memory left.
    pub fn fault(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, access: Access) -> bool {
        let Some(end) = self.find(va).filter(|v| access.allowed(v.prot)).map(|v| v.end) else { return false };
        let p = page_of(va);
        if !self.pages.contains_key(&p) {
            if self.page(k, pool, va).is_none() {
                return false;
            }
            // First touches in a row (the page below is there already): the
            // next few pages of the region come with this fault, so filling
            // a buffer costs a fault every AHEAD pages, not one a page. A
            // touch on its own gets its one page; none of this when memory
            // is short.
            if AHEAD > 1 && self.pages.contains_key(&p.wrapping_sub(1)) {
                for q in p + 1..p + AHEAD {
                    let at = q << PAGE_BITS;
                    if at >= end || pool.available() < AHEAD_FLOOR || self.pages.contains_key(&q) || self.page(k, pool, at).is_none() {
                        break;
                    }
                }
            }
            return true;
        }
        let hw = want(&self.vmas, p);
        let Some(page) = self.pages.get_mut(&p) else { return false };
        let at = p << 2 | access as u64;
        if page.hw != hw {
            self.spin = (at, 0);
        } else if self.spin.0 != at {
            self.spin = (at, 1);
        } else {
            self.spin.1 += 1;
            if self.spin.1 > SPIN_LIMIT {
                return false;
            }
        }
        // Mapping again also drops a stale TLB entry.
        set(self.pd, p, page, hw);
        access.served(page.hw)
    }

    fn copy(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: Va, buf: *mut u8, len: usize, to_app: bool) -> bool {
        let mut done = 0;
        while done < len {
            let Some(at) = self.page(k, pool, va) else { return false };
            let n = (PAGE - (va as usize & (PAGE - 1))).min(len - done);
            unsafe {
                if to_app {
                    core::ptr::copy_nonoverlapping(buf.add(done) as *const u8, at as *mut u8, n);
                } else {
                    core::ptr::copy_nonoverlapping(at as *const u8, buf.add(done), n);
                }
            }
            done += n;
            va = va.wrapping_add(n as Va);
        }
        true
    }
    /// Copy from app memory for a system call. All of it must be readable by
    /// the app (in regions with some access), or nothing is copied.
    pub fn read(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, out: &mut [u8]) -> bool {
        vma::permits(&self.vmas, va, out.len(), 7) && self.copy(k, pool, va, out.as_mut_ptr(), out.len(), false)
    }
    /// Copy to app memory for a system call. All of it must be writable by
    /// the app, or nothing is written: a system call cannot change memory the
    /// app itself may not (its code, read-only data, memory outside regions).
    pub fn write(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, data: &[u8]) -> bool {
        vma::permits(&self.vmas, va, data.len(), PROT_WRITE)
            && self.copy(k, pool, va, data.as_ptr() as *mut u8, data.len(), true)
    }
    /// Whether a system call may write `len` bytes at `va` (as `write` checks).
    pub fn writable(&self, va: Va, len: usize) -> bool {
        vma::permits(&self.vmas, va, len, PROT_WRITE)
    }
    /// The loader placing a program: written with the runtime's authority,
    /// whatever the regions' protection, code included.
    fn place(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, data: &[u8]) -> bool {
        vma::permits(&self.vmas, va, data.len(), vma::ANY)
            && self.copy(k, pool, va, data.as_ptr() as *mut u8, data.len(), true)
    }
    /// File contents for mmap: written into the mapping even if it is
    /// read-only or has no access, but never into a 64 KiB page that holds
    /// code (the hardware would run them) and only inside regions.
    pub fn poke(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, data: &[u8]) -> bool {
        let Some(end) = Va::try_from(data.len()).ok().and_then(|n| va.checked_add(n)) else { return false };
        !vma::near_code(&self.vmas, va, end) && self.place(k, pool, va, data)
    }
    pub fn read_u32(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(k, pool, va, &mut b).then(|| u32::from_le_bytes(b))
    }
    pub fn write_u32(&mut self, k: &mut Kernel, pool: &mut Pool, va: Va, v: u32) -> bool {
        self.write(k, pool, va, &v.to_le_bytes())
    }
    pub fn read_string(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: Va, limit: usize) -> Option<String> {
        let mut bytes = Vec::new();
        loop {
            let mut b = [0u8];
            if !self.read(k, pool, va, &mut b) || bytes.len() > limit {
                return None;
            }
            if b[0] == 0 {
                return String::from_utf8(bytes).ok();
            }
            bytes.push(b[0]);
            va = va.wrapping_add(1);
        }
    }

    /// Mark [start, end) valid with `prot`, replacing whatever was there;
    /// pages already there take the new rights. No policy check: `refuses`.
    pub fn add(&mut self, start: Va, end: Va, prot: u32) {
        vma::insert(&mut self.vmas, start, end, prot);
        self.sync(start, end);
    }
    /// The W^X policy: whether an app may not map [start, end) with `prot`
    /// (also anything over the trampoline page).
    pub fn refuses(&self, start: Va, end: Va, prot: u32) -> bool {
        self.pinned(start, end) || vma::refuses(&self.vmas, start, end, prot, true)
    }
    /// Remove a range and free pages no region covers any more. The
    /// trampoline page stays, whatever the range (callers refuse first).
    pub fn remove(&mut self, k: &mut Kernel, pool: &mut Pool, start: Va, end: Va) {
        if start >= end {
            return;
        }
        if self.pinned(start, end) {
            let stubs = self.layout.stubs;
            self.remove(k, pool, start, stubs);
            return self.remove(k, pool, stubs.saturating_add(WINDOW), end);
        }
        vma::carve(&mut self.vmas, start, end);
        let inside: Vec<u64> = self.pages.range(page_of(start)..=page_of(end - 1)).map(|(&p, _)| p).collect();
        for p in inside {
            let at = p << PAGE_BITS;
            if vma::union(&self.vmas, at, at.saturating_add(PAGE as Va)) == 0 {
                self.unmap_page(k, pool, p);
            }
        }
        self.sync(start, end);
    }
    /// mprotect: only addresses that are already valid change protection.
    pub fn protect(&mut self, start: Va, end: Va, prot: u32) -> Result<(), i32> {
        let mut pieces = Vec::new();
        for v in self.vmas.iter() {
            if v.end > start && v.start < end {
                pieces.push(Vma { start: v.start.max(start), end: v.end.min(end), prot });
            }
        }
        if pieces.is_empty() {
            return Err(ENOMEM);
        }
        if self.pinned(start, end) || vma::refuses(&self.vmas, start, end, prot, false) {
            return Err(EACCES);
        }
        for piece in pieces {
            self.add(piece.start, piece.end, piece.prot);
        }
        self.merge();
        Ok(())
    }
    /// A free range of `len` bytes for mmap, highest first, below the
    /// layout's mmap top.
    pub fn free_range(&self, len: Va) -> Option<Va> {
        // Mappings start on a 64 KiB WINDOW (the W^X policy's unit, vma.rs).
        elf::free_in(&self.vmas, self.layout.mmap_base, self.layout.mmap_top, len)
    }
    pub fn overlaps(&self, start: Va, end: Va) -> bool {
        vma::union(&self.vmas, start, end) != 0
    }
    /// Move the break, as brk(2) does. Returns the new break.
    pub fn set_brk(&mut self, k: &mut Kernel, pool: &mut Pool, want: Va) -> Va {
        if want < self.brk_start || want > self.layout.mmap_base {
            return self.brk;
        }
        let old_end = align_up(self.brk, 4096);
        let new_end = align_up(want, 4096);
        if new_end > old_end {
            if self.capped() || self.overlaps(old_end, new_end) || self.refuses(old_end, new_end, PROT_READ | PROT_WRITE) {
                return self.brk;
            }
            self.add(old_end, new_end, PROT_READ | PROT_WRITE);
            // Merge into one region so later page frees see it whole.
            self.merge();
        } else if new_end < old_end {
            self.remove(k, pool, new_end, old_end);
        }
        self.brk = want;
        want
    }
    fn merge(&mut self) {
        let mut out: Vec<Vma> = Vec::with_capacity(self.vmas.len());
        for v in self.vmas.drain(..) {
            if let Some(last) = out.last_mut() {
                if last.end == v.start && last.prot == v.prot {
                    last.end = v.end;
                    continue;
                }
            }
            out.push(v);
        }
        self.vmas = out;
    }

    /// Release every page. Kernel objects go when the caller revokes `untyped`.
    pub fn destroy(&mut self, k: &mut Kernel, pool: &mut Pool) {
        let pages: Vec<u64> = self.pages.keys().copied().collect();
        for p in pages {
            self.unmap_page(k, pool, p);
        }
        self.vmas.clear();
    }

    /// Instructions were written through the runtime's mapping: make them
    /// visible to the app's fetches from the pages it has mapped.
    pub fn sync_code(&self, start: Va, end: Va) {
        if start >= end {
            return;
        }
        for (_, page) in self.pages.range(page_of(start)..=page_of(end - 1)) {
            if page.hw != HW_NONE {
                sys::page_unify(page.cap);
            }
        }
    }
}

pub const AT_NULL: u32 = 0;
const AT_PHDR: u32 = 3;
const AT_PHENT: u32 = 4;
const AT_PHNUM: u32 = 5;
const AT_PAGESZ: u32 = 6;
const AT_BASE: u32 = 7;
const AT_FLAGS: u32 = 8;
const AT_ENTRY: u32 = 9;
const AT_UID: u32 = 11;
const AT_EUID: u32 = 12;
const AT_GID: u32 = 13;
const AT_EGID: u32 = 14;
const AT_PLATFORM: u32 = 15;
const AT_HWCAP: u32 = 16;
const AT_CLKTCK: u32 = 17;
const AT_SECURE: u32 = 23;
const AT_RANDOM: u32 = 25;
const AT_HWCAP2: u32 = 26;
const AT_EXECFN: u32 = 31;
/// half, thumb, fast mult, VFP, EDSP, NEON, VFPv3, TLS, VFPv4, IDIVA, IDIVT, VFPD32;
/// on AArch64 (vma::WIDE) FP and ASIMD, all an ARMv8.0 core is sure to have.
const HWCAP: u32 = if vma::WIDE { 1 | 2 } else { 2 | 4 | 16 | 64 | 128 | 4096 | 8192 | 32768 | 65536 | 131072 | 262144 | 524288 };
/// AT_PLATFORM, and the bytes of one word of the initial stack.
const PLATFORM: &[u8] = if vma::WIDE { b"aarch64\0" } else { b"v7l\0" };
const WORD: Va = if vma::WIDE { 8 } else { 4 };

/// The dynamic loader a program needs, if it is dynamically linked: only
/// elf::LOADER is ever named (a program naming another is refused by `load`).
pub fn interp(image: &[u8]) -> Option<&'static str> {
    elf::plan(image).ok().filter(|p| p.interp).map(|_| elf::LOADER)
}

/// Place one ELF image's segments at `base` (all below `limit`). Each
/// segment gets the protection its header asks for (text read-execute,
/// constants read-only, data read-write); code sharing a 64 KiB page with
/// anything else is refused. Returns the end of the highest segment.
fn segments(space: &mut Space, k: &mut Kernel, pool: &mut Pool, image: &[u8], plan: &elf::Plan, base: Va, limit: Va, code: &mut Vec<(Va, Va)>) -> Result<Va, &'static str> {
    let mut end: Va = 0;
    for s in &plan.segs {
        // The regions by the W^X rules (elf.rs, shared with execve's dry run `elf::check`).
        let (va, seg_end, start, stop) = elf::region(&mut space.vmas, s, base, limit)?;
        space.sync(start, stop);
        // elf::plan checked that the file holds [offset, offset + filesz).
        // (On a 32-bit runtime the file is in memory, so its offsets fit usize.)
        let from = usize::try_from(s.offset).map_err(|_| "bad segment")?;
        let to = usize::try_from(s.offset + s.filesz).map_err(|_| "bad segment")?;
        let data = image.get(from..to).ok_or("bad segment")?;
        if !space.place(k, pool, va, data) {
            return Err("out of memory loading the program");
        }
        // The rest (bss) gets its pages when it is touched: pool pages come
        // zeroed, the tail of the last file page included.
        if s.flags & 1 != 0 {
            code.push((va, seg_end));
        }
        end = end.max(seg_end);
    }
    Ok(end)
}

/// Load an ELF executable at `layout` and build its stack. `random` fills
/// AT_RANDOM. A dynamically linked program needs `interp`, the contents of
/// its loader (elf::LOADER, from the system image; the caller looks it up):
/// the loader goes at a random address in the mmap area, the thread starts
/// at its entry, and AT_BASE tells it where it is. A program with a segment
/// both writable and executable, or with code sharing a 64 KiB page with any
/// other segment (the hardware runs whole pages), is refused, and so is a
/// loader that breaks the same rules. The stack is never executable.
pub fn load(
    space: &mut Space,
    k: &mut Kernel,
    pool: &mut Pool,
    image: &[u8],
    interp: Option<&[u8]>,
    argv: &[String],
    envp: &[String],
    random: [u8; 16],
    layout: Layout,
) -> Result<Loaded, &'static str> {
    let plan = elf::plan(image)?;
    let base = if plan.pie { layout.pie_base } else { 0 };
    space.layout = layout;
    let mut code: Vec<(Va, Va)> = Vec::new();
    let end = segments(space, k, pool, image, &plan, base, layout.mmap_base, &mut code)?;
    let main_entry = base.wrapping_add(plan.entry);
    let (mut entry, mut ld_base) = (main_entry, 0);
    if plan.interp {
        let ld_image = interp.ok_or(elf::NO_LOADER)?;
        let ld = elf::plan(ld_image)?;
        if !ld.loader_ok() {
            return Err(elf::NOT_LOADER);
        }
        let (lo, hi) = ld.span();
        let lo = align_down(lo, WINDOW);
        let at = space.free_range(hi - lo).ok_or("no room for the dynamic loader")?;
        ld_base = at.wrapping_sub(lo);
        segments(space, k, pool, ld_image, &ld, ld_base, layout.mmap_top, &mut code)?;
        entry = ld_base.wrapping_add(ld.entry);
    }
    if vma::mixed(&space.vmas) {
        return Err("code shares a 64 KiB page with data");
    }
    for (start, stop) in code {
        space.sync_code(start, stop);
    }
    let phdr_va = plan.phdr_at(base);
    let (phentsize, phnum) = (plan.phentsize, plan.phnum);
    // The heap starts a random distance past the program.
    let heap = align_up(end, 4096);
    space.brk_start = heap.checked_add(layout.brk_gap).filter(|&b| b <= layout.mmap_base).unwrap_or(heap);
    space.brk = space.brk_start;

    // The stack: strings at the top, then the vectors the C runtime reads.
    space.add(layout.stack_top - STACK_SIZE, layout.stack_top, PROT_READ | PROT_WRITE);
    let mut sp = layout.stack_top - 16 - layout.stack_pad;
    let mut push_bytes = |space: &mut Space, bytes: &[u8]| -> Result<Va, &'static str> {
        sp -= bytes.len() as Va;
        if !space.write(k, pool, sp, bytes) {
            return Err("out of memory building the stack");
        }
        Ok(sp)
    };
    let platform = push_bytes(space, PLATFORM)?;
    let rand = push_bytes(space, &random)?;
    let mut arg_ptrs = Vec::new();
    for a in argv {
        let mut s = a.clone().into_bytes();
        s.push(0);
        arg_ptrs.push(push_bytes(space, &s)?);
    }
    let mut env_ptrs = Vec::new();
    for e in envp {
        let mut s = e.clone().into_bytes();
        s.push(0);
        env_ptrs.push(push_bytes(space, &s)?);
    }
    let execfn = arg_ptrs.first().copied().unwrap_or(0);
    let aux: [(u32, Va); 19] = [
        (AT_PHDR, phdr_va),
        (AT_PHENT, phentsize.into()),
        (AT_PHNUM, phnum.into()),
        (AT_PAGESZ, 4096),
        (AT_BASE, ld_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, main_entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_PLATFORM, platform),
        (AT_HWCAP, HWCAP.into()),
        (AT_HWCAP2, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, rand),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ];
    let mut words: Vec<Va> = Vec::new();
    words.push(argv.len() as Va);
    words.extend(&arg_ptrs);
    words.push(0);
    words.extend(&env_ptrs);
    words.push(0);
    for (key, value) in aux {
        words.push(key.into());
        words.push(value);
    }
    // Words of the app's ABI: AArch64's are 8 bytes, AArch32's 4 (its
    // addresses are below 4 GiB, so the low word is the whole value).
    sp = (sp - words.len() as Va * WORD) & !15;
    let mut bytes = Vec::with_capacity(words.len() * WORD as usize);
    for w in &words {
        bytes.extend_from_slice(&w.to_le_bytes()[..WORD as usize]);
    }
    if !space.write(k, pool, sp, &bytes) {
        return Err("out of memory building the stack");
    }
    Ok(Loaded { entry, sp })
}
