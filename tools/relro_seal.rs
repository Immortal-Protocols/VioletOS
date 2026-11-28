//! Makes a static PIE's RELRO read-only once the program has relocated itself.
//!
//! A position-independent program keeps its constant pointer tables (Rust
//! trait-object vtables, C function-pointer tables, the GOT, .init_array) in
//! PT_GNU_RELRO: memory its start code must write once, to add the load
//! address, and that nothing writes after. musl's dynamic loader makes that
//! range read-only for a dynamically linked program; a static PIE has no
//! loader, and musl's self-relocating start file (rcrt1.o) never does it. So
//! every static PIE in the image is linked with this object
//! (tools/build_initfs.py `pie_flags`, `c_toolchain`; tools/build_python.py):
//! its .init_array entry runs after rcrt1 has applied every relocation and
//! before main, and asks the Open Runtime for PROT_READ over the range (the
//! W^X policy allows dropping write: runtime/open/vma.rs `refuses`). From
//! then on a write there faults, in the app and in the runtime's copies for
//! system calls (procmgr.rs `Space::write` checks the same regions).
//!
//! If the range cannot be found or sealed the program stops (abort) rather
//! than run with its tables writable. tools/elf_check.py refuses a static
//! PIE without this object (VIOLET_RELRO_SEAL), without PT_GNU_RELRO, or
//! whose RELRO does not end on a page boundary.
//!
//! Built with `rustc --crate-type=lib --emit=obj` (no std, no panics, no
//! calls into core): it needs only musl's getauxval, mprotect and abort.
//! `relro_range` is plain arithmetic, tested on the host in
//! runtime/open/tests/memres_host.rs.
#![no_std]

/// The bytes tools/elf_check.py looks for (SEAL_MARK there).
#[no_mangle]
#[used]
pub static VIOLET_RELRO_SEAL: [u8; 20] = *b"violet:relro-seal:1\0";

const PT_PHDR: u32 = 6;
const PT_GNU_RELRO: u32 = 0x6474_e552;
/// The Open Runtime's page (AT_PAGESZ, procmgr.rs).
pub const PAGE: u32 = 4096;

/// The range to make read-only, from the program headers (`phdrs`, at
/// address `at` in memory): PT_GNU_RELRO moved by the load bias that
/// PT_PHDR gives, its start rounded down to a page (the page's head is the
/// same segment's mapping) and its end rounded down (a partial last page
/// stays writable; elf_check refuses a program with one). None if either
/// header is missing or the range wraps (32-bit addresses, as on the phone).
pub fn relro_range(phdrs: &[[u32; 8]], at: u32) -> Option<(u32, u32)> {
    let (mut bias, mut relro) = (None, None);
    for p in phdrs {
        if p[0] == PT_PHDR {
            bias = Some(at.wrapping_sub(p[2]));
        } else if p[0] == PT_GNU_RELRO {
            relro = Some((p[2], p[5]));
        }
    }
    let (bias, (va, size)) = (bias?, relro?);
    let start = bias.checked_add(va)?;
    let end = start.checked_add(size)?;
    Some((start & !(PAGE - 1), end & !(PAGE - 1)))
}

/// `relro_range` for ELF64 program headers (AArch64): each row is the 56-byte
/// header as seven words, the first holding p_type in its low half and
/// p_flags in its high half. None if either header is missing or the range wraps.
pub fn relro_range64(phdrs: &[[u64; 7]], at: u64) -> Option<(u64, u64)> {
    let (mut bias, mut relro) = (None, None);
    for p in phdrs {
        let kind = p[0] as u32;
        if kind == PT_PHDR {
            bias = Some(at.wrapping_sub(p[2]));
        } else if kind == PT_GNU_RELRO {
            relro = Some((p[2], p[5]));
        }
    }
    let (bias, (va, size)) = (bias?, relro?);
    let start = bias.checked_add(va)?;
    let end = start.checked_add(size)?;
    let page = PAGE as u64;
    Some((start & !(page - 1), end & !(page - 1)))
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod start64 {
    extern "C" {
        fn getauxval(kind: u64) -> u64;
        fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
        fn abort() -> !;
    }
    const AT_PHDR: u64 = 3;
    const AT_PHENT: u64 = 4;
    const AT_PHNUM: u64 = 5;
    const PROT_READ: i32 = 1;

    unsafe extern "C" fn seal() {
        let _ = core::ptr::read_volatile(super::VIOLET_RELRO_SEAL.as_ptr());
        let (at, entry, count) = (getauxval(AT_PHDR), getauxval(AT_PHENT), getauxval(AT_PHNUM) as usize);
        if at == 0 || entry != 56 || at % 8 != 0 {
            abort();
        }
        let phdrs = core::slice::from_raw_parts(at as *const [u64; 7], count);
        match super::relro_range64(phdrs, at) {
            Some((start, end)) if end > start => {
                if mprotect(start as *mut u8, (end - start) as usize, PROT_READ) != 0 {
                    abort();
                }
            }
            Some(_) => {}
            None => abort(),
        }
    }

    #[used]
    #[link_section = ".init_array"]
    static SEAL: unsafe extern "C" fn() = seal;
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
mod start {
    extern "C" {
        fn getauxval(kind: u32) -> u32;
        fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
        fn abort() -> !;
    }
    const AT_PHDR: u32 = 3;
    const AT_PHENT: u32 = 4;
    const AT_PHNUM: u32 = 5;
    const PROT_READ: i32 = 1;

    unsafe extern "C" fn seal() {
        // Keeps the mark in the program (the linker drops what nothing reads).
        let _ = core::ptr::read_volatile(super::VIOLET_RELRO_SEAL.as_ptr());
        let (at, entry, count) = (getauxval(AT_PHDR), getauxval(AT_PHENT), getauxval(AT_PHNUM) as usize);
        if at == 0 || entry != 32 {
            abort();
        }
        let phdrs = core::slice::from_raw_parts(at as *const [u32; 8], count);
        match super::relro_range(phdrs, at) {
            Some((start, end)) if end > start => {
                if mprotect(start as *mut u8, (end - start) as usize, PROT_READ) != 0 {
                    abort();
                }
            }
            // Less than a page of RELRO: nothing whole to seal.
            Some(_) => {}
            None => abort(),
        }
    }

    // Run by musl's __libc_start_main (libc_start_init) after rcrt1's
    // relocation and before main.
    #[used]
    #[link_section = ".init_array"]
    static SEAL: unsafe extern "C" fn() = seal;
}
