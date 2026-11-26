//! /dev/violet/hci: the Bluetooth controller's HCI for one open app at a time,
//! through two pages shared with Radio on the handset (targets/moto-g-pure/
//! bt.rs). QEMU has no controller, so there the HCI is never up. The layout
//! is described in bt.rs.
//!
//! Each write is one H4 packet, type byte first: a command (0x01) or ACL
//! data (0x02). Each read returns one H4 packet from the controller: an event
//! (0x04) or ACL data (0x02). Both give ENETDOWN while the controller is not
//! on (on the handset it runs while Radio runs the connectivity chip).
//!
//! The runtime notes the commands that make the phone visible or listening,
//! so Settings reports what the app did rather than what it says it did, and
//! it resets the controller when the app lets go, so nothing it started
//! outlives it.
use core::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicUsize, Ordering};

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

static OPEN: AtomicBool = AtomicBool::new(false);
/// The app slot holding the HCI, while OPEN.
static HOLDER: AtomicUsize = AtomicUsize::new(0);
/// What the holder's commands left the controller doing: ADVERTISING and the bits after it.
static SEEN: AtomicU32 = AtomicU32::new(0);

pub const ADVERTISING: u32 = 1;
pub const SCANNING: u32 = 2;
/// The scan sends scan requests, which ask devices nearby for more.
pub const ACTIVE_SCAN: u32 = 4;
/// Advertising uses a random address. Without it, the controller's own
/// public address, which is the default.
pub const RANDOM_ADV: u32 = 8;
/// Scanning (active scans send it) uses a random address.
pub const RANDOM_SCAN: u32 = 16;

const RESET: u16 = 0x0c03;
const LE_SET_ADV_PARAMS: u16 = 0x2006;
const LE_SET_ADV_ENABLE: u16 = 0x200a;
const LE_SET_SCAN_PARAMS: u16 = 0x200b;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;

/// `seen` after the controller takes the H4 packet `p`. Parameters take
/// effect when the next enable comes, as the controller applies them.
pub fn noted(seen: u32, p: &[u8]) -> u32 {
    if p.len() < 4 || p[0] != 1 {
        return seen;
    }
    let params = &p[4..];
    let flag = |seen: u32, bit: u32, on: bool| if on { seen | bit } else { seen & !bit };
    // Own address types 1 and 3 are random (3 when the controller has no key to resolve).
    match u16::from_le_bytes([p[1], p[2]]) {
        RESET => 0,
        // Interval min and max, type, own address type, …
        LE_SET_ADV_PARAMS if params.len() >= 6 => flag(seen, RANDOM_ADV, params[5] & 1 == 1),
        LE_SET_ADV_ENABLE if !params.is_empty() => flag(seen, ADVERTISING, params[0] != 0),
        // Type, interval, window, own address type, filter.
        LE_SET_SCAN_PARAMS if params.len() >= 6 => flag(flag(seen, ACTIVE_SCAN, params[0] == 1), RANDOM_SCAN, params[5] & 1 == 1),
        LE_SET_SCAN_ENABLE if !params.is_empty() => flag(seen, SCANNING, params[0] != 0),
        _ => seen,
    }
}
/// What the controller was told to do (ADVERTISING and the bits after it); 0 while no app holds it.
pub fn seen() -> u32 {
    if is_open() { SEEN.load(Ordering::Relaxed) } else { 0 }
}
/// The app slot holding the HCI, if one does.
pub fn holder() -> Option<usize> {
    is_open().then(|| HOLDER.load(Ordering::Relaxed))
}
/// Radio's words for the Bluetooth page: state, last error, STP frames
/// received, sent and rejected, event bytes dropped, Read Local Version.
pub fn telemetry() -> [u32; 8] {
    if get(0) != MAGIC {
        return [0; 8];
    }
    [get(1), get(2), get(9), get(10), get(11), get(8), get(14), get(15)]
}

