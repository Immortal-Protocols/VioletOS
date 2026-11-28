//! An AArch64 program's system call, as the Open Runtime's one dispatcher
//! takes it: the AArch32 call of the same meaning, its arguments in 32-bit
//! words, and the structure the dispatcher writes, made into the AArch64
//! one after it ran (`After`).
//!
//! Every app's address space is below 4 GiB (runtime/open/vma.rs), so an
//! address an AArch64 program passes fits a 32-bit word. A word that cannot
//! name the app's memory becomes NOWHERE, an address in no app's regions (so
//! a pointer is EFAULT and a size too large), never cut to some low address
//! that might be the app's: for an address or size argument (`MEMORY`) any
//! value of 4 GiB or more, sign-extended ones included (0xffff_ffff_bef0_0000
//! is a kernel-half address on Linux, not the app's stack at 0xbef0_0000);
//! for an int argument (fd, AT_FDCWD, pid -1, flags with bit 31) a value that
//! is neither below 4 GiB nor a sign-extended int (`narrow`). It is not
//! refused outright: the registers a call does not use hold whatever the
//! program left there (musl passes three for write, Linux reads three), and
//! those must not fail the call.
//!
//! On the way back (`widen`), x0 of an answered call is its 32-bit result
//! made 64-bit (an error -4095..-1 sign-extended), never the caller's x0;
//! the other registers are the thread's own unless the runtime changed them.
//!
//! Three kinds of call, by linux.rs `Layout`:
//! - `Same`: the arguments narrowed, the dispatcher's handler as it is.
//! - `Words` whose 64-bit values AArch32 splits into register pairs (lseek,
//!   pread64, ftruncate, mmap's byte offset...) or passes another way
//!   (nanosleep is clock_nanosleep; getrlimit is prlimit64): rewritten here.
//! - `Words` whose structures differ: pointer-free ones the dispatcher fills
//!   in are converted after it (`After`, from compat.rs); the ones holding
//!   pointers (iovec, sigaction, stack_t, argv, clone) are read by the
//!   dispatcher itself in the app's layout (`WIDE_IN_DISPATCH`). The rest are
//!   ENOSYS, named by `unconverted`.
use super::compat;
use super::linux::Layout;
use super::{internal_call, Word, AARCH64};

pub const EFAULT: i32 = -14;
pub const EINVAL: i32 = -22;
pub const ENOSYS: i32 = -38;
pub const EOVERFLOW: i32 = -75;

/// What the dispatcher wrote for the AArch32 call, at this app address, to
/// turn into the AArch64 structure once it has run.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum After {
    None,
    /// struct stat64 (104 bytes) to struct stat (128).
    Stat(u32),
    /// A time64 timespec (clock_gettime64) to struct timeval.
    Timeval(u32),
    /// struct sysinfo of 32-bit longs (64 bytes) to 64-bit ones (112).
    Sysinfo(u32),
    /// struct rusage (72 bytes) to 144.
    Rusage(u32),
    /// struct statfs64 (84 bytes) to AArch64's struct statfs (120).
    Statfs(u32),
    /// struct tms of 32-bit clock_t (16 bytes) to 64-bit (32).
    Tms(u32),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Entry {
    /// Dispatch AArch32 call `number` with `args`, then `after`.
    Call { number: u32, args: [u32; 6], after: After },
    /// Answer at once with this result.
    Answer(i32),
}

/// AArch64 calls (their AArch64 numbers) with pointer-holding structures
/// that the dispatcher reads and writes in the app's own layout itself:
/// readv, writev, sigaltstack, rt_sigaction, rt_sigreturn (the AArch64
/// frame, runtime/open/abi64.rs), sendmsg, recvmsg (the address and first
/// buffer, as runtime/open/sockets.rs reads them), clone, execve.
pub const WIDE_IN_DISPATCH: &[Word] = &[65, 66, 132, 134, 139, 211, 212, 220, 221];

