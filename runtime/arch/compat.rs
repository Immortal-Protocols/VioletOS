//! Structures whose layout differs between the AArch32 and AArch64 Linux
//! ABIs (the `Layout::Words` calls of linux.rs), read and written for either
//! ABI. Pure code: the Open Runtime's handlers keep one meaning per call and
//! use these at the edge where they read or write app memory.
//!
//! Two kinds:
//! - Pointer-free structures the runtime fills in (stat, timeval, rlimit,
//!   rusage): the AArch64 bytes are made from the AArch32 bytes the existing
//!   handler writes, so the handler stays one.
//! - Structures that hold pointers (iovec, msghdr, cmsghdr, sigaction,
//!   sigaltstack): read into 64-bit fields for either ABI, since a 64-bit
//!   app's addresses do not fit the 32-bit forms.
use super::Abi;

fn get(b: &[u8], at: usize, bytes: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(bytes)?)?;
    let mut w = [0u8; 8];
    w[..bytes].copy_from_slice(s);
    Some(u64::from_le_bytes(w))
}
fn put(b: &mut [u8], at: usize, bytes: usize, v: u64) {
    b[at..at + bytes].copy_from_slice(&v.to_le_bytes()[..bytes]);
}
fn word(abi: &Abi) -> usize {
    abi.word_bytes()
}
/// A word of the app's ABI at `at`.
pub fn get_word(abi: &Abi, b: &[u8], at: usize) -> Option<u64> {
    get(b, at, word(abi))
}
pub fn put_word(abi: &Abi, b: &mut [u8], at: usize, v: u64) {
    put(b, at, word(abi), v)
}

// ---- stat ----

/// AArch32 `struct stat64` (fstat64, fstatat64) and AArch64 `struct stat`
/// (fstat, newfstatat).
pub const STAT32: usize = 104;
pub const STAT64: usize = 128;
/// The AArch64 `struct stat` holding what an AArch32 `struct stat64` holds.
pub fn stat_to_aarch64(s: &[u8; STAT32]) -> [u8; STAT64] {
    let g = |at, n| get(s, at, n).unwrap_or(0);
    let mut d = [0u8; STAT64];
    put(&mut d, 0, 8, g(0, 8)); // st_dev
    put(&mut d, 8, 8, g(96, 8)); // st_ino (the 64-bit one)
    for at in [16, 20, 24, 28] {
        put(&mut d, at, 4, g(at, 4)); // mode, nlink, uid, gid
    }
    put(&mut d, 32, 8, g(32, 8)); // st_rdev
    put(&mut d, 48, 8, g(48, 8)); // st_size
    put(&mut d, 56, 4, g(56, 4)); // st_blksize
    put(&mut d, 64, 8, g(64, 8)); // st_blocks
    // atime, mtime, ctime: 32-bit seconds and nanoseconds become 64-bit.
    for (k, at) in [72usize, 80, 88].iter().enumerate() {
        put(&mut d, 72 + 16 * k, 8, g(*at, 4) as u32 as i32 as i64 as u64);
        put(&mut d, 80 + 16 * k, 8, g(at + 4, 4));
    }
    d
}

// ---- time and limits ----

/// struct timeval / the old struct timespec with a `long` seconds field.
pub fn timeval(abi: &Abi, seconds: i64, fraction: i64) -> ([u8; 16], usize) {
    let mut b = [0u8; 16];
    let w = word(abi);
    put(&mut b, 0, w, seconds as u64);
    put(&mut b, w, w, fraction as u64);
    (b, 2 * w)
}
/// struct rlimit (getrlimit/setrlimit; prlimit64 is 64-bit on both).
/// AArch32's RLIM_INFINITY is all ones in 32 bits, AArch64's in 64.
pub fn rlimit(abi: &Abi, current: u64, maximum: u64) -> ([u8; 16], usize) {
    let w = word(abi);
    let clamp = |v: u64| if w == 4 && v > 0xffff_ffff { 0xffff_ffff } else { v };
    let mut b = [0u8; 16];
    put(&mut b, 0, w, clamp(current));
    put(&mut b, w, w, clamp(maximum));
    (b, 2 * w)
}
pub fn read_rlimit(abi: &Abi, b: &[u8]) -> Option<(u64, u64)> {
    let w = word(abi);
    let widen = |v: u64| if w == 4 && v == 0xffff_ffff { u64::MAX } else { v };
    Some((widen(get(b, 0, w)?), widen(get(b, w, w)?)))
}
/// struct rusage: two timevals, then fourteen longs.
pub const RUSAGE32: usize = 72;
pub const RUSAGE64: usize = 144;
pub fn rusage_to_aarch64(s: &[u8; RUSAGE32]) -> [u8; RUSAGE64] {
    let mut d = [0u8; RUSAGE64];
    for k in 0..18 {
        let v = get(s, 4 * k, 4).unwrap_or(0) as u32 as i32 as i64;
        put(&mut d, 8 * k, 8, v as u64);
    }
    d
}

// ---- pointer-holding structures ----

/// struct iovec: base, length.
pub fn iovec_bytes(abi: &Abi) -> usize {
    2 * word(abi)
}
pub fn iovec(abi: &Abi, b: &[u8], index: usize) -> Option<(u64, u64)> {
    let at = index.checked_mul(iovec_bytes(abi))?;
    Some((get_word(abi, b, at)?, get_word(abi, b, at + word(abi))?))
}

