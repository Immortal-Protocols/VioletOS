//! TCP for open apps, through pages shared with Radio (on the handset) or with
//! the emulator's host relay (in QEMU). The layout matches
//! targets/moto-g-pure/ip/sockets.c and tools/net_relay.py.
//!
//! DNS never leaves the phone as a query: the Open Runtime answers a program's
//! UDP queries to port 53 itself, giving each name an address in 198.18.0.0/15,
//! and a connect to such an address hands Radio the name instead. Radio then
//! resolves it on a joined network, or lets the phone's relay resolve it.
use alloc::string::String;
use alloc::vec::Vec;

const BASE: usize = 0x0940_0000;
pub const SLOTS: usize = 8;
const TX: u32 = 8192;
const RX: u32 = 16384;
const STRIDE: usize = (TX + RX) as usize;
const MAGIC: u32 = 0x3154_454e;
pub const CMD_CONNECT: u32 = 1;
pub const CMD_CLOSE: u32 = 2;
pub const IDLE: u32 = 0;
pub const CONNECTING: u32 = 1;
pub const OPEN: u32 = 2;
pub const PEER_CLOSED: u32 = 3;
pub const FAILED: u32 = 4;
/// Stand-in addresses for names: 198.18.0.0/15.
const FAKE_BASE: u32 = 0xc612_0000;
const FAKE_MASK: u32 = 0xfffe_0000;

fn word(slot: usize, i: usize) -> *mut u32 {
    (BASE + 256 + slot * 128 + i * 4) as *mut u32
}
fn get(slot: usize, i: usize) -> u32 {
    unsafe { word(slot, i).read_volatile() }
}
fn set(slot: usize, i: usize, v: u32) {
    unsafe { word(slot, i).write_volatile(v) }
}
fn tx(slot: usize) -> *mut u8 {
    (BASE + 4096 + slot * STRIDE) as *mut u8
}
fn rx(slot: usize) -> *const u8 {
    (BASE + 4096 + slot * STRIDE + TX as usize) as *const u8
}

/// Slots in use, one bit each. The Open Runtime is single-threaded.
static USED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Close a connection; called when the last descriptor for it closes.
pub fn close(slot: usize) {
    set(slot, 1, CMD_CLOSE);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    set(slot, 0, get(slot, 0).wrapping_add(1));
    USED.fetch_and(!(1 << slot), core::sync::atomic::Ordering::SeqCst);
}

pub struct Net {
    /// The pages are mapped (the board has a socket server).
    pub present: bool,
    names: Vec<String>,
}
impl Net {
    pub fn new(present: bool) -> Net {
        Net { present, names: Vec::new() }
    }
    /// 0 offline, 1 joined network, 2 phone relay, 3 the emulator's host.
    pub fn online(&self) -> u32 {
        if !self.present {
            return 0;
        }
        let page = BASE as *const u32;
        unsafe {
            if page.read_volatile() != MAGIC {
                return 0;
            }
            page.add(1).read_volatile()
        }
    }
    /// The stand-in address for a name, allocating one the first time.
    pub fn address_for(&mut self, name: &str) -> u32 {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        let index = match self.names.iter().position(|n| *n == name) {
            Some(i) => i,
            None => {
                self.names.push(name);
                self.names.len() - 1
            }
        };
        FAKE_BASE + 1 + index as u32
    }
    fn name_for(&self, ip: u32) -> Option<&str> {
        if ip & FAKE_MASK != FAKE_BASE {
            return None;
        }
        self.names.get((ip - FAKE_BASE).wrapping_sub(1) as usize).map(|s| s.as_str())
    }
    /// Start a connection; returns the slot.
    pub fn connect(&mut self, ip: u32, port: u16) -> Result<usize, i32> {
        if self.online() == 0 {
            return Err(-101); // ENETUNREACH
        }
        let used = USED.load(core::sync::atomic::Ordering::SeqCst);
        let slot = (0..SLOTS).find(|&i| used & (1 << i) == 0).ok_or(-24)?;
        USED.fetch_or(1 << slot, core::sync::atomic::Ordering::SeqCst);
        let mut name = [0u8; 64];
        if let Some(n) = self.name_for(ip) {
            let b = n.as_bytes();
            name[..b.len().min(63)].copy_from_slice(&b[..b.len().min(63)]);
        }
        for (k, chunk) in name.chunks_exact(4).enumerate() {
            set(slot, 16 + k, u32::from_le_bytes(chunk.try_into().unwrap()));
        }
        set(slot, 5, ip);
        set(slot, 6, u32::from(port));
        // Both rings start empty for the new connection.
        for i in [7, 8, 9, 10, 11] {
            set(slot, i, 0);
        }
        set(slot, 3, CONNECTING);
        set(slot, 4, 0);
        set(slot, 1, CMD_CONNECT);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        set(slot, 0, get(slot, 0).wrapping_add(1));
        Ok(slot)
    }
    /// State, and the error (a positive errno) when it failed. A command the
    /// server has not taken yet still counts as connecting.
    pub fn state(&self, slot: usize) -> (u32, i32) {
        if get(slot, 0) != get(slot, 2) {
            return (CONNECTING, 0);
        }
        (get(slot, 3), get(slot, 4) as i32)
    }
    pub fn tx_space(&self, slot: usize) -> u32 {
        TX - get(slot, 7).wrapping_sub(get(slot, 8))
    }
    pub fn rx_ready(&self, slot: usize) -> u32 {
        get(slot, 9).wrapping_sub(get(slot, 10))
    }
    pub fn send(&mut self, slot: usize, data: &[u8]) -> usize {
        let n = (self.tx_space(slot) as usize).min(data.len());
        let head = get(slot, 7);
        for (k, b) in data[..n].iter().enumerate() {
            unsafe { tx(slot).add((head.wrapping_add(k as u32) % TX) as usize).write_volatile(*b) };
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        set(slot, 7, head.wrapping_add(n as u32));
        n
    }
    pub fn recv(&mut self, slot: usize, max: usize) -> Vec<u8> {
        let n = (self.rx_ready(slot) as usize).min(max);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let tail = get(slot, 10);
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            out.push(unsafe { rx(slot).add((tail.wrapping_add(k as u32) % RX) as usize).read_volatile() });
        }
        set(slot, 10, tail.wrapping_add(n as u32));
        out
    }
    pub fn shutdown_send(&mut self, slot: usize) {
        if get(slot, 11) == 0 {
            set(slot, 11, 1);
        }
    }
}

/// Answer a DNS query (RFC 1035) for an A record with a stand-in address,
/// and every other type with no records.
pub fn answer_dns(net: &mut Net, query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 || query[2] & 0x80 != 0 || u16::from_be_bytes([query[4], query[5]]) != 1 {
        return None;
    }
    let mut at = 12;
    let mut labels: Vec<&str> = Vec::new();
    loop {
        let len = *query.get(at)? as usize;
        at += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None;
        }
        labels.push(core::str::from_utf8(query.get(at..at + len)?).ok()?);
        at += len;
    }
    let qtype = u16::from_be_bytes([*query.get(at)?, *query.get(at + 1)?]);
    let question_end = at + 4;
    let mut r = Vec::with_capacity(question_end + 16);
    r.extend_from_slice(&query[..2]);
    r.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0]);
    r.extend_from_slice(query.get(12..question_end)?);
    if qtype == 1 && !labels.is_empty() {
        let name = labels.join(".");
        let ip = net.address_for(&name);
        r[7] = 1;
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        r.extend_from_slice(&ip.to_be_bytes());
    }
    Some(r)
}
