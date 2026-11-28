//! Host tests for the memory-protection rules in vma.rs that do not need an
//! address space (`make test-unit`; tests/memprot_host.rs has those that do):
//! - `buffer`, the check in front of every system call that copies in a
//!   length the app chose: nothing is allocated for a range the app may not
//!   read, and a length the heap cannot hold fails the call, not the runtime;
//! - the page rules at both page sizes the runtime has used: with 4 KiB
//!   pages every page has exactly its region's rights (a guard page is
//!   unmapped, a read-only page is read-only), with 64 KiB pages a page can
//!   be looser than a region in it but never executable for anything but the
//!   loader's code, and never writable and executable at either size;
//! - `Layout::random`, against the plain reading of its 24 bytes;
//! - `resize_file`, the only way a write or ftruncate grows a file: an
//!   offset the app chose is bounded, never wraps, and a heap that cannot
//!   hold it fails the call;
//! - tools/relro_seal.rs `relro_range`, the range every static PIE makes
//!   read-only before main, and that the W^X policy lets it.
//!
//! Each test keeps to its own thread's state, so they run in parallel.
#![allow(dead_code)]
// tools/relro_seal.rs is a crate root of its own (#![no_std]) when built for the phone.
#![allow(unused_attributes)]
extern crate alloc;

#[path = "../vma.rs"]
mod vma;
#[path = "../../../tools/relro_seal.rs"]
mod relro_seal;

use std::alloc::{GlobalAlloc, Layout as Block, System};
use std::cell::Cell;
use vma::{Layout, Vma, HW_NONE, HW_R, HW_RW, HW_RX, PROT_EXEC, PROT_READ, PROT_WRITE};

/// The Open Runtime's heap is 48 MiB: a request it cannot hold comes back
/// null. This allocator does the same for the test's threads (LIMIT), and
/// counts each thread's large requests so a test can say "nothing was allocated".
struct Small;
const LIMIT: usize = 48 << 20;
/// Requests at least this large are counted (the tests' own vectors are smaller).
const COUNTED: usize = 1 << 20;
thread_local! {
    static LARGE: Cell<usize> = const { Cell::new(0) };
    static REFUSED: Cell<usize> = const { Cell::new(0) };
    /// What this thread's heap holds at most in one request (LIMIT unless a test lowers it).
    static CAP: Cell<usize> = const { Cell::new(LIMIT) };
}
fn cap() -> usize {
    CAP.try_with(|c| c.get()).unwrap_or(LIMIT)
}
unsafe impl GlobalAlloc for Small {
    unsafe fn alloc(&self, block: Block) -> *mut u8 {
        if block.size() >= COUNTED {
            let _ = LARGE.try_with(|n| n.set(n.get() + 1));
        }
        if block.size() > cap() {
            let _ = REFUSED.try_with(|n| n.set(n.get() + 1));
            return core::ptr::null_mut();
        }
        System.alloc(block)
    }
    unsafe fn alloc_zeroed(&self, block: Block) -> *mut u8 {
        if block.size() >= COUNTED {
            let _ = LARGE.try_with(|n| n.set(n.get() + 1));
        }
        if block.size() > cap() {
            let _ = REFUSED.try_with(|n| n.set(n.get() + 1));
            return core::ptr::null_mut();
        }
        System.alloc_zeroed(block)
    }
    unsafe fn dealloc(&self, p: *mut u8, block: Block) {
        System.dealloc(p, block)
    }
}
#[global_allocator]
static HEAP: Small = Small;

const R: u32 = PROT_READ;
const RW: u32 = PROT_READ | PROT_WRITE;
const RX: u32 = PROT_READ | PROT_EXEC;

fn regions(list: &[(u32, u32, u32)]) -> Vec<Vma> {
    let mut vmas = Vec::new();
    for &(start, end, prot) in list {
        vma::insert(&mut vmas, start, end, prot);
    }
    vmas
}

// ---- buffer: the app's length never spends the runtime's heap unchecked ----

#[test]
fn buffer_is_zeroed_and_exactly_the_length_asked() {
    let vmas = regions(&[(0x10000, 0x30000, RW)]);
    for len in [1usize, 7, 4096, 0x20000] {
        let b = vma::buffer(&vmas, 0x10000, len).expect("a readable range");
        assert_eq!(b.len(), len);
        assert!(b.iter().all(|&x| x == 0));
        // It is an ordinary vector afterwards: it grows and is freed like one.
        let mut b = b;
        b.extend_from_slice(&[1, 2, 3]);
        assert_eq!(b.len(), len + 3);
    }
    // Nothing to copy is not an error, wherever it points (as `permits`).
    assert_eq!(vma::buffer(&vmas, 0x10000, 0), Some(Vec::new()));
    assert_eq!(vma::buffer(&[], 0xdead_0000, 0), Some(Vec::new()));
}

