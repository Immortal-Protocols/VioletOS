//! /dev/violet/hci: the Bluetooth controller's HCI, shared by up to four open
//! apps at once (hcimux.rs decides who sees and may do what), through two
//! pages shared with Radio on the handset (targets/moto-g-pure/bt.rs) or with
//! the emulator's virtual controller in QEMU (tools/bluetooth_controller.py).
//! The layout is described in bt.rs.
//!
//! Each write is one H4 packet, type byte first: a command (0x01) or ACL
//! data (0x02). Each read returns one H4 packet for this app: an event
//! (0x04) or ACL data (0x02). Both give ENETDOWN while the controller is not
//! on (on the handset it runs while Radio runs the connectivity chip).
use core::sync::atomic::{fence, Ordering};

#[path = "hcimux.rs"]
pub mod mux;

const BASE: usize = 0x0944_0000;
const TX: usize = BASE + 2048;
const TX_SIZE: u32 = 2048;
const RX: usize = BASE + 4096;
const RX_SIZE: u32 = 4096;
const MAGIC: u32 = 0x3149_4348;
const ON: u32 = 2;

fn get(i: usize) -> u32 {
    unsafe { (BASE as *const u32).add(i).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { (BASE as *mut u32).add(i).write_volatile(v) }
}
fn rx_byte(at: u32) -> u8 {
    unsafe { ((RX + (at % RX_SIZE) as usize) as *const u8).read_volatile() }
}
fn now_ms() -> u64 {
    super::sys::now_ns() / 1_000_000
}

/// The multiplexer while any app has the device open; whether the controller
/// was on when last looked at. The Open Runtime's event loop is one thread.
static mut STATE: Option<alloc::boxed::Box<(mux::Mux, bool)>> = None;
fn state() -> &'static mut Option<alloc::boxed::Box<(mux::Mux, bool)>> {
    unsafe { &mut *core::ptr::addr_of_mut!(STATE) }
}

pub fn up() -> bool {
    get(0) == MAGIC && get(1) == ON
}
/// Whether any app has /dev/violet/hci open.
pub fn is_open() -> bool {
    state().as_ref().map_or(false, |b| b.0.clients() > 0)
}

/// A client for process `pid` of app `app` (its Shield owner name; `system`:
/// a core app from the boot image), or None when this process or another of
/// the same app already has one or no place is left (hcimux.rs `open`). The
/// first client powers the controller and drops what arrived before.
pub fn open(pid: u32, app: &[u8], system: bool) -> Option<u8> {
    let s = state();
    if s.is_none() {
        set(3, 1);
        set(7, get(6));
        *s = Some(alloc::boxed::Box::new((mux::Mux::new(now_ms()), up())));
    }
    s.as_mut().unwrap().0.open(pid, app, system)
}
/// Ends client `c`: the multiplexer disconnects its links and stops what it
/// ran (tick finishes that after the last client is gone).
pub fn close(c: u8) {
    let Some(b) = state().as_mut() else { return };
    b.0.close(c, now_ms());
    pump();
    tick();
}
/// Called from the runtime's poll: keeps the multiplexer's own commands going
/// while no app reads, and lets it go once the last client's clean-up is done
/// (or the controller is off, which ends everything anyway).
pub fn tick() {
    if state().is_none() {
        return;
    }
    pump();
    if state().as_ref().map_or(false, |b| b.0.clients() == 0 && (b.0.idle() || !up())) {
        set(3, 0);
        *state() = None;
    }
}

/// The length of the complete packet at the front of the event ring, if any.
/// A byte that starts no packet is skipped, so the stream resynchronizes.
/// Err: Radio restarted the controller.
fn next() -> Result<Option<u32>, ()> {
    loop {
        let head = get(6);
        let tail = get(7);
        let have = head.wrapping_sub(tail);
        if have > RX_SIZE {
            set(7, head);
            return Err(());
        }
        if have == 0 {
            return Ok(None);
        }
        let need = match rx_byte(tail) {
            4 if have >= 3 => 3 + rx_byte(tail + 2) as u32,
            2 if have >= 5 => 5 + u32::from(u16::from_le_bytes([rx_byte(tail + 3), rx_byte(tail + 4)])),
            4 | 2 => return Ok(None),
            _ => {
                set(7, tail.wrapping_add(1));
                continue;
            }
        };
        return Ok((have >= need).then_some(need));
    }
}
fn space() -> u32 {
    TX_SIZE - get(4).wrapping_sub(get(5)).min(TX_SIZE)
}
/// Moves the controller's packets into the multiplexer and its packets
/// into the command ring, as far as there is room.
fn pump() {
    let Some(b) = state().as_mut() else { return };
    let (m, was_up) = &mut **b;
    let now = now_ms();
    let on = up();
    if on && !*was_up {
        set(7, get(6));
        m.restarted(now);
    }
    *was_up = on;
    if !on {
        return;
    }
    loop {
        match next() {
            Err(()) => m.restarted(now),
            Ok(Some(n)) => {
                fence(Ordering::Acquire);
                let tail = get(7);
                let p: alloc::vec::Vec<u8> = (0..n).map(|k| rx_byte(tail.wrapping_add(k))).collect();
                set(7, tail.wrapping_add(n));
                m.receive(&p, now);
            }
            Ok(None) => break,
        }
    }
    m.poll(now);
    while let Some(p) = m.out.first() {
        if space() < p.len() as u32 {
            break;
        }
        let mut head = get(4);
        for &b in p {
            unsafe { ((TX + (head % TX_SIZE) as usize) as *mut u8).write_volatile(b) };
            head = head.wrapping_add(1);
        }
        fence(Ordering::Release);
        set(4, head);
        m.out.remove(0);
    }
}

pub fn readable(c: u8) -> bool {
    pump();
    up() && state().as_ref().map_or(false, |b| b.0.readable(c))
}
pub fn writable(c: u8) -> bool {
    pump();
    up() && state().as_ref().map_or(false, |b| b.0.writable(c))
}
/// One packet for client `c`, cut to `max` bytes, if one is waiting.
pub fn take(c: u8, max: usize) -> Option<alloc::vec::Vec<u8>> {
    pump();
    let mut p = state().as_mut()?.0.take(c)?;
    p.truncate(max);
    Some(p)
}
/// One packet from client `c`.
pub fn send(c: u8, p: &[u8]) -> Result<(), mux::Refusal> {
    let Some(b) = state().as_mut() else { return Err(mux::Refusal::Invalid) };
    let r = b.0.write(c, p, now_ms());
    pump();
    r
}
