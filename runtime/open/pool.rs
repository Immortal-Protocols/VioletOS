//! Physical memory for apps: the supervisor's pool untypeds, cut into pages
//! (sys::PAGE: 4 KiB). Every page stays mapped in the Open Runtime at
//! POOL_VA + index * PAGE, writable and Execute-Never, so reading or writing
//! app memory is a plain memory access and nothing an app wrote can run in
//! this process. Apps get copies of the page capabilities, mapped with their
//! own rights (procmgr.rs).
//!
//! One capability slot per page, in one run, and one more for each page an
//! app has mapped: sys::CNODE_BITS is sized for both (pages.rs).
use super::sys::*;
use alloc::vec::Vec;

pub struct Pool {
    first_slot: u32,
    free: Vec<u32>,
    pub total: usize,
    /// Which pages are free (lifecycle.rs): a page given back while still
    /// free, or one that was never the apps', is refused and counted, not
    /// handed out again (a stale give-back of a page taken again is not caught).
    map: super::lifecycle::PageMap,
    /// Pages the apps may ever hold (the pool less the heap and the image).
    pub app_pages: usize,
    /// Refused give-backs since boot.
    pub refused: u32,
}

/// The kernel memory the Open Runtime carves its own objects from.
pub struct Kernel {
    next_slot: u32,
    free_slots: Vec<u32>,
    /// MCS: Reply objects answered and ready for reuse, by slot.
    idle_replies: Vec<u32>,
    /// A small untyped kept for Reply objects (0: none yet), so carving app
    /// slices from the rest of kernel memory cannot starve them.
    pub replies: u32,
}

/// The CSpace holds the pool's pages, one mapping of every page the apps
/// may hold (each is held by one address space and mapped once there, so
/// repeated mappings of a screen cannot use slots up), and the kernel
/// objects (pages.rs `slots_needed`, sys::CNODE_BITS, supervisor.c OPEN_BITS).
const _: () = assert!(slots_needed(POOL_PAGES, super::HEAP_PAGES) <= SLOTS as usize);

/// The last slot: handed out when every other is in use, to every caller
/// that asks then. The first finds it empty; the rest find it taken and
/// their kernel call fails: a system call gets an error, and a fault closes
/// its app as out of memory (mod.rs). Only kernel objects past
/// pages.rs OBJECT_SLOTS can get here; the runtime itself does not stop.
const LAST_SLOT: u32 = SLOTS - 1;

impl Kernel {
    pub fn new() -> Self {
        Self { next_slot: SLOT_DYNAMIC, free_slots: Vec::new(), idle_replies: Vec::new(), replies: 0 }
    }
    pub fn slot(&mut self) -> u32 {
        if let Some(s) = self.free_slots.pop() {
            return s;
        }
        if self.next_slot >= LAST_SLOT {
            return LAST_SLOT;
        }
        self.next_slot += 1;
        self.next_slot - 1
    }
    /// MCS: after save_caller moved the pending Reply object out, put an idle
    /// one (or a new one) where the next receive expects it.
    pub fn refill_reply(&mut self) -> Result<(), i32> {
        if let Some(idle) = self.idle_replies.pop() {
            move_cap(SLOT_REPLY, idle)?;
            self.release(idle);
            return Ok(());
        }
        if self.replies != 0 && retype(self.replies, Object::Reply, 0, SLOT_REPLY).is_ok() {
            return Ok(());
        }
        retype(SLOT_KERNEL, Object::Reply, 0, SLOT_REPLY)
    }
    /// A saved reply that has been answered: without MCS the capability is
    /// gone with the answer; with MCS the Reply object is kept for reuse.
    pub fn reply_done(&mut self, slot: u32) {
        if mcs() {
            self.idle_replies.push(slot);
        } else {
            self.release(slot);
        }
    }
    /// Return a slot that is already empty.
    pub fn release(&mut self, slot: u32) {
        if slot != LAST_SLOT {
            self.free_slots.push(slot);
        }
    }
    pub fn object(&mut self, untyped: u32, o: Object, bits: u32) -> Result<u32, i32> {
        let slot = self.slot();
        match retype(untyped, o, bits, slot) {
            Ok(()) => Ok(slot),
            Err(e) => {
                self.release(slot);
                Err(e)
            }
        }
    }
}

