//! The Linux system calls the 64-bit bring-up runtime answers. Pure logic over
//! three traits (the program's memory, the console, the clock), so the same
//! code is tested on the host (tests/host.rs) and runs in the root task.
//!
//! It is deliberately small: enough for a statically linked program to start,
//! print, allocate and exit. Everything else is ENOSYS, by name on the
//! console, never a pretended success. It is not the Open Runtime: no files,
//! no threads, no signals, no policy.
use super::arch::{self, Protection, Syscall};

pub const ENOENT: i64 = -2;
pub const EBADF: i64 = -9;
pub const ENOMEM: i64 = -12;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const ENOTTY: i64 = -25;
pub const ENOSYS: i64 = -38;

pub const PAGE: u64 = 4096;

/// The program's address space.
pub trait Memory {
    /// Copy out of the program. False when any byte is not readable by it.
    fn read(&self, va: u64, out: &mut [u8]) -> bool;
    /// Copy into the program. False when any byte is not writable by it.
    fn write(&mut self, va: u64, data: &[u8]) -> bool;
    /// Fresh zeroed pages at `va`. False when out of memory or already mapped.
    fn map(&mut self, va: u64, pages: u64, protection: Protection) -> bool;
    /// Remove whole pages; pages not mapped are skipped.
    fn unmap(&mut self, va: u64, pages: u64);
    /// Change the protection of mapped pages. False if one is not mapped.
    fn protect(&mut self, va: u64, pages: u64, protection: Protection) -> bool;
}
pub trait Console {
    fn write(&mut self, bytes: &[u8]);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The value to return in the result register.
    Return(i64),
    /// The program ended with this status.
    Exit(i32),
}

/// One single-threaded program.
pub struct Process {
    pub pid: i32,
    /// The program break: `brk_start` is the end of its image.
    brk_start: u64,
    brk: u64,
    brk_limit: u64,
    /// Where the next anonymous mapping goes (they grow upward).
    mmap_next: u64,
    mmap_limit: u64,
}

fn pages(bytes: u64) -> u64 {
    bytes.div_ceil(PAGE)
}

impl Process {
    /// `image_end`: the first address after the program's segments.
    /// `mmap_base..mmap_limit`: the range anonymous mappings may use.
    pub fn new(pid: i32, image_end: u64, mmap_base: u64, mmap_limit: u64) -> Self {
        let start = image_end.div_ceil(PAGE) * PAGE;
        Process { pid, brk_start: start, brk: start, brk_limit: start + (16 << 20), mmap_next: mmap_base, mmap_limit }
    }