#[test]
fn buffer_allocates_nothing_for_memory_the_app_may_not_read() {
    // A hole, a region with no access, and one byte past the end.
    let vmas = regions(&[(0x10000, 0x20000, RW), (0x20000, 0x30000, 0), (0x40000, 0x400_0000, R)]);
    let before = LARGE.with(|n| n.get());
    let big = 2 << 20;
    assert!(vma::buffer(&vmas, 0x18000, big).is_none(), "runs into PROT_NONE memory");
    assert!(vma::buffer(&vmas, 0x30000, big).is_none(), "starts in a hole");
    assert!(vma::buffer(&vmas, 0x3_f000, big).is_none(), "starts just before the region");
    assert!(vma::buffer(&vmas, 0x400_0000 - big as u32 + 1, big).is_none(), "one byte past the end");
    assert!(vma::buffer(&[], 0, big).is_none(), "no regions at all");
    // The lengths an app can name without owning any memory: all refused
    // before the heap is asked for anything.
    for len in [0x7fff_ffffusize, 0xffff_ffff, usize::MAX] {
        assert!(vma::buffer(&vmas, 0x40000, len).is_none());
        assert!(vma::buffer(&[], 0x1000, len).is_none());
    }
    assert_eq!(LARGE.with(|n| n.get()), before, "a refused range must not reach the allocator");
    // The same range, readable: now it allocates, once.
    assert_eq!(vma::buffer(&vmas, 0x40000, big).map(|b| b.len()), Some(big));
    assert_eq!(LARGE.with(|n| n.get()), before + 1);
}

#[test]
fn buffer_fails_the_call_when_the_heap_cannot_hold_it() {
    // An app can own a huge readable mapping it never touched (mmap is lazy)
    // and name all of it in one write(2). `vec![0; len]` would end the
    // process on the allocator's null; `buffer` answers None (write: ENOMEM,
    // as the range is readable; an unreadable one is EFAULT).
    let vmas = regions(&[(0x4000_0000, 0xb000_0000, RW)]);
    let refused = REFUSED.with(|n| n.get());
    assert!(vma::buffer(&vmas, 0x4000_0000, LIMIT + 1).is_none());
    assert!(vma::buffer(&vmas, 0x4000_0000, 0x7000_0000).is_none(), "1.75 GiB");
    // How write(2) tells the two Nones apart (syscall.rs): readable, so ENOMEM.
    assert!(vma::permits(&vmas, 0x4000_0000, LIMIT + 1, 7));
    assert!(!vma::permits(&vmas, 0xb000_0000, 1, 7));
    assert_eq!(REFUSED.with(|n| n.get()), refused + 2, "the heap was asked and said no");
    // What fits still works, right after.
    assert_eq!(vma::buffer(&vmas, 0x4000_0000, 1 << 20).map(|b| b.len()), Some(1 << 20));
}

// ---- the page rules at 4 KiB and 64 KiB hardware pages ----

/// A small deterministic generator (the tests must not depend on the host's).
fn next(seed: &mut u64) -> u32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*seed >> 33) as u32
}

/// The protection of the region holding `va`, if any.
fn at(vmas: &[Vma], va: u32) -> Option<u32> {
    vma::find(vmas, va).map(|v| v.prot)
}

