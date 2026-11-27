//! TCP and UDP for open apps, through pages shared with Radio (on the handset) or with
//! the emulator's host relay (in QEMU). The layout ("NET2") matches
//! targets/moto-g-pure/ip/sockets.c and tools/net_relay.py; tools/test_net_layout.py
//! checks that the three agree.
//!
//! Page 0: word 0 the magic, word 1 how the server is online, words 2-4 the
//! slots it serves and the ring sizes. A server with another magic or other
//! ring sizes is treated as no network at all. Slot headers of 512 bytes
//! follow from page 1 (16 words, then from byte 64 a name of up to 253 bytes
//! and its NUL; the rest is spare). Each slot has an 8 KiB send ring and a
//! 16 KiB receive ring: slots 0-8 from byte 0x5000, slots 9-31 from byte
//! 0x43000 (the Bluetooth pages, hci.rs, sit between the two runs).
//!
//! Of a slot's ring words the Open Runtime writes 7 (queued to send), 10
//! (read) and 11, the server 8 (sent) and 9 (received). The server empties
//! its two when it takes a command, so until it has (`taken`) they may
//! still be the slot's previous connection's, perhaps another app's: nothing
//! is read, and what is queued to send counts from zero.
//!
//! slots.rs decides who may take a slot; names.rs is the resolver's table.
//! A listening slot (CMD_LISTEN) holds no connection: the server counts the
//! connections waiting on it in word 15, and each accept (CMD_ACCEPT) takes
//! one into a slot of its own, so accepted connections count against the
//! app's share like any other. Loopback sockets never come here (loopback.rs).
//!
//! DNS never leaves the phone as a query: the Open Runtime answers a program's
//! UDP queries to port 53 itself, giving each name an address in 198.18.0.0/15,
//! and a connect to such an address hands Radio the name instead. Radio then
//! resolves it on a joined network, or lets the phone's relay resolve it.
use super::names::{self, Lookup, Names};
use super::slots;
use alloc::string::String;
use alloc::vec::Vec;

const BASE: usize = 0x0940_0000;
pub const SLOTS: usize = slots::SLOTS;
const TX: u32 = 8192;
const RX: u32 = 16384;
const STRIDE: usize = (TX + RX) as usize;
const MAGIC: u32 = 0x3254_454e;
/// Slot headers: where they start, and the size of one.
const HEADERS: usize = 0x1000;
const HEADER: usize = 512;
/// The name field of a header: from byte 64, NUL-terminated.
const NAME: usize = 256;
/// Rings: the first run of slots, how many it holds, and the second run.
const RINGS_A: usize = 0x5000;
const FIRST: usize = 9;
const RINGS_B: usize = 0x4_3000;
pub const CMD_CONNECT: u32 = 1;
pub const CMD_CLOSE: u32 = 2;
/// Open a UDP socket: the rings then carry datagrams, each a little-endian
/// u16 length of what follows, the IPv4 address (u32), the port (u16), a
/// name length (u8, sending only) and name, then the payload. Radio puts the
/// socket's local port in word 12 and the address it has on the network in 13.
pub const CMD_UDP: u32 = 3;
/// Listen for connections from the network: word 5 the backlog, word 6 the
/// port (never 0: the Open Runtime picks ports, loopback.rs). Radio listens
/// on every address the phone has; OPEN once listening (word 12 the port, 13
/// the phone's address), and word 15 counts the connections waiting to be
/// accepted. FAILED with EADDRINUSE, or EOPNOTSUPP through the phone's relay
/// (it carries outgoing connections only).
pub const CMD_LISTEN: u32 = 4;
/// Take the oldest connection waiting on a listening slot (word 5) of the
/// same owner: OPEN (or PEER_CLOSED) with the peer's port in word 12 and its
/// address in 13; FAILED with EAGAIN when none is waiting, EINVAL when word 5
/// is not that owner's listener.
pub const CMD_ACCEPT: u32 = 5;
/// The largest UDP payload an app can send.
pub const MAX_DATAGRAM: usize = 1472;
pub const IDLE: u32 = 0;
pub const CONNECTING: u32 = 1;
pub const OPEN: u32 = 2;
pub const PEER_CLOSED: u32 = 3;
pub const FAILED: u32 = 4;
const ENETUNREACH: i32 = -101;
/// A stand-in address whose name the resolver no longer has.
pub const EHOSTUNREACH: i32 = -113;

