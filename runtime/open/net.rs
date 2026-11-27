//! TCP and UDP for open apps, through pages shared with Radio (on the handset) or with
//! the emulator's host relay (in QEMU). The layout matches
//! targets/moto-g-pure/ip/sockets.c and tools/net_relay.py.
//!
//! DNS never leaves the phone as a query: the Open Runtime answers a program's
//! UDP queries to port 53 itself, giving each name an address in 198.18.0.0/15,
//! and a connect to such an address hands Radio the name instead. Radio then
//! resolves it on a joined network, or lets the phone's relay resolve it.
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(not(target_arch = "aarch64"))]
const BASE: usize = 0x0940_0000;
// The 64-bit QEMU root maps the relay above its embedded ELF and filesystem.
#[cfg(target_arch = "aarch64")]
pub const BASE: usize = 0x7_0000_0000;
pub const SLOTS: usize = if cfg!(target_arch = "aarch64") { 64 } else { 8 };
const DATA_OFFSET: usize = (256 + 128 * SLOTS + 4095) & !4095;
const TX: u32 = 8192;
const RX: u32 = 16384;
const STRIDE: usize = (TX + RX) as usize;
const MAGIC: u32 = 0x3154_454e;
pub const CMD_CONNECT: u32 = 1;
pub const CMD_CLOSE: u32 = 2;
/// Open a UDP socket: the rings then carry datagrams, each a little-endian
/// u16 length of what follows, the IPv4 address (u32), the port (u16), a
/// name length (u8, sending only) and name, then the payload. Radio puts the
/// socket's local port in word 12 and the address it has on the network in 13.
pub const CMD_UDP: u32 = 3;
/// The largest UDP payload an app can send.
pub const MAX_DATAGRAM: usize = 1472;
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
    (BASE + DATA_OFFSET + slot * STRIDE) as *mut u8
}
fn rx(slot: usize) -> *const u8 {
    (BASE + DATA_OFFSET + slot * STRIDE + TX as usize) as *const u8
}

/// Slots in use, one bit each. The Open Runtime is single-threaded.
static USED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The slot holds a connection that has not been closed.
pub fn in_use(slot: usize) -> bool {
    USED.load(core::sync::atomic::Ordering::SeqCst) & (1 << slot) != 0
}

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
    /// The source of the datagram read last (for recvfrom).
    pub last_from: (u32, u16),
}
impl Net {
    pub fn new(present: bool) -> Net {
        Net { present, names: Vec::new(), last_from: (0, 0) }
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
    /// The name behind a stand-in address, or the address in dotted form.
    pub fn host_for(&self, ip: u32) -> String {
        match self.name_for(ip) {
            Some(n) => String::from(n),
            None => alloc::format!("{}.{}.{}.{}", ip >> 24, (ip >> 16) & 255, (ip >> 8) & 255, ip & 255),
        }
    }
    /// An address the resolver handed out for a name.
    pub fn is_stand_in(&self, ip: u32) -> bool {
        ip & FAKE_MASK == FAKE_BASE
    }
    fn name_for(&self, ip: u32) -> Option<&str> {
        if ip & FAKE_MASK != FAKE_BASE {
            return None;
        }
        self.names.get((ip - FAKE_BASE).wrapping_sub(1) as usize).map(|s| s.as_str())
    }
    /// Start a connection; returns the slot.
    pub fn connect(&mut self, ip: u32, port: u16) -> Result<usize, i32> {
        self.start(CMD_CONNECT, ip, port)
    }
    /// Open a UDP socket; returns the slot.
    pub fn udp_open(&mut self) -> Result<usize, i32> {
        self.start(CMD_UDP, 0, 0)
    }
    fn start(&mut self, cmd: u32, ip: u32, port: u16) -> Result<usize, i32> {
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
        for i in [7, 8, 9, 10, 11, 12, 13] {
            set(slot, i, 0);
        }
        set(slot, 3, CONNECTING);
        set(slot, 4, 0);
        set(slot, 1, cmd);
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
    pub fn peek(&self, slot: usize, max: usize) -> Vec<u8> {
        let n = (self.rx_ready(slot) as usize).min(max);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let tail = get(slot, 10);
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            out.push(unsafe { rx(slot).add((tail.wrapping_add(k as u32) % RX) as usize).read_volatile() });
        }
        out
    }
    pub fn recv(&mut self, slot: usize, max: usize) -> Vec<u8> {
        let out = self.peek(slot, max);
        set(slot, 10, get(slot, 10).wrapping_add(out.len() as u32));
        out
    }
    /// A UDP socket's local address: (the phone's address on the network, port).
    pub fn local(&self, slot: usize) -> (u32, u16) {
        (get(slot, 13), get(slot, 12) as u16)
    }
    /// Queue a datagram on a UDP slot; false when the ring has no room for it.
    /// `name` is sent instead of a stand-in address, for Radio to resolve.
    pub fn send_datagram(&mut self, slot: usize, ip: u32, port: u16, data: &[u8]) -> bool {
        let name = self.name_for(ip).map(|n| String::from(n)).unwrap_or_default();
        let name = &name.as_bytes()[..name.len().min(63)];
        let data = &data[..data.len().min(MAX_DATAGRAM)];
        let body = 7 + name.len() + data.len();
        if (self.tx_space(slot) as usize) < body + 2 {
            return false;
        }
        let mut frame = Vec::with_capacity(body + 2);
        frame.extend_from_slice(&(body as u16).to_le_bytes());
        frame.extend_from_slice(&if name.is_empty() { ip } else { 0 }.to_le_bytes());
        frame.extend_from_slice(&port.to_le_bytes());
        frame.push(name.len() as u8);
        frame.extend_from_slice(name);
        frame.extend_from_slice(data);
        self.send(slot, &frame) == frame.len()
    }
    /// The next datagram a UDP slot received: its source and payload.
    pub fn recv_datagram(&mut self, slot: usize) -> Option<((u32, u16), Vec<u8>)> {
        let ready = self.rx_ready(slot) as usize;
        if ready < 2 {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let tail = get(slot, 10);
        let at = |k: u32| unsafe { rx(slot).add((tail.wrapping_add(k) % RX) as usize).read_volatile() };
        let body = u16::from_le_bytes([at(0), at(1)]) as usize;
        if ready < body + 2 || body < 7 {
            return None;
        }
        let frame = self.recv(slot, body + 2);
        let ip = u32::from_le_bytes(frame[2..6].try_into().unwrap());
        let port = u16::from_le_bytes([frame[6], frame[7]]);
        let skip = 9 + frame[8] as usize;
        Some(((ip, port), frame.get(skip..).unwrap_or(&[]).to_vec()))
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
