//! Process and memory manager for open apps, used by the Open Runtime.
//!
//! Each app has a page directory, regions (VMAs) that say which addresses are
//! valid, and pages mapped on first touch from the pool. Pages are 64 KiB; a
//! region may start on any 4 KiB boundary, as Linux allows. The ELF loader
//! handles static executables (ET_EXEC) and static PIE (ET_DYN without an
//! interpreter), and builds the Linux initial stack with its auxiliary vector.
use super::pool::{page_va, Kernel, Pool};
use super::sys::{self, Object, PAGE, PAGE_BITS};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

/// App address-space layout.
pub const MMAP_BASE: u32 = 0x4000_0000;
pub const MMAP_TOP: u32 = 0xb000_0000;
pub const STACK_TOP: u32 = 0xbf00_0000;
pub const STACK_SIZE: u32 = 8 << 20;
/// One 64 KiB page of ARM-mode trampolines: thread i starts at STUBS + 8 * i.
pub const STUBS: u32 = 0xbff0_0000;
/// Highest address an app may use; the kernel starts at 0xe0000000.
pub const USER_TOP: u32 = 0xc000_0000;
const PIE_BASE: u32 = 0x0040_0000;

#[derive(Clone, Copy)]
pub struct Vma {
    pub start: u32,
    pub end: u32,
    pub prot: u32,
}

struct Page {
    pool: u32,
    cap: u32,
    /// Pages a device owns (the screen) go back to it, not to the pool.
    owned: bool,
}

pub struct Space {
    pub pd: u32,
    pub untyped: u32,
    tables: BTreeMap<u32, u32>,
    pages: BTreeMap<u32, Page>,
    pub vmas: Vec<Vma>,
    pub brk_start: u32,
    pub brk: u32,
    /// Capability slots holding this app's kernel objects (freed by revoking `untyped`).
    pub object_slots: Vec<u32>,
}

pub struct Loaded {
    pub entry: u32,
    pub sp: u32,
}

