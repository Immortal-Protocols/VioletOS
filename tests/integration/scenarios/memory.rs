//! Open-app memory protection: what targets/qemu/test_wx.py and its wxprobe
//! app checked, against the shipped vma.rs, procmgr.rs, elf.rs and
//! entropy.rs. The fake MMU (kernel/ `sys`) holds every mapping procmgr.rs
//! makes and panics on one that is writable and executable; an access it
//! does not serve goes to the runtime's fault handler, as on the phone.
//! That real page tables do the same is the one thing kept in QEMU
//! (targets/qemu/test_system.py, wxprobe).
use crate::open::front::Touched;
use crate::open::vma::Access::{Exec, Read, Write};
use crate::world::*;
use std::collections::BTreeSet;

const NONE: u32 = 0;
const R: u32 = 1;
const RW: u32 = 3;
const RX: u32 = 5;
const RWX: u32 = 7;
const ANON: u32 = 0x22; // MAP_PRIVATE | MAP_ANONYMOUS
const FIXED: u32 = 0x10;
const PRIVATE: u32 = 2;
/// world::program: constants, code, data.
const CONSTANT: u32 = 0x10000;
const CODE: u32 = 0x20d90;
const CODE_PAGE: u32 = 0x20000;
const DATA: u32 = 0x39cf0;

fn phone() -> (World, usize) {
    let mut w = World::boot(Image::new().app("wxprobe", ""));
    let p = w.open_app("wxprobe");
    (w, p)
}
fn ended(w: &World, name: &str, access: &str) {
    assert_eq!(w.logged(&format!("{name} (pid")), 2, "{}", w.log());
    assert_eq!(w.logged(&format!("bad memory access ({access})")), 1, "{}", w.log());
    assert_eq!(w.logged("exited with status 139"), 1);
}

#[test]
fn no_writable_and_executable_memory_and_no_data_turned_into_code() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    assert_eq!(p.mmap(0, 65536, RWX, ANON, -1), Err(EACCES));
    assert_eq!(p.mmap(0, 65536, RX, ANON, -1), Err(EACCES));
    let data = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    assert_eq!(p.touch(data, Write), Touched::Ok);
    assert_eq!(p.mprotect(data, 65536, RX), Err(EACCES), "RW -> RX");
    assert_eq!(p.mprotect(data, 65536, R), Ok(0));
    assert_eq!(p.mprotect(data, 65536, RX), Err(EACCES), "R -> RX");
    assert_eq!(p.mprotect(CODE_PAGE, 4096, RWX), Err(EACCES), "code made RWX");
    assert_eq!(p.mprotect(CODE_PAGE, 4096, RW), Err(EACCES), "part of a code page made writable");
    assert_eq!(p.touch(CODE, Exec), Touched::Ok, "code still runs");
    assert_eq!(p.mapping(CODE), Some((false, true)));
    assert_eq!(p.mapping(data), Some((false, false)));
}

#[test]
fn nothing_of_the_apps_goes_into_a_page_that_holds_code() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    p.write_file("/tmp/wxprobe.code", &[0x1e, 0xff, 0x2f, 0xe1].repeat(1024)).unwrap();
    let shell = p.open("/tmp/wxprobe.code", O_RDWR).unwrap();
    let beside = CODE_PAGE + 0xf000;
    assert_eq!(p.mmap(CODE_PAGE, 4096, R, PRIVATE | FIXED, shell), Err(EACCES), "file mapped over code");
    assert_eq!(p.mmap(beside, 4096, R, PRIVATE | FIXED, shell), Err(EACCES), "file mapped beside code");
    assert_eq!(p.mmap(beside, 4096, NONE, PRIVATE | FIXED, shell), Err(EACCES), "PROT_NONE file beside code");
    assert_eq!(p.mmap(beside, 4096, R, ANON | FIXED, -1), Err(EACCES), "anonymous read-only beside code");
    for page in (CODE_PAGE..CODE_PAGE + 0x10000).step_by(4096) {
        assert_eq!(p.mmap(page, 4096, R, PRIVATE | FIXED, shell), Err(EACCES), "file mapped at {page:#x}");
    }
    assert!(p.mprotect(beside, 4096, R).is_err(), "a read-only hole in a page of code");
    assert_eq!(p.touch(CODE, Exec), Touched::Ok);
    // The code is as loaded: no byte of the file reached it.
    let rt = &mut w.rt;
    let app = rt.apps[probe].as_mut().unwrap();
    let mut code = [0u8; 16];
    assert!(app.space.read(&mut rt.k, &mut rt.pool, u64::from(CODE), &mut code));
    assert_eq!(code, [0x22; 16]);
}

