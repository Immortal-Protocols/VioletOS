//! Memory-protection rules for open apps, as plain functions over a sorted
//! region list (no kernel calls, so the host tests in tests/memprot_host.rs
//! and tests/memres_host.rs run them directly): what the hardware gives a
//! page, the W^X policy, which accesses a region allows, the one check every
//! copy between the runtime and app memory passes, and the randomized
//! address-space layout.
//!
//! The rules (see README.md, "Memory protection"):
//! - A hardware page gets the OR of the protections of the regions in it:
//!   read-only, read-write or read-execute. It is never writable and
//!   executable, and everything not executable is Execute-Never. The rules
//!   hold for any page size that regions are multiples of: `hardware` and
//!   `union` take the page's range from the caller (procmgr.rs). With 4 KiB
//!   pages a page holds one region, so it has exactly that region's rights.
//! - Executable memory comes only from the program loader (and the runtime's
//!   trampoline page). mmap and mprotect cannot create it and cannot make a
//!   region writable and executable.
//! - A 64 KiB window (WINDOW, the largest hardware page the runtime has
//!   used) that holds code holds nothing else. The loader refuses a program
//!   that breaks this, and mmap, brk and mprotect cannot put any other region
//!   (whatever its protection) in such a window, so nothing but the loader
//!   ever fills one, whatever the page size.
//! - The runtime copies to or from app memory only after `permits` (or
//!   `buffer`, which also makes the copy's buffer) has checked the whole
//!   range against the regions. A file write or ftruncate grows the file's
//!   contents only through `resize_file` (bounded, fallible).
//! - mprotect may drop write from a program's PT_GNU_RELRO (it is never near
//!   code): every static PIE does so before main (tools/relro_seal.rs).
use alloc::vec::Vec;

/// The runtime serves AArch64 programs (built for aarch64-unknown-none, the
/// 64-bit Open Runtime): every app is then an ELF64 AArch64 program, still
/// placed by this file's 32-bit layout below 4 GiB, so an address the app
/// passes fits the runtime's u32 regions (system calls, ../arch/enter64.rs).
/// False on the host and for the AArch32 runtime.
pub const WIDE: bool = cfg!(all(target_arch = "aarch64", target_os = "none"));

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

/// App address-space layout: the fixed bounds the random layout stays within.
pub const MMAP_BASE: u32 = 0x4000_0000;
pub const MMAP_TOP: u32 = 0xb000_0000;
pub const STACK_TOP: u32 = 0xbf00_0000;
pub const STACK_SIZE: u32 = 8 << 20;
/// The trampoline page (thread i starts at stubs + 8 * i) is one of these pages.
pub const STUBS_BASE: u32 = 0xbf10_0000;
pub const STUBS_PAGES: u32 = 224;
/// Highest address an app may use; the kernel starts at 0xe0000000.
pub const USER_TOP: u32 = 0xc000_0000;
const PIE_BASE: u32 = 0x0040_0000;
/// The W^X policy's unit, and the layout's: 64 KiB.
const WINDOW: u32 = 0x1_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vma {
    pub start: u32,
    pub end: u32,
    pub prot: u32,
}

/// How the app's mapping of one hardware page is set up.
pub const HW_NONE: u8 = 0;
pub const HW_R: u8 = 1;
pub const HW_RW: u8 = 3;
pub const HW_RX: u8 = 5;

