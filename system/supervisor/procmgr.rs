//! Process and memory manager for open apps, used by the Open Runtime.
//!
//! Each app has a page directory, regions (VMAs) that say which addresses are
//! valid and how they may be used, and pages mapped on first touch from the
//! pool. Pages are 64 KiB; a region may start on any 4 KiB boundary, as Linux
//! allows. The hardware mapping of a page follows its regions: read-only,
//! read-write or read-execute, never writable and executable, and
//! Execute-Never unless it holds code (the rules are in ../../runtime/open/vma.rs).
//! A page that holds code holds nothing else, and only the loader fills it.
//! The runtime's copies to and from app memory check the same regions.
//! The trampoline page belongs to the runtime: the app cannot unmap, replace
//! or change it, so its pool page never returns to the pool while the app lives.
//! The ELF loader handles static executables (ET_EXEC) and static PIE (ET_DYN
//! without an interpreter), places them by a random layout, and builds the
//! Linux initial stack with its auxiliary vector.
use super::pool::{page_va, Kernel, Pool};
use super::sys::{self, Object, PAGE, PAGE_BITS};
use super::syscall::{EACCES, EEXIST, ENOMEM};
use super::vma::{self, HW_NONE, HW_RW, HW_RX};
#[allow(unused_imports)]
pub use super::vma::{Access, Layout, Vma, MMAP_BASE, MMAP_TOP, PROT_EXEC, PROT_READ, PROT_WRITE, STACK_SIZE, USER_TOP};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

struct Page {
    pool: u32,
    cap: u32,
    /// Pages a device owns (the screen) go back to it, not to the pool.
    owned: bool,
    /// How the app's mapping is set up now (vma::HW_*).
    hw: u8,
}

pub struct Space {
    pub pd: u32,
    pub untyped: u32,
    tables: BTreeMap<u32, u32>,
    pages: BTreeMap<u32, Page>,
    pub vmas: Vec<Vma>,
    pub brk_start: u32,
    pub brk: u32,
    /// Where this program's parts are (random per start; set by `load`).
    pub layout: Layout,
    /// The last fault that changed nothing, and how often it repeated.
    spin: (u32, u32),
    /// Capability slots holding this app's kernel objects (freed by revoking `untyped`).
    pub object_slots: Vec<u32>,
}

pub struct Loaded {
    pub entry: u32,
    pub sp: u32,
}

/// A fault that leaves the mapping as it was may be a stale TLB entry or a
/// second thread arriving at a page the first just got. This many in a row
/// at one place is an access the hardware keeps refusing: the app is stopped.
const SPIN_LIMIT: u32 = 1024;

fn page_of(va: u32) -> u32 {
    va >> PAGE_BITS
}
fn align_down(v: u32, a: u32) -> u32 {
    v & !(a - 1)
}
fn align_up(v: u32, a: u32) -> u32 {
    v.wrapping_add(a - 1) & !(a - 1)
}

/// The hardware mapping the regions in page `p` call for.
fn want(vmas: &[Vma], p: u32) -> u8 {
    let start = p << PAGE_BITS;
    vma::hardware(vma::union(vmas, start, start.saturating_add(PAGE as u32)))
}