fn page_of(va: u32) -> u32 {
    va >> PAGE_BITS
}
fn align_down(v: u32, a: u32) -> u32 {
    v & !(a - 1)
}
fn align_up(v: u32, a: u32) -> u32 {
    v.wrapping_add(a - 1) & !(a - 1)
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

    /// Map pool page `index` at the 64 KiB page holding `va`.
    pub fn map(&mut self, k: &mut Kernel, pool: &Pool, va: u32, index: u32, owned: bool) -> Result<(), i32> {
        let p = page_of(va);
        if self.pages.contains_key(&p) {
            return Err(super::syscall::EEXIST);
        }
        self.table(k, va)?;
        let cap = k.slot();
        if let Err(e) = sys::copy(cap, pool.cap(index)) {
            k.release(cap);
            return Err(e);
        }
        if let Err(e) = sys::page_map(cap, self.pd, p << PAGE_BITS, true) {
            sys::delete(cap);
            k.release(cap);
            return Err(e);
        }
        self.pages.insert(p, Page { pool: index, cap, owned });
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

    pub fn find(&self, va: u32) -> Option<&Vma> {
        self.vmas.iter().find(|v| v.start <= va && va < v.end)
    }

    /// The runtime's view of an app address, mapping a zeroed page on demand.
    pub fn resolve(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32) -> Option<usize> {
        let p = page_of(va);
        if !self.pages.contains_key(&p) {
            self.find(va).filter(|v| v.prot != 0)?;
            let index = pool.take()?;
            if self.map(k, pool, va, index, true).is_err() {
                pool.give_back(index);
                return None;
            }
        }
        let page = self.pages.get(&p)?;
        Some(page_va(page.pool) + (va as usize & (PAGE - 1)))
    }

    /// Resolve a page fault: true if the access was to a valid region.
    pub fn fault(&mut self, k: &mut Kernel, pool: &mut Pool, va: u32) -> bool {
        self.resolve(k, pool, va).is_some()
    }

    pub fn read(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: u32, out: &mut [u8]) -> bool {
        let mut done = 0;
        while done < out.len() {
            let Some(src) = self.resolve(k, pool, va) else { return false };
            let n = (PAGE - (va as usize & (PAGE - 1))).min(out.len() - done);
            unsafe { core::ptr::copy_nonoverlapping(src as *const u8, out[done..].as_mut_ptr(), n) };
            done += n;
            va = va.wrapping_add(n as u32);
        }
        true
    }
    pub fn write(&mut self, k: &mut Kernel, pool: &mut Pool, mut va: u32, data: &[u8]) -> bool {
        let mut done = 0;
        while done < data.len() {
            let Some(dst) = self.resolve(k, pool, va) else { return false };
            let n = (PAGE - (va as usize & (PAGE - 1))).min(data.len() - done);
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst as *mut u8, n) };
            done += n;
            va = va.wrapping_add(n as u32);
        }
        true
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
            va += 1;
        }
    }

    /// Mark [start, end) valid with `prot`, replacing whatever was there.
    pub fn add(&mut self, start: u32, end: u32, prot: u32) {
        self.carve(start, end);
        self.vmas.push(Vma { start, end, prot });
        self.vmas.sort_by_key(|v| v.start);
    }
    /// Remove [start, end) from the regions, splitting any that straddle it.
    fn carve(&mut self, start: u32, end: u32) {
        let mut out = Vec::with_capacity(self.vmas.len() + 1);
        for v in self.vmas.drain(..) {
            if v.end <= start || v.start >= end {
                out.push(v);
                continue;
            }
            if v.start < start {
                out.push(Vma { start: v.start, end: start, prot: v.prot });
            }
            if v.end > end {
                out.push(Vma { start: end, end: v.end, prot: v.prot });
            }
        }
        self.vmas = out;
    }
    /// Remove a range and free pages no region covers any more.
    pub fn remove(&mut self, k: &mut Kernel, pool: &mut Pool, start: u32, end: u32) {
        self.carve(start, end);
        let first = page_of(start);
        let last = page_of(end.wrapping_sub(1));
        for p in first..=last {
            let (ps, pe) = (p << PAGE_BITS, (p << PAGE_BITS).wrapping_add(PAGE as u32));
            let covered = self.vmas.iter().any(|v| v.start < pe && v.end > ps);
            if !covered {
                self.unmap_page(k, pool, p);
            }
        }
    }
    pub fn protect(&mut self, start: u32, end: u32, prot: u32) -> bool {
        // Only addresses that are already valid can change protection.
        let mut pieces = Vec::new();
        for v in self.vmas.iter() {
            if v.end > start && v.start < end {
                pieces.push(Vma { start: v.start.max(start), end: v.end.min(end), prot });
            }
        }
        if pieces.is_empty() {
            return false;
        }
        for piece in pieces {
            self.add(piece.start, piece.end, piece.prot);
        }
        true
    }
    /// A free range of `len` bytes for mmap, highest first, below MMAP_TOP.
    pub fn free_range(&self, len: u32) -> Option<u32> {
        // Mappings start on a 64 KiB page so unmapping one frees whole pages.
        let len = align_up(len, PAGE as u32);
        let mut top = MMAP_TOP;
        for v in self.vmas.iter().rev() {
            if v.start >= MMAP_TOP {
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
        self.vmas.iter().any(|v| v.start < end && v.end > start)
    }
    /// Move the break, as brk(2) does. Returns the new break.
    pub fn set_brk(&mut self, k: &mut Kernel, pool: &mut Pool, want: u32) -> u32 {
        if want < self.brk_start || want > MMAP_BASE {
            return self.brk;
        }
        let old_end = align_up(self.brk, 4096);
        let new_end = align_up(want, 4096);
        if new_end > old_end {
            if self.overlaps(old_end, new_end) {
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

    /// Write instructions an app will run: make them visible to its fetches.
    pub fn sync_code(&self, start: u32, end: u32) {
        for p in page_of(start)..=page_of(end.wrapping_sub(1)) {
            if let Some(page) = self.pages.get(&p) {
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

/// Load a static ELF executable and build its stack. `random` fills AT_RANDOM.
pub fn load(
    space: &mut Space,
    k: &mut Kernel,
    pool: &mut Pool,
    image: &[u8],
    argv: &[String],
    envp: &[String],
    random: [u8; 16],
) -> Result<Loaded, &'static str> {
    if image.len() < 52 || &image[..4] != b"\x7fELF" || image[4] != 1 || image[5] != 1 {
        return Err("not a 32-bit little-endian ELF file");
    }
    if u16_at(image, 18) != 40 {
        return Err("not an ARM executable");
    }
    let kind = u16_at(image, 16);
    let (phoff, phentsize, phnum) = (u32_at(image, 28) as usize, u16_at(image, 42) as usize, u16_at(image, 44) as usize);
    if phentsize < 32 || phoff + phentsize * phnum > image.len() {
        return Err("bad program headers");
    }
    let base = match kind {
        2 => 0,
        3 => PIE_BASE,
        _ => return Err("not an executable"),
    };
    let mut end = 0u32;
    let mut phdr_va = 0u32;
    for i in 0..phnum {
        let h = &image[phoff + i * phentsize..];
        let (ptype, offset, vaddr, filesz, memsz, flags) =
            (u32_at(h, 0), u32_at(h, 4), u32_at(h, 8), u32_at(h, 16), u32_at(h, 20), u32_at(h, 24));
        if ptype == 3 {
            return Err("dynamically linked: build it statically");
        }
        if ptype == 6 {
            phdr_va = base + vaddr;
        }
        if ptype != 1 || memsz == 0 {
            continue;
        }
        if filesz > memsz || offset as usize + filesz as usize > image.len() {
            return Err("bad segment");
        }
        let va = base.checked_add(vaddr).ok_or("bad segment address")?;
        let seg_end = va.checked_add(memsz).filter(|&e| e <= MMAP_BASE).ok_or("segment too high")?;
        let mut prot = 0;
        if flags & 4 != 0 { prot |= PROT_READ; }
        if flags & 2 != 0 { prot |= PROT_WRITE; }
        if flags & 1 != 0 { prot |= PROT_EXEC; }
        // Neighbouring segments may share a 4 KiB page; keep both valid.
        space.add(align_down(va, 4096), align_up(seg_end, 4096), prot | PROT_READ);
        if !space.write(k, pool, va, &image[offset as usize..(offset + filesz) as usize]) {
            return Err("out of memory loading the program");
        }
        // Touch the rest so the bss pages exist and are zero.
        let mut a = align_down(va + filesz, PAGE as u32);
        while a < seg_end {
            space.resolve(k, pool, a.max(va)).ok_or("out of memory")?;
            a += PAGE as u32;
        }
        if flags & 1 != 0 {
            space.sync_code(va, seg_end);
        }
        if phdr_va == 0 && offset as usize <= phoff && phoff < (offset + filesz) as usize {
            phdr_va = va + (phoff as u32 - offset);
        }
        end = end.max(seg_end);
    }
    if end == 0 {
        return Err("no loadable segments");
    }
    space.brk_start = align_up(end, 4096);
    space.brk = space.brk_start;

    // The stack: strings at the top, then the vectors the C runtime reads.
    space.add(STACK_TOP - STACK_SIZE, STACK_TOP, PROT_READ | PROT_WRITE);
    let mut sp = STACK_TOP - 16;
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
        (AT_ENTRY, base + u32_at(image, 24)),
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
    Ok(Loaded { entry: base + u32_at(image, 24), sp })
}