fn word(slot: usize, i: usize) -> *mut u32 {
    (BASE + HEADERS + slot * HEADER + i * 4) as *mut u32
}
fn get(slot: usize, i: usize) -> u32 {
    unsafe { word(slot, i).read_volatile() }
}
fn set(slot: usize, i: usize, v: u32) {
    unsafe { word(slot, i).write_volatile(v) }
}
fn tx(slot: usize) -> *mut u8 {
    (BASE + if slot < FIRST { RINGS_A + slot * STRIDE } else { RINGS_B + (slot - FIRST) * STRIDE }) as *mut u8
}
fn rx(slot: usize) -> *const u8 {
    unsafe { tx(slot).add(TX as usize) }
}
/// The server has taken the slot's last command: its rings are this use's.
fn taken(slot: usize) -> bool {
    get(slot, 0) == get(slot, 2)
}

/// Who holds each slot. The Open Runtime is single-threaded.
struct Shared(core::cell::UnsafeCell<slots::Table>);
unsafe impl Sync for Shared {}
static TABLE: Shared = Shared(core::cell::UnsafeCell::new(slots::Table::new()));
fn table() -> &'static mut slots::Table {
    unsafe { &mut *TABLE.0.get() }
}

/// The slot holds a connection that has not been closed.
pub fn in_use(slot: usize) -> bool {
    table().in_use(slot)
}

/// Slots an owner (a pid, or slots::SYSTEM) holds.
pub fn held(owner: u32) -> usize {
    table().held(owner)
}

/// Who holds a slot (a pid, or slots::SYSTEM).
pub fn owner(slot: usize) -> Option<u32> {
    table().owner(slot)
}

/// An address in dotted form.
pub fn literal(ip: u32) -> String {
    alloc::format!("{}.{}.{}.{}", ip >> 24, (ip >> 16) & 255, (ip >> 8) & 255, ip & 255)
}

/// Close a connection; called when the last descriptor for it closes.
pub fn close(slot: usize) {
    if !table().in_use(slot) {
        return;
    }
    // A socket with bytes the server has not sent yet lingers, and is closed
    // once they are gone (a stream also once the peer has closed: it is shut
    // for sending, so the server sends them, then FIN), or after LINGER_NS
    // (linger_tick). As on Linux, `send` then `close` (or exit) delivers
    // what was sent, a datagram included.
    let stream = matches!(get(slot, 1), CMD_CONNECT | CMD_ACCEPT);
    if (stream || get(slot, 1) == CMD_UDP) && taken(slot) && get(slot, 3) == OPEN && get(slot, 7) != get(slot, 8) {
        let l = lingering();
        if l.0 & (1 << slot) == 0 {
            if stream {
                set(slot, 11, 1);
            }
            l.0 |= 1 << slot;
            l.1[slot] = super::sys::now_ns() + LINGER_NS;
            return;
        }
    }
    lingering().0 &= !(1 << slot);
    set(slot, 1, CMD_CLOSE);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    set(slot, 0, get(slot, 0).wrapping_add(1));
    table().free(slot);
}

/// How long a closed stream may keep its slot to deliver what it sent.
const LINGER_NS: u64 = 5_000_000_000;
/// Closed streams still delivering (a bit per slot) and when each gives up.
struct Linger(core::cell::UnsafeCell<(u32, [u64; SLOTS])>);
unsafe impl Sync for Linger {}
static LINGER: Linger = Linger(core::cell::UnsafeCell::new((0, [0; SLOTS])));
fn lingering() -> &'static mut (u32, [u64; SLOTS]) {
    unsafe { &mut *LINGER.0.get() }
}
/// Finish the closes that were waiting: sent and answered with the peer's
/// close, failed, or out of time. Run from the loop.
pub fn linger_tick(now: u64) {
    let l = lingering();
    for slot in 0..SLOTS {
        if l.0 & (1 << slot) != 0
            && ((get(slot, 7) == get(slot, 8) && (get(slot, 3) != OPEN || get(slot, 1) == CMD_UDP))
                || get(slot, 3) == FAILED
                || now > l.1[slot])
        {
            close(slot);
        }
    }
}