    pub fn syscall<M: Memory, C: Console>(&mut self, call: &Syscall, memory: &mut M, console: &mut C, now_ns: u64) -> Outcome {
        let a = call.args;
        let name = arch::linux::aarch64(call.number).map(|c| c.name).unwrap_or("?");
        Outcome::Return(match name {
            "exit" | "exit_group" => return Outcome::Exit(a[0] as i32),
            "write" => self.write(a[0], a[1], a[2], memory, console),
            "writev" => {
                // struct iovec { void *base; size_t len; }: two 64-bit words.
                if a[2] > 16 {
                    return Outcome::Return(EINVAL);
                }
                let mut total = 0i64;
                for i in 0..a[2] {
                    let mut entry = [0u8; 16];
                    if !memory.read(a[1].wrapping_add(i * 16), &mut entry) {
                        return Outcome::Return(if total > 0 { total } else { EFAULT });
                    }
                    let base = u64::from_le_bytes(entry[..8].try_into().unwrap());
                    let len = u64::from_le_bytes(entry[8..].try_into().unwrap());
                    match self.write(a[0], base, len, memory, console) {
                        n if n < 0 => return Outcome::Return(if total > 0 { total } else { n }),
                        n => total += n,
                    }
                }
                total
            }
            "getpid" | "gettid" | "set_tid_address" => i64::from(self.pid),
            // The console is not a terminal the program can query or control.
            "ioctl" => if a[0] <= 2 { ENOTTY } else { EBADF },
            "brk" => {
                let wanted = a[0];
                if wanted >= self.brk_start && wanted <= self.brk_limit {
                    let (old, new) = (pages(self.brk - self.brk_start), pages(wanted - self.brk_start));
                    if new > old && !memory.map(self.brk_start + old * PAGE, new - old, Protection::ReadWrite) {
                        return Outcome::Return(self.brk as i64); // unchanged: the caller sees the failure
                    }
                    if new < old {
                        memory.unmap(self.brk_start + new * PAGE, old - new);
                    }
                    self.brk = wanted;
                }
                self.brk as i64
            }
            "mmap" => {
                const MAP_PRIVATE: u64 = 2;
                const MAP_FIXED: u64 = 0x10;
                const MAP_ANONYMOUS: u64 = 0x20;
                let (length, prot, flags) = (a[1], a[2], a[3]);
                // Only fresh private memory at an address this runtime picks.
                if length == 0 || flags & (MAP_ANONYMOUS | MAP_PRIVATE) != MAP_ANONYMOUS | MAP_PRIVATE || flags & MAP_FIXED != 0 {
                    return Outcome::Return(EINVAL);
                }
                let count = pages(length);
                let at = self.mmap_next;
                if count > (self.mmap_limit - at) / PAGE {
                    return Outcome::Return(ENOMEM);
                }
                if prot & 7 == 0 {
                    // PROT_NONE: the range is reserved (the next mapping goes
                    // after it) and left unmapped, so any access faults.
                    self.mmap_next = at + count * PAGE;
                    return Outcome::Return(at as i64);
                }
                let Some(protection) = protection(prot) else { return Outcome::Return(EINVAL) };
                if !memory.map(at, count, protection) {
                    return Outcome::Return(ENOMEM);
                }
                self.mmap_next = at + count * PAGE;
                at as i64
            }
            "munmap" => {
                if a[0] % PAGE != 0 || a[1] == 0 {
                    return Outcome::Return(EINVAL);
                }
                memory.unmap(a[0], pages(a[1]));
                0
            }
            "mprotect" => match protection(a[2]) {
                Some(p) if a[0] % PAGE == 0 => if memory.protect(a[0], pages(a[1]), p) { 0 } else { ENOMEM },
                // Includes PROT_WRITE | PROT_EXEC, and PROT_NONE: a page cannot
                // be made inaccessible and later readable again with its contents.
                _ => EINVAL,
            },
            "clock_gettime" => {
                // CLOCK_REALTIME has no source here; only the monotonic clocks answer.
                if a[0] != 1 && a[0] != 4 && a[0] != 7 {
                    return Outcome::Return(EINVAL);
                }
                // struct timespec { i64 tv_sec; i64 tv_nsec; }
                let mut ts = [0u8; 16];
                ts[..8].copy_from_slice(&(now_ns / 1_000_000_000).to_le_bytes());
                ts[8..].copy_from_slice(&(now_ns % 1_000_000_000).to_le_bytes());
                if memory.write(a[1], &ts) { 0 } else { EFAULT }
            }
            _ => {
                // Say which, by name when the table has it, else by number.
                console.write(b"qemu64: unhandled Linux call ");
                if name == "?" {
                    let mut digits = [0u8; 20];
                    let mut at = digits.len();
                    let mut n = call.number;
                    loop {
                        at -= 1;
                        digits[at] = b'0' + (n % 10) as u8;
                        n /= 10;
                        if n == 0 {
                            break;
                        }
                    }
                    console.write(&digits[at..]);
                } else {
                    console.write(name.as_bytes());
                }
                console.write(b"\n");
                ENOSYS
            }
        })
    }

    fn write<M: Memory, C: Console>(&mut self, fd: u64, va: u64, length: u64, memory: &mut M, console: &mut C) -> i64 {
        if fd != 1 && fd != 2 {
            return EBADF;
        }
        let mut done = 0;
        let mut chunk = [0u8; 128];
        while done < length {
            // Never across a page boundary, so a buffer that runs off the
            // program's memory is written up to its last readable page.
            let at = va.wrapping_add(done);
            let n = (length - done).min(chunk.len() as u64).min(PAGE - at % PAGE) as usize;
            if !memory.read(at, &mut chunk[..n]) {
                return if done > 0 { done as i64 } else { EFAULT };
            }
            console.write(&chunk[..n]);
            done += n as u64;
        }
        done as i64
    }
}

/// Linux PROT_* bits as a mapping protection. Writable and executable
/// together is refused (W^X), as in the Open Runtime. PROT_NONE is not a
/// protection a mapped page can have here: never readable by mistake.
fn protection(prot: u64) -> Option<Protection> {
    match prot & 7 {
        1 => Some(Protection::ReadOnly),
        2 | 3 => Some(Protection::ReadWrite),
        4 | 5 => Some(Protection::ReadExecute),
        _ => None,
    }
}