#[test]
fn the_trampoline_page_is_the_runtimes() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let stubs = p.layout().stubs as u32; // (an AArch32 app: a 32-bit address)
    p.write_file("/tmp/wxprobe.code", &[0xcc; 4096]).unwrap();
    let shell = p.open("/tmp/wxprobe.code", O_RDWR).unwrap();
    assert_eq!(p.munmap(stubs, 65536), Err(EINVAL));
    assert_eq!(p.munmap(0xbf10_0000, 0xe0_0000), Err(EINVAL), "munmap of every candidate");
    assert_eq!(p.mmap(stubs, 65536, R, PRIVATE | FIXED, shell), Err(EACCES));
    assert_eq!(p.mmap(stubs + 4096, 4096, R, PRIVATE | FIXED, shell), Err(EACCES));
    assert_eq!(p.mmap(stubs, 65536, RW, ANON | FIXED, -1), Err(EACCES));
    assert_eq!(p.mprotect(stubs, 65536, R), Err(EACCES));
    assert_eq!(p.syscall(3, [shell as u32, stubs, 8, 0, 0, 0]), EFAULT, "read() into the trampoline page");
    assert_eq!(p.touch(stubs, Exec), Touched::Ok, "a thread still starts through it");
    assert_eq!(p.touch(stubs, Write), Touched::Ended(139));
}

#[test]
fn system_calls_do_not_write_where_the_app_may_not() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let fd = p.open("/apps/wxprobe", O_RDONLY).unwrap() as u32;
    assert_eq!(p.syscall(3, [fd, CODE, 16, 0, 0, 0]), EFAULT, "read() into code");
    assert_eq!(p.syscall(3, [fd, CONSTANT, 4, 0, 0, 0]), EFAULT, "read() into constants");
    let data = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    assert_eq!(p.mprotect(data, 65536, R), Ok(0));
    assert_eq!(p.syscall(3, [fd, data, 16, 0, 0, 0]), EFAULT, "read() into read-only memory");
    assert_eq!(p.syscall(384, [CODE, 16, 0, 0, 0, 0]), EFAULT, "getrandom() into code");
    // A buffer that starts writable and runs into read-only memory: EFAULT
    // with nothing written, not a partial fill.
    let two = p.mmap(0, 131072, RW, ANON, -1).unwrap();
    assert_eq!(p.mprotect(two + 65536, 65536, R), Ok(0));
    assert_eq!(p.syscall(384, [two + 65536 - 512, 1024, 0, 0, 0, 0]), EFAULT);
    // PROT_NONE refuses both ways and keeps its contents.
    let hidden = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    let va = p.string(0, "kept");
    assert_eq!(p.syscall(384, [hidden, 16, 0, 0, 0, 0]), 16);
    assert_eq!(p.mprotect(hidden, 65536, NONE), Ok(0));
    assert_eq!(p.syscall(3, [fd, hidden, 16, 0, 0, 0]), EFAULT, "read() into PROT_NONE");
    let out = p.open("/tmp/out", O_WRONLY | O_CREAT).unwrap() as u32;
    assert_eq!(p.syscall(4, [out, hidden, 4, 0, 0, 0]), EFAULT, "write() from PROT_NONE");
    assert_eq!(p.syscall(4, [out, va, 4, 0, 0, 0]), 4);
    assert_eq!(p.mprotect(hidden, 65536, RW), Ok(0));
    let rt = &mut w.rt;
    let app = rt.apps[probe].as_mut().unwrap();
    let (mut code, mut before, mut kept) = ([0u8; 16], [1u8; 512], [0u8; 16]);
    assert!(app.space.read(&mut rt.k, &mut rt.pool, u64::from(CODE), &mut code) && code == [0x22; 16], "code unchanged");
    assert!(app.space.read(&mut rt.k, &mut rt.pool, u64::from(two + 65536 - 512), &mut before) && before == [0; 512], "nothing written before the refusal");
    assert!(app.space.read(&mut rt.k, &mut rt.pool, u64::from(hidden), &mut kept) && kept != [0; 16], "PROT_NONE keeps contents");
}

#[test]
fn getrandom_is_seeded_and_never_repeats() {
    let (mut w, probe) = phone();
    let buf = w.app(probe).mmap(0, 65536, RW, ANON, -1).unwrap();
    let mut seen = BTreeSet::new();
    for _ in 0..8 {
        assert_eq!(w.app(probe).syscall(384, [buf, 32, 0, 0, 0, 0]), 32);
        seen.insert(read32(&mut w, probe, buf));
    }
    assert_eq!(seen.len(), 8);
    assert!(!seen.contains(&[0u8; 32]));
}
fn read32(w: &mut World, i: usize, va: u32) -> [u8; 32] {
    let rt = &mut w.rt;
    let app = rt.apps[i].as_mut().unwrap();
    let mut out = [0u8; 32];
    assert!(app.space.read(&mut rt.k, &mut rt.pool, u64::from(va), &mut out));
    out
}