/// The mapping for a page whose regions' protections OR to `prot`. ARMv7
/// short descriptors have no write-only or execute-only pages, so both imply
/// read. Write wins over execute: a page is never both (the policy below
/// keeps the two out of one page; this is the backstop).
pub fn hardware(prot: u32) -> u8 {
    if prot & 7 == 0 {
        HW_NONE
    } else if prot & PROT_WRITE != 0 {
        HW_RW
    } else if prot & PROT_EXEC != 0 {
        HW_RX
    } else {
        HW_R
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    Write,
    Exec,
}

/// The access behind a seL4 VM fault: message register 2 is nonzero for an
/// instruction fetch, register 3 is the fault status register (the kernel is
/// not built as a hypervisor, so it is the raw DFSR, whose bit 11 is set for a write).
/// On AArch64 (`WIDE`) it is ESR_EL1, whose WnR is bit 6.
pub fn access(prefetch: u32, fsr: u32) -> Access {
    let wnr = if WIDE { 1 << 6 } else { 1 << 11 };
    if prefetch != 0 {
        Access::Exec
    } else if fsr & wnr != 0 {
        Access::Write
    } else {
        Access::Read
    }
}

impl Access {
    /// Whether a region with `prot` allows it.
    pub fn allowed(self, prot: u32) -> bool {
        match self {
            Access::Read => prot & 7 != 0,
            Access::Write => prot & PROT_WRITE != 0,
            Access::Exec => prot & PROT_EXEC != 0,
        }
    }
    /// Whether a page mapped as `hw` lets it through.
    pub fn served(self, hw: u8) -> bool {
        match self {
            Access::Read => hw != HW_NONE,
            Access::Write => hw == HW_RW,
            Access::Exec => hw == HW_RX,
        }
    }
}

/// Index of the first region ending above `va` (regions are sorted and disjoint).
fn first(vmas: &[Vma], va: u32) -> usize {
    vmas.partition_point(|v| v.end <= va)
}

pub fn find(vmas: &[Vma], va: u32) -> Option<&Vma> {
    vmas.get(first(vmas, va)).filter(|v| v.start <= va)
}

/// Marks a `union` that found at least one region, even one with no access.
pub const ANY: u32 = 8;

/// The OR of the protections of the regions touching [start, end), with ANY
/// set if there is one; 0 if there is none.
pub fn union(vmas: &[Vma], start: u32, end: u32) -> u32 {
    let mut prot = 0;
    if start < end {
        for v in &vmas[first(vmas, start)..] {
            if v.start >= end {
                break;
            }
            prot |= v.prot | ANY;
        }
    }
    prot
}

/// Whether every byte of [start, start + len) lies in a region whose
/// protection has a bit of `mask` (mask ANY: in any region at all).
pub fn permits(vmas: &[Vma], start: u32, len: usize, mask: u32) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = u32::try_from(len).ok().and_then(|l| start.checked_add(l)) else { return false };
    let mut at = start;
    for v in &vmas[first(vmas, start)..] {
        if v.start > at || (v.prot | ANY) & mask == 0 {
            return false;
        }
        if v.end >= end {
            return true;
        }
        at = v.end;
    }
    false
}

