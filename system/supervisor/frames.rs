//! The frame ledger: which app address space holds each page of the pool.
//!
//! The process manager (procmgr.rs) is the only code that maps pool pages
//! into apps, and it asks here first. The rules, checked on every mapping
//! whatever the caller passed:
//!
//! - a page is held by at most one address space at a time, so no page is
//!   ever mapped in two apps (nothing in the system shares memory between
//!   apps; a page index that is stale, or simply wrong, is refused instead
//!   of quietly joining two apps' memory);
//! - only the apps' pages can be mapped: never the runtime's heap below them
//!   or the unpacked system image above them;
//! - a page leaves its space only when its last mapping there is removed.
//!
//! The pool's own bitmap (lifecycle.rs `PageMap`) knows free from taken but
//! not who took a page; this is the owner it lacks. Plain data, no kernel
//! calls: runtime/open/tests/tcb_host.rs, and memprot_host.rs beside it for the
//! ledger inside the process manager.
use alloc::vec::Vec;

/// Bits of an entry that count the owner's mappings of the page.
const COUNT_BITS: u32 = 12;
const COUNT: u32 = (1 << COUNT_BITS) - 1;
/// Owners are capability slots of page directories, nonzero and below this.
pub const OWNERS: u32 = 1 << (32 - COUNT_BITS);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Not one of the apps' pages (the heap, the system image, past the pool).
    Range,
    /// Another address space holds it.
    Held,
    /// Not an owner, or it has mapped the page too many times to count.
    Count,
}

pub struct Frames {
    /// Per page: owner << COUNT_BITS | mappings (0: nobody holds it).
    held: Vec<u32>,
    /// The apps' pages: [lo, hi). Empty until `limit` says what they are.
    lo: u32,
    hi: u32,
    /// Mappings refused because another space held the page.
    pub refused: u32,
}

impl Frames {
    /// A ledger for the apps' pages [lo, hi). The runtime's starts empty
    /// (0, 0): until boot has called `limit`, nothing can be mapped.
    pub const fn new(lo: u32, hi: u32) -> Frames {
        Frames { held: Vec::new(), lo, hi, refused: 0 }
    }
    /// The apps' pages are [lo, hi): the pool less the heap below and the
    /// system image above.
    pub fn limit(&mut self, lo: u32, hi: u32) {
        (self.lo, self.hi) = (lo, hi);
    }
    /// The space that holds page `index` (0: none).
    pub fn owner(&self, index: u32) -> u32 {
        self.held.get(index as usize).map_or(0, |e| e >> COUNT_BITS)
    }
    /// Space `owner` is about to map page `index`. Refused if it is not one
    /// of the apps' pages, or another space holds it.
    pub fn claim(&mut self, owner: u32, index: u32) -> Result<(), Refused> {
        if index < self.lo || index >= self.hi {
            return Err(Refused::Range);
        }
        if owner == 0 || owner >= OWNERS {
            return Err(Refused::Count);
        }
        let i = index as usize;
        if self.held.len() <= i {
            self.held.resize(i + 1, 0);
        }
        let e = self.held[i];
        if e != 0 && e >> COUNT_BITS != owner {
            self.refused += 1;
            return Err(Refused::Held);
        }
        if e & COUNT == COUNT {
            return Err(Refused::Count);
        }
        self.held[i] = (owner << COUNT_BITS) | ((e & COUNT) + 1);
        Ok(())
    }
    /// One of `owner`'s mappings of page `index` is gone. False (and nothing
    /// changes) if `owner` does not hold the page.
    pub fn release(&mut self, owner: u32, index: u32) -> bool {
        match self.held.get_mut(index as usize) {
            Some(e) if *e != 0 && *e >> COUNT_BITS == owner => {
                *e = if *e & COUNT == 1 { 0 } else { *e - 1 };
                true
            }
            _ => false,
        }
    }
    /// Pages held by `owner` (for tests and reports).
    pub fn held_by(&self, owner: u32) -> usize {
        self.held.iter().filter(|&&e| e != 0 && e >> COUNT_BITS == owner).count()
    }
}
