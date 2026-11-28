//! One program's address space and its loading, for the 64-bit bootstrap
//! task (root.rs). Pure logic over the `Frames` trait, so the same code runs
//! on the host against a model of the kernel (tests/host.rs) and on the target.
//!
//! Containment between programs rests on two rules:
//! - Every capability a program's address space uses (its frames, their
//!   aliases here, every page table, its thread and capability space) comes
//!   from one arena, and `contained` gives the whole arena back (seL4_CNode_Revoke
//!   on the arena's untyped) on every way out of a program, so nothing one
//!   program did or failed to do is left for the next.
//! - Within a program, a page it unmaps keeps its frame for the next page it
//!   maps (zeroed first), so mapping and unmapping in a loop never uses more
//!   than `MAX_PAGES` frames.
use super::arch::{self, Elf, Kernel, MapError, Protection, Word, NATIVE};
use super::linux::{Memory, PAGE};

/// A program's page at `va` is also mapped in this task at `ALIAS + va`,
/// read-write, so system calls can copy to and from it. Programs live below `ALIAS`.
pub const ALIAS: u64 = 0x10_0000_0000;
/// Frames one program may hold at once.
pub const MAX_PAGES: usize = 512;

/// What the address space needs from the kernel besides the page-table walk.
/// Every capability handed out belongs to the current program's arena.
pub trait Frames: Kernel {
    /// A fresh, zeroed 4 KiB frame.
    fn frame(&mut self) -> Result<usize, MapError>;
    /// A second capability to `frame`, for its alias here.
    fn copy(&mut self, frame: usize) -> Result<usize, MapError>;
    /// Unmap this capability of a frame, wherever it is mapped.
    fn unmap(&mut self, frame: usize);
    /// Clean the data cache and invalidate the instruction cache for a mapped
    /// frame, so the program fetches what was written through its alias.
    fn unify(&mut self, frame: usize);
    /// This task's own address space.
    fn own(&self) -> usize;
    /// Copy to, from, or zero this task's own memory (alias addresses).
    fn poke(&mut self, va: u64, data: &[u8]);
    fn peek(&self, va: u64, out: &mut [u8]);
    fn zero(&mut self, va: u64, bytes: usize);
    /// Take back every capability of the arena, unmapping all of them.
    fn reset(&mut self);
}

/// Run one program's whole life with `kernel`, then take everything it was
/// given back, whatever `body` returned and from wherever it returned.
pub fn contained<K: Frames, R>(kernel: &mut K, body: impl FnOnce(&mut K) -> R) -> R {
    let result = body(kernel);
    kernel.reset();
    result
}

/// A recorded frame. `va == SPARE`: unmapped, kept for the next page.
#[derive(Clone, Copy)]
struct Page {
    va: u64,
    app: usize,
    alias: usize,
    protection: Protection,
}
const SPARE: u64 = u64::MAX;

pub struct Space<'k, K: Frames> {
    pub kernel: &'k mut K,
    vspace: usize,
    pages: [Page; MAX_PAGES],
    count: usize,
}