/// Give the app's mapping of page `p` the rights `hw`: unmapped, read-only,
/// read-write or read-execute, and Execute-Never unless it is the last.
/// Mapping a page that is mapped changes its rights in place. If that fails
/// the page ends up unmapped (`page.hw` says so), never with the old rights.
fn set(pd: u32, p: u32, page: &mut Page, hw: u8) {
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
            layout: Layout::fixed(),
            spin: (0, 0),
            object_slots: alloc::vec![pd],
        })
    }

    pub fn object(&mut self, k: &mut Kernel, o: Object, bits: u32) -> Result<u32, i32> {
        let slot = k.object(self.untyped, o, bits)?;
        self.object_slots.push(slot);
        Ok(slot)
    }

    fn table(&mut self, k: &mut Kernel, va: u32) -> Result<(), i32> {
        let mib = va >> 20;
        if self.tables.contains_key(&mib) {
            return Ok(());
        }
        let pt = self.object(k, Object::PageTable, 0)?;
        sys::pt_map(pt, self.pd, mib << 20)?;
        self.tables.insert(mib, pt);
        Ok(())
    }

    /// Map pool page `index` at the 64 KiB page holding `va`, with the rights
    /// the regions there call for (none: the page is kept but not mapped).
    pub fn map(&mut self, k: &mut Kernel, pool: &Pool, va: u32, index: u32, owned: bool) -> Result<(), i32> {
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
        Ok(())
    }

    fn unmap_page(&mut self, k: &mut Kernel, pool: &mut Pool, p: u32) {
        if let Some(page) = self.pages.remove(&p) {
            sys::delete(page.cap);
            k.release(page.cap);
            if page.owned {
                pool.give_back(page.pool);
            }
        }
    }

    /// Bring the mappings of the pages in [start, end) in line with the regions.
    fn sync(&mut self, start: u32, end: u32) {
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

    /// Pool pages owned by this space (64 KiB each), and page tables made.
    pub fn usage(&self) -> (usize, usize) {
        (self.pages.values().filter(|page| page.owned).count(), self.tables.len())
    }

    pub fn find(&self, va: u32) -> Option<&Vma> {
        vma::find(&self.vmas, va)
    }

    /// The runtime's own view of the page holding `va`, taking a zeroed page
    /// from the pool if there is none yet. No protection check: callers check.
    fn page(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32) -> Option<usize> {
        let p = page_of(va);
        if !self.pages.contains_key(&p) {
            let index = pool.take()?;
            if self.map(k, pool, va, index, true).is_err() {
                pool.give_back(index);
                return None;
            }
            self.spin.1 = 0;
        }
        let page = self.pages.get(&p)?;
        Some(page_va(page.pool) + (va as usize & (PAGE - 1)))
    }

    /// Whether [start, end) touches the trampoline page, which only the
    /// runtime may map, unmap or change (munmap, mmap and mprotect refuse).
    pub fn pinned(&self, start: u32, end: u32) -> bool {
        let stubs = self.layout.stubs;
        start < end && start < stubs.saturating_add(PAGE as u32) && end > stubs
    }

    /// Write thread `slot`'s trampoline (`ldr pc, [pc, #-4]`, then `target`)
    /// and return its address in the app. The page is looked up on every
    /// call, and only the runtime's whole read-execute region will do: None
    /// (nothing written) if it is missing or altered, or memory ran out.
    pub fn stub(&mut self, k: &mut Kernel, pool: &mut Pool, slot: usize, target: u32) -> Option<u32> {
        let stubs = self.layout.stubs;
        let whole = Vma { start: stubs, end: stubs.checked_add(PAGE as u32)?, prot: PROT_READ | PROT_EXEC };
        if slot >= PAGE / 8 || self.find(stubs) != Some(&whole) {
            return None;
        }
        let at = self.page(k, pool, stubs)? + 8 * slot;
        unsafe {
            (at as *mut u32).write_volatile(0xe51f_f004);
            ((at + 4) as *mut u32).write_volatile(target);
        }
        self.sync_code(stubs, whole.end);
        Some(stubs + 8 * slot as u32)
    }

    /// The runtime's view of an app address inside a region, for the
    /// runtime's own writes (the loader's bss). Not for app-given addresses.
    pub fn resolve(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32) -> Option<usize> {
        self.find(va)?;
        self.page(k, pool, va)
    }

    /// Resolve a page fault: true if the region at `va` allows `access` and
    /// the page is now mapped to serve it. False stops the app: an access the
    /// region does not allow (a write to read-only memory, running data), no
    /// region, or no memory left.
    pub fn fault(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, access: Access) -> bool {
        if !self.find(va).is_some_and(|v| access.allowed(v.prot)) {
            return false;
        }
        let p = page_of(va);
        if !self.pages.contains_key(&p) {
            return self.page(k, pool, va).is_some();
        }
        let hw = want(&self.vmas, p);
        let Some(page) = self.pages.get_mut(&p) else { return false };
        let at = p << 2 | access as u32;
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

    fn copy(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: u32, buf: *mut u8, len: usize, to_app: bool) -> bool {
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
            va = va.wrapping_add(n as u32);
        }
        true
    }
    /// Copy from app memory for a system call. All of it must be readable by
    /// the app (in regions with some access), or nothing is copied.
    pub fn read(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, out: &mut [u8]) -> bool {
        vma::permits(&self.vmas, va, out.len(), 7) && self.copy(k, pool, va, out.as_mut_ptr(), out.len(), false)
    }
    /// Copy to app memory for a system call. All of it must be writable by
    /// the app, or nothing is written: a system call cannot change memory the
    /// app itself may not (its code, read-only data, memory outside regions).
    pub fn write(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, data: &[u8]) -> bool {
        vma::permits(&self.vmas, va, data.len(), PROT_WRITE)
            && self.copy(k, pool, va, data.as_ptr() as *mut u8, data.len(), true)
    }
    /// Whether a system call may write `len` bytes at `va` (as `write` checks).
    pub fn writable(&self, va: u32, len: usize) -> bool {
        vma::permits(&self.vmas, va, len, PROT_WRITE)
    }
    /// The loader placing a program: written with the runtime's authority,
    /// whatever the regions' protection, code included.
    fn place(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, data: &[u8]) -> bool {
        vma::permits(&self.vmas, va, data.len(), vma::ANY)
            && self.copy(k, pool, va, data.as_ptr() as *mut u8, data.len(), true)
    }
    /// File contents for mmap: written into the mapping even if it is
    /// read-only or has no access, but never into a 64 KiB page that holds
    /// code (the hardware would run them) and only inside regions.
    pub fn poke(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, data: &[u8]) -> bool {
        let Some(end) = u32::try_from(data.len()).ok().and_then(|n| va.checked_add(n)) else { return false };
        !vma::near_code(&self.vmas, va, end) && self.place(k, pool, va, data)
    }
    pub fn read_u32(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(k, pool, va, &mut b).then(|| u32::from_le_bytes(b))
    }
    pub fn write_u32(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32, v: u32) -> bool {
        self.write(k, pool, va, &v.to_le_bytes())
    }
    pub fn read_string(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: u32, limit: usize) -> Option<String> {
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
    pub fn add(&mut self, start: u32, end: u32, prot: u32) {
        vma::insert(&mut self.vmas, start, end, prot);
        self.sync(start, end);
    }
    /// The W^X policy: whether an app may not map [start, end) with `prot`
    /// (also anything over the trampoline page).
    pub fn refuses(&self, start: u32, end: u32, prot: u32) -> bool {
        self.pinned(start, end) || vma::refuses(&self.vmas, start, end, prot, true)
    }
    /// Remove a range and free pages no region covers any more. The
    /// trampoline page stays, whatever the range (callers refuse first).
    pub fn remove(&mut self, k: &mut Kernel, pool: &mut Pool, start: u32, end: u32) {
        if start >= end {
            return;
        }
        if self.pinned(start, end) {
            let stubs = self.layout.stubs;
            self.remove(k, pool, start, stubs);
            return self.remove(k, pool, stubs.saturating_add(PAGE as u32), end);
        }
        vma::carve(&mut self.vmas, start, end);
        let inside: Vec<u32> = self.pages.range(page_of(start)..=page_of(end - 1)).map(|(&p, _)| p).collect();
        for p in inside {
            let at = p << PAGE_BITS;
            if vma::union(&self.vmas, at, at.saturating_add(PAGE as u32)) == 0 {
                self.unmap_page(k, pool, p);
            }
        }
        self.sync(start, end);
    }
    /// mprotect: only addresses that are already valid change protection.
    pub fn protect(&mut self, start: u32, end: u32, prot: u32) -> Result<(), i32> {
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
    pub fn free_range(&self, len: u32) -> Option<u32> {
        // Mappings start on a 64 KiB page so unmapping one frees whole pages.
        let len = align_up(len, PAGE as u32);
        let mut top = self.layout.mmap_top;
        for v in self.vmas.iter().rev() {
            if v.start >= top {
                continue;
            }
            let floor = align_up(v.end.max(MMAP_BASE), PAGE as u32);
            if floor <= top && top - floor >= len {
                return Some(top - len);
            }
            top = top.min(align_down(v.start, PAGE as u32));
            if top <= MMAP_BASE {
                return None;
            }
        }
        (top >= MMAP_BASE + len).then(|| top - len)
    }
    pub fn overlaps(&self, start: u32, end: u32) -> bool {
        vma::union(&self.vmas, start, end) != 0
    }
    /// Move the break, as brk(2) does. Returns the new break.
    pub fn set_brk(&mut self, k: &mut Kernel, pool: &mut Pool, want: u32) -> u32 {
        if want < self.brk_start || want > MMAP_BASE {
            return self.brk;
        }
        let old_end = align_up(self.brk, 4096);
        let new_end = align_up(want, 4096);
        if new_end > old_end {
            if self.overlaps(old_end, new_end) || self.refuses(old_end, new_end, PROT_READ | PROT_WRITE) {
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
        let pages: Vec<u32> = self.pages.keys().copied().collect();
        for p in pages {
            self.unmap_page(k, pool, p);
        }
        self.vmas.clear();
    }

    /// Instructions were written through the runtime's mapping: make them
    /// visible to the app's fetches from the pages it has mapped.
    pub fn sync_code(&self, start: u32, end: u32) {
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

fn u16_at(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
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
/// half, thumb, fast mult, VFP, EDSP, NEON, VFPv3, TLS, VFPv4, IDIVA, IDIVT, VFPD32.
const HWCAP: u32 = 2 | 4 | 16 | 64 | 128 | 4096 | 8192 | 32768 | 65536 | 131072 | 262144 | 524288;

/// Load a static ELF executable at `layout` and build its stack. `random`
/// fills AT_RANDOM. Each segment gets the protection its header asks for
/// (text read-execute, constants read-only, data read-write); a program with
/// a segment both writable and executable, or with code sharing a 64 KiB page
/// with any other segment (the hardware runs whole pages), is refused. The
/// stack is never executable.
pub fn load(
    space: &mut Space,
    k: &mut Kernel,
    pool: &mut Pool,
    image: &[u8],
    argv: &[String],
    envp: &[String],
    random: [u8; 16],
    layout: Layout,
) -> Result<Loaded, &'static str> {
    if image.len() < 52 || &image[..4] != b"\x7fELF" || image[4] != 1 || image[5] != 1 {
        return Err("not a 32-bit little-endian ELF file");
    }
    if u16_at(image, 18) != 40 {
        return Err("not an ARM executable");
    }
    let kind = u16_at(image, 16);
    let (phoff, phentsize, phnum) = (u32_at(image, 28) as usize, u16_at(image, 42) as usize, u16_at(image, 44) as usize);
    let headers_end = phentsize.checked_mul(phnum).and_then(|n| n.checked_add(phoff));
    if phentsize < 32 || headers_end.map_or(true, |e| e > image.len()) {
        return Err("bad program headers");
    }
    let base = match kind {
        2 => 0,
        3 => layout.pie_base,
        _ => return Err("not an executable"),
    };
    space.layout = layout;
    let mut end = 0u32;
    let mut phdr_va = 0u32;
    let mut code: Vec<(u32, u32)> = Vec::new();
    for i in 0..phnum {
        let h = &image[phoff + i * phentsize..];
        let (ptype, offset, vaddr, filesz, memsz, flags) =
            (u32_at(h, 0), u32_at(h, 4), u32_at(h, 8), u32_at(h, 16), u32_at(h, 20), u32_at(h, 24));
        if ptype == 3 {
            return Err("dynamically linked: build it statically");
        }
        if ptype == 6 {
            phdr_va = base.wrapping_add(vaddr);
        }
        if ptype != 1 || memsz == 0 {
            continue;
        }
        let file_end = offset.checked_add(filesz).map(|e| e as usize).filter(|&e| e <= image.len());
        let Some(file_end) = file_end.filter(|_| filesz <= memsz) else { return Err("bad segment") };
        let va = base.checked_add(vaddr).ok_or("bad segment address")?;
        let seg_end = va.checked_add(memsz).filter(|&e| e <= MMAP_BASE).ok_or("segment too high")?;
        if flags & 3 == 3 {
            return Err("a segment is writable and executable");
        }
        let mut prot = PROT_READ;
        if flags & 2 != 0 { prot |= PROT_WRITE; }
        if flags & 1 != 0 { prot |= PROT_EXEC; }
        // Neighbouring data segments may share a 4 KiB page; it keeps both protections.
        let (start, stop) = (align_down(va, 4096), align_up(seg_end, 4096));
        let shared: Vec<Vma> = space.vmas.iter().filter(|v| v.end > start && v.start < stop).copied().collect();
        // Never code with anything else: the merged page would run both.
        if shared.iter().any(|v| (v.prot ^ prot) & PROT_EXEC != 0) {
            return Err("code shares a 64 KiB page with data");
        }
        space.add(start, stop, prot);
        for v in shared {
            space.add(v.start.max(start), v.end.min(stop), v.prot | prot);
        }
        if !space.place(k, pool, va, &image[offset as usize..file_end]) {
            return Err("out of memory loading the program");
        }
        // Touch the rest so the bss pages exist and are zero.
        let mut a = align_down(va + filesz, PAGE as u32);
        while a < seg_end {
            space.resolve(k, pool, a.max(va)).ok_or("out of memory")?;
            a += PAGE as u32;
        }
        if flags & 1 != 0 {
            code.push((va, seg_end));
        }
        if phdr_va == 0 && offset as usize <= phoff && phoff < file_end {
            phdr_va = va.wrapping_add(phoff as u32 - offset);
        }
        end = end.max(seg_end);
    }
    if end == 0 {
        return Err("no loadable segments");
    }
    if vma::mixed(&space.vmas) {
        return Err("code shares a 64 KiB page with data");
    }
    for (start, stop) in code {
        space.sync_code(start, stop);
    }
    // The heap starts a random distance past the program.
    let heap = align_up(end, 4096);
    space.brk_start = heap.checked_add(layout.brk_gap).filter(|&b| b <= MMAP_BASE).unwrap_or(heap);
    space.brk = space.brk_start;

    // The stack: strings at the top, then the vectors the C runtime reads.
    space.add(layout.stack_top - STACK_SIZE, layout.stack_top, PROT_READ | PROT_WRITE);
    let mut sp = layout.stack_top - 16 - layout.stack_pad;
    let mut push_bytes = |space: &mut Space, bytes: &[u8]| -> Result<u32, &'static str> {
        sp -= bytes.len() as u32;
        if !space.write(k, pool, sp, bytes) {
            return Err("out of memory building the stack");
        }
        Ok(sp)
    };
    let platform = push_bytes(space, b"v7l\0")?;
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
    let aux = [
        (AT_PHDR, phdr_va),
        (AT_PHENT, phentsize as u32),
        (AT_PHNUM, phnum as u32),
        (AT_PAGESZ, 4096),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, base.wrapping_add(u32_at(image, 24))),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_PLATFORM, platform),
        (AT_HWCAP, HWCAP),
        (AT_HWCAP2, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, rand),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ];
    let mut words: Vec<u32> = Vec::new();
    words.push(argv.len() as u32);
    words.extend(&arg_ptrs);
    words.push(0);
    words.extend(&env_ptrs);
    words.push(0);
    for (key, value) in aux {
        words.push(key);
        words.push(value);
    }
    sp = (sp - words.len() as u32 * 4) & !15;
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in &words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    if !space.write(k, pool, sp, &bytes) {
        return Err("out of memory building the stack");
    }
    Ok(Loaded { entry: base.wrapping_add(u32_at(image, 24)), sp })
}
