//! Probe the private QEMU gateway using one TX and one RX virtio-net descriptor.
//! The radio process alone maps the device and its dedicated uncached DMA page.
use crate::protocol::{ticks, ticks_per_ms};
use core::ptr::{read_volatile, write_volatile};
const DEVICE: usize = 0x09001000;
const DMA: usize = 0x09002000;
const PHYSICAL: u32 = 0x49201000;
const MAC: [u8; 6] = [0x52, 0x54, 0, 0x56, 0, 1];
const GUEST: [u8; 4] = [10, 0, 2, 15];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
fn reg(n: usize) -> u32 {
    unsafe { read_volatile((DEVICE + n) as *const u32) }
}
fn set(n: usize, v: u32) {
    unsafe { write_volatile((DEVICE + n) as *mut u32, v) }
}
fn fence() {
    unsafe { core::arch::asm!("dmb sy", options(nostack, preserves_flags)) }
}
fn word(off: usize, v: u32) {
    unsafe { write_volatile((DMA + off) as *mut u32, v) }
}
fn half(off: usize, v: u16) {
    unsafe { write_volatile((DMA + off) as *mut u16, v) }
}
fn queue(index: u32, base: usize, buf: usize, length: u32, receive: bool) -> bool {
    set(0x30, index);
    if reg(0x34) < 2 || reg(0x44) != 0 {
        return false;
    }
    set(0x38, 2);
    for (r, off) in [(0x80, base), (0x90, base + 32), (0xa0, base + 48)] {
        set(r, PHYSICAL + off as u32);
        set(r + 4, 0);
    }
    word(base, PHYSICAL + buf as u32);
    word(base + 8, length);
    half(base + 12, if receive { 2 } else { 0 });
    half(base + 34, 1); // available.idx; ring[0] contains descriptor zero
    fence();
    set(0x44, 1);
    true
}
pub fn probe() -> bool {
    if reg(0) != 0x74726976 || reg(4) != 2 || reg(8) != 1 {
        return false;
    }
    set(0x70, 0);
    if reg(0x70) != 0 {
        return false;
    }
    set(0x70, 1);
    set(0x70, 3);
    set(0x14, 1);
    if reg(0x10) & 1 == 0 {
        return false;
    }
    set(0x24, 1);
    set(0x20, 1);
    set(0x24, 0);
    set(0x20, 0);
    set(0x70, 11);
    if reg(0x70) & 8 == 0 {
        return false;
    }
    const RX: usize = 512;
    const TX: usize = 2304;
    const HEADER: usize = 12;
    unsafe {
        core::ptr::write_bytes(DMA as *mut u8, 0, 4096);
    }
    let mut frame = [0u8; 60];
    frame[..6].fill(255);
    frame[6..12].copy_from_slice(&MAC);
    frame[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
    frame[22..28].copy_from_slice(&MAC);
    frame[28..32].copy_from_slice(&GUEST);
    frame[38..42].copy_from_slice(&GATEWAY);
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), (DMA + TX + HEADER) as *mut u8, frame.len());
    }
    if !queue(0, 0, RX, 1600, true) || !queue(1, 128, TX, (HEADER + 60) as u32, false) {
        return false;
    }
    fence();
    set(0x70, 15);
    set(0x50, 0);
    set(0x50, 1);
    let start = unsafe { ticks() };
    while unsafe { read_volatile((DMA + 50) as *const u16) } == 0 {
        if unsafe { ticks().wrapping_sub(start) > ticks_per_ms * 500 } {
            return false;
        }
    }
    fence();
    let len = unsafe { read_volatile((DMA + 56) as *const u32) };
    if len < (HEADER + 42) as u32 {
        return false;
    }
    let mut reply = [0; 42];
    for (i, b) in reply.iter_mut().enumerate() {
        *b = unsafe { read_volatile((DMA + RX + HEADER + i) as *const u8) };
    }
    reply[..6] == MAC
        && reply[12..22] == [8, 6, 0, 1, 8, 0, 6, 4, 0, 2]
        && reply[28..32] == GATEWAY
        && reply[32..38] == MAC
        && reply[38..42] == GUEST
}