/// A 64-bit argument as a 32-bit word: a value below 4 GiB, or a
/// sign-extended negative int (AT_FDCWD, -1, an ioctl request with bit 31).
pub fn narrow(v: Word) -> Option<u32> {
    u32::try_from(v).ok().or_else(|| i32::try_from(v as i64).ok().map(|n| n as u32))
}
/// A 64-bit address or size argument as a 32-bit word: only a value below
/// 4 GiB names app memory.
pub fn narrow_memory(v: Word) -> Option<u32> {
    u32::try_from(v).ok()
}

/// The arguments (bit k: argument k) of AArch64 call `number` that are an
/// address in the app or a size of app memory, by the Linux prototypes. A
/// call not named here, and the arguments of one named that are not set,
/// are ints or may be either (ioctl's and fcntl's third, prctl's, futex's
/// fourth), so `narrow` keeps a sign-extended int there.
pub fn memory_args(number: Word) -> u8 {
    const fn b(args: &[u8]) -> u8 {
        let mut m = 0;
        let mut k = 0;
        while k < args.len() {
            m |= 1 << args[k];
            k += 1;
        }
        m
    }
    match number {
        17 => b(&[0, 1]),                    // getcwd(buf, size)
        21 => b(&[3]),                       // epoll_ctl(epfd, op, fd, event)
        22 => b(&[1, 4, 5]),                 // epoll_pwait(epfd, events, max, timeout, sigmask, size)
        34 | 35 | 48 | 53 | 54 | 56 | 439 => b(&[1]), // *at(dirfd, path, ...)
        36 => b(&[0, 2]),                    // symlinkat(target, dirfd, path)
        37 | 38 | 276 => b(&[1, 3]),         // linkat/renameat(2)(dirfd, old, dirfd, new, ...)
        43 => b(&[0, 1]),                    // statfs(path, buf)
        44 | 80 | 87 | 108 | 112 | 113 | 114 | 102 | 163 | 164 | 165 => b(&[1]), // (int, struct *)
        45 | 49 | 59 | 96 | 153 | 160 | 179 | 279 => b(&[0]), // (path or struct *, ...)
        61 | 63 | 64 | 67 | 68 => b(&[1, 2]), // (fd, buf, count, ...)
        65 | 66 | 69 | 70 | 211 | 212 | 269 => b(&[1]), // (fd, iov or msghdr, ...)
        71 => b(&[2, 3]),                    // sendfile(out, in, offset, count)
        72 => b(&[1, 2, 3, 4, 5]),           // pselect6(n, r, w, e, timeout, sigmask)
        73 => b(&[0, 1, 2, 3, 4]),           // ppoll(fds, nfds, timeout, sigmask, size)
        78 => b(&[1, 2, 3]),                 // readlinkat(dirfd, path, buf, size)
        79 | 88 => b(&[1, 2]),               // newfstatat/utimensat(dirfd, path, struct, flags)
        86 | 110 => b(&[2, 3]),              // timerfd_settime/timer_settime(id, flags, new, old)
        95 => b(&[2, 4]),                    // waitid(idtype, id, info, options, rusage)
        98 => b(&[0, 4]),                    // futex(uaddr, op, val, timeout or val2, uaddr2, val3)
        99 | 101 | 132 | 133 | 136 | 169 | 170 | 214 | 215 | 226 | 227 | 228 | 229 | 233 | 278 | 435 => b(&[0, 1]),
        100 | 122 | 123 | 202 | 204 | 205 | 242 | 200 | 203 => b(&[1, 2]),
        103 | 107 => b(&[1, 2]),             // setitimer(which, new, old), timer_create(clock, sevp, id)
        115 => b(&[2, 3]),                   // clock_nanosleep(clock, flags, req, rem)
        134 | 135 => b(&[1, 2, 3]),          // rt_sigaction/rt_sigprocmask(int, new, old, size)
        137 => b(&[0, 1, 2, 3]),             // rt_sigtimedwait(set, info, timeout, size)
        138 => b(&[2]),                      // rt_sigqueueinfo(pid, sig, info)
        199 => b(&[3]),                      // socketpair(domain, type, protocol, sv)
        206 | 207 => b(&[1, 2, 4, 5]),       // sendto/recvfrom(fd, buf, len, flags, addr, addrlen)
        208 | 209 => b(&[3, 4]),             // set/getsockopt(fd, level, name, value, len)
        216 => b(&[0, 1, 2, 4]),             // mremap(old, old_size, new_size, flags, new)
        220 => b(&[1, 2, 3, 4]),             // clone(flags, stack, ptid, tls, ctid)
        221 => b(&[0, 1, 2]),                // execve(path, argv, envp)
        222 => b(&[0, 1]),                   // mmap(addr, length, ...)
        240 => b(&[3]),                      // rt_tgsigqueueinfo(tgid, tid, sig, info)
        243 => b(&[1, 4]),                   // recvmmsg(fd, vec, vlen, flags, timeout)
        260 => b(&[1, 3]),                   // wait4(pid, status, options, rusage)
        261 => b(&[2, 3]),                   // prlimit64(pid, resource, new, old)
        281 => b(&[1, 2, 3]),                // execveat(dirfd, path, argv, envp, flags)
        291 => b(&[1, 4]),                   // statx(dirfd, path, flags, mask, buf)
        _ => 0,
    }
}