pub struct Net {
    /// The pages are mapped (the board has a socket server).
    pub present: bool,
    /// Loopback connections open on the phone (loopback.rs).
    pub pipes: super::loopback::Budget,
    /// Where the next port 0 bind starts looking.
    pub next_port: u16,
    /// The resolver's table of stand-in addresses.
    pub names: Names,
    /// The source of the datagram read last (for recvfrom).
    pub last_from: (u32, u16),
}
impl Net {
    pub fn new(present: bool) -> Net {
        Net { present, pipes: super::loopback::Budget::new(), next_port: super::loopback::EPHEMERAL, names: Names::new(), last_from: (0, 0) }
    }
    /// Slots the server serves; 0 unless it speaks this layout.
    fn served(&self) -> usize {
        if !self.present {
            return 0;
        }
        let page = BASE as *const u32;
        unsafe {
            if page.read_volatile() != MAGIC || page.add(3).read_volatile() != TX || page.add(4).read_volatile() != RX {
                return 0;
            }
            (page.add(2).read_volatile() as usize).min(SLOTS)
        }
    }
    /// 0 offline, 1 joined network, 2 phone relay, 3 the emulator's host.
    pub fn online(&self) -> u32 {
        if self.served() == 0 {
            return 0;
        }
        unsafe { (BASE as *const u32).add(1).read_volatile() }
    }
    /// The Open Runtime's own stand-in address for a name (the Sentinel's
    /// endpoints); 0, which no connect accepts, if the table cannot hold it.
    pub fn address_for(&mut self, name: &str) -> u32 {
        self.names.resolve(slots::SYSTEM, name, &mut |_| false).unwrap_or(0)
    }
    /// The host an owner means by an address: the name behind a stand-in
    /// address, or the address in dotted form. None for a stand-in address
    /// this owner has no name for; that is never a real destination.
    pub fn host_of(&mut self, owner: u32, ip: u32) -> Option<String> {
        match self.names.lookup(owner, ip) {
            Lookup::Name(n) => Some(String::from(n)),
            Lookup::Stale => None,
            Lookup::Literal => Some(literal(ip)),
        }
    }
    /// An address the resolver hands out for names.
    pub fn is_stand_in(&self, ip: u32) -> bool {
        names::is_stand_in(ip)
    }
    /// Forget what an owner resolved (its app exited).
    pub fn forget(&mut self, owner: u32) {
        self.names.purge(owner);
    }
    /// Start a connection of the Open Runtime's own; returns the slot.
    pub fn connect(&mut self, ip: u32, port: u16) -> Result<usize, i32> {
        if ip == 0 {
            return Err(EHOSTUNREACH);
        }
        self.start(slots::SYSTEM, 0, CMD_CONNECT, ip, port)
    }
    /// Start a connection for an app (`owner` is its pid, `apps` how many
    /// apps can run at once); returns the slot.
    pub fn connect_for(&mut self, owner: u32, apps: usize, ip: u32, port: u16) -> Result<usize, i32> {
        self.start(owner, apps, CMD_CONNECT, ip, port)
    }
    /// Open a UDP socket for an app; returns the slot.
    pub fn udp_open_for(&mut self, owner: u32, apps: usize) -> Result<usize, i32> {
        self.start(owner, apps, CMD_UDP, 0, 0)
    }
    /// Open a UDP socket on a port the app bound (0: any).
    pub fn udp_bind_for(&mut self, owner: u32, apps: usize, port: u16) -> Result<usize, i32> {
        self.start(owner, apps, CMD_UDP, 0, port)
    }
    /// Listen on the network for an app; returns the slot.
    pub fn listen_for(&mut self, owner: u32, apps: usize, port: u16, backlog: u32) -> Result<usize, i32> {
        self.start(owner, apps, CMD_LISTEN, backlog, port)
    }
    /// Take a connection waiting on an app's listening slot; returns its new slot.
    pub fn accept_for(&mut self, owner: u32, apps: usize, listener: usize) -> Result<usize, i32> {
        self.start(owner, apps, CMD_ACCEPT, listener as u32, 0)
    }
    /// Connections waiting on a listening slot.
    pub fn waiting(&self, slot: usize) -> u32 {
        if self.state(slot).0 == OPEN { get(slot, 15) } else { 0 }
    }
    fn start(&mut self, owner: u32, apps: usize, cmd: u32, ip: u32, port: u16) -> Result<usize, i32> {
        if self.online() == 0 {
            return Err(ENETUNREACH);
        }
        let mut name = [0u8; NAME];
        // Only a connect's word 5 is an address (a listen's is its backlog, an accept's a slot).
        match if cmd == CMD_CONNECT { self.names.lookup(owner, ip) } else { Lookup::Literal } {
            Lookup::Name(n) if n.len() < NAME => name[..n.len()].copy_from_slice(n.as_bytes()),
            Lookup::Literal => {}
            _ => return Err(EHOSTUNREACH),
        }
        // Every app the runtime can run keeps its floor, whoever asks.
        let slot = table().alloc(owner, self.served(), apps.max(super::MAX_APPS))?;
        for (k, b) in name.iter().enumerate() {
            unsafe { (word(slot, 16) as *mut u8).add(k).write_volatile(*b) };
        }
        set(slot, 5, ip);
        set(slot, 6, u32::from(port));
        // This side's ring words start at zero; the server zeroes its own
        // (8, 9, 12, 13, 15) when it takes the command, after it has let go of
        // the slot's previous connection.
        for i in [7, 10, 11] {
            set(slot, i, 0);
        }
        // Whose socket it is, for the server's per-app limits.
        set(slot, 14, owner);
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
        if !taken(slot) {
            return (CONNECTING, 0);
        }
        (get(slot, 3), get(slot, 4) as i32)
    }
    pub fn tx_space(&self, slot: usize) -> u32 {
        // Nothing of this use is sent before the server takes its command.
        let sent = if taken(slot) { get(slot, 8) } else { 0 };
        TX.saturating_sub(get(slot, 7).wrapping_sub(sent))
    }
    pub fn rx_ready(&self, slot: usize) -> u32 {
        if !taken(slot) {
            return 0;
        }
        get(slot, 9).wrapping_sub(get(slot, 10)).min(RX)
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
    /// A UDP or listening socket's local address: (the phone's address on the
    /// network, port); an accepted connection's peer.
    pub fn local(&self, slot: usize) -> (u32, u16) {
        if !taken(slot) {
            return (0, 0);
        }
        (get(slot, 13), get(slot, 12) as u16)
    }
    /// Queue a datagram on a UDP slot; Ok(false) when the ring has no room
    /// for it. A name `owner` resolved is sent instead of its stand-in
    /// address, for Radio to resolve; a stand-in address with no name is an error.
    pub fn send_datagram(&mut self, slot: usize, owner: u32, ip: u32, port: u16, data: &[u8]) -> Result<bool, i32> {
        let name = match self.names.lookup(owner, ip) {
            Lookup::Name(n) if n.len() < NAME => String::from(n),
            Lookup::Literal => String::new(),
            _ => return Err(EHOSTUNREACH),
        };
        let name = name.as_bytes();
        let data = &data[..data.len().min(MAX_DATAGRAM)];
        let body = 7 + name.len() + data.len();
        if (self.tx_space(slot) as usize) < body + 2 {
            return Ok(false);
        }
        let mut frame = Vec::with_capacity(body + 2);
        frame.extend_from_slice(&(body as u16).to_le_bytes());
        frame.extend_from_slice(&if name.is_empty() { ip } else { 0 }.to_le_bytes());
        frame.extend_from_slice(&port.to_le_bytes());
        frame.push(name.len() as u8);
        frame.extend_from_slice(name);
        frame.extend_from_slice(data);
        Ok(self.send(slot, &frame) == frame.len())
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
