//! C entry points into the same socket rings and DNS responder used by the
//! production Open Runtime. The AArch64 root dispatches requests serially.
#![no_std]
#![allow(dead_code)]
extern crate alloc;
#[path = "../heap.rs"] mod heap;
#[path = "../net.rs"] mod net;

use core::{ptr, slice};

static mut NETWORK: Option<net::Net> = None;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    extern "C" { fn net_panic() -> !; }
    unsafe { net_panic() }
}

unsafe fn state() -> &'static mut net::Net {
    (*ptr::addr_of_mut!(NETWORK)).as_mut().unwrap()
}

#[no_mangle]
pub unsafe extern "C" fn ring_init(heap_base: usize, bytes: usize) -> usize {
    heap::HEAP.add(heap_base, bytes);
    NETWORK = Some(net::Net::new(true));
    net::BASE
}
#[no_mangle]
pub unsafe extern "C" fn ring_connect(ip: u32, port: u16) -> i32 {
    state().connect(ip, port).map(|s| s as i32).unwrap_or_else(|e| e)
}
#[no_mangle]
pub extern "C" fn ring_close(slot: usize) { net::close(slot); }
#[no_mangle]
pub unsafe extern "C" fn ring_state(slot: usize, error: *mut i32) -> u32 {
    let (status, errno) = state().state(slot);
    *error = errno;
    status
}
#[no_mangle]
pub unsafe extern "C" fn ring_local(slot: usize, ip: *mut u32, port: *mut u16) {
    let address = state().local(slot);
    *ip = address.0;
    *port = address.1;
}
#[no_mangle]
pub unsafe extern "C" fn ring_space(slot: usize) -> usize { state().tx_space(slot) as usize }
#[no_mangle]
pub unsafe extern "C" fn ring_avail(slot: usize) -> usize { state().rx_ready(slot) as usize }
#[no_mangle]
pub unsafe extern "C" fn ring_send(slot: usize, src: *const u8, len: usize) -> usize {
    state().send(slot, slice::from_raw_parts(src, len))
}
#[no_mangle]
pub unsafe extern "C" fn ring_recv(slot: usize, dst: *mut u8, len: usize, peek: bool) -> usize {
    let bytes = if peek { state().peek(slot, len) } else { state().recv(slot, len) };
    ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
    bytes.len()
}
#[no_mangle]
pub unsafe extern "C" fn ring_shutdown(slot: usize) { state().shutdown_send(slot); }
#[no_mangle]
pub unsafe extern "C" fn ring_dns(query: *const u8, len: usize, dst: *mut u8, capacity: usize) -> usize {
    let Some(reply) = net::answer_dns(state(), slice::from_raw_parts(query, len)) else { return 0 };
    if reply.len() > capacity { return 0; }
    ptr::copy_nonoverlapping(reply.as_ptr(), dst, reply.len());
    reply.len()
}

#[no_mangle]
pub unsafe extern "C" fn ring_udp_open() -> i32 {
    state().udp_open().map(|s| s as i32).unwrap_or_else(|e| e)
}
#[no_mangle]
pub unsafe extern "C" fn ring_udp_send(slot: usize, ip: u32, port: u16, src: *const u8, len: usize) -> bool {
    state().send_datagram(slot, ip, port, slice::from_raw_parts(src, len))
}
#[no_mangle]
pub unsafe extern "C" fn ring_udp_recv(slot: usize, ip: *mut u32, port: *mut u16, dst: *mut u8) -> i32 {
    let Some(((from, src), data)) = state().recv_datagram(slot) else { return -11 };
    *ip = from;
    *port = src;
    let len = data.len().min(net::MAX_DATAGRAM);
    ptr::copy_nonoverlapping(data.as_ptr(), dst, len);
    len as i32
}

#[no_mangle]
pub extern "C" fn ring_slots() -> usize { net::SLOTS }
