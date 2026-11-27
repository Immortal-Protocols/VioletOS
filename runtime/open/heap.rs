//! The Open Runtime's heap: a first-fit, address-ordered free list with
//! coalescing, over memory taken from the app pool at startup.
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

struct Free {
    size: usize,
    next: *mut Free,
}
pub struct Heap {
    head: core::cell::UnsafeCell<*mut Free>,
}
unsafe impl Sync for Heap {}
#[global_allocator]
pub static HEAP: Heap = Heap { head: core::cell::UnsafeCell::new(null_mut()) };

/// Bytes handed to the heap in all, for the `memory` report.
static TOTAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
/// Below this much free heap the lifecycle closes cached apps (mod.rs): an
/// allocation that fails takes every open app down with the runtime.
pub const LOW_BYTES: usize = 4 << 20;

const ALIGN: usize = 8;
const HEADER: usize = 8;
fn round(n: usize) -> usize {
    (n + ALIGN - 1) & !(ALIGN - 1)
}

impl Heap {
    /// Hand the heap a region it owns from now on.
    pub unsafe fn add(&self, start: usize, len: usize) {
        let start = round(start);
        let block = start as *mut Free;
        (*block).size = len & !(ALIGN - 1);
        (*block).next = null_mut();
        TOTAL.fetch_add(len & !(ALIGN - 1), core::sync::atomic::Ordering::Relaxed);
        self.insert(block);
    }
    pub fn total(&self) -> usize {
        TOTAL.load(core::sync::atomic::Ordering::Relaxed)
    }
    unsafe fn insert(&self, block: *mut Free) {
        let head = &mut *self.head.get();
        let mut prev: *mut Free = null_mut();
        let mut cur = *head;
        while !cur.is_null() && (cur as usize) < (block as usize) {
            prev = cur;
            cur = (*cur).next;
        }
        (*block).next = cur;
        if prev.is_null() {
            *head = block;
        } else {
            (*prev).next = block;
        }
        // Merge with the following block, then with the preceding one.
        if !cur.is_null() && block as usize + (*block).size == cur as usize {
            (*block).size += (*cur).size;
            (*block).next = (*cur).next;
        }
        if !prev.is_null() && prev as usize + (*prev).size == block as usize {
            (*prev).size += (*block).size;
            (*prev).next = (*block).next;
        }
    }
}

impl Heap {
    /// Free blocks on the list and their total size.
    pub fn stats(&self) -> (usize, usize) {
        let (mut blocks, mut bytes) = (0, 0);
        let mut cur = unsafe { *self.head.get() };
        while !cur.is_null() {
            blocks += 1;
            bytes += unsafe { (*cur).size };
            cur = unsafe { (*cur).next };
        }
        (blocks, bytes)
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(ALIGN);
        let need = round(layout.size().max(1)) + HEADER;
        let head = &mut *self.head.get();
        let mut prev: *mut Free = null_mut();
        let mut cur = *head;
        while !cur.is_null() {
            let start = cur as usize;
            let end = start + (*cur).size;
            // The user pointer follows an 8-byte header that records the block.
            let user = (start + HEADER + align - 1) & !(align - 1);
            let block_start = user - HEADER;
            let block_end = round(user + layout.size().max(1));
            if block_end <= end && block_end - start >= need.min(block_end - start) {
                let next = (*cur).next;
                // Unlink, then return the unused head and tail to the list.
                if prev.is_null() {
                    *head = next;
                } else {
                    (*prev).next = next;
                }
                let lead = block_start - start;
                let tail = end - block_end;
                let (mut start_used, mut end_used) = (block_start, block_end);
                if lead >= core::mem::size_of::<Free>() + ALIGN {
                    let b = start as *mut Free;
                    (*b).size = lead;
                    self.insert(b);
                } else {
                    start_used = start;
                }
                if tail >= 32 {
                    let b = block_end as *mut Free;
                    (*b).size = tail;
                    self.insert(b);
                } else {
                    end_used = end;
                }
                let header = (user - HEADER) as *mut usize;
                *header = start_used;
                *header.add(1) = end_used - start_used;
                return user as *mut u8;
            }
            prev = cur;
            cur = (*cur).next;
        }
        null_mut()
    }
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let header = (ptr as usize - HEADER) as *const usize;
        let start = *header;
        let size = *header.add(1);
        let b = start as *mut Free;
        (*b).size = size;
        self.insert(b);
    }
}