/// A program as the loader places it (constants, code, data in separate
/// 64 KiB windows), then `steps` random mmap/mprotect/munmap requests, each
/// applied only if the W^X policy lets an app make it.
fn random_space(seed: &mut u64, steps: usize) -> Vec<Vma> {
    let base = 0x40_0000 + (next(seed) & 0xff) * 0x1_0000;
    let mut vmas = regions(&[(base, base + 0x3000, R), (base + 0x1_0000, base + 0x2_5000, RX), (base + 0x3_0000, base + 0x3_9000, RW)]);
    for _ in 0..steps {
        // 4 KiB steps over the program and 2 MiB after it, so requests land
        // beside the code, in its windows and across region boundaries.
        let start = base - 0x1_0000 + (next(seed) % 0x220) * 0x1000;
        let end = start + (1 + next(seed) % 24) * 0x1000;
        let prot = [0, R, RW, RX, 7, PROT_EXEC, PROT_WRITE][(next(seed) % 7) as usize];
        match next(seed) % 3 {
            0 => {
                // mmap (MAP_FIXED or brk): a fresh mapping.
                if !vma::refuses(&vmas, start, end, prot, true) {
                    vma::insert(&mut vmas, start, end, prot);
                }
            }
            1 => {
                // mprotect: only what is already mapped changes.
                if !vma::refuses(&vmas, start, end, prot, false) {
                    let pieces: Vec<Vma> = vmas.iter().filter(|v| v.end > start && v.start < end).copied().collect();
                    for v in pieces {
                        vma::insert(&mut vmas, v.start.max(start), v.end.min(end), prot);
                    }
                }
            }
            _ => {
                // munmap, which an app may always do (the loader's code included).
                vma::carve(&mut vmas, start, end);
            }
        }
    }
    vmas
}

#[test]
fn every_page_has_the_right_rights_at_both_page_sizes() {
    let mut seed = 0x5eed_1234_abcd_ef01u64;
    let (mut exact_pages, mut loose_pages) = (0u32, 0u32);
    for round in 0..300 {
        let vmas = random_space(&mut seed, 40);
        for pair in vmas.windows(2) {
            assert!(pair[0].start < pair[0].end && pair[0].end <= pair[1].start, "round {round}: regions out of order");
        }
        assert!(!vma::mixed(&vmas), "round {round}: a window holds code and something else");
        let (lo, hi) = (vmas.first().map_or(0, |v| v.start) & !0xffff, vmas.last().map_or(0, |v| v.end + 0xffff) & !0xffff);
        for size in [0x1000u32, 0x1_0000] {
            let mut page = lo;
            while page < hi {
                let prot = vma::union(&vmas, page, page + size);
                let hw = vma::hardware(prot);
                // The 4 KiB units of this page, by their own regions.
                let units: Vec<Option<u32>> = (page..page + size).step_by(0x1000).map(|va| at(&vmas, va)).collect();
                // W^X: the hardware mapping is one of four, none writable and executable.
                assert!([HW_NONE, HW_R, HW_RW, HW_RX].contains(&hw));
                match hw {
                    // Executable: only code is in the page. No unit of it is
                    // a region the app could have filled (writable, read-only
                    // or inaccessible); a unit with no region holds zeroes.
                    HW_RX => assert!(units.iter().all(|u| u.map_or(true, |p| p & PROT_EXEC != 0 && p & PROT_WRITE == 0)),
                                     "round {round}: page {page:#x} ({size:#x}) runs something that is not code: {units:x?}"),
                    // Writable: no code in the page.
                    HW_RW => assert!(units.iter().all(|u| u.map_or(true, |p| p & PROT_EXEC == 0))),
                    HW_R => assert!(units.iter().all(|u| u.map_or(true, |p| p & (PROT_WRITE | PROT_EXEC) == 0))),
                    _ => assert!(units.iter().all(|u| u.map_or(true, |p| p & 7 == 0)), "an unmapped page holds accessible memory"),
                }
                // Never tighter than a region asks: what a region allows, its page serves.
                for u in units.iter().flatten() {
                    for access in [vma::Access::Read, vma::Access::Write, vma::Access::Exec] {
                        assert!(!access.allowed(*u) || access.served(hw) || (u & PROT_WRITE != 0 && access == vma::Access::Exec),
                                "round {round}: page {page:#x} ({size:#x}) is {hw} but a region in it is {u:#x}");
                    }
                }
                if size == 0x1000 {
                    // One region to a page: the page has exactly its rights,
                    // so a guard page is unmapped and a read-only page read-only.
                    assert_eq!(hw, units[0].map_or(HW_NONE, vma::hardware), "round {round}: page {page:#x}");
                    exact_pages += 1;
                } else if units.iter().any(|u| u.map_or(HW_NONE, vma::hardware) != hw) {
                    // 64 KiB: looser than some region in it (the limit 4 KiB pages remove).
                    loose_pages += 1;
                }
                page += size;
            }
        }
    }
    assert!(exact_pages > 10_000 && loose_pages > 100, "the spaces were too plain to test anything: {exact_pages} {loose_pages}");
}

