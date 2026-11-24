//! The Moto G Pure's DISP_RDMA0 (MediaTek MMSYS, 0x1400d000 in the phone's
//! device tree), mapped read-only by the supervisor at 0x09804000.

/// DISP_RDMA0's output line counter (offset 0xfc): the row the panel is
/// reading now, 0..1599; during vertical blanking it holds the last line.
pub fn scan_line() -> u32 {
    unsafe { (0x0980_40fc as *const u32).read_volatile() & 0x1fff }
}
