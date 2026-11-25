//! The Bluetooth controller in the MT6765 connectivity chip. Radio already
//! runs the chip for Wi-Fi; WMT function control turns its Bluetooth core on,
//! and HCI then travels in STP channel 0 beside WMT (wmt.rs).
//!
//! Open apps reach the controller through two pages shared with the Open
//! Runtime (runtime/open/hci.rs has the same layout), at 0x0b040000 here:
//! word 0 magic "HCI1" (Radio), 1 state, 2 last error (Radio), 3 power
//! request (Open Runtime: 1 on), 4 bytes queued for the controller in total
//! (Open Runtime), 5 bytes of them sent (Radio), 6 bytes received in total
//! (Radio), 7 bytes of them read (Open Runtime), 8 bytes dropped because the
//! event ring was full, 9-11 STP frames received, sent and rejected, 12-13
//! the controller's address, 14-15 its Read Local Version reply. The command
//! ring (H4 packets, type byte first) is bytes 2048-4095 of the first page,
//! the event ring the whole second page.
//!
//! USB (`moto-package bt-state`) reads a copy of words 0-15 at 0x09200800 and
//! the first 1 KiB the controller sent at 0x09200a00.
use super::wmt;

const BASE: usize = 0x0b04_0000;
const TX: usize = BASE + 2048;
const TX_SIZE: u32 = 2048;
const RX: usize = BASE + 4096;
const RX_SIZE: u32 = 4096;
pub const MAGIC: u32 = 0x3149_4348; // "HCI1"
pub const OFF: u32 = 0;
pub const STARTING: u32 = 1;
pub const ON: u32 = 2;
pub const FAILED: u32 = 3;

const TELEMETRY: usize = 0x0920_0800;
const LOG: usize = 0x0920_0a00;
const LOG_SIZE: u32 = 1024;