/// x0 of an answered call, from the dispatcher's 32-bit result: an error
/// (-4095..-1) sign-extended, as musl's __syscall_ret reads it; anything
/// else (a count, an fd, an address below 4 GiB) zero-extended.
pub fn result64(r: u32) -> u64 {
    if (r as i32) < 0 && (r as i32) > -4096 {
        r as i32 as i64 as u64
    } else {
        u64::from(r)
    }
}

/// The 64-bit message words for the answer `mrs` to a fault whose words
/// were `wide`, shown to the dispatcher as `shown` (an AArch32 call message:
/// x0..x7, FaultIP at 8). `syscall`: the fault is an unknown-syscall one.
///
/// - x0 of an answered system call (word 8 moved off the svc) is its
///   result (`result64`), whatever it equals: a result of 0xfffffff7 (EBADF)
///   is -9 even when the call's own x0 was 0x00000000_fffffff7.
/// - Any other word, and x0 of a call made again (word 8 back on the svc,
///   its registers as they were), goes back as the thread had it if the
///   runtime left it as shown, else as the 32-bit value the runtime wrote.
pub fn widen(syscall: bool, wide: &[u64; 16], shown: &[u32; 16], mrs: &[u32], out: &mut [u64; 16]) -> usize {
    let n = mrs.len().min(16);
    let answered = syscall && n > 8 && mrs[8] != shown[8];
    for k in 0..n {
        out[k] = if k == 0 && answered {
            result64(mrs[0])
        } else if mrs[k] == shown[k] {
            wide[k]
        } else if k == 0 {
            result64(mrs[0])
        } else {
            u64::from(mrs[k])
        };
    }
    n
}

/// The top page: above every app's regions (vma.rs USER_TOP is 0xc000_0000)
/// and every other 32-bit address the runtime gives an app.
pub const NOWHERE: u32 = 0xffff_f000;

fn lo(v: Word) -> u32 {
    v as u32
}
fn hi(v: Word) -> u32 {
    (v >> 32) as u32
}

/// Whether an AArch64 call of `Layout::Words` is answered ENOSYS here
/// because nothing converts its structures yet.
pub fn unconverted(number: Word) -> bool {
    matches!(internal_call(&AARCH64, number), Some((_, Layout::Words))) && translate(number, [0; 6]) == Entry::Answer(ENOSYS)
}