#[test]
fn no_app_starts_until_the_generator_is_seeded() {
    let mut w = World::boot(Image::new().app("wxprobe", ""));
    w.rt.entropy = crate::open::entropy::Drbg::new(crate::open::sha256);
    assert!(w.start("/apps/wxprobe", None).is_err());
    assert!(w.rt.apps.iter().flatten().all(|a| a.name == "home"));
}

/// The six things the hardware must stop, each ending the app with status
/// 139 and the kind of access in the log (the QEMU test's PROBES).
#[test]
fn a_write_to_code_ends_the_app() {
    let (mut w, probe) = phone();
    assert_eq!(w.app(probe).touch(CODE, Write), Touched::Ended(139));
    ended(&w, "wxprobe", "Write");
    assert!(!w.running(probe));
}
#[test]
fn a_write_to_a_constant_ends_the_app() {
    let (mut w, probe) = phone();
    assert_eq!(w.app(probe).touch(CONSTANT, Read), Touched::Ok);
    assert_eq!(w.app(probe).touch(CONSTANT, Write), Touched::Ended(139));
    ended(&w, "wxprobe", "Write");
}
#[test]
fn running_the_stack_ends_the_app() {
    let (mut w, probe) = phone();
    let sp = w.app(probe).sp();
    assert_eq!(w.app(probe).touch(sp - 64, Write), Touched::Ok);
    assert_eq!(w.app(probe).touch(sp - 64, Exec), Touched::Ended(139));
    ended(&w, "wxprobe", "Exec");
}
#[test]
fn running_the_heap_ends_the_app() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let heap = p.brk(0);
    assert_eq!(p.brk(heap + 0x4000), heap + 0x4000);
    assert_eq!(p.touch(heap, Write), Touched::Ok);
    assert_eq!(p.touch(DATA, Write), Touched::Ok, "data is writable");
    assert_eq!(p.touch(heap, Exec), Touched::Ended(139));
    ended(&w, "wxprobe", "Exec");
}
#[test]
fn reading_prot_none_memory_ends_the_app() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let hidden = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    assert_eq!(p.touch(hidden, Write), Touched::Ok);
    assert_eq!(p.mprotect(hidden, 65536, NONE), Ok(0));
    assert_eq!(p.mapping(hidden), None, "the hardware mapping went with the protection");
    assert_eq!(p.touch(hidden, Read), Touched::Ended(139));
    ended(&w, "wxprobe", "Read");
}
#[test]
fn a_write_to_memory_made_read_only_after_it_was_mapped_ends_the_app() {
    // The page was mapped writable all along: only a change of the hardware
    // mapping in place stops this store.
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let data = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    assert_eq!(p.touch(data, Write), Touched::Ok);
    assert_eq!(p.mapping(data), Some((true, false)));
    assert_eq!(p.mprotect(data, 65536, R), Ok(0));
    assert_eq!(p.mapping(data), Some((false, false)), "remapped read-only in place");
    assert_eq!(p.touch(data, Read), Touched::Ok);
    assert_eq!(p.touch(data, Write), Touched::Ended(139));
    ended(&w, "wxprobe", "Write");
}
#[test]
fn a_harmless_run_exits_cleanly() {
    let (mut w, probe) = phone();
    assert_eq!(w.app(probe).syscall(248, [0; 6]), 0);
    assert_eq!(w.logged("wxprobe (pid 101) exited with status 0"), 1, "{}", w.log());
    assert_eq!(w.logged("bad memory access"), 0);
}

/// Across launches (and boots) the stack, heap, mmap area, trampoline page
/// and AT_RANDOM move.
#[test]
fn the_layout_moves_between_launches_and_boots() {
    let image = Image::new().app("wxprobe", "");
    let (mut stacks, mut heaps, mmaps, mut stubs, mut randoms) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::<u32>::new(), BTreeSet::new(), BTreeSet::new());
    let mut mmaps = mmaps;
    let mut launches = 0;
    for boot in 1..=3u64 {
        let mut w = World::boot_with(image.clone(), Persisted::default(), boot);
        for _ in 0..7 {
            let i = w.open_app("wxprobe");
            let mut p = w.app(i);
            let (l, sp, random) = (p.layout(), p.sp(), p.at_random());
            assert!(u64::from(sp) < l.stack_top && u64::from(sp) > l.stack_top - 0x2_0000);
            stacks.insert(sp);
            heaps.insert(p.brk(0));
            mmaps.insert(p.mmap(0, 65536, RW, ANON, -1).unwrap());
            stubs.insert(l.stubs);
            assert_ne!(random, [0; 16], "AT_RANDOM is zero");
            randoms.insert(random);
            launches += 1;
            w.rt.end(i, 0);
            w.front(None);
        }
    }
    // 19 bits for the stack and 128 for AT_RANDOM never repeat; mmap has 12
    // bits and the heap 8, so allow a rare repeat there.
    assert_eq!(stacks.len(), launches);
    assert_eq!(randoms.len(), launches);
    assert!(mmaps.len() >= launches - 1, "{mmaps:x?}");
    assert!(heaps.len() >= launches / 2, "{heaps:x?}");
    assert!(stubs.len() >= launches / 2);
}

