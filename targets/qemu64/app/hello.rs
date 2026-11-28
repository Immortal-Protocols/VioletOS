//! The test program of the `qemu64` target: a static AArch64 Linux program
//! with no C library, so every system call below is the raw `svc #0` a C
//! library would make. It checks what the port must get right (the kernel
//! patch, the register context, the 64-bit structure layouts, W^X) and
//! prints one line per check. targets/qemu64/README.md lists the lines.
#![no_std]
#![no_main]
use core::arch::{asm, global_asm};

global_asm!(
    r#"
.section .text.start,"ax"
.global _start
_start:
    mov x29, #0
    mov x30, #0
    mov x0, sp
    bl main
"#
);

const WRITE: u64 = 64;
const WRITEV: u64 = 66;
const EXIT_GROUP: u64 = 94;
const CLOCK_GETTIME: u64 = 113;
const GETPID: u64 = 172;
const BRK: u64 = 214;
const MUNMAP: u64 = 215;
const MMAP: u64 = 222;
const MPROTECT: u64 = 226;

/// A Linux system call with x7 set to `x7`: the register seL4 itself dispatches on.
fn call7(number: u64, a: [u64; 3], x7: u64) -> i64 {
    let result: i64;
    unsafe {
        asm!("svc #0", in("x8") number, inlateout("x0") a[0] => result, in("x1") a[1], in("x2") a[2],
             in("x3") 0u64, in("x4") 0u64, in("x5") 0u64, in("x7") x7);
    }
    result
}
fn call6(number: u64, a: [u64; 6]) -> i64 {
    let result: i64;
    unsafe {
        asm!("svc #0", in("x8") number, inlateout("x0") a[0] => result, in("x1") a[1], in("x2") a[2],
             in("x3") a[3], in("x4") a[4], in("x5") a[5]);
    }
    result
}
fn call(number: u64, a: [u64; 3]) -> i64 {
    call7(number, a, 0)
}
fn print(text: &str) {
    call(WRITE, [1, text.as_ptr() as u64, text.len() as u64]);
}
fn check(name: &str, ok: bool) -> bool {
    print(name);
    print(if ok { ": ok\n" } else { ": FAILED\n" });
    ok
}

/// Every register a system call must leave alone, set to a pattern across one call.
fn context_preserved() -> bool {
    let seed = 0x0123_4567_89ab_cdefu64;
    let mut ok = true;
    let (mut r9, mut r10, mut r11, mut r12, mut r13, mut r14, mut r15) = (seed ^ 9, seed ^ 10, seed ^ 11, seed ^ 12, seed ^ 13, seed ^ 14, seed ^ 15);
    let (mut r20, mut r21, mut r22, mut r23, mut r24, mut r25, mut r26, mut r27, mut r28) =
        (seed ^ 20, seed ^ 21, seed ^ 22, seed ^ 23, seed ^ 24, seed ^ 25, seed ^ 26, seed ^ 27, seed ^ 28);
    let (mut d8, mut d31) = (1.5f64, -2.25f64);
    let (mut r1, mut r2, mut r3, mut r4, mut r5, mut r6, mut r7) = (seed ^ 1, seed ^ 2, seed ^ 3, seed ^ 4, seed ^ 5, seed ^ 6, seed ^ 7);
    let tls: u64;
    let pid: u64;
    unsafe {
        asm!("msr tpidr_el0, {tls_in}", "svc #0", "mrs {tls_out}, tpidr_el0",
             tls_in = in(reg) seed ^ 0x77, tls_out = out(reg) tls,
             in("x8") GETPID, lateout("x0") pid,
             inout("x1") r1, inout("x2") r2, inout("x3") r3, inout("x4") r4, inout("x5") r5, inout("x6") r6, inout("x7") r7,
             inout("x9") r9, inout("x10") r10, inout("x11") r11, inout("x12") r12, inout("x13") r13, inout("x14") r14, inout("x15") r15,
             inout("x20") r20, inout("x21") r21, inout("x22") r22, inout("x23") r23, inout("x24") r24,
             inout("x25") r25, inout("x26") r26, inout("x27") r27, inout("x28") r28,
             inout("d8") d8, inout("d31") d31);
    }
    for (i, r) in [r1, r2, r3, r4, r5, r6, r7].iter().enumerate() {
        ok &= *r == seed ^ (i as u64 + 1);
    }
    for (i, r) in [r9, r10, r11, r12, r13, r14, r15].iter().enumerate() {
        ok &= *r == seed ^ (i as u64 + 9);
    }
    for (i, r) in [r20, r21, r22, r23, r24, r25, r26, r27, r28].iter().enumerate() {
        ok &= *r == seed ^ (i as u64 + 20);
    }
    ok && d8 == 1.5 && d31 == -2.25 && tls == seed ^ 0x77 && pid != 0 && (pid as i64) > 0
}