/// The AArch32 call for AArch64 call `number` with arguments `a`.
pub fn translate(number: Word, a: [Word; 6]) -> Entry {
    let Some((n32, layout)) = internal_call(&AARCH64, number) else { return Entry::Answer(ENOSYS) };
    let call = |number: u32, args: [u32; 6], after: After| Entry::Call { number, args, after };
    // Arguments that are 32-bit words (fds, flags, addresses).
    let mut w = [0u32; 6];
    let mut wide_args = 0u8; // bit k: argument k is a 64-bit value, split below
    match number {
        45 | 46 | 62 | 67 | 68 => wide_args = match number { 45 | 46 => 1 << 1, 62 => 1 << 1, _ => 1 << 3 },
        47 => wide_args = 1 << 2 | 1 << 3,
        223 => wide_args = 1 << 1 | 1 << 2,
        222 => wide_args = 1 << 5,
        _ => {}
    }
    let memory = memory_args(number);
    for k in 0..6 {
        if wide_args & 1 << k == 0 {
            let v = if memory & 1 << k != 0 { narrow_memory(a[k]) } else { narrow(a[k]) };
            w[k] = v.unwrap_or(NOWHERE);
        }
    }
    match number {
        // lseek(fd, offset, whence): the 32-bit lseek, which answers an int.
        62 => match i32::try_from(a[1] as i64) {
            Ok(off) => call(19, [w[0], off as u32, w[2], 0, 0, 0], After::None),
            Err(_) => Entry::Answer(EOVERFLOW),
        },
        // pread64/pwrite64(fd, buf, count, offset): the offset as a pair after a pad.
        67 | 68 => call(n32, [w[0], w[1], w[2], 0, lo(a[3]), hi(a[3])], After::None),
        // truncate(path, length), ftruncate(fd, length): truncate64's pair.
        45 | 46 => call(n32, [w[0], 0, lo(a[1]), hi(a[1]), 0, 0], After::None),
        // fallocate(fd, mode, offset, length).
        47 => call(n32, [w[0], w[1], lo(a[2]), hi(a[2]), lo(a[3]), hi(a[3])], After::None),
        // fadvise64(fd, offset, length, advice): ARM's puts advice second.
        223 => call(n32, [w[0], w[3], lo(a[1]), hi(a[1]), lo(a[2]), hi(a[2])], After::None),
        // mmap(addr, length, prot, flags, fd, offset in bytes): mmap2's pages.
        222 => match compat::mmap_pages(&AARCH64, a[5]).and_then(|p| u32::try_from(p).ok()) {
            Some(pages) => call(n32, [w[0], w[1], w[2], w[3], w[4], pages], After::None),
            None => Entry::Answer(EINVAL),
        },
        // nanosleep(req, rem): CLOCK_MONOTONIC, relative, with 64-bit timespecs.
        101 => call(407, [1, 0, w[0], w[1], 0, 0], After::None),
        // getrlimit/setrlimit(resource, rlim): prlimit64's 64-bit rlimit is AArch64's.
        163 => call(369, [0, w[0], 0, w[1], 0, 0], After::None),
        164 => call(369, [0, w[0], w[1], 0, 0, 0], After::None),
        // gettimeofday(tv, tz): clock_gettime64 of the wall clock, then microseconds.
        169 if w[0] == 0 => Entry::Answer(0),
        169 => call(403, [0, w[0], 0, 0, 0, 0], After::Timeval(w[0])),
        79 => call(n32, w, After::Stat(w[2])),
        80 => call(n32, w, After::Stat(w[1])),
        179 => call(n32, w, After::Sysinfo(w[0])),
        165 => call(n32, w, After::Rusage(w[1])),
        260 => call(n32, w, if w[3] != 0 { After::Rusage(w[3]) } else { After::None }),
        153 => call(n32, w, if w[0] != 0 { After::Tms(w[0]) } else { After::None }),
        // statfs(path, buf), fstatfs(fd, buf): statfs64's size argument.
        43 | 44 => call(n32, [w[0], 84, w[1], 0, 0, 0], After::Statfs(w[1])),
        // fcntl: AArch64's struct flock is AArch32's struct flock64 (fcntl64).
        // sendfile: its offset is a pointer to a 64-bit value on both.
        // set_robust_list, mremap and rt_sigtimedwait take nothing the
        // dispatcher reads differently (it never reads the robust list;
        // siginfo is 128 bytes on both, the timespec 64-bit).
        25 | 71 | 99 | 216 | 137 => call(n32, w, After::None),
        n if WIDE_IN_DISPATCH.contains(&n) => call(n32, w, After::None),
        _ if layout == Layout::Words => Entry::Answer(ENOSYS),
        _ => call(n32, w, After::None),
    }
}

