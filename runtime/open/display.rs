//! The Moto G Pure's display engine (MediaTek MMSYS), mapped by the
//! supervisor at 0x09800000: config, DISP_MUTEX, DISP_OVL0, DISP_OVL0_2L,
//! DISP_RDMA0 and DSI0, one 4 KiB page each (addresses from the phone's own
//! device tree). `@sys disp` logs their non-zero registers.
use alloc::string::String;
use core::fmt::Write;

pub const BLOCKS: [(&str, usize); 6] = [
    ("mmsys", 0x0980_0000),
    ("mutex", 0x0980_1000),
    ("ovl0", 0x0980_2000),
    ("ovl0_2l", 0x0980_3000),
    ("rdma0", 0x0980_4000),
    ("dsi0", 0x0980_5000),
];

pub fn read(block: usize, offset: usize) -> u32 {
    unsafe { ((BLOCKS[block].1 + offset) as *const u32).read_volatile() }
}

/// "name off=value ..." for every non-zero word of block b in [from, to).
/// A block whose clock is gated can stall the bus; the host reads a block
/// at a time, so a stall points at the block.
pub fn dump(b: usize, from: usize, to: usize) -> String {
    let mut line = String::new();
    let _ = write!(line, "{} {from:x}:", BLOCKS[b].0);
    for off in (from & !3..to.min(0x1000)).step_by(4) {
        let v = read(b, off);
        if v != 0 {
            let _ = write!(line, " {off:x}={v:x}");
        }
    }
    line
}

/// DISP_RDMA0's output line counter (offset 0xfc): the row the panel is
/// reading now, 0..1599; during vertical blanking it holds the last line.
pub fn scan_line() -> u32 {
    read(4, 0xfc) & 0x1fff
}

/// Sample the scan line for `ms` milliseconds: the frames seen (wraps), the
/// average frame period, and the largest and smallest line read.
pub fn sample_beam(ms: u64) -> String {
    let start = super::sys::now_ns();
    let (mut last, mut wraps, mut first_wrap, mut last_wrap) = (scan_line(), 0u32, 0u64, 0u64);
    let (mut lo, mut hi, mut reads, mut backwards) = (u32::MAX, 0u32, 0u32, 0u32);
    loop {
        let now = super::sys::now_ns();
        if now - start > ms * 1_000_000 {
            break;
        }
        let l = scan_line();
        reads += 1;
        lo = lo.min(l);
        hi = hi.max(l);
        if l + 200 < last {
            wraps += 1;
            if first_wrap == 0 {
                first_wrap = now;
            }
            last_wrap = now;
        } else if l < last {
            backwards += 1;
        }
        last = l;
    }
    let period = if wraps > 1 { (last_wrap - first_wrap) / (wraps as u64 - 1) / 1000 } else { 0 };
    alloc::format!("beam: {reads} reads, {wraps} frames, period {period} us, lines {lo}..={hi}, {backwards} small steps back")
}