pub fn up() -> bool {
    get(0) == MAGIC && get(1) == ON
}
/// Whether an app has /dev/violet/hci open.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Claims the controller for the app in slot `app`. Events that arrived before are dropped.
pub fn open(app: usize) -> bool {
    if OPEN.swap(true, Ordering::Relaxed) {
        return false;
    }
    HOLDER.store(app, Ordering::Relaxed);
    SEEN.store(0, Ordering::Relaxed);
    set(3, 1);
    set(7, get(6));
    true
}
/// Lets go of the controller. A Reset stops any advertising and scanning the
/// app left running, even when it crashed or was closed mid-way.
pub fn close() {
    if up() {
        send(&[1, 0x03, 0x0c, 0]);
    }
    SEEN.store(0, Ordering::Relaxed);
    set(3, 0);
    OPEN.store(false, Ordering::Relaxed);
}
/// The length of the complete packet at the front of the event ring, if any.
/// A byte that starts no packet is skipped, so the stream resynchronizes.
fn next() -> Option<u32> {
    loop {
        let head = get(6);
        let tail = get(7);
        let have = head.wrapping_sub(tail);
        if have > RX_SIZE {
            // Radio restarted the controller: start again from what is there.
            set(7, head);
            return None;
        }
        if have == 0 {
            return None;
        }
        let need = match rx_byte(tail) {
            4 if have >= 3 => 3 + rx_byte(tail + 2) as u32,
            2 if have >= 5 => 5 + u32::from(u16::from_le_bytes([rx_byte(tail + 3), rx_byte(tail + 4)])),
            4 | 2 => return None,
            _ => {
                set(7, tail.wrapping_add(1));
                continue;
            }
        };
        return (have >= need).then_some(need);
    }
}
pub fn readable() -> bool {
    up() && next().is_some()
}
/// One packet from the controller, if a whole one has arrived.
pub fn take(max: usize) -> Option<alloc::vec::Vec<u8>> {
    let n = next()?;
    fence(Ordering::Acquire);
    let tail = get(7);
    let v: alloc::vec::Vec<u8> = (0..n.min(max as u32)).map(|k| rx_byte(tail.wrapping_add(k))).collect();
    set(7, tail.wrapping_add(n));
    Some(v)
}
pub fn space() -> u32 {
    TX_SIZE - get(4).wrapping_sub(get(5)).min(TX_SIZE)
}
/// Checks that `p` is one whole command or ACL packet.
pub fn valid(p: &[u8]) -> bool {
    match p.first() {
        Some(1) => p.len() >= 4 && p.len() == 4 + p[3] as usize,
        Some(2) => p.len() >= 5 && p.len() == 5 + u16::from_le_bytes([p[3], p[4]]) as usize && p.len() <= 1028,
        _ => false,
    }
}
/// Queues one packet; false when the ring has no room for it now.
pub fn send(p: &[u8]) -> bool {
    if space() < p.len() as u32 {
        return false;
    }
    let mut head = get(4);
    for &b in p {
        unsafe { ((TX + (head % TX_SIZE) as usize) as *mut u8).write_volatile(b) };
        head = head.wrapping_add(1);
    }
    fence(Ordering::Release);
    set(4, head);
    SEEN.store(noted(SEEN.load(Ordering::Relaxed), p), Ordering::Relaxed);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(op: u16, params: &[u8]) -> std::vec::Vec<u8> {
        let mut p = std::vec![1, op as u8, (op >> 8) as u8, params.len() as u8];
        p.extend_from_slice(params);
        p
    }

    #[test]
    fn bitchat_start() {
        // Bitchat's own sequence (apps/open/bitchat/src/ble.rs): random address, passive scan.
        let mut s = 0;
        for p in [
            cmd(RESET, &[]),
            cmd(LE_SET_ADV_PARAMS, &[0xa0, 0, 0xa0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 0]),
            cmd(LE_SET_SCAN_PARAMS, &[0, 0xa0, 0, 0x60, 0, 1, 0]),
            cmd(LE_SET_SCAN_ENABLE, &[1, 1]),
            cmd(LE_SET_ADV_ENABLE, &[1]),
        ] {
            s = noted(s, &p);
        }
        assert_eq!(s, ADVERTISING | SCANNING | RANDOM_ADV | RANDOM_SCAN);
        s = noted(s, &cmd(LE_SET_ADV_ENABLE, &[0]));
        assert_eq!(s, SCANNING | RANDOM_ADV | RANDOM_SCAN);
        assert_eq!(noted(s, &cmd(RESET, &[])), 0);
    }

    #[test]
    fn active_public_scan() {
        let s = noted(RANDOM_SCAN, &cmd(LE_SET_SCAN_PARAMS, &[1, 0x10, 0, 0x10, 0, 0, 0]));
        assert_eq!(noted(s, &cmd(LE_SET_SCAN_ENABLE, &[1, 0])), SCANNING | ACTIVE_SCAN);
    }

    #[test]
    fn advertising_without_parameters_is_public() {
        assert_eq!(noted(0, &cmd(LE_SET_ADV_ENABLE, &[1])), ADVERTISING);
    }

    #[test]
    fn ignores_data_and_short_commands() {
        assert_eq!(noted(ADVERTISING, &[2, 1, 0, 1, 0, 9]), ADVERTISING);
        assert_eq!(noted(ADVERTISING, &cmd(LE_SET_ADV_ENABLE, &[])), ADVERTISING);
        assert_eq!(noted(0, &cmd(0x0c13, &[0])), 0);
    }
}