/// Whether a call answering `result` wrote its structure, to convert: any
/// result but an error (wait4 answers the child's pid, times a tick count).
pub fn converts(result: i32) -> bool {
    !(-4095..0).contains(&result)
}

/// Bytes the dispatcher writes for `after`, and the AArch64 structure's.
pub fn sizes(after: After) -> Option<(u32, usize, usize)> {
    Some(match after {
        After::None => return None,
        After::Stat(p) => (p, compat::STAT32, compat::STAT64),
        After::Timeval(p) => (p, 16, 16),
        After::Sysinfo(p) => (p, 64, 112),
        After::Rusage(p) => (p, compat::RUSAGE32, compat::RUSAGE64),
        After::Statfs(p) => (p, 84, 120),
        After::Tms(p) => (p, 16, 32),
    })
}

fn get(b: &[u8], at: usize, n: usize) -> u64 {
    let mut w = [0u8; 8];
    w[..n].copy_from_slice(&b[at..at + n]);
    u64::from_le_bytes(w)
}
fn put(b: &mut [u8], at: usize, n: usize, v: u64) {
    b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
}

/// The largest AArch64 structure `convert` makes.
pub const MAX_AFTER: usize = 144;
/// The AArch64 structure for `after` (its first `sizes(after).2` bytes)
/// from the `sizes(after).1` bytes the dispatcher wrote.
pub fn convert(after: After, s: &[u8]) -> [u8; MAX_AFTER] {
    let mut out = [0u8; MAX_AFTER];
    let Some((_, from, to)) = sizes(after) else { return out };
    if s.len() < from {
        return out;
    }
    let d = &mut out[..to];
    match after {
        After::None => {}
        After::Stat(_) => d.copy_from_slice(&compat::stat_to_aarch64(s[..from].try_into().unwrap())),
        After::Rusage(_) => d.copy_from_slice(&compat::rusage_to_aarch64(s[..from].try_into().unwrap())),
        After::Timeval(_) => {
            put(d, 0, 8, get(s, 0, 8));
            put(d, 8, 8, get(s, 8, 4) / 1000);
        }
        After::Sysinfo(_) => {
            // uptime (signed), loads[3], six memory sizes: longs.
            put(d, 0, 8, get(s, 0, 4) as u32 as i32 as i64 as u64);
            for k in 1..10 {
                put(d, 8 * k, 8, get(s, 4 * k, 4));
            }
            put(d, 80, 2, get(s, 40, 2)); // procs
            put(d, 88, 8, get(s, 44, 4)); // totalhigh
            put(d, 96, 8, get(s, 48, 4)); // freehigh
            put(d, 104, 4, get(s, 52, 4)); // mem_unit
        }
        After::Statfs(_) => {
            // type, bsize; blocks..ffree (five 64-bit); fsid; namelen, frsize, flags, spare[4].
            put(d, 0, 8, get(s, 0, 4));
            put(d, 8, 8, get(s, 4, 4));
            for k in 0..5 {
                put(d, 16 + 8 * k, 8, get(s, 8 + 8 * k, 8));
            }
            put(d, 56, 8, get(s, 48, 8));
            for k in 0..7 {
                put(d, 64 + 8 * k, 8, get(s, 56 + 4 * k, 4));
            }
        }
        After::Tms(_) => {
            for k in 0..4 {
                put(d, 8 * k, 8, get(s, 4 * k, 4) as u32 as i32 as i64 as u64);
            }
        }
    }
    out
}

/// A VM fault's address as the runtime's 32-bit regions see it: one above
/// 4 GiB is in no region of any app, so it becomes an address that is not
/// either (the top page), never a low one it might alias.
pub fn fault_address(address: Word) -> u32 {
    u32::try_from(address).unwrap_or(NOWHERE)
}
