//! Software blits: the compositor has no GPU to hand pixels to (README.md),
//! so these loops are the whole rendering path.
//!
//! On the phone and in QEMU (32-bit Arm) the inner loops are NEON: 64 bytes a
//! pass for copies and fills, four source pixels a pass for the 3x widening
//! of a built-in app's row. The components are built without floating point
//! (armv7a-none-eabi), so the compiler itself never touches a NEON register
//! and the loops need not save any; the kernel is built with CONFIG_HAVE_FPU
//! and switches the registers with the thread. Everywhere else (the host
//! tests) the plain loops below run, and tests/neon/ runs the NEON loops
//! against them on an emulated Arm core.

/// Copy `src` over `dst` (as many pixels as the shorter holds).
pub fn copy(dst: &mut [u32], src: &[u32]) {
    let n = dst.len().min(src.len());
    let (Some(dst), Some(src)) = (dst.get_mut(..n), src.get(..n)) else { return };
    let done = neon::copy(dst, src);
    // Written so that nothing here can panic (the components carry no
    // panic messages, and their formatting code would triple Display's size).
    if let (Some(d), Some(s)) = (dst.get_mut(done..), src.get(done..)) {
        plain::copy(d, s);
    }
}

pub fn fill(dst: &mut [u32], color: u32) {
    let done = neon::fill(dst, color);
    if let Some(d) = dst.get_mut(done..) {
        plain::fill(d, color);
    }
}

/// Each pixel of `src` three times: `dst` holds three pixels for each.
pub fn widen3(dst: &mut [u32], src: &[u32]) {
    let n = src.len().min(dst.len() / 3);
    let (Some(dst), Some(src)) = (dst.get_mut(..3 * n), src.get(..n)) else { return };
    let done = neon::widen3(dst, src);
    if let (Some(d), Some(s)) = (dst.get_mut(3 * done..), src.get(done..)) {
        plain::widen3(d, s);
    }
}

/// The plain loops alone, to check the NEON ones against. On Arm they only
/// finish what the NEON loops left (under 16 pixels), so there they are
/// kept as loops: the compiler would otherwise call a general memcpy and
/// memset, which cost Display more code than everything else here.
pub mod plain {
    // On a 64-bit Arm device (aarch64-unknown-none) these loops are the
    // whole path, and they stay one 32-bit store a pixel too: the compiler
    // would otherwise widen them to 16-byte vector stores or memset, which
    // fault on a surface the kernel maps as device memory, where every access
    // must be aligned to its own size.
    #[inline(always)]
    fn put(d: &mut u32, p: u32) {
        #[cfg(any(target_arch = "arm", all(target_arch = "aarch64", target_os = "none")))]
        unsafe {
            core::ptr::write_volatile(d, p)
        }
        #[cfg(not(any(target_arch = "arm", all(target_arch = "aarch64", target_os = "none"))))]
        {
            *d = p;
        }
    }
    pub fn copy(dst: &mut [u32], src: &[u32]) {
        for (d, s) in dst.iter_mut().zip(src) {
            put(d, *s);
        }
    }
    pub fn fill(dst: &mut [u32], color: u32) {
        for d in dst {
            put(d, color);
        }
    }
    pub fn widen3(dst: &mut [u32], src: &[u32]) {
        for (d, s) in dst.chunks_exact_mut(3).zip(src) {
            for d in d {
                put(d, *s);
            }
        }
    }
}

// The loops below use d0-d7 without declaring them: they cannot be named as
// operands in this soft-float build (no `vfp2` feature, so the compiler
// itself never uses those registers; rustc refuses `out("d0")` here). A build
// with hardware float would have to declare them, so it is refused instead
// of silently corrupting the caller's floating-point registers. (Stable
// rustc does not expose Arm's float features to cfg; the hard-float ABI it
// does. Turning them on by -Ctarget-feature alone is unstable and not done.)
#[cfg(all(target_arch = "arm", any(target_abi = "eabihf", target_feature = "vfp2", target_feature = "neon")))]
compile_error!("blit.rs: declare d0-d7 as clobbers of the NEON loops before building with hardware float");
/// Each returns how many pixels (of the source, for `widen3`) it handled;
/// the caller finishes the rest.
#[cfg(target_arch = "arm")]
mod neon {
    use core::arch::asm;

    pub fn copy(dst: &mut [u32], src: &[u32]) -> usize {
        let blocks = dst.len() / 16;
        if blocks != 0 {
            unsafe {
                asm!(
                    ".fpu neon",
                    "2:",
                    "pld [{s}, #256]",
                    "vld1.32 {{d0-d3}}, [{s}]!",
                    "vld1.32 {{d4-d7}}, [{s}]!",
                    "vst1.32 {{d0-d3}}, [{d}]!",
                    "vst1.32 {{d4-d7}}, [{d}]!",
                    "subs {n}, {n}, #1",
                    "bne 2b",
                    d = inout(reg) dst.as_mut_ptr() => _,
                    s = inout(reg) src.as_ptr() => _,
                    n = inout(reg) blocks => _,
                    options(nostack),
                );
            }
        }
        blocks * 16
    }

    pub fn fill(dst: &mut [u32], color: u32) -> usize {
        let blocks = dst.len() / 16;
        if blocks != 0 {
            unsafe {
                asm!(
                    ".fpu neon",
                    "vdup.32 q0, {c}",
                    "vmov q1, q0",
                    "2:",
                    "vst1.32 {{d0-d3}}, [{d}]!",
                    "vst1.32 {{d0-d3}}, [{d}]!",
                    "subs {n}, {n}, #1",
                    "bne 2b",
                    d = inout(reg) dst.as_mut_ptr() => _,
                    c = in(reg) color,
                    n = inout(reg) blocks => _,
                    options(nostack),
                );
            }
        }
        blocks * 16
    }

    pub fn widen3(dst: &mut [u32], src: &[u32]) -> usize {
        let blocks = src.len() / 4;
        if blocks != 0 {
            unsafe {
                asm!(
                    ".fpu neon",
                    "2:",
                    "vld1.32 {{d0-d1}}, [{s}]!",
                    "vmov q1, q0",
                    "vmov q2, q0",
                    // Element by element from three registers: p0 p0 p0 p1 p1 p1.
                    "vst3.32 {{d0, d2, d4}}, [{d}]!",
                    "vst3.32 {{d1, d3, d5}}, [{d}]!",
                    "subs {n}, {n}, #1",
                    "bne 2b",
                    d = inout(reg) dst.as_mut_ptr() => _,
                    s = inout(reg) src.as_ptr() => _,
                    n = inout(reg) blocks => _,
                    options(nostack),
                );
            }
        }
        blocks * 4
    }
}

#[cfg(not(target_arch = "arm"))]
mod neon {
    pub fn copy(_: &mut [u32], _: &[u32]) -> usize {
        0
    }
    pub fn fill(_: &mut [u32], _: u32) -> usize {
        0
    }
    pub fn widen3(_: &mut [u32], _: &[u32]) -> usize {
        0
    }
}