#[test]
fn a_guard_page_is_enforced_by_4k_pages_and_not_by_64k_ones() {
    // A thread stack as musl maps it: 8 KiB of PROT_NONE under read-write memory.
    let vmas = regions(&[(0x5000_0000, 0x5000_2000, 0), (0x5000_2000, 0x5002_0000, RW)]);
    // 4 KiB pages: the guard has no mapping, the stack is read-write.
    assert_eq!(vma::hardware(vma::union(&vmas, 0x5000_0000, 0x5000_1000)), HW_NONE);
    assert_eq!(vma::hardware(vma::union(&vmas, 0x5000_1000, 0x5000_2000)), HW_NONE);
    assert_eq!(vma::hardware(vma::union(&vmas, 0x5000_2000, 0x5000_3000)), HW_RW);
    // One 64 KiB page over both: read-write, the guard with it. System calls
    // and first touch still refuse it either way (`permits`, `Access::allowed`).
    assert_eq!(vma::hardware(vma::union(&vmas, 0x5000_0000, 0x5001_0000)), HW_RW);
    assert!(!vma::permits(&vmas, 0x5000_1ff0, 32, PROT_WRITE));
    assert!(!vma::Access::Read.allowed(at(&vmas, 0x5000_1000).unwrap()));
    // Read-only data made so by mprotect, beside writable data.
    let vmas = regions(&[(0x6000_0000, 0x6000_1000, R), (0x6000_1000, 0x6000_4000, RW)]);
    assert_eq!(vma::hardware(vma::union(&vmas, 0x6000_0000, 0x6000_1000)), HW_R);
    assert_eq!(vma::hardware(vma::union(&vmas, 0x6000_0000, 0x6001_0000)), HW_RW);
}

#[test]
fn the_policy_window_stays_64k_whatever_the_page_size() {
    // Code from the loader; an app asks for memory in the same 64 KiB window
    // but another 4 KiB page. Refused: with 4 KiB pages the hardware would
    // keep them apart, but the rule is the stricter one at both sizes.
    let vmas = regions(&[(0x41_0000, 0x41_3000, RX)]);
    for prot in [0, R, RW] {
        assert!(vma::refuses(&vmas, 0x41_8000, 0x41_9000, prot, true), "prot {prot}");
        assert!(vma::refuses(&vmas, 0x40_f000, 0x41_1000, prot, true), "prot {prot}");
    }
    assert!(!vma::refuses(&vmas, 0x42_0000, 0x42_1000, RW, true));
    assert!(!vma::refuses(&vmas, 0x40_f000, 0x41_0000, RW, true));
    // And no executable memory from an app at any size or place.
    for (start, end) in [(0x50_0000, 0x50_1000), (0x50_0000, 0x51_0000), (0x41_3000, 0x41_4000)] {
        assert!(vma::refuses(&vmas, start, end, RX, true));
        assert!(vma::refuses(&vmas, start, end, 7, true));
    }
    assert!(vma::near_code(&vmas, 0x41_f000, 0x42_0000) && !vma::near_code(&vmas, 0x42_0000, 0x42_1000));
}

// ---- Layout::random ----