static mut COUNTER: u64 = 7;

#[no_mangle]
extern "C" fn main(sp: *const u64) -> ! {
    let argc = unsafe { *sp };
    let pid = call(GETPID, [0; 3]);
    print("hello from a static AArch64 Linux program\n");
    let mut ok = check("argc and pid", (argc == 1 || argc == 2) && pid > 0);

    // x7 holds each of seL4's own call numbers (-1 Call .. -8 NBRecv) while
    // a Linux call is made. An unpatched kernel would run its own call
    // instead (Yield, Reply and NBSend return without any error).
    let mut collisions = true;
    for native in 1..=8u64 {
        collisions &= call7(GETPID, [0; 3], native.wrapping_neg()) == pid;
    }
    ok &= check("x7 never selects a kernel call", collisions);
    ok &= check("registers, FP and TPIDR_EL0 survive a call", context_preserved());

    // The program break and an anonymous mapping: fresh, zeroed, writable.
    let start = call(BRK, [0; 3]) as u64;
    let grown = call(BRK, [start + 3 * 4096 + 100, 0, 0]) as u64;
    let heap = start as *mut u64;
    let mut memory = grown == start + 3 * 4096 + 100;
    if memory {
        unsafe {
            memory &= heap.add(1000).read_volatile() == 0;
            heap.add(1000).write_volatile(0xfeed);
            memory &= heap.add(1000).read_volatile() == 0xfeed;
        }
    }
    let map = call6(MMAP, [0, 2 * 4096, 3, 0x22, u64::MAX, 0]);
    memory &= map > 0;
    if map > 0 {
        let p = map as *mut u8;
        unsafe {
            memory &= p.add(4096 + 5).read_volatile() == 0;
            p.add(4096 + 5).write_volatile(9);
            memory &= p.add(4096 + 5).read_volatile() == 9;
        }
        memory &= call(MUNMAP, [map as u64, 2 * 4096, 0]) == 0;
    }
    ok &= check("brk and mmap give zeroed writable memory", memory);

    // W^X: no mapping is ever writable and executable.
    let wx = call6(MMAP, [0, 4096, 7, 0x22, u64::MAX, 0]) == -22 && call(MPROTECT, [start, 4096, 6]) == -22;
    ok &= check("writable and executable is refused", wx);

    // struct iovec is two 64-bit words here.
    let (a, b) = ("writev: two ", "parts\n");
    let iov = [a.as_ptr() as u64, a.len() as u64, b.as_ptr() as u64, b.len() as u64];
    ok &= check("writev with 64-bit iovec", call(WRITEV, [1, iov.as_ptr() as u64, 2]) == (a.len() + b.len()) as i64);

    // struct timespec is two 64-bit words; the monotonic clock moves forward.
    let mut t = [[0u64; 2]; 2];
    let mut clock = call(CLOCK_GETTIME, [1, t[0].as_mut_ptr() as u64, 0]) == 0;
    for _ in 0..100_000 {
        unsafe { COUNTER = core::ptr::read_volatile(&raw const COUNTER) + 1 };
    }
    clock &= call(CLOCK_GETTIME, [1, t[1].as_mut_ptr() as u64, 0]) == 0;
    clock &= t[0][1] < 1_000_000_000 && (t[1][0], t[1][1]) > (t[0][0], t[0][1]);
    ok &= check("monotonic clock with 64-bit timespec", clock);

    // Errors are the Linux ones: a bad pointer, a bad descriptor, an unknown call.
    let errors = call(WRITE, [1, 0xdead_0000, 4]) == -14 && call(WRITE, [7, a.as_ptr() as u64, 1]) == -9 && call(441, [0; 3]) == -38;
    ok &= check("EFAULT, EBADF and ENOSYS", errors);

    if argc == 2 {
        // Second run: write to this program's own code. The page is read and
        // execute only, so the kernel faults the thread and the runtime stops it.
        print("writing to the program's code\n");
        unsafe { (0x40_0000 as *mut u32).write_volatile(0) };
        print("the write to code was not stopped: FAILED\n");
        ok = false;
    }
    call(EXIT_GROUP, [if ok { 0 } else { 1 }, 0, 0]);
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    call(EXIT_GROUP, [101, 0, 0]);
    loop {}
}