impl<'k, K: Frames> Space<'k, K> {
    pub fn new(kernel: &'k mut K, vspace: usize) -> Self {
        Space { kernel, vspace, pages: [Page { va: SPARE, app: 0, alias: 0, protection: Protection::ReadOnly }; MAX_PAGES], count: 0 }
    }
    /// Pages mapped now, and frames held (mapped or spare).
    pub fn usage(&self) -> (usize, usize) {
        (self.pages[..self.count].iter().filter(|p| p.va != SPARE).count(), self.count)
    }
    fn find(&self, va: u64) -> Option<usize> {
        let page = va & !(PAGE - 1);
        self.pages[..self.count].iter().position(|p| p.va == page && p.va != SPARE)
    }
    /// The bytes of `[va, va + len)` that lie in one page, at their alias here.
    fn page_of(&self, va: u64, len: usize, writable: bool) -> Option<(u64, usize)> {
        let page = &self.pages[self.find(va)?];
        if writable && page.protection != Protection::ReadWrite {
            return None;
        }
        let within = (va % PAGE) as usize;
        Some((ALIAS + va, len.min(PAGE as usize - within)))
    }
    /// Map a zeroed page at `va`. On any failure nothing new stays mapped,
    /// here or in the program, and a frame obtained is kept as a spare.
    pub fn map_one(&mut self, va: u64, protection: Protection) -> Result<(), MapError> {
        if va % PAGE != 0 || va >= ALIAS {
            return Err(MapError::Kernel(u32::MAX));
        }
        if self.find(va).is_some() {
            return Err(MapError::Occupied);
        }
        let index = match self.pages[..self.count].iter().position(|p| p.va == SPARE) {
            Some(index) => {
                let page = self.pages[index];
                self.map_alias(page.alias, va)?;
                self.kernel.zero(ALIAS + va, PAGE as usize);
                index
            }
            None => {
                if self.count == MAX_PAGES {
                    return Err(MapError::Exhausted);
                }
                let app = self.kernel.frame()?;
                let alias = self.kernel.copy(app)?;
                // Recorded before mapping, so a failure below leaves a spare, not a leak.
                self.pages[self.count] = Page { va: SPARE, app, alias, protection };
                self.count += 1;
                self.map_alias(alias, va)?;
                self.count - 1
            }
        };
        let page = self.pages[index];
        if let Err(error) = arch::map_frame(NATIVE, self.kernel, self.vspace, page.app, va, 12, protection, false).map(|_| ()) {
            self.kernel.unmap(page.alias);
            return Err(error);
        }
        if protection == Protection::ReadExecute {
            self.kernel.unify(page.app);
        }
        self.pages[index].va = va;
        self.pages[index].protection = protection;
        Ok(())
    }
    fn map_alias(&mut self, alias: usize, va: u64) -> Result<(), MapError> {
        let own = self.kernel.own();
        arch::map_frame(NATIVE, self.kernel, own, alias, ALIAS + va, 12, Protection::ReadWrite, false).map(|_| ())
    }
    /// Copy into the program regardless of its own protection (loading its image).
    pub fn load(&mut self, va: u64, data: &[u8]) -> bool {
        let mut done = 0;
        while done < data.len() {
            let Some(at) = va.checked_add(done as u64) else { return false };
            let Some((alias, n)) = self.page_of(at, data.len() - done, false) else { return false };
            self.kernel.poke(alias, &data[done..done + n]);
            done += n;
        }
        true
    }
    /// After writing code through the alias: make the program fetch it.
    fn unify_code(&mut self, va: u64) {
        if let Some(index) = self.find(va) {
            if self.pages[index].protection == Protection::ReadExecute {
                self.kernel.unify(self.pages[index].app);
            }
        }
    }
}

impl<K: Frames> Memory for Space<'_, K> {
    fn read(&self, va: u64, out: &mut [u8]) -> bool {
        let mut done = 0;
        while done < out.len() {
            let Some(at) = va.checked_add(done as u64) else { return false };
            let Some((alias, n)) = self.page_of(at, out.len() - done, false) else { return false };
            self.kernel.peek(alias, &mut out[done..done + n]);
            done += n;
        }
        true
    }
    fn write(&mut self, va: u64, data: &[u8]) -> bool {
        // All or nothing: check every page before touching one.
        let mut checked = 0;
        while checked < data.len() {
            let Some(at) = va.checked_add(checked as u64) else { return false };
            let Some((_, n)) = self.page_of(at, data.len() - checked, true) else { return false };
            checked += n;
        }
        self.load(va, data)
    }
    fn map(&mut self, va: u64, pages: u64, protection: Protection) -> bool {
        for i in 0..pages {
            let Some(at) = i.checked_mul(PAGE).and_then(|o| va.checked_add(o)) else {
                self.unmap(va, i);
                return false;
            };
            if self.map_one(at, protection).is_err() {
                self.unmap(va, i);
                return false;
            }
        }
        true
    }
    fn unmap(&mut self, va: u64, pages: u64) {
        for i in 0..pages {
            if let Some(index) = self.find(va.wrapping_add(i.wrapping_mul(PAGE))) {
                let page = self.pages[index];
                self.kernel.unmap(page.app);
                self.kernel.unmap(page.alias);
                self.pages[index].va = SPARE;
            }
        }
    }
    fn protect(&mut self, va: u64, pages: u64, protection: Protection) -> bool {
        if (0..pages).any(|i| self.find(va.wrapping_add(i.wrapping_mul(PAGE))).is_none()) {
            return false;
        }
        for i in 0..pages {
            let index = self.find(va.wrapping_add(i.wrapping_mul(PAGE))).unwrap();
            let page = self.pages[index];
            // The kernel rewrites the entry of a frame already mapped at this address.
            if self.kernel.map_frame(page.app, self.vspace, page.va, 12, protection, false).is_err() {
                return false;
            }
            self.pages[index].protection = protection;
            if protection == Protection::ReadExecute {
                self.kernel.unify(page.app);
            }
        }
        true
    }
}

