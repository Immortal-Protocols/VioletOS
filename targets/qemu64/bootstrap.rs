//! Where the bootstrap task's memory and device frames come from: pure
//! planning over the kernel's untyped list, with no kernel access, so it is
//! tested on the host (tests/host.rs).
//!
//! seL4 hands out an untyped region front to back, and each object lands at
//! the next address aligned to its own size. To make an object at one exact
//! physical address (a device frame), the gap before it is consumed first
//! with filler untypeds, each as large as alignment and the gap allow.

/// One of the kernel's untyped regions, as the boot information lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Untyped {
    pub cap: usize,
    pub paddr: u64,
    pub bits: u8,
    pub device: bool,
    /// The first address not yet handed out.
    pub cursor: u64,
}

impl Untyped {
    pub fn new(cap: usize, paddr: u64, bits: u8, device: bool) -> Self {
        Untyped { cap, paddr, bits, device, cursor: paddr }
    }
    pub fn end(&self) -> u64 {
        self.paddr + (1u64 << self.bits)
    }
    /// Can an object of `bits` still be made at exactly `paddr`?
    pub fn reaches(&self, paddr: u64, bits: u8) -> bool {
        paddr & ((1 << bits) - 1) == 0 && paddr >= self.cursor && paddr.checked_add(1 << bits).is_some_and(|end| end <= self.end())
    }
    /// The size (in bits) of the next filler on the way from the cursor to
    /// `target`, or `None` when the cursor is there. The smallest untyped the
    /// kernel makes is 16 bytes, so both must be multiples of 16.
    pub fn filler(&self, target: u64) -> Option<u8> {
        if self.cursor >= target {
            return None;
        }
        let gap = target - self.cursor;
        // Largest power of two that the cursor is aligned to and that fits the gap.
        let aligned = if self.cursor == 0 { 63 } else { self.cursor.trailing_zeros() };
        let fits = 63 - gap.leading_zeros();
        Some(aligned.min(fits) as u8)
    }
    /// Account for an object of `bits` made now: it lands at the cursor
    /// rounded up to its size. Returns its address, or `None` if it does not fit.
    pub fn take(&mut self, bits: u8) -> Option<u64> {
        let size = 1u64 << bits;
        let at = self.cursor.checked_add(size - 1)? & !(size - 1);
        if at.checked_add(size)? > self.end() {
            return None;
        }
        self.cursor = at + size;
        Some(at)
    }
}

/// The device region that can still produce an object of `bits` at `paddr`.
pub fn device_for(regions: &[Untyped], paddr: u64, bits: u8) -> Option<usize> {
    regions.iter().position(|r| r.device && r.reaches(paddr, bits))
}

/// The ordinary memory region to take the next object of `bits` from: the
/// first that still has room (the kernel lists them in address order).
pub fn memory_for(regions: &[Untyped], bits: u8) -> Option<usize> {
    regions.iter().position(|r| !r.device && r.clone().take(bits).is_some())
}
