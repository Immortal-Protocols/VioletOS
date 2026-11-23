//! Linux ARM EABI syscall dispatcher over `seL4_Fault_UnknownSyscall`.
//!
//! On AArch32 seL4, kernel syscalls use negative `r7` (-1..-32), while Linux
//! ARM EABI uses positive `r7` (0..450, plus ARM-private `0x0f0005` `set_tls`).
//! Unmodified musl, CPython, Rust `std`, and Go binaries trap here automatically.

use super::policy::{AccessMode, Action, AppId, Decision, Policy};
use super::OpenRuntime;

// Standard Linux ARM EABI syscall numbers (the ~60 calls apps actually need).
pub const SYS_EXIT: u32 = 1;
pub const SYS_READ: u32 = 3;
pub const SYS_WRITE: u32 = 4;
pub const SYS_CLOSE: u32 = 6;
pub const SYS_BRK: u32 = 45;
pub const SYS_IOCTL: u32 = 54;
pub const SYS_MUNMAP: u32 = 91;
pub const SYS_CLONE: u32 = 120;
pub const SYS_MPROTECT: u32 = 125;
pub const SYS_POLL: u32 = 168;
pub const SYS_RT_SIGACTION: u32 = 174;
pub const SYS_RT_SIGPROCMASK: u32 = 175;
pub const SYS_SIGALTSTACK: u32 = 186;
pub const SYS_MMAP2: u32 = 192;
pub const SYS_GETDENTS64: u32 = 217;
pub const SYS_FCNTL64: u32 = 221;
pub const SYS_FUTEX: u32 = 240;
pub const SYS_EXIT_GROUP: u32 = 248;
pub const SYS_EPOLL_CREATE1: u32 = 357;
pub const SYS_EPOLL_CTL: u32 = 251;
pub const SYS_EPOLL_PWAIT: u32 = 346;
pub const SYS_TGKILL: u32 = 268;
pub const SYS_SOCKET: u32 = 281;
pub const SYS_BIND: u32 = 282;
pub const SYS_CONNECT: u32 = 283;
pub const SYS_SENDTO: u32 = 290;
pub const SYS_RECVFROM: u32 = 292;
pub const SYS_OPENAT: u32 = 322;
pub const SYS_GETRANDOM: u32 = 384;
pub const SYS_STATX: u32 = 397;
pub const SYS_CLOCK_GETTIME64: u32 = 403;
pub const SYS_CLOCK_NANOSLEEP64: u32 = 407;
pub const SYS_ARM_CACHEFLUSH: u32 = 0x0f_0002;
pub const SYS_ARM_SET_TLS: u32 = 0x0f_0005;

pub const ENOSYS: i32 = -38;
pub const EACCES: i32 = -13;

/// Register snapshot delivered by seL4 on `seL4_Fault_UnknownSyscall`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TrapFrame {
    pub r0: u32,
    pub r1: u32,
    pub r2: u32,
    pub r3: u32,
    pub r4: u32,
    pub r5: u32,
    pub r6: u32,
    pub r7: u32, // syscall number
    pub sp: u32,
    pub lr: u32,
    pub pc: u32,
    pub cpsr: u32,
}

/// Blocking syscalls never stall the Open Runtime loop: instead, we stash the
/// thread's seL4 reply cap in a wait queue and resume it when the event fires.
#[derive(Clone, Copy, Debug)]
pub enum WaitOn {
    FdReadable { fd: u32, deadline_us: Option<u64> },
    Epoll { epfd: u32, deadline_us: Option<u64> },
    Futex { uaddr: u32, expected: u32, deadline_us: Option<u64> },
    Sleep { wakeup_us: u64 },
    ServiceReply { ticket: u32 },
}

pub enum SysReply {
    /// Immediately resume the faulting thread with `r0 = ret`.
    Resume(i32),
    /// Save the seL4 reply cap and resume later when `WaitOn` completes.
    Block(WaitOn),
    /// Thread/process exited; do not reply.
    Exit,
}

