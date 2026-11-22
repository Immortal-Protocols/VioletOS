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
fn exchange(frame: &[u8], reply: &mut [u8], timeout_ms: u32) -> bool {
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
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), (DMA + TX + HEADER) as *mut u8, frame.len());
    }
    if !queue(0, 0, RX, 1600, true) || !queue(1, 128, TX, (HEADER + frame.len()) as u32, false) {
        return false;
    }
    fence();
    set(0x70, 15);
    set(0x50, 0);
    set(0x50, 1);
    let start = unsafe { ticks() };
    while unsafe { read_volatile((DMA + 50) as *const u16) } == 0 {
        unsafe {
            crate::protocol::delay_ms(1);
        }
        if unsafe { ticks().wrapping_sub(start) > ticks_per_ms * timeout_ms } {
            return false;
        }
    }
    fence();
    let len = unsafe { read_volatile((DMA + 56) as *const u32) };
    if len < (HEADER + reply.len()) as u32 {
        return false;
    }

    for (i, b) in reply.iter_mut().enumerate() {
        *b = unsafe { read_volatile((DMA + RX + HEADER + i) as *const u8) };
    }
    true
}
pub fn probe() -> bool {
    let mut frame = [0u8; 60];
    frame[..6].fill(255);
    frame[6..12].copy_from_slice(&MAC);
    frame[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
    frame[22..28].copy_from_slice(&MAC);
    frame[28..32].copy_from_slice(&GUEST);
    frame[38..42].copy_from_slice(&GATEWAY);
    let mut reply = [0; 42];
    if !exchange(&frame, &mut reply, 500) {
        return false;
    }
    reply[..6] == MAC
        && reply[12..22] == [8, 6, 0, 1, 8, 0, 6, 4, 0, 2]
        && reply[28..32] == GATEWAY
        && reply[32..38] == MAC
        && reply[38..42] == GUEST
}

const PEER: [u8; 6] = [2, 0x56, 0x49, 0x4f, 0x4c, 0x54];
fn transfer(op: u8, send: &[u8], recv: &mut [u8]) -> Option<usize> {
    let nonce = unsafe { ticks() }.to_be_bytes();
    let mut frame = [0; 1048];
    let mut reply = [0; 1048];
    frame[..6].copy_from_slice(&PEER);
    frame[6..12].copy_from_slice(&MAC);
    frame[12..14].copy_from_slice(&[0x88, 0xb5]);
    frame[14] = op;
    frame[16..20].copy_from_slice(&nonce);
    frame[20..22].copy_from_slice(&(send.len().max(recv.len()) as u16).to_be_bytes());
    frame[24..24 + send.len()].copy_from_slice(send);
    if !exchange(&frame, &mut reply, 5500)
        || reply[..6] != MAC
        || reply[6..12] != PEER
        || reply[12..14] != [0x88, 0xb5]
        || reply[14] != op
        || reply[15] != 1
        || reply[16..20] != nonce
    {
        return None;
    }
    let n = u16::from_be_bytes([reply[20], reply[21]]) as usize;
    if n > recv.len() {
        return None;
    }
    recv[..n].copy_from_slice(&reply[24..24 + n]);
    Some(n)
}
#[no_mangle]
pub unsafe extern "C" fn tunnel_read(_: usize, buf: *mut u8, n: usize) -> i32 {
    transfer(3, &[], core::slice::from_raw_parts_mut(buf, n.min(1024)))
        .filter(|n| *n > 0)
        .map_or(-1, |n| n as i32)
}
#[no_mangle]
pub unsafe extern "C" fn tunnel_write(_: usize, buf: *const u8, n: usize) -> i32 {
    let n = n.min(1024);
    transfer(2, core::slice::from_raw_parts(buf, n), &mut []).map_or(-1, |_| n as i32)
}
extern "C" {
    fn guest_https(uuid: *mut u8) -> i32;
    fn guest_https_text(host: *const u8, path: *const u8, out: *mut u8, cap: usize) -> i32;
    fn guest_https_range(host: *const u8, path: *const u8, from: u32, to: u32, out: *mut u8,
                         status: *mut u32, total: *mut u32) -> i32;
}
/// Bytes `from..=to` of a music library file, through the emulator relay.
pub fn endpoint_range(path: &[u8], from: u32, to: u32, out: &mut [u8]) -> Result<(usize, u32), u32> {
    let host = crate::protocol::MUSIC_HOST;
    if out.len() < (to - from + 1) as usize || transfer(1, &host[..host.len() - 1], &mut []).is_none() {
        return Err(0);
    }
    let (mut status, mut total) = (0, 0);
    let n = unsafe {
        guest_https_range(host.as_ptr(), path.as_ptr(), from, to, out.as_mut_ptr(), &mut status, &mut total)
    };
    transfer(4, &[], &mut []);
    if n < 0 { Err(status) } else { Ok((n as usize, total)) }
}
/// Fetch one line of weather for `city` from wttr.in through the emulator relay.
/// The connect command names the host; the Mac relay allows only known endpoints.
pub fn endpoint_weather(city: usize) -> Option<([u8; 40], usize)> {
    let mut path = [0; 64];
    crate::protocol::weather_path(city, &mut path)?;
    transfer(1, b"wttr.in", &mut [])?;
    let mut text = [0; 40];
    let n = unsafe { guest_https_text(b"wttr.in\0".as_ptr(), path.as_ptr(), text.as_mut_ptr(), 40) };
    transfer(4, &[], &mut []);
    (n > 0).then_some((text, n as usize))
}
pub fn endpoint_uuid() -> Option<[u8; 36]> {
    transfer(1, &[], &mut [])?;
    let mut uuid = [0; 36];
    let ok = unsafe { guest_https(uuid.as_mut_ptr()) } == 1;
    transfer(4, &[], &mut []);
    ok.then_some(uuid)
}