fn get(i: usize) -> u32 {
    unsafe { (BASE as *const u32).add(i).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { (BASE as *mut u32).add(i).write_volatile(v) }
}
fn log_total() -> u32 {
    unsafe { (TELEMETRY as *const u32).add(16).read_volatile() }
}

fn sink(bytes: &[u8]) {
    let mut head = get(6);
    let tail = get(7);
    let mut logged = log_total();
    for &b in bytes {
        if logged < LOG_SIZE {
            unsafe { ((LOG + logged as usize) as *mut u8).write_volatile(b) };
            logged += 1;
        }
        if head.wrapping_sub(tail) >= RX_SIZE {
            set(8, get(8).wrapping_add(1));
            continue;
        }
        unsafe { ((RX + (head % RX_SIZE) as usize) as *mut u8).write_volatile(b) };
        head = head.wrapping_add(1);
    }
    unsafe { (TELEMETRY as *mut u32).add(16).write_volatile(logged) };
    super::radio_io::barrier();
    set(6, head);
}

fn tx_byte(at: u32) -> u8 {
    unsafe { ((TX + (at % TX_SIZE) as usize) as *const u8).read_volatile() }
}
/// Sends every complete H4 packet the Open Runtime has queued, one STP frame each.
fn pump() {
    let head = get(4);
    let mut tail = get(5);
    while head.wrapping_sub(tail) >= 4 {
        let queued = head.wrapping_sub(tail);
        let len = match tx_byte(tail) {
            // Command: opcode, length.
            1 => 4 + tx_byte(tail + 3) as u32,
            // ACL data: handle, 16-bit length.
            2 if queued >= 5 => {
                5 + u32::from(u16::from_le_bytes([tx_byte(tail + 3), tx_byte(tail + 4)]))
            }
            2 => break,
            _ => {
                // Not a packet the controller takes: drop everything queued.
                set(2, 0x30);
                set(5, head);
                return;
            }
        };
        if len > 1028 || len > TX_SIZE {
            set(2, 0x31);
            set(5, head);
            return;
        }
        if queued < len {
            break;
        }
        let mut packet = [0u8; 1028];
        for (i, b) in packet[..len as usize].iter_mut().enumerate() {
            *b = tx_byte(tail + i as u32);
        }
        if let Err(e) = wmt::send_bt(&packet[..len as usize]) {
            set(2, e);
        }
        tail = tail.wrapping_add(len);
        set(5, tail);
    }
}

pub fn publish() {
    let c = wmt::counts();
    set(9, c.rx_frames);
    set(10, c.tx_frames);
    set(11, c.bad);
    for i in 0..16 {
        unsafe { (TELEMETRY as *mut u32).add(i).write_volatile(get(i)) };
    }
}

/// Radio's event loop calls this on every pass while the chip runs.
pub fn service() {
    wmt::poll();
    if get(1) == ON {
        pump();
    }
    publish();
}

/// Reads H4 events from the ring until one answers `opcode`, for up to `ms`.
/// Returns the return parameters after the status byte. Used only while
/// Radio starts the controller, before the Open Runtime may use the rings.
fn wait_complete(opcode: u16, ms: u32, out: &mut [u8; 64]) -> Result<usize, u32> {
    use super::radio_io::{expired, now};
    let start = now();
    let mut ev = [0u8; 260];
    let mut have = 0usize;
    loop {
        wmt::poll();
        while get(7) != get(6) {
            let tail = get(7);
            let b = unsafe { ((RX + (tail % RX_SIZE) as usize) as *const u8).read_volatile() };
            set(7, tail.wrapping_add(1));
            if have == 0 && b != 4 {
                continue;
            }
            ev[have] = b;
            have += 1;
            if have >= 3 && have == 3 + ev[2] as usize {
                have = 0;
                let p = &ev[3..3 + ev[2] as usize];
                // Command Complete: packets, opcode, status, parameters.
                if ev[1] == 0x0e && p.len() >= 4 && u16::from_le_bytes([p[1], p[2]]) == opcode {
                    if p[3] != 0 {
                        return Err(0x100 | p[3] as u32);
                    }
                    let n = (p.len() - 4).min(64);
                    out[..n].copy_from_slice(&p[4..4 + n]);
                    return Ok(n);
                }
                // Command Status with an error.
                if ev[1] == 0x0f && p.len() >= 4 && u16::from_le_bytes([p[2], p[3]]) == opcode && p[0] != 0 {
                    return Err(0x100 | p[0] as u32);
                }
            }
        }
        if expired(start, ms) {
            return Err(0x40);
        }
    }
}
fn command(opcode: u16, params: &[u8], out: &mut [u8; 64]) -> Result<usize, u32> {
    let mut p = [0u8; 68];
    let code = opcode.to_le_bytes();
    p[..4].copy_from_slice(&[1, code[0], code[1], params.len() as u8]);
    p[4..4 + params.len()].copy_from_slice(params);
    wmt::send_bt(&p[..4 + params.len()])?;
    wait_complete(opcode, 2000, out)
}

fn start() -> Result<(), u32> {
    wmt::bluetooth(true)?;
    let mut out = [0u8; 64];
    // MediaTek's Bluetooth HAL for CONNAC chips (radiomod.c, with the
    // stBtDefault_connac_soc_1_0 defaults) sets the radio, the transmit power
    // offsets and sleep, then resets. The address stays the chip's default:
    // apps use a random one.
    command(0xfc79, &[7, 0, 0, 0, 5, 7, 0, 0], &mut out)?;
    command(0xfc93, &[0xff; 16], &mut out)?;
    command(0xfc7a, &[3, 4, 0, 0, 0, 0, 0], &mut out)?;
    command(0x0c03, &[], &mut out)?; // Reset
    let n = command(0x1009, &[], &mut out)?; // Read BD_ADDR
    if n >= 6 {
        set(12, u32::from_le_bytes(out[..4].try_into().unwrap()));
        set(13, u32::from(u16::from_le_bytes([out[4], out[5]])));
    }
    let n = command(0x1001, &[], &mut out)?; // Read Local Version Information
    if n >= 8 {
        set(14, u32::from_le_bytes(out[..4].try_into().unwrap()));
        set(15, u32::from_le_bytes(out[4..8].try_into().unwrap()));
    }
    Ok(())
}
/// Turns the Bluetooth core on once WMT has the chip running, resets the
/// controller and opens the rings to the Open Runtime.
pub fn power_on() {
    for i in 0..16 {
        set(i, 0);
    }
    unsafe { (TELEMETRY as *mut u32).add(16).write_volatile(0) };
    set(0, MAGIC);
    set(1, STARTING);
    wmt::set_sink(sink);
    match start() {
        Ok(()) => {
            // The Open Runtime starts reading from here: nothing of the start-up remains.
            set(7, get(6));
            set(1, ON);
        }
        Err(e) => {
            set(2, e);
            set(1, FAILED);
        }
    }
    publish();
}

/// The chip is about to stop: the controller goes with it.
pub fn power_off() {
    if get(0) == MAGIC && get(1) != OFF {
        set(1, OFF);
        wmt::set_sink(|_| {});
        publish();
    }
}