impl<P: Policy> OpenRuntime<P> {
    pub fn dispatch_syscall(&mut self, app: AppId, tid: u32, f: &TrapFrame) -> SysReply {
        match f.r7 {
            // --- Memory (backed by Supervisor's dynamic PagePool + VmaTable) ---
            SYS_BRK => self.sys_brk(app, f.r0),
            SYS_MMAP2 => self.sys_mmap2(app, f.r0, f.r1, f.r2, f.r3, f.r4 as i32, f.r5),
            SYS_MUNMAP => self.sys_munmap(app, f.r0, f.r1),
            SYS_MPROTECT => SysReply::Resume(0), // TODO: update VMA + page rights

            // --- VFS & /dev/violet/* virtual devices ---
            SYS_OPENAT => self.sys_openat(app, f.r0 as i32, f.r1, f.r2, f.r3),
            SYS_CLOSE => self.sys_close(app, f.r0),
            SYS_READ => self.sys_read(app, f.r0, f.r1, f.r2),
            SYS_WRITE => self.sys_write(app, f.r0, f.r1, f.r2),
            SYS_IOCTL => self.sys_ioctl(app, f.r0, f.r1, f.r2),
            SYS_STATX | SYS_GETDENTS64 | SYS_FCNTL64 => self.sys_vfs_meta(app, f),

            // --- Networking (forwarded to Radio's open lwIP TCP/UDP/DNS stack) ---
            SYS_SOCKET => self.sys_socket(app, f.r0, f.r1, f.r2),
            SYS_CONNECT => self.sys_connect(app, f.r0, f.r1, f.r2),
            SYS_BIND => self.sys_bind(app, f.r0, f.r1, f.r2),
            SYS_SENDTO | SYS_RECVFROM => self.sys_sock_io(app, f),

            // --- Waiting & Clocks (epoll unifies touch events, sockets, timers) ---
            SYS_EPOLL_CREATE1 | SYS_EPOLL_CTL | SYS_EPOLL_PWAIT | SYS_POLL => {
                self.sys_wait_mux(app, tid, f)
            }
            SYS_CLOCK_GETTIME64 => self.sys_clock_gettime(app, f.r0, f.r1),
            SYS_CLOCK_NANOSLEEP64 => self.sys_nanosleep(app, f.r2),
            SYS_GETRANDOM => self.sys_getrandom(app, f.r0, f.r1),

            // --- Threads & Sync (unlocks Go goroutines + Python/Rust threads) ---
            SYS_CLONE => self.sys_clone(app, tid, f),
            SYS_FUTEX => self.sys_futex(app, tid, f),
            SYS_ARM_SET_TLS => self.sys_set_tls(app, tid, f.r0),
            SYS_ARM_CACHEFLUSH => self.sys_cacheflush(app, f.r0, f.r1),
            SYS_EXIT | SYS_EXIT_GROUP => self.sys_exit(app, tid, f.r0 as i32, f.r7 == SYS_EXIT_GROUP),

            // --- Signals (Milestone 4: Go uses GODEBUG=asyncpreemptoff=1 initially,
            //     then full ucontext SIGURG/rt_sigreturn via seL4_TCB_Read/WriteRegisters) ---
            SYS_RT_SIGACTION | SYS_RT_SIGPROCMASK | SYS_SIGALTSTACK => SysReply::Resume(0),
            SYS_TGKILL => self.sys_tgkill(app, f.r0, f.r1, f.r2),

            _ => SysReply::Resume(ENOSYS),
        }
    }

    /// Illustrative path check showing how `Policy` gates `openat` in one line.
    fn sys_openat(&mut self, app: AppId, _dirfd: i32, path_ptr: u32, flags: u32, _mode: u32) -> SysReply {
        let path = self.read_user_str(app, path_ptr);
        let mode = if flags & 3 == 0 { AccessMode::Read } else { AccessMode::Write };
        if self.policy.check(app, &Action::OpenPath { path, mode }) != Decision::Allow {
            return SysReply::Resume(EACCES);
        }
        // Route "/dev/violet/*" to virtual device handlers; everything else to StorageFsService.
        let _ = (path, mode);
        todo!("allocate FD in app's table and open device or StorageFsService file")
    }
}