/// Where a program's stack and anonymous mappings go, chosen at random per
/// program, and the 16 bytes AT_RANDOM points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub stack_top: u64,
    pub mmap_base: u64,
    pub mmap_limit: u64,
    pub random: [u8; 16],
}
pub const STACK_PAGES: u64 = 16;
/// The stack's top is a page in `(STACK_HIGH - STACK_SPREAD, STACK_HIGH]`.
pub const STACK_HIGH: u64 = 0x8000_0000;
pub const STACK_SPREAD: u64 = 0x0800_0000;
/// Anonymous mappings start at a page in `[MMAP_LOW, MMAP_LOW + MMAP_SPREAD)` and may use 4 GiB.
pub const MMAP_LOW: u64 = 0x1_0000_0000;
pub const MMAP_SPREAD: u64 = 0x4000_0000;
pub const MMAP_BYTES: u64 = 0x1_0000_0000;

impl Layout {
    /// From 32 random bytes: 8 for the stack, 8 for the mappings, 16 for AT_RANDOM.
    pub fn from_random(bytes: &[u8; 32]) -> Layout {
        let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let stack_top = STACK_HIGH - (word(0) % (STACK_SPREAD / PAGE)) * PAGE;
        let mmap_base = MMAP_LOW + (word(8) % (MMAP_SPREAD / PAGE)) * PAGE;
        Layout { stack_top, mmap_base, mmap_limit: mmap_base + MMAP_BYTES, random: bytes[16..].try_into().unwrap() }
    }
}

/// Where a loaded program starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    pub entry: u64,
    pub sp: u64,
    /// The first page after its segments: where its break starts.
    pub image_end: u64,
}

/// Map and fill `elf`'s segments, its stack and its initial stack contents.
pub fn load<K: Frames>(space: &mut Space<'_, K>, elf: &Elf, args: &[&[u8]], layout: &Layout) -> Result<Start, &'static str> {
    let mut image_end = 0;
    let mut headers = 0;
    let (phoff, phentsize, phnum) = elf.program_headers();
    // The whole table: a C library reads every entry through AT_PHDR.
    let table_end = phentsize.checked_mul(phnum).and_then(|n| n.checked_add(phoff)).ok_or("bad program headers")?;
    for segment in elf.segments() {
        let (first, last) = segment.page_span(PAGE).ok_or("bad segment")?;
        for page in (first..last).step_by(PAGE as usize) {
            space.map_one(page, segment.protection).map_err(|_| "cannot map the program")?;
            if let Some((offset, bytes)) = elf.page_bytes(&segment, page) {
                space.load(page + offset as u64, bytes);
            }
            space.unify_code(page);
        }
        image_end = image_end.max(last);
        let file_end = segment.offset.checked_add(segment.file_bytes).ok_or("bad segment")?;
        if segment.offset <= phoff && table_end <= file_end {
            headers = segment.va + (phoff - segment.offset);
        }
    }
    let stack_bottom = layout.stack_top.checked_sub(STACK_PAGES * PAGE).ok_or("no room for the stack")?;
    if !space.map(stack_bottom, STACK_PAGES, Protection::ReadWrite) {
        return Err("no memory for the stack");
    }
    // AT_RANDOM's 16 bytes sit at the very top; the strings and vectors below them.
    let random_at = layout.stack_top - 16;
    let aux: [(Word, Word); 6] = [(arch::AT_PHDR, headers), (arch::AT_PHENT, phentsize), (arch::AT_PHNUM, phnum),
                                  (arch::AT_PAGESZ, PAGE), (arch::AT_ENTRY, elf.entry), (arch::AT_RANDOM, random_at)];
    // Without a mapped header table, no AT_PHDR (and its size fields) at all.
    let aux = if headers != 0 { &aux[..] } else { &aux[3..] };
    let mut top = [0u8; PAGE as usize];
    top[PAGE as usize - 16..].copy_from_slice(&layout.random);
    let sp = arch::build_stack(NATIVE, &mut top[..PAGE as usize - 16], random_at, args, aux).ok_or("arguments too long")?;
    if !space.load(layout.stack_top - PAGE, &top) {
        return Err("no memory for the stack");
    }
    Ok(Start { entry: elf.entry, sp, image_end })
}