#[test]
fn a_program_with_writable_code_is_not_loaded() {
    // PF_W|PF_X in one segment, and code sharing a 64 KiB page with data.
    let rwx = elf(2, 0x20000, &[(0x20000, vec![0x22; 0x100], 0x100, 7)]);
    let mixed = elf(2, 0x20000, &[(0x20000, vec![0x22; 0x100], 0x100, 5), (0x28000, vec![0x33; 0x100], 0x100, 6)]);
    let mut w = World::boot(Image::new().file("/apps/rwx", &rwx).file("/apps/mixed", &mixed));
    assert!(w.start("/apps/rwx", None).is_err());
    assert!(w.start("/apps/mixed", None).is_err());
    assert_eq!(w.rt.apps.iter().flatten().count(), 1, "only Home runs");
}

/// With the 4 KiB pages the phone ships (runtime/open/sys.rs), one page in
/// the middle of a 64 KiB mapping can be a guard: procmgr.rs gives every
/// page its own rights. (With 64 KiB pages this cannot be expressed.)
#[cfg(not(page64))]
#[test]
fn a_one_page_guard_inside_a_mapping_is_enforced() {
    assert_eq!(crate::kernel::sys::PAGE, 4096);
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let data = p.mmap(0, 65536, RW, ANON, -1).unwrap();
    assert_eq!(p.touch(data, Write), Touched::Ok);
    assert_eq!(p.mprotect(data + 4096, 4096, NONE), Ok(0));
    assert_eq!(p.touch(data + 8192, Write), Touched::Ok, "the page after the guard is still the app's");
    assert_eq!(p.mapping(data), Some((true, false)), "the page before keeps its rights");
    assert_eq!(p.mapping(data + 4096), None, "the guard page is not mapped");
    assert_eq!(p.touch(data + 4096, Read), Touched::Ended(139));
    ended(&w, "wxprobe", "Read");
}

/// procmgr.rs read-ahead (AHEAD = 4 with 4 KiB pages): first touches in a
/// row bring the next pages of the region in with the fault, never a page
/// past the region's end, and never with rights the region does not give.
#[cfg(not(page64))]
#[test]
fn read_ahead_stays_inside_the_region_and_keeps_its_rights() {
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    // Three pages, then a read-only page right after them.
    let base = p.mmap(0, 4 * 4096, RW, ANON, -1).unwrap();
    assert_eq!(p.mprotect(base + 3 * 4096, 4096, R), Ok(0));
    assert_eq!(p.touch(base, Write), Touched::Ok);
    assert_eq!(p.mapping(base + 4096), None, "a touch on its own gets its one page");
    assert_eq!(p.touch(base + 4096, Write), Touched::Ok);
    assert_eq!(p.mapping(base + 2 * 4096), Some((true, false)), "the next page came with the fault");
    assert_ne!(p.mapping(base + 3 * 4096), Some((true, false)), "read-ahead made a read-only page writable");
    assert_eq!(p.touch(base + 3 * 4096, Write), Touched::Ended(139), "the read-only page refuses a store");
    // A longer run: 16 pages are served by far fewer faults than 16.
    let (mut w, probe) = phone();
    let mut p = w.app(probe);
    let run = p.mmap(0, 16 * 4096, RW, ANON, -1).unwrap();
    let before = crate::kernel::sys::HW.with(|h| h.borrow().maps);
    let mut faults = 0;
    for n in 0..16 {
        if p.mapping(run + n * 4096).is_none() {
            faults += 1;
        }
        assert_eq!(p.touch(run + n * 4096, Write), Touched::Ok);
    }
    assert!(faults <= 6, "{faults} faults for 16 pages in a row");
    assert!(crate::kernel::sys::HW.with(|h| h.borrow().maps) - before >= 16);
    assert!(p.mapping(run + 16 * 4096).is_none(), "nothing mapped past the region");
}
