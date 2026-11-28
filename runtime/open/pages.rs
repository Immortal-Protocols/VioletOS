//! The page size and the sizes the Open Runtime's capability space is cut
//! to, in one file that sys.rs and the host paging tests both include
//! (tests/paging_host.rs), so the tests check the shipped numbers, not a copy.
//! Plain constants: no code of its own lands in the runtime.
use super::Object;

/// The page apps' memory is handed out and mapped in: 4 KiB (12), the unit
/// Linux programs expect. `--cfg page64` selects 64 KiB large pages (16),
/// the earlier setting; the host tests are built both ways
/// (`make test-unit`). PAGE_OBJECT follows it; nothing else changes.
pub const PAGE_BITS: u32 = if cfg!(page64) { 16 } else { 12 };
pub const PAGE: usize = 1 << PAGE_BITS;
pub const PAGE_OBJECT: Object = if PAGE_BITS == 12 { Object::SmallPage } else { Object::LargePage };
/// For reports: KiB in a page, and pages in a MiB.
pub const PAGE_KIB: usize = PAGE >> 10;
pub const MIB_PAGES: usize = (1 << 20) / PAGE;

/// Per board, [emulator, handset]: the size of the Open Runtime's CSpace
/// (supervisor.c OPEN_BITS; the two must agree) and the sizes, as bit
/// counts, of the pool untypeds the supervisor places at SLOT_POOL onwards
/// (384 MiB and 256 MiB).
pub const BOARD_CNODE_BITS: [u32; 2] = [18, 17];
pub const BOARD_POOL_BITS: [&[u32]; 2] = [&[27, 28], &[28]];
const BOARD: usize = cfg!(moto_board) as usize;
pub const CNODE_BITS: u32 = BOARD_CNODE_BITS[BOARD];
pub const SLOTS: u32 = 1 << CNODE_BITS;
pub const POOL_BITS: &[u32] = BOARD_POOL_BITS[BOARD];
/// First slot handed out dynamically.
pub const SLOT_DYNAMIC: u32 = 64;
/// The runtime's heap: the first pool pages (48 MiB: the runtime, pushed
/// apps and file data). Never mapped into an app.
pub const HEAP_PAGES: usize = 48 * MIB_PAGES;
/// Slots kept for kernel objects (the apps' page tables, threads and
/// scheduling contexts, saved replies, the pool's own page tables). The
/// apps' are bounded by their kernel memory and thread limits, not by this
/// count: past it, the call or fault that finds no slot fails, and a fault
/// closes its app as out of memory (mod.rs).
pub const OBJECT_SLOTS: usize = 8192;

pub const fn pool_pages(bits: &[u32]) -> usize {
    let mut total = 0;
    let mut i = 0;
    while i < bits.len() {
        total += 1 << (bits[i] - PAGE_BITS);
        i += 1;
    }
    total
}
pub const POOL_PAGES: usize = pool_pages(POOL_BITS);

/// Slots a CSpace needs: one for each pool page (pool.rs), one more for
/// each app mapping, which is at most one per page an app may hold (a page
/// is held by one address space, frames.rs, and mapped once there,
/// procmgr.rs `map`), and the kernel objects.
pub const fn slots_needed(pool_pages: usize, heap_pages: usize) -> usize {
    SLOT_DYNAMIC as usize + pool_pages + (pool_pages - heap_pages) + OBJECT_SLOTS
}
