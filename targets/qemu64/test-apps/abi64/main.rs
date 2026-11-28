//! An AArch64 program's signals and processes under the Open Runtime
//! (targets/qemu/test_system.py --arch aarch64): a handler through signal(3)
//! and raise, an SA_SIGINFO handler's siginfo (the 64-bit layout), a SIGSEGV
//! handler that makes the faulting page writable and returns so the write runs
//! again with the SIMD registers intact, and std::process::Command (musl's
//! posix_spawn: clone with CLONE_VM|CLONE_VFORK, execve, pipes, wait4).
//! It also tries the supervisor's timer (the EL0 physical timer, which the
//! AArch64 kernel gives only to the timekeeper: kernel/sel4/aarch64
//! ellis-timer.patch): a read and two writes must each trap (SIGILL, skipped
//! by its handler), and a sleep afterwards must still end on time.
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// musl's struct sigaction on AArch64.
#[repr(C)]
struct SigAction {
    handler: usize,
    mask: [u64; 16],
    flags: i32,
    restorer: usize,
}
const SA_SIGINFO: i32 = 4;
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn raise(sig: i32) -> i32;
    fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
    fn mmap(addr: usize, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> usize;
    fn mprotect(addr: usize, len: usize, prot: i32) -> i32;
}
static HITS: AtomicU64 = AtomicU64::new(0);
static INFO: AtomicU64 = AtomicU64::new(0);
static FAULT: AtomicU64 = AtomicU64::new(0);
static PAGE: AtomicU64 = AtomicU64::new(0);
static ILL: AtomicU64 = AtomicU64::new(0);

extern "C" fn on_usr1(_: i32) {
    HITS.fetch_add(1, Ordering::SeqCst);
}
extern "C" fn on_usr2(sig: i32, info: *const u8, _uc: *const u8) {
    // siginfo_t: si_signo at 0, si_code at 8.
    let code = unsafe { *(info.add(8) as *const i32) };
    INFO.store((sig as u64) << 32 | code as u32 as u64, Ordering::SeqCst);
}
extern "C" fn on_segv(_: i32, info: *const u8, _uc: *const u8) {
    // si_addr: a 64-bit pointer at 16.
    FAULT.store(unsafe { *(info.add(16) as *const u64) }, Ordering::SeqCst);
    unsafe { mprotect(PAGE.load(Ordering::SeqCst) as usize, 4096, 3) };
}
extern "C" fn on_ill(_: i32, _info: *const u8, uc: *mut u8) {
    ILL.fetch_add(1, Ordering::SeqCst);
    // ucontext_t: uc_mcontext (struct sigcontext) at 176: fault_address, regs[31], sp, then pc.
    unsafe { *(uc.add(176 + 8 + 31 * 8 + 8) as *mut u64) += 4 };
}
fn handler(f: usize) -> SigAction {
    SigAction { handler: f, mask: [0; 16], flags: SA_SIGINFO, restorer: 0 }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        println!("from the child");
        std::process::exit(7);
    }
    unsafe {
        signal(10, on_usr1 as *const () as usize);
        raise(10);
        raise(10);
        println!("abi64: usr1 handler ran {}", HITS.load(Ordering::SeqCst));
        sigaction(12, &handler(on_usr2 as *const () as usize), std::ptr::null_mut());
        raise(12);
        let info = INFO.load(Ordering::SeqCst);
        println!("abi64: siginfo signo {} code {}", info >> 32, info as u32 as i32);
        // A read-only page; its handler makes it writable and the write runs again.
        let page = mmap(0, 4096, 1, 0x22, -1, 0);
        PAGE.store(page as u64, Ordering::SeqCst);
        sigaction(11, &handler(on_segv as *const () as usize), std::ptr::null_mut());
        let x: f64 = std::hint::black_box(1.5);
        std::ptr::write_volatile((page + 8) as *mut u64, 0x5eed);
        let y: f64 = std::hint::black_box(x * 2.0);
        let wrote = std::ptr::read_volatile((page + 8) as *const u64);
        println!("abi64: fault at page+{:#x}, then wrote {:#x}, fp {}", FAULT.load(Ordering::SeqCst).wrapping_sub(page as u64), wrote, y);
    }
    unsafe {
        // The supervisor's timer: each access traps unless the kernel lets this thread program it.
        sigaction(4, &handler(on_ill as *const () as usize), std::ptr::null_mut());
        let mut ctl: u64 = 0x5a;
        core::arch::asm!("mrs {0}, cntp_ctl_el0", inout(reg) ctl);
        core::arch::asm!("msr cntp_tval_el0, {0}", in(reg) 1u64);
        core::arch::asm!("msr cntp_ctl_el0, {0}", in(reg) 0u64);
        println!("abi64: physical timer refused {} of 3, read {:#x}", ILL.load(Ordering::SeqCst), ctl);
    }
    let start = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let slept = start.elapsed().as_millis();
    println!("abi64: a 100 ms sleep took 100 to 1000 ms {}", (100..1000).contains(&slept));
    let status = Command::new("/apps/abi64").arg("child").stdout(Stdio::null()).status();
    println!("abi64: child exit {:?}", status.ok().and_then(|s| s.code()));
    let out = Command::new("/apps/abi64").arg("child").output();
    println!("abi64: child said {:?}", out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default());
    println!("abi64: missing program refused {}", Command::new("/apps/no-such-program").status().is_err());
    println!("abi64 done");
}