/// The buffer for a system call that copies `len` bytes in from app memory
/// at `va`: zeroed, and made only once all of [va, va + len) is known to be
/// the app's to read. The length is the app's word, so it is never trusted
/// with the runtime's heap: None if the range is not readable (nothing is
/// allocated), and None if the heap cannot hold it (the call fails; the
/// runtime does not).
pub fn buffer(vmas: &[Vma], va: u32, len: usize) -> Option<Vec<u8>> {
    if !permits(vmas, va, len, 7) || len > isize::MAX as usize {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    // As `vec![0; len]`, but a heap too small for it is None, not the end of
    // the runtime. The layout is valid: align 1, and 0 < len <= isize::MAX.
    unsafe {
        let p = alloc::alloc::alloc_zeroed(core::alloc::Layout::from_size_align_unchecked(len, 1));
        (!p.is_null()).then(|| Vec::from_raw_parts(p, len, len))
    }
}

/// The largest in-memory file (/tmp, /data) a write or ftruncate may make.
pub const FILE_MAX: u64 = 16 << 20;
pub const EFBIG: i32 = -27;
pub const ENOSPC: i32 = -28;

/// Sizes a file's contents to `end` bytes, zero-filled: for a write ending
/// there (`shrink` false: a file only grows) or an ftruncate (`shrink`
/// true). `end` comes from the app's offset and length, so it is never
/// trusted with the runtime's heap: EFBIG past FILE_MAX (no wrap, no cut to
/// 32 bits), ENOSPC when the heap cannot hold it (the call fails; the runtime
/// does not). On Ok, [0, end) is in `content`.
pub fn resize_file(content: &mut Vec<u8>, end: u64, shrink: bool) -> Result<(), i32> {
    if end > FILE_MAX {
        return Err(EFBIG);
    }
    let end = end as usize;
    if end > content.len() {
        content.try_reserve(end - content.len()).map_err(|_| ENOSPC)?;
        content.resize(end, 0);
    } else if shrink {
        content.truncate(end);
    }
    Ok(())
}

fn page_down(v: u32) -> u32 {
    v & !(WINDOW - 1)
}
fn page_up(v: u32) -> u32 {
    v.saturating_add(WINDOW - 1) & !(WINDOW - 1)
}

/// The W^X policy for an app changing its own memory: whether giving
/// [start, end) the protection `prot` is refused. `fresh` is a new mapping
/// (mmap, brk); otherwise a change to regions that exist (mprotect).
pub fn refuses(vmas: &[Vma], start: u32, end: u32, prot: u32, fresh: bool) -> bool {
    if prot & PROT_EXEC != 0 {
        // Never writable and executable; and code comes only from the loader,
        // so a change may keep execute where every part has it, never add it.
        if prot & PROT_WRITE != 0 || fresh {
            return true;
        }
        for v in &vmas[first(vmas, start)..] {
            if v.start >= end {
                break;
            }
            if v.prot & PROT_EXEC == 0 {
                return true;
            }
        }
    }
    // Nothing that is not code goes into a 64 KiB window that keeps code after
    // the change: the hardware runs the whole page, so a read-only or even
    // inaccessible region there (filled from a file, say) would be code.
    prot & PROT_EXEC == 0
        && (union(vmas, page_down(start), start) | union(vmas, end, page_up(end))) & PROT_EXEC != 0
}

/// Whether a 64 KiB window touched by [start, end) holds code.
pub fn near_code(vmas: &[Vma], start: u32, end: u32) -> bool {
    union(vmas, page_down(start), page_up(end)) & PROT_EXEC != 0
}

/// Whether some 64 KiB window holds code and anything else: a region that is
/// writable and executable, or an executable region sharing its pages with
/// one that is not. The loader refuses such a program; `refuses` keeps an
/// app from getting there.
pub fn mixed(vmas: &[Vma]) -> bool {
    vmas.iter().any(|v| {
        v.prot & PROT_EXEC != 0
            && (v.prot & PROT_WRITE != 0
                || vmas[first(vmas, page_down(v.start))..]
                    .iter()
                    .take_while(|u| u.start < page_up(v.end))
                    .any(|u| u.prot & PROT_EXEC == 0))
    })
}

/// Mark [start, end) with `prot`, replacing whatever was there.
pub fn insert(vmas: &mut Vec<Vma>, start: u32, end: u32, prot: u32) {
    carve(vmas, start, end);
    let at = vmas.partition_point(|v| v.start < start);
    vmas.insert(at, Vma { start, end, prot });
}

/// Remove [start, end) from the regions, splitting any that straddle it.
pub fn carve(vmas: &mut Vec<Vma>, start: u32, end: u32) {
    let mut out = Vec::with_capacity(vmas.len() + 1);
    for v in vmas.drain(..) {
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
    *vmas = out;
}

/// Where a program's parts go. Everything is a multiple of 64 KiB (the
/// policy's window) except the stack pointer's padding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Load address of a position-independent executable (ET_DYN).
    pub pie_base: u32,
    /// Distance from the end of the program to the start of the heap (brk).
    pub brk_gap: u32,
    /// mmap hands out addresses downward from here.
    pub mmap_top: u32,
    pub stack_top: u32,
    /// Bytes left unused at the top of the stack.
    pub stack_pad: u32,
    /// The trampoline page.
    pub stubs: u32,
}

impl Layout {
    /// The layout before randomization, for tests that need known addresses.
    pub const fn fixed() -> Layout {
        Layout { pie_base: PIE_BASE, brk_gap: 0, mmap_top: MMAP_TOP, stack_top: STACK_TOP, stack_pad: 0, stubs: STUBS_BASE }
    }

    /// A layout from 24 random bytes: 12 bits of 64 KiB steps for the PIE
    /// base and for mmap, 11 for the stack (plus 8 bits of 16-byte padding),
    /// 8 for the heap gap, and one of 224 trampoline pages.
    pub fn random(bytes: &[u8]) -> Layout {
        // Six little-endian words; a short slice leaves the rest zero.
        let mut r = [0u32; 6];
        for (word, b) in r.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        Layout {
            pie_base: PIE_BASE + (r[0] & 0xfff) * WINDOW,
            brk_gap: (r[1] & 0xff) * WINDOW,
            mmap_top: MMAP_TOP - (r[2] & 0xfff) * WINDOW,
            stack_top: STACK_TOP - (r[3] & 0x7ff) * WINDOW,
            stack_pad: (r[4] & 0xff) * 16,
            stubs: STUBS_BASE + (r[5] % STUBS_PAGES) * WINDOW,
        }
    }
}

// ---- the AArch64 layout ----

/// An AArch64 app's address space (the kernel gives user space 47 bits,
/// runtime/arch AARCH64.user_top). Sparse and wide: the PIE, its heap, mmap,
/// the stack and the trampoline page each move within their own band, every
/// band lies above 4 GiB (so an address cut to 32 bits anywhere in the
/// runtime faults instead of reaching other memory), and the mmap band leaves
/// tens of TiB for the PROT_NONE reservations a JavaScript engine makes
/// (LibJS reserves 64 GiB and more). Units are WINDOW (64 KiB), as above, so
/// the W^X window rule holds unchanged.
pub const USER_TOP64: u64 = 0x8000_0000_0000;
pub const PIE_BASE64: u64 = 0x1000_0000_0000;
pub const MMAP_BASE64: u64 = 0x2000_0000_0000;
pub const MMAP_TOP64: u64 = 0x7000_0000_0000;
pub const STACK_TOP64: u64 = 0x7ff0_0000_0000;
pub const STUBS_BASE64: u64 = 0x7ff8_0000_0000;
pub const STUBS_PAGES64: u64 = 4096;
/// Random bits of each band, in WINDOW steps.
pub const PIE_BITS64: u32 = 24;
pub const BRK_BITS64: u32 = 12;
pub const MMAP_BITS64: u32 = 28;
pub const STACK_BITS64: u32 = 22;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout64 {
    pub pie_base: u64,
    pub brk_gap: u64,
    pub mmap_top: u64,
    pub stack_top: u64,
    pub stack_pad: u64,
    pub stubs: u64,
}

impl Layout64 {
    pub const fn fixed() -> Layout64 {
        Layout64 { pie_base: PIE_BASE64, brk_gap: 0, mmap_top: MMAP_TOP64, stack_top: STACK_TOP64, stack_pad: 0, stubs: STUBS_BASE64 }
    }
    /// A layout from 24 random bytes, as `Layout::random`: 24 bits for the
    /// PIE base, 12 for the heap gap, 28 for mmap, 22 for the stack (plus 8
    /// bits of 16-byte padding), and one of 4096 trampoline pages.
    pub fn random(bytes: &[u8]) -> Layout64 {
        let mut r = [0u64; 6];
        for (word, b) in r.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        }
        let w = u64::from(WINDOW);
        let bits = |v: u64, n: u32| v & ((1u64 << n) - 1);
        Layout64 {
            pie_base: PIE_BASE64 + bits(r[0], PIE_BITS64) * w,
            brk_gap: bits(r[1], BRK_BITS64) * w,
            mmap_top: MMAP_TOP64 - bits(r[2], MMAP_BITS64) * w,
            stack_top: STACK_TOP64 - bits(r[3], STACK_BITS64) * w,
            stack_pad: (r[4] & 0xff) * 16,
            stubs: STUBS_BASE64 + (r[5] % STUBS_PAGES64) * w,
        }
    }
}