/// struct msghdr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Msghdr {
    pub name: u64,
    pub namelen: u32,
    pub iov: u64,
    pub iovlen: u64,
    pub control: u64,
    pub controllen: u64,
    pub flags: u32,
}
pub fn msghdr_bytes(abi: &Abi) -> usize {
    if word(abi) == 4 { 28 } else { 56 }
}
pub fn msghdr(abi: &Abi, b: &[u8]) -> Option<Msghdr> {
    if word(abi) == 4 {
        let g = |at| get(b, at, 4);
        Some(Msghdr { name: g(0)?, namelen: g(4)? as u32, iov: g(8)?, iovlen: g(12)?, control: g(16)?, controllen: g(20)?, flags: g(24)? as u32 })
    } else {
        // name 8, namelen 4 + pad, iov 8, iovlen 8, control 8, controllen 8, flags 4 + pad.
        let g = |at, n| get(b, at, n);
        Some(Msghdr { name: g(0, 8)?, namelen: g(8, 4)? as u32, iov: g(16, 8)?, iovlen: g(24, 8)?, control: g(32, 8)?, controllen: g(40, 8)?, flags: g(48, 4)? as u32 })
    }
}
/// Write back msg_namelen, msg_controllen and msg_flags (recvmsg's outputs).
pub fn msghdr_outputs(abi: &Abi, b: &mut [u8], namelen: u32, controllen: u64, flags: u32) {
    if word(abi) == 4 {
        put(b, 4, 4, u64::from(namelen));
        put(b, 20, 4, controllen);
        put(b, 24, 4, u64::from(flags));
    } else {
        put(b, 8, 4, u64::from(namelen));
        put(b, 40, 8, controllen);
        put(b, 48, 4, u64::from(flags));
    }
}
/// struct cmsghdr: cmsg_len (a size_t), level, type; data aligned to a word.
pub fn cmsg_header(abi: &Abi) -> usize {
    word(abi) + 8
}
pub fn cmsg_align(abi: &Abi, n: u64) -> u64 {
    let w = word(abi) as u64;
    n.saturating_add(w - 1) & !(w - 1)
}
/// CMSG_LEN for `data` bytes, and CMSG_SPACE.
pub fn cmsg_len(abi: &Abi, data: u64) -> u64 {
    cmsg_align(abi, cmsg_header(abi) as u64) + data
}
pub fn cmsg_space(abi: &Abi, data: u64) -> u64 {
    cmsg_align(abi, cmsg_header(abi) as u64) + cmsg_align(abi, data)
}
/// One control message at `at` in `control`: (cmsg_len, level, type, data
/// offset). None if it does not fit `control`.
pub fn cmsg(abi: &Abi, control: &[u8], at: usize) -> Option<(u64, u32, u32, usize)> {
    let w = word(abi);
    let len = get(control, at, w)?;
    let level = get(control, at + w, 4)? as u32;
    let kind = get(control, at + w + 4, 4)? as u32;
    let data = at + cmsg_align(abi, cmsg_header(abi) as u64) as usize;
    if len < cmsg_header(abi) as u64 || (at as u64).checked_add(len)? > control.len() as u64 {
        return None;
    }
    Some((len, level, kind, data))
}

/// struct sigaction as rt_sigaction takes it (the kernel's, not libc's):
/// handler, flags, restorer, mask (8 bytes on both).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sigaction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}
pub fn sigaction_bytes(abi: &Abi) -> usize {
    3 * word(abi) + 8
}
pub fn sigaction(abi: &Abi, b: &[u8]) -> Option<Sigaction> {
    let w = word(abi);
    Some(Sigaction { handler: get(b, 0, w)?, flags: get(b, w, w)?, restorer: get(b, 2 * w, w)?, mask: get(b, 3 * w, 8)? })
}
pub fn put_sigaction(abi: &Abi, a: &Sigaction) -> ([u8; 32], usize) {
    let w = word(abi);
    let mut b = [0u8; 32];
    put(&mut b, 0, w, a.handler);
    put(&mut b, w, w, a.flags);
    put(&mut b, 2 * w, w, a.restorer);
    put(&mut b, 3 * w, 8, a.mask);
    (b, sigaction_bytes(abi))
}
/// stack_t: ss_sp, ss_flags (int), ss_size (size_t).
pub fn stack_t(abi: &Abi, b: &[u8]) -> Option<(u64, u32, u64)> {
    let w = word(abi);
    Some((get(b, 0, w)?, get(b, w, 4)? as u32, get(b, 2 * w, w)?))
}
pub fn put_stack_t(abi: &Abi, sp: u64, flags: u32, size: u64) -> ([u8; 24], usize) {
    let w = word(abi);
    let mut b = [0u8; 24];
    put(&mut b, 0, w, sp);
    put(&mut b, w, 4, u64::from(flags));
    put(&mut b, 2 * w, w, size);
    (b, 3 * w)
}

// ---- register packing ----

/// mmap's offset as the dispatcher's mmap2 takes it: pages. AArch64 passes
/// bytes, which must then be page aligned (EINVAL otherwise: None).
pub fn mmap_pages(abi: &Abi, offset: u64) -> Option<u64> {
    if word(abi) == 4 {
        Some(offset)
    } else if offset % abi.page_size == 0 {
        Some(offset / abi.page_size)
    } else {
        None
    }
}
