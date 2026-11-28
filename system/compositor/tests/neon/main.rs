//! The NEON blits and the compositor's built-in-app path on an emulated Arm
//! core, bare metal (no kernel): the same source, target and optimisation as
//! the Display package, checked against plain loops. `run.sh` builds and runs
//! it; it prints "neon: ok" and exits 0, or says what differed and exits 1.
//! A local check for whoever changes blit.rs; not part of `make test-unit`.
#![no_std]
#![no_main]
#![allow(dead_code)]

#[path = "../../blit.rs"]
mod blit;
#[path = "../../display.rs"]
mod display;
#[path = "../../scene.rs"]
mod scene;

use core::arch::{asm, global_asm};
use display::{Compositor, Hardware, CLASSIC_H, CLASSIC_W, PIXELS, TAIL_WORDS};

// Entry: a stack, then NEON switched on (the kernel does this for a thread).
global_asm!(
    ".section .entry, \"ax\"",
    ".global _start",
    "_start:",
    "ldr sp, =stack_top",
    "mrc p15, 0, r0, c1, c0, 2",
    "orr r0, r0, #0x00f00000",
    "mcr p15, 0, r0, c1, c0, 2",
    "isb",
    "mov r0, #0x40000000",
    ".fpu neon",
    "vmsr fpexc, r0",
    "bl main",
    "b .",
);

fn semihost(op: u32, arg: u32) -> u32 {
    let mut r0 = op;
    unsafe { asm!("svc 0x123456", inout("r0") r0, in("r1") arg) };
    r0
}
fn say(text: &'static [u8]) {
    // SYS_WRITE0: a NUL-terminated string.
    semihost(4, text.as_ptr() as u32);
}
fn exit(code: u32) -> ! {
    // SYS_EXIT_EXTENDED: ADP_Stopped_ApplicationExit and the exit code.
    let block = [0x20026u32, code];
    semihost(0x20, block.as_ptr() as u32);
    loop {}
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    say(b"neon: panic\n\0");
    exit(1)
}
fn check(ok: bool, what: &'static [u8]) {
    if !ok {
        say(b"neon: FAILED: \0");
        say(what);
        exit(1);
    }
}

fn next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed ^ (*seed >> 15)
}

static mut SCREEN: [u32; PIXELS + TAIL_WORDS] = [0; PIXELS + TAIL_WORDS];
static mut WANT: [u32; PIXELS] = [0; PIXELS];
static mut SURFACE: [u32; CLASSIC_W * CLASSIC_H] = [0; CLASSIC_W * CLASSIC_H];

struct Board;
impl Hardware for Board {
    fn classic_row(&mut self, _: u32, y: usize, out: &mut [u32; CLASSIC_W]) {
        let s = unsafe { &*core::ptr::addr_of!(SURFACE) };
        out.copy_from_slice(&s[y * CLASSIC_W..(y + 1) * CLASSIC_W]);
    }
    fn scanout(&mut self) -> &mut [u32] {
        unsafe { &mut *core::ptr::addr_of_mut!(SCREEN) }
    }
    fn ticks(&mut self) -> u32 {
        0
    }
    fn ticks_per_ms(&mut self) -> u32 {
        1000
    }
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    let mut seed = 21u32;
    // Every length across several 64-byte blocks, at every word offset in one.
    let mut src = [0u32; 256];
    let (mut a, mut b) = ([0u32; 800], [0u32; 800]);
    for n in 0..200usize {
        for offset in 0..5usize {
            for p in src.iter_mut() {
                *p = next(&mut seed);
            }
            a.fill(1);
            b.fill(1);
            blit::copy(&mut a[offset..offset + n], &src[offset..offset + n]);
            blit::plain::copy(&mut b[offset..offset + n], &src[offset..offset + n]);
            check(a == b, b"copy\n\0");
            blit::fill(&mut a[offset..offset + n], 0xabcd_0000 + n as u32);
            blit::plain::fill(&mut b[offset..offset + n], 0xabcd_0000 + n as u32);
            check(a == b, b"fill\n\0");
            a.fill(2);
            b.fill(2);
            blit::widen3(&mut a[offset..offset + 3 * n], &src[..n]);
            blit::plain::widen3(&mut b[offset..offset + 3 * n], &src[..n]);
            check(a == b, b"widen3\n\0");
        }
    }
    // A built-in app's whole surface, and a band of it, through the compositor.
    let surface = unsafe { &mut *core::ptr::addr_of_mut!(SURFACE) };
    for p in surface.iter_mut() {
        *p = next(&mut seed);
    }
    let want = unsafe { &mut *core::ptr::addr_of_mut!(WANT) };
    let mut c = Compositor::new();
    for (first, end) in [(0usize, 400usize), (17, 230), (399, 400)] {
        for p in surface[first * CLASSIC_W..end * CLASSIC_W].iter_mut() {
            *p = next(&mut seed);
        }
        check(c.present_classic(&mut Board, 1, first as u32, end as u32).is_ok(), b"present refused\n\0");
        for y in first..end {
            for x in 0..CLASSIC_W {
                let color = surface[y * CLASSIC_W + x];
                for dy in 0..4 {
                    for dx in 0..3 {
                        want[(4 * y + dy) * 720 + 3 * x + dx] = color;
                    }
                }
            }
        }
        let screen = unsafe { &*core::ptr::addr_of!(SCREEN) };
        check(screen[..PIXELS] == want[..], b"present_classic pixels\n\0");
    }
    c.clear(&mut Board);
    let screen = unsafe { &*core::ptr::addr_of!(SCREEN) };
    check(screen[..PIXELS].iter().all(|&p| p == 0), b"clear\n\0");
    say(b"neon: ok\n\0");
    exit(0)
}