pub fn page_va(index: u32) -> usize {
    pool_base() + (index as usize) * PAGE
}

impl Pool {
    /// Retype every pool untyped into pages and map them all into this process.
    /// The slots are allocated as one contiguous run so a page's index finds its cap.
    pub fn new(k: &mut Kernel, heap_pages: usize) -> Result<(Pool, usize), i32> {
        let first_slot = k.next_slot;
        k.next_slot += POOL_PAGES as u32;
        let mut mapped_tables = 0usize;
        let mut index = 0u32;
        for (u, &bits) in POOL_BITS.iter().enumerate() {
            for _ in 0..1u32 << (bits - PAGE_BITS) {
                retype(SLOT_POOL + u as u32, PAGE_OBJECT, PAGE_BITS, first_slot + index)?;
                let va = (POOL_VA + index as usize * PAGE) as u32;
                if va & 0xf_ffff == 0 {
                    let pt = k.object(SLOT_KERNEL, Object::PageTable, 0)?;
                    pt_map(pt, SLOT_PD, va)?;
                    mapped_tables += 1;
                }
                // Writable, never executable: this is app-controlled data (and
                // the runtime's heap) inside the runtime's own address space.
                page_map(first_slot + index, SLOT_PD, va, true, false)?;
                index += 1;
            }
        }
        // The heap is the first pages; apps get the rest, highest first. Nothing
        // above allocated, because the heap did not exist yet.
        unsafe { super::heap::HEAP.add(POOL_VA, heap_pages * PAGE) };
        let free = (heap_pages as u32..POOL_PAGES as u32).rev().collect();
        let _ = mapped_tables;
        let map = super::lifecycle::PageMap::new(heap_pages..POOL_PAGES);
        let app_pages = POOL_PAGES - heap_pages;
        Ok((Pool { first_slot, free, total: POOL_PAGES, map, app_pages, refused: 0 }, heap_pages * PAGE))
    }
    pub fn cap(&self, index: u32) -> u32 {
        self.first_slot + index
    }
    pub fn available(&self) -> usize {
        self.free.len()
    }
    /// The most pages one app may hold (procmgr.rs `Space::cap`, counted
    /// over the app and every process it forked or they exec'd): seven
    /// eighths of the apps' memory, so no single app can take all of it and
    /// leave the low-memory killer nothing to close but every other app.
    pub fn app_cap(&self) -> u32 {
        (self.app_pages - self.app_pages / 8) as u32
    }
    /// A zeroed page.
    pub fn take(&mut self) -> Option<u32> {
        let index = self.free.pop()?;
        if !self.map.take(index) {
            // On the free list but not free: never hand it out twice.
            self.refused += 1;
            return self.take();
        }
        unsafe { core::ptr::write_bytes(page_va(index) as *mut u8, 0, PAGE) };
        Some(index)
    }
    /// The `count` highest pages, which the linear map makes one contiguous
    /// range; they are never returned. For the unpacked file system image.
    pub fn take_top(&mut self, count: usize) -> Option<usize> {
        if count == 0 || count > self.free.len() || self.free[0] as usize != POOL_PAGES - 1 {
            return None;
        }
        let first = self.free[count - 1];
        if self.free[0] - first != count as u32 - 1 {
            return None;
        }
        self.free.drain(..count);
        for i in first..first + count as u32 {
            self.map.take(i);
        }
        self.map.retire_top(count);
        self.app_pages -= count;
        Some(page_va(first))
    }
    pub fn give_back(&mut self, index: u32) {
        if self.map.give(index) {
            self.free.push(index);
        } else {
            self.refused += 1;
            crate::log!("pool: refused page {} given back while free or not the apps'", index);
        }
    }
}