#[test]
fn random_layout_reads_its_bytes_as_six_words() {
    let mut seed = 0x0dd_ba11u64;
    for round in 0..2000 {
        let mut bytes = [0u8; 32];
        for b in bytes.iter_mut() {
            *b = next(&mut seed) as u8;
        }
        // Any length: words the slice does not hold are zero (no randomness
        // there, but no panic and no read past the end either).
        let len = if round < 40 { round as usize % 33 } else { 24 };
        let word = |i: usize| bytes[..len].get(i * 4..i * 4 + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let l = Layout::random(&bytes[..len]);
        assert_eq!(l.pie_base, 0x40_0000 + (word(0) & 0xfff) * 0x1_0000);
        assert_eq!(l.brk_gap, (word(1) & 0xff) * 0x1_0000);
        assert_eq!(l.mmap_top, vma::MMAP_TOP - (word(2) & 0xfff) * 0x1_0000);
        assert_eq!(l.stack_top, vma::STACK_TOP - (word(3) & 0x7ff) * 0x1_0000);
        assert_eq!(l.stack_pad, (word(4) & 0xff) * 16);
        assert_eq!(l.stubs, vma::STUBS_BASE + (word(5) % vma::STUBS_PAGES) * 0x1_0000);
        // A position-independent program (every shipped one) lands in the
        // first 256 MiB above 4 MiB, on a 64 KiB boundary, below the mmap area.
        assert!(l.pie_base % 0x1_0000 == 0 && l.pie_base >= 0x40_0000 && l.pie_base < 0x1040_0000 && l.pie_base < vma::MMAP_BASE);
    }
    assert_eq!(Layout::random(&[]), Layout::fixed());
}

#[test]
fn a_pie_gets_twelve_bits_of_base() {
    // Every one of the 4096 bases is reachable, and each word steers only its own part.
    let mut seen = std::collections::BTreeSet::new();
    for n in 0u32..4096 {
        let mut bytes = [0u8; 24];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        let l = Layout::random(&bytes);
        seen.insert(l.pie_base);
        assert_eq!(Layout { pie_base: Layout::fixed().pie_base, ..l }, Layout::fixed());
    }
    assert_eq!(seen.len(), 4096);
}

// ---- resize_file: a write or ftruncate at the app's offset ----

/// What write(2) does with `resize_file` (syscall.rs `write`, Kind::File).
fn file_write(content: &mut Vec<u8>, offset: u64, data: &[u8]) -> Result<(), i32> {
    vma::resize_file(content, offset.saturating_add(data.len() as u64), false)?;
    let start = offset as usize;
    content[start..start + data.len()].copy_from_slice(data);
    Ok(())
}

#[test]
fn a_write_grows_the_file_and_keeps_what_was_there() {
    let mut f = b"hello".to_vec();
    file_write(&mut f, 3, b"p me").unwrap();
    assert_eq!(f, b"help me");
    file_write(&mut f, 10, b"!").unwrap();
    assert_eq!(f, b"help me\0\0\0!");
    // A write inside the file never shrinks it.
    file_write(&mut f, 0, b"H").unwrap();
    assert_eq!(f.len(), 11);
    // Exactly FILE_MAX is allowed.
    let mut g = Vec::new();
    file_write(&mut g, vma::FILE_MAX - 1, b"x").unwrap();
    assert_eq!(g.len() as u64, vma::FILE_MAX);
}

#[test]
fn a_far_or_wrapping_offset_is_efbig_not_a_runtime_abort() {
    let mut f = b"keep".to_vec();
    // lseek(fd, 0xfffffff8) then write 16: start + 16 used to wrap to 8 and panic
    // on the slice; 4 GiB and more used to be cut to 32 bits (a write at 4 GiB
    // landed at 0). All are EFBIG now, and the file is as it was.
    for offset in [0xffff_fff8u64, 1 << 32, (1 << 32) + 2, u64::MAX - 3, i64::MAX as u64, vma::FILE_MAX] {
        assert_eq!(file_write(&mut f, offset, &[7; 16]), Err(vma::EFBIG), "{offset:#x}");
        assert_eq!(f, b"keep");
    }
    // ftruncate past the bound (40 MiB, or a 64-bit length) likewise.
    for len in [40u64 << 20, vma::FILE_MAX + 1, u64::MAX] {
        assert_eq!(vma::resize_file(&mut f, len, true), Err(vma::EFBIG));
        assert_eq!(f, b"keep");
    }
}

#[test]
fn ftruncate_grows_with_zeros_and_shrinks() {
    let mut f = b"abcdef".to_vec();
    vma::resize_file(&mut f, 2, true).unwrap();
    assert_eq!(f, b"ab");
    vma::resize_file(&mut f, 5, true).unwrap();
    assert_eq!(f, b"ab\0\0\0");
    vma::resize_file(&mut f, 0, true).unwrap();
    assert!(f.is_empty());
}

#[test]
fn a_file_the_heap_cannot_hold_is_enospc_and_the_runtime_lives() {
    // A heap with 1 MiB left: a 4 MiB file fails that call only.
    CAP.with(|c| c.set(1 << 20));
    let mut f = vec![1u8; 100];
    let refused = REFUSED.with(|n| n.get());
    assert_eq!(vma::resize_file(&mut f, 4 << 20, true), Err(vma::ENOSPC));
    assert_eq!(file_write(&mut f, 4 << 20, b"x"), Err(vma::ENOSPC));
    assert!(REFUSED.with(|n| n.get()) >= refused + 2, "the heap was asked and said no");
    assert_eq!(f, vec![1u8; 100], "nothing changed");
    // What fits still works.
    file_write(&mut f, 1000, b"y").unwrap();
    assert_eq!(f.len(), 1001);
    CAP.with(|c| c.set(LIMIT));
}

// ---- RELRO: sealed read-only by every static PIE before main ----

fn phdr(kind: u32, vaddr: u32, memsz: u32) -> [u32; 8] {
    [kind, 0, vaddr, vaddr, memsz, memsz, 4, 4]
}

#[test]
fn relro_range_is_the_segment_at_the_load_bias_in_whole_pages() {
    // `home` as lld lays it out (llvm-readelf -l): PHDR at 0x34, RELRO
    // 0x1e5640..0x1ed000, loaded at a random base.
    let headers = [phdr(6, 0x34, 0x160), phdr(1, 0, 0x9d194), phdr(0x6474_e552, 0x1e_5640, 0x79c0)];
    for base in [0x40_0000u32, 0x40_0000 + 0xfff * 0x1_0000, 0] {
        let (start, end) = relro_seal::relro_range(&headers, base + 0x34).unwrap();
        assert_eq!((start, end), (base + 0x1e_5000, base + 0x1ed_000), "{base:#x}");
    }
    // A last page that is only partly RELRO stays writable (elf_check refuses such a program).
    let partial = [phdr(6, 0x34, 0x40), phdr(0x6474_e552, 0x3_0000, 0x1800)];
    assert_eq!(relro_seal::relro_range(&partial, 0x40_0034), Some((0x43_0000, 0x43_1000)));
    // Either header missing: None (the seal stops the program).
    assert_eq!(relro_seal::relro_range(&[phdr(6, 0x34, 0x40)], 0x40_0034), None);
    assert_eq!(relro_seal::relro_range(&[phdr(0x6474_e552, 0x3_0000, 0x1000)], 0x40_0034), None);
    assert_eq!(relro_seal::relro_range(&[], 0x40_0034), None);
    // A range that would wrap the address space.
    assert_eq!(relro_seal::relro_range(&[phdr(6, 0, 0), phdr(0x6474_e552, 0xffff_f000, 0x2000)], 0x1000), None);
}

fn phdr64(kind: u32, vaddr: u64, memsz: u64) -> [u64; 7] {
    [kind as u64 | 4 << 32, vaddr, vaddr, vaddr, memsz, memsz, 8]
}

#[test]
fn relro_range64_reads_elf64_headers_the_same_way() {
    // An AArch64 PIE: PHDR at 0x40, loaded far above 4 GiB.
    let headers = [phdr64(6, 0x40, 0x230), phdr64(1, 0, 0x9d194), phdr64(0x6474_e552, 0x1e_5640, 0x79c0)];
    for base in [0x40_0000u64, 0x1000_0000_0000, 0] {
        assert_eq!(relro_seal::relro_range64(&headers, base + 0x40), Some((base + 0x1e_5000, base + 0x1ed_000)), "{base:#x}");
    }
    // p_flags in the high half of the first word does not hide the type.
    assert_eq!(relro_seal::relro_range64(&[phdr64(6, 0x40, 0x40)], 0x40_0040), None);
    assert_eq!(relro_seal::relro_range64(&[], 0x40_0040), None);
    assert_eq!(relro_seal::relro_range64(&[phdr64(6, 0, 0), phdr64(0x6474_e552, u64::MAX - 0xfff, 0x2000)], 0x1000), None);
}

#[test]
fn the_policy_lets_relro_drop_write_and_never_regain_execute() {
    // A loaded static PIE (home's segments at base 0x400000, 4 KiB rounded):
    // constants, code, RELRO, data.
    let b = 0x40_0000;
    let mut vmas = regions(&[(b, b + 0x9e000, R), (b + 0xad000, b + 0x1d6000, RX),
                             (b + 0x1e5000, b + 0x1ed000, RW), (b + 0x1fc000, b + 0x1fe000, RW)]);
    let (start, end) = (b + 0x1e5000, b + 0x1ed000);
    assert!(!vma::refuses(&vmas, start, end, PROT_READ, false), "mprotect(RELRO, PROT_READ) is allowed");
    vma::insert(&mut vmas, start, end, R);
    // Sealed: no write from the app (the page is HW_R) nor from the runtime's copies.
    assert_eq!(vma::hardware(vma::union(&vmas, start, start + 0x1000) & 7), HW_R);
    assert!(!vma::permits(&vmas, start + 0x640, 4, PROT_WRITE));
    assert!(vma::permits(&vmas, b + 0x1fc000, 4, PROT_WRITE), "ordinary data stays writable");
    // Turning it back into code is refused; read-write again is the app's own choice, as on Linux.
    assert!(vma::refuses(&vmas, start, end, RX, false));
    assert!(vma::refuses(&vmas, start, end, RW | PROT_EXEC, false));
}
