//! A small Bluetooth LE host over /dev/violet/hci, enough for bitchat's
//! peripheral role: advertise the bitchat service, accept connections, and
//! serve one GATT characteristic that peers write packets to and subscribe
//! to for ours.
//!
//! Layering: HCI commands and events, ACL data with L2CAP reassembly and
//! segmentation, the ATT server (CID 4), and refusals for SMP (CID 6) and
//! L2CAP signalling (CID 5).
use std::collections::VecDeque;

/// The bitchat service and characteristic (release builds of the apps).
pub const SERVICE: u128 = 0xF47B5E2D_4A9E_4C5A_9B3F_8E1D2C3A4B5C;
pub const CHARACTERISTIC: u128 = 0xA1B2C3D4_E5F6_4A5B_8C9D_0E1F2A3B4C5D;

/// The largest ATT MTU this host accepts.
const MTU: u16 = 517;

// HCI opcodes.
const RESET: u16 = 0x0c03;
const SET_EVENT_MASK: u16 = 0x0c01;
const READ_BUFFER_SIZE: u16 = 0x1005;
const LE_SET_EVENT_MASK: u16 = 0x2001;
const LE_READ_BUFFER_SIZE: u16 = 0x2002;
const LE_SET_RANDOM_ADDRESS: u16 = 0x2005;
const LE_SET_ADV_PARAMS: u16 = 0x2006;
const LE_SET_ADV_DATA: u16 = 0x2008;
const LE_SET_SCAN_RESPONSE: u16 = 0x2009;
const LE_SET_ADV_ENABLE: u16 = 0x200a;
const LE_RAND: u16 = 0x2018;
const LE_SET_SCAN_PARAMS: u16 = 0x200b;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;
const LE_SET_DATA_LENGTH: u16 = 0x2022;

// GATT handles.
const H_GAP: u16 = 1;
const H_NAME_DECL: u16 = 2;
const H_NAME: u16 = 3;
const H_APPEARANCE_DECL: u16 = 4;
const H_APPEARANCE: u16 = 5;
const H_GATT: u16 = 6;
const H_SERVICE: u16 = 7;
const H_CHAR_DECL: u16 = 8;
const H_VALUE: u16 = 9;
const H_CCCD: u16 = 10;
const LAST: u16 = 10;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Uuid {
    Short(u16),
    Long(u128),
}
impl Uuid {
    fn bytes(self) -> Vec<u8> {
        match self {
            Uuid::Short(u) => u.to_le_bytes().to_vec(),
            Uuid::Long(u) => u.to_le_bytes().to_vec(),
        }
    }
    fn matches(self, b: &[u8]) -> bool {
        b == self.bytes().as_slice() || matches!(self, Uuid::Short(s) if b.len() == 16 && b == base_uuid(s).to_le_bytes().as_slice())
    }
}
fn base_uuid(short: u16) -> u128 {
    0x0000_0000_0000_1000_8000_0080_5f9b_34fb | (short as u128) << 96
}

/// What the host tells the app.
#[derive(Debug)]
pub enum Event {
    /// A central connected (handle).
    Connected(u16),
    Disconnected(u16),
    /// The central subscribed to notifications.
    Subscribed(u16),
    /// A peer wrote bytes to the bitchat characteristic.
    Data(u16, Vec<u8>),
    /// A device advertising the bitchat service: its address, type and signal.
    Seen([u8; 6], u8, i8),
}

struct Link {
    handle: u16,
    mtu: u16,
    subscribed: bool,
    /// L2CAP PDU being reassembled.
    rx: Vec<u8>,
    /// Long writes (Prepare Write) waiting for Execute.
    prepared: Vec<u8>,
}

pub struct Host {
    /// H4 packets for the controller, oldest first.
    pub out: VecDeque<Vec<u8>>,
    pub events: VecDeque<Event>,
    links: Vec<Link>,
    /// ACL packets in flight and the controller's limits.
    acl_len: usize,
    acl_free: usize,
    acl_queue: VecDeque<(u16, Vec<u8>)>,
    pending: Option<u16>,
    commands: VecDeque<(u16, Vec<u8>)>,
    /// Command results the app asked for (LE Rand), by opcode.
    pub results: Vec<(u16, u8, Vec<u8>)>,
    pub address: [u8; 6],
    pub ready: bool,
    pub advertising: bool,
    pub failure: Option<String>,
    name: Vec<u8>,
    /// Advertising reports heard while scanning.
    pub reports: u32,
}

/// Whether advertising data lists a 128-bit service UUID.
fn advertises(mut data: &[u8], uuid: u128) -> bool {
    while data.len() >= 2 {
        let n = data[0] as usize;
        let Some(field) = data.get(1..1 + n) else { return false };
        if n >= 1 && matches!(field[0], 0x06 | 0x07) && field[1..].chunks_exact(16).any(|u| u == uuid.to_le_bytes()) {
            return true;
        }
        data = &data[1 + n..];
    }
    false
}

fn command(op: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![1];
    p.extend_from_slice(&op.to_le_bytes());
    p.push(params.len() as u8);
    p.extend_from_slice(params);
    p
}

impl Host {
    pub fn new(name: &str) -> Host {
        Host {
            out: VecDeque::new(),
            events: VecDeque::new(),
            links: Vec::new(),
            acl_len: 27,
            acl_free: 0,
            acl_queue: VecDeque::new(),
            pending: None,
            commands: VecDeque::new(),
            results: Vec::new(),
            address: [0; 6],
            ready: false,
            advertising: false,
            failure: None,
            reports: 0,
            name: name.as_bytes()[..name.len().min(20)].to_vec(),
        }
    }

    fn queue(&mut self, op: u16, params: &[u8]) {
        self.commands.push_back((op, params.to_vec()));
        self.next_command();
    }
    fn next_command(&mut self) {
        if self.pending.is_none() {
            if let Some((op, p)) = self.commands.pop_front() {
                self.pending = Some(op);
                self.out.push_back(command(op, &p));
            }
        }
    }

    /// Resets the controller and reads its limits, then asks for 16 random bytes.
    pub fn start(&mut self) {
        self.queue(RESET, &[]);
        self.queue(SET_EVENT_MASK, &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3f]);
        // Connection complete, advertising report, connection update,
        // remote features, LTK request, parameter request, data length change.
        self.queue(LE_SET_EVENT_MASK, &[0x7f, 0, 0, 0, 0, 0, 0, 0]);
        self.queue(READ_BUFFER_SIZE, &[]);
        self.queue(LE_READ_BUFFER_SIZE, &[]);
        self.queue(LE_RAND, &[]);
        self.queue(LE_RAND, &[]);
    }

    /// Takes a random static address and starts advertising the bitchat service.
    pub fn advertise(&mut self, mut address: [u8; 6]) {
        address[5] |= 0xc0;
        self.address = address;
        self.queue(LE_SET_RANDOM_ADDRESS, &address);
        // 100 ms connectable undirected advertising from the random address.
        self.queue(LE_SET_ADV_PARAMS, &[0xa0, 0, 0xa0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 7, 0]);
        let mut adv = vec![2, 0x01, 0x06, 17, 0x07];
        adv.extend_from_slice(&SERVICE.to_le_bytes());
        let mut data = [0u8; 32];
        data[0] = adv.len() as u8;
        data[1..1 + adv.len()].copy_from_slice(&adv);
        self.queue(LE_SET_ADV_DATA, &data);
        let mut scan = [0u8; 32];
        let n = self.name.len();
        scan[0] = n as u8 + 2;
        scan[1] = n as u8 + 1;
        scan[2] = 0x09;
        scan[3..3 + n].copy_from_slice(&self.name);
        self.queue(LE_SET_SCAN_RESPONSE, &scan);
        self.set_advertising(true);
        // Listen too: passive, 60 ms window every 100 ms, each device reported once.
        self.queue(LE_SET_SCAN_PARAMS, &[0, 0xa0, 0, 0x60, 0, 1, 0]);
        self.queue(LE_SET_SCAN_ENABLE, &[1, 1]);
    }
    fn set_advertising(&mut self, on: bool) {
        self.advertising = on;
        self.queue(LE_SET_ADV_ENABLE, &[on as u8]);
    }

    pub fn links(&self) -> usize {
        self.links.len()
    }
    pub fn subscribed(&self) -> Vec<(u16, usize)> {
        self.links.iter().filter(|l| l.subscribed).map(|l| (l.handle, l.mtu as usize - 3)).collect()
    }

    /// One H4 packet from the controller.
    pub fn receive(&mut self, p: &[u8]) {
        match p.first() {
            Some(4) if p.len() >= 3 => self.event(p[1], &p[3..]),
            Some(2) if p.len() >= 5 => self.acl(p),
            _ => {}
        }
    }

    fn event(&mut self, code: u8, p: &[u8]) {
        match code {
            // Command Complete.
            0x0e if p.len() >= 4 => {
                let op = u16::from_le_bytes([p[1], p[2]]);
                self.completed(op, p[3], &p[4..]);
            }
            // Command Status.
            0x0f if p.len() >= 4 => {
                let op = u16::from_le_bytes([p[2], p[3]]);
                self.completed(op, p[0], &[]);
            }
            // Disconnection Complete.
            0x05 if p.len() >= 4 && p[0] == 0 => {
                let handle = u16::from_le_bytes([p[1], p[2]]) & 0xfff;
                self.links.retain(|l| l.handle != handle);
                self.acl_queue.retain(|(h, _)| *h != handle);
                self.events.push_back(Event::Disconnected(handle));
                if self.ready {
                    // Legacy advertising stops on a connection: offer another.
                    self.set_advertising(true);
                }
            }
            // Number Of Completed Packets.
            0x13 if !p.is_empty() => {
                for k in 0..p[0] as usize {
                    if let Some(c) = p.get(3 + 4 * k..5 + 4 * k) {
                        self.acl_free += u16::from_le_bytes([c[0], c[1]]) as usize;
                    }
                }
                self.flush_acl();
            }
            // LE Meta.
            0x3e if !p.is_empty() => match p[0] {
                // (Enhanced) Connection Complete: status, handle, role.
                0x01 | 0x0a if p.len() >= 5 && p[1] == 0 => {
                    let handle = u16::from_le_bytes([p[2], p[3]]) & 0xfff;
                    self.links.push(Link { handle, mtu: 23, subscribed: false, rx: Vec::new(), prepared: Vec::new() });
                    self.advertising = false;
                    self.events.push_back(Event::Connected(handle));
                    // Longer link-layer packets, if the controller and peer allow.
                    let mut dl = handle.to_le_bytes().to_vec();
                    dl.extend_from_slice(&251u16.to_le_bytes());
                    dl.extend_from_slice(&2120u16.to_le_bytes());
                    self.queue(LE_SET_DATA_LENGTH, &dl);
                    if self.links.len() < 4 {
                        self.set_advertising(true);
                    }
                }
                // Advertising reports.
                0x02 if p.len() >= 2 => {
                    let mut at = 2;
                    for _ in 0..p[1] {
                        let Some(r) = p.get(at..at + 9) else { break };
                        let n = r[8] as usize;
                        let Some(data) = p.get(at + 9..at + 9 + n) else { break };
                        let rssi = p.get(at + 9 + n).copied().unwrap_or(0) as i8;
                        self.reports += 1;
                        if advertises(data, SERVICE) {
                            self.events.push_back(Event::Seen(r[2..8].try_into().unwrap(), r[1], rssi));
                        }
                        at += 10 + n;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn completed(&mut self, op: u16, status: u8, ret: &[u8]) {
        if self.pending != Some(op) {
            return;
        }
        self.pending = None;
        match op {
            READ_BUFFER_SIZE if status == 0 && ret.len() >= 7 => {
                self.acl_len = u16::from_le_bytes([ret[0], ret[1]]) as usize;
                self.acl_free = u16::from_le_bytes([ret[3], ret[4]]) as usize;
            }
            LE_READ_BUFFER_SIZE if status == 0 && ret.len() >= 3 => {
                let (len, n) = (u16::from_le_bytes([ret[0], ret[1]]) as usize, ret[2] as usize);
                if len > 0 && n > 0 {
                    self.acl_len = len;
                    self.acl_free = n;
                }
            }
            LE_RAND | RESET | LE_SET_ADV_ENABLE | LE_SET_SCAN_ENABLE => {}
            // Optional: older controllers do not know it.
            LE_SET_DATA_LENGTH => {}
            _ if status != 0 && self.failure.is_none() => {
                self.failure = Some(format!("command {op:04x} failed with {status:02x}"));
            }
            _ => {}
        }
        if op == RESET && status != 0 {
            self.failure = Some(format!("reset failed with {status:02x}"));
        }
        if op == LE_SET_ADV_ENABLE && status == 0 && self.advertising {
            self.ready = true;
        }
        self.results.push((op, status, ret.to_vec()));
        self.next_command();
    }

    fn acl(&mut self, p: &[u8]) {
        let hf = u16::from_le_bytes([p[1], p[2]]);
        let (handle, pb) = (hf & 0xfff, (hf >> 12) & 3);
        let len = u16::from_le_bytes([p[3], p[4]]) as usize;
        let Some(data) = p.get(5..5 + len) else { return };
        let Some(link) = self.links.iter_mut().find(|l| l.handle == handle) else { return };
        if pb == 1 {
            link.rx.extend_from_slice(data);
        } else {
            link.rx = data.to_vec();
        }
        if link.rx.len() < 4 {
            return;
        }
        let want = u16::from_le_bytes([link.rx[0], link.rx[1]]) as usize + 4;
        if link.rx.len() < want {
            return;
        }
        let pdu = std::mem::take(&mut link.rx);
        let cid = u16::from_le_bytes([pdu[2], pdu[3]]);
        let body = &pdu[4..want];
        match cid {
            4 => self.att(handle, body),
            // Signalling: refuse every request ("command not understood").
            5 if body.len() >= 4 && body[0] & 1 == 0 && body[0] != 0x01 => {
                self.l2cap(handle, 5, &[0x01, body[1], 2, 0, 0, 0]);
            }
            // SMP: pairing is not supported.
            6 if body.first() == Some(&0x01) => self.l2cap(handle, 6, &[0x05, 0x05]),
            _ => {}
        }
    }

    /// Sends one L2CAP PDU, cut to the controller's ACL size.
    fn l2cap(&mut self, handle: u16, cid: u16, body: &[u8]) {
        let mut pdu = (body.len() as u16).to_le_bytes().to_vec();
        pdu.extend_from_slice(&cid.to_le_bytes());
        pdu.extend_from_slice(body);
        for (k, chunk) in pdu.chunks(self.acl_len.max(27)).enumerate() {
            let flags = if k == 0 { 0u16 } else { 1 << 12 };
            let mut p = vec![2];
            p.extend_from_slice(&(handle | flags).to_le_bytes());
            p.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            p.extend_from_slice(chunk);
            self.acl_queue.push_back((handle, p));
        }
        self.flush_acl();
    }
    fn flush_acl(&mut self) {
        while self.acl_free > 0 {
            let Some((_, p)) = self.acl_queue.pop_front() else { break };
            self.acl_free -= 1;
            self.out.push_back(p);
        }
    }
    /// ACL packets still waiting for controller buffers.
    pub fn backlog(&self) -> usize {
        self.acl_queue.len()
    }

    /// Sends `value` to a subscribed central as one notification.
    pub fn notify(&mut self, handle: u16, value: &[u8]) {
        let mut b = vec![0x1b];
        b.extend_from_slice(&H_VALUE.to_le_bytes());
        b.extend_from_slice(value);
        self.l2cap(handle, 4, &b);
    }

    fn attribute(&self, h: u16) -> Option<(Uuid, Vec<u8>)> {
        let decl = |props: u8, value: u16, uuid: Uuid| {
            let mut v = vec![props];
            v.extend_from_slice(&value.to_le_bytes());
            v.extend_from_slice(&uuid.bytes());
            v
        };
        Some(match h {
            H_GAP => (Uuid::Short(0x2800), 0x1800u16.to_le_bytes().to_vec()),
            H_NAME_DECL => (Uuid::Short(0x2803), decl(0x02, H_NAME, Uuid::Short(0x2a00))),
            H_NAME => (Uuid::Short(0x2a00), self.name.clone()),
            H_APPEARANCE_DECL => (Uuid::Short(0x2803), decl(0x02, H_APPEARANCE, Uuid::Short(0x2a01))),
            H_APPEARANCE => (Uuid::Short(0x2a01), vec![0, 0]),
            H_GATT => (Uuid::Short(0x2800), 0x1801u16.to_le_bytes().to_vec()),
            H_SERVICE => (Uuid::Short(0x2800), SERVICE.to_le_bytes().to_vec()),
            // Read, write without response, write, notify.
            H_CHAR_DECL => (Uuid::Short(0x2803), decl(0x1e, H_VALUE, Uuid::Long(CHARACTERISTIC))),
            H_VALUE => (Uuid::Long(CHARACTERISTIC), Vec::new()),
            H_CCCD => (Uuid::Short(0x2902), vec![0, 0]),
            _ => return None,
        })
    }
    /// The last handle of the service starting at `h`.
    fn group_end(h: u16) -> u16 {
        match h {
            H_GAP => H_APPEARANCE,
            H_GATT => H_GATT,
            _ => LAST,
        }
    }

    fn att(&mut self, handle: u16, req: &[u8]) {
        let Some(&op) = req.first() else { return };
        let mtu = self.links.iter().find(|l| l.handle == handle).map_or(23, |l| l.mtu) as usize;
        let u16_at = |k: usize| req.get(k..k + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let error = |h: u16, code: u8| {
            let mut e = vec![0x01, op];
            e.extend_from_slice(&h.to_le_bytes());
            e.push(code);
            e
        };
        let reply: Option<Vec<u8>> = match op {
            // Exchange MTU.
            0x02 => {
                let theirs = u16_at(1).unwrap_or(23).max(23);
                if let Some(l) = self.links.iter_mut().find(|l| l.handle == handle) {
                    l.mtu = theirs.min(MTU);
                }
                let mut r = vec![0x03];
                r.extend_from_slice(&MTU.to_le_bytes());
                Some(r)
            }
            // Find Information: handles and types.
            0x04 => {
                let (s, e) = (u16_at(1).unwrap_or(0), u16_at(3).unwrap_or(0));
                let mut r = vec![0x05, 0];
                for h in s.max(1)..=e.min(LAST) {
                    let (uuid, _) = self.attribute(h).unwrap();
                    let format = if matches!(uuid, Uuid::Short(_)) { 1 } else { 2 };
                    if r[1] == 0 {
                        r[1] = format;
                    } else if r[1] != format {
                        break;
                    }
                    let entry = 2 + uuid.bytes().len();
                    if r.len() + entry > mtu {
                        break;
                    }
                    r.extend_from_slice(&h.to_le_bytes());
                    r.extend_from_slice(&uuid.bytes());
                }
                Some(if r[1] == 0 { error(s, 0x0a) } else { r })
            }
            // Find By Type Value: services with this UUID.
            0x06 => {
                let (s, e, t) = (u16_at(1).unwrap_or(0), u16_at(3).unwrap_or(0), u16_at(5).unwrap_or(0));
                let value = req.get(7..).unwrap_or(&[]);
                let mut r = vec![0x07];
                for h in s.max(1)..=e.min(LAST) {
                    let (uuid, v) = self.attribute(h).unwrap();
                    if uuid == Uuid::Short(t) && v == value && r.len() + 4 <= mtu {
                        r.extend_from_slice(&h.to_le_bytes());
                        r.extend_from_slice(&Self::group_end(h).to_le_bytes());
                    }
                }
                Some(if r.len() == 1 { error(s, 0x0a) } else { r })
            }
            // Read By Type (characteristics, or a value by its type) and
            // Read By Group Type (primary services).
            0x08 | 0x10 => {
                let (s, e) = (u16_at(1).unwrap_or(0), u16_at(3).unwrap_or(0));
                let t = req.get(5..).unwrap_or(&[]);
                if s == 0 || s > e {
                    Some(error(s, 0x01))
                } else {
                    let mut r = vec![op + 1, 0];
                    for h in s..=e.min(LAST) {
                        let (uuid, v) = self.attribute(h).unwrap();
                        if !uuid.matches(t) {
                            continue;
                        }
                        let mut entry = h.to_le_bytes().to_vec();
                        if op == 0x10 {
                            entry.extend_from_slice(&Self::group_end(h).to_le_bytes());
                        }
                        entry.extend_from_slice(&v);
                        entry.truncate(mtu - 2);
                        if r[1] == 0 {
                            r[1] = entry.len() as u8;
                        } else if r[1] as usize != entry.len() || r.len() + entry.len() > mtu {
                            break;
                        }
                        r.extend_from_slice(&entry);
                    }
                    Some(if r[1] == 0 { error(s, 0x0a) } else { r })
                }
            }
            // Read and Read Blob.
            0x0a | 0x0c => {
                let h = u16_at(1).unwrap_or(0);
                let offset = if op == 0x0c { u16_at(3).unwrap_or(0) as usize } else { 0 };
                match self.attribute(h) {
                    None => Some(error(h, 0x01)),
                    Some((_, mut v)) => {
                        if h == H_CCCD {
                            let on = self.links.iter().any(|l| l.handle == handle && l.subscribed);
                            v = vec![on as u8, 0];
                        }
                        if offset > v.len() {
                            Some(error(h, 0x07))
                        } else {
                            let mut r = vec![op + 1];
                            r.extend_from_slice(&v[offset..v.len().min(offset + mtu - 1)]);
                            Some(r)
                        }
                    }
                }
            }
            // Write Request and Write Command.
            0x12 | 0x52 => {
                let h = u16_at(1).unwrap_or(0);
                let value = req.get(3..).unwrap_or(&[]).to_vec();
                let ok = self.write(handle, h, value);
                match (op, ok) {
                    (0x12, Ok(())) => Some(vec![0x13]),
                    (0x12, Err(code)) => Some(error(h, code)),
                    _ => None,
                }
            }
            // Prepare Write (part of a long write): echoed back.
            0x16 => {
                let h = u16_at(1).unwrap_or(0);
                if h != H_VALUE {
                    Some(error(h, 0x03))
                } else {
                    if let Some(l) = self.links.iter_mut().find(|l| l.handle == handle) {
                        let offset = u16_at(3).unwrap_or(0) as usize;
                        l.prepared.truncate(offset);
                        l.prepared.extend_from_slice(req.get(5..).unwrap_or(&[]));
                    }
                    let mut r = req.to_vec();
                    r[0] = 0x17;
                    Some(r)
                }
            }
            // Execute Write.
            0x18 => {
                let value = self
                    .links
                    .iter_mut()
                    .find(|l| l.handle == handle)
                    .map(|l| std::mem::take(&mut l.prepared))
                    .unwrap_or_default();
                if req.get(1) == Some(&1) && !value.is_empty() {
                    self.events.push_back(Event::Data(handle, value));
                }
                Some(vec![0x19])
            }
            // Confirmations and other commands need no answer.
            0x1e => None,
            _ if op & 0x40 != 0 => None,
            _ => Some(error(0, 0x06)),
        };
        if let Some(r) = reply {
            self.l2cap(handle, 4, &r);
        }
    }

    fn write(&mut self, handle: u16, h: u16, value: Vec<u8>) -> Result<(), u8> {
        match h {
            H_VALUE => {
                self.events.push_back(Event::Data(handle, value));
                Ok(())
            }
            H_CCCD => {
                let on = value.first().map_or(false, |b| b & 1 != 0);
                let mut newly = false;
                if let Some(l) = self.links.iter_mut().find(|l| l.handle == handle) {
                    newly = on && !l.subscribed;
                    l.subscribed = on;
                }
                if newly {
                    self.events.push_back(Event::Subscribed(handle));
                }
                Ok(())
            }
            1..=LAST => Err(0x03),
            _ => Err(0x01),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> Host {
        let mut h = Host::new("violet");
        h.start();
        let mut guard = 0;
        while let Some(p) = h.out.pop_front() {
            guard += 1;
            assert!(guard < 50);
            let op = u16::from_le_bytes([p[1], p[2]]);
            let ret: Vec<u8> = match op {
                LE_READ_BUFFER_SIZE => vec![0xfb, 0, 4],
                READ_BUFFER_SIZE => vec![0, 0, 0, 0, 0, 0, 0],
                LE_RAND => vec![1, 2, 3, 4, 5, 6, 7, 8],
                _ => vec![],
            };
            let mut e = vec![4, 0x0e, 0, 1, p[1], p[2], 0];
            e.extend_from_slice(&ret);
            e[2] = (e.len() - 3) as u8;
            h.receive(&e);
        }
        h.advertise([1, 2, 3, 4, 5, 6]);
        while let Some(p) = h.out.pop_front() {
            h.receive(&[4, 0x0e, 4, 1, p[1], p[2], 0]);
        }
        assert!(h.ready && h.address[5] == 0xc6);
        // Connection Complete for handle 0x40, peripheral role.
        h.receive(&[4, 0x3e, 19, 0x01, 0, 0x40, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x18, 0, 0, 0, 0x48, 0, 0]);
        h.out.clear();
        h.acl_free = 100;
        h
    }
    fn att(h: &mut Host, req: &[u8]) -> Vec<u8> {
        let mut p = vec![2, 0x40, 0x20];
        p.extend_from_slice(&((req.len() + 4) as u16).to_le_bytes());
        p.extend_from_slice(&(req.len() as u16).to_le_bytes());
        p.extend_from_slice(&[4, 0]);
        p.extend_from_slice(req);
        h.receive(&p);
        let mut out = Vec::new();
        while let Some(p) = h.out.pop_front() {
            if p[0] == 2 {
                out.extend_from_slice(&p[5..]);
            }
        }
        if out.is_empty() {
            return out;
        }
        assert_eq!(u16::from_le_bytes([out[0], out[1]]) as usize, out.len() - 4);
        out[4..].to_vec()
    }

    #[test]
    fn a_central_finds_the_service_subscribes_and_writes() {
        let mut h = connected();
        assert_eq!(att(&mut h, &[0x02, 0x0f, 0x02]), vec![0x03, 0x05, 0x02]);
        // Primary services.
        let r = att(&mut h, &[0x10, 1, 0, 0xff, 0xff, 0x00, 0x28]);
        assert_eq!(r[..2], [0x11, 6]);
        assert_eq!(r[2..8], [1, 0, 5, 0, 0x00, 0x18]);
        let r = att(&mut h, &[0x10, 7, 0, 0xff, 0xff, 0x00, 0x28]);
        assert_eq!(r[1], 20);
        assert_eq!(r[2..6], [7, 0, 10, 0]);
        assert_eq!(r[6..22], SERVICE.to_le_bytes());
        // By UUID, as iOS does for discoverServices([uuid]).
        let mut q = vec![0x06, 1, 0, 0xff, 0xff, 0x00, 0x28];
        q.extend_from_slice(&SERVICE.to_le_bytes());
        assert_eq!(att(&mut h, &q), vec![0x07, 7, 0, 10, 0]);
        // Characteristics.
        let r = att(&mut h, &[0x08, 7, 0, 10, 0, 0x03, 0x28]);
        assert_eq!(r[..6], [0x09, 21, 8, 0, 0x1e, 9]);
        assert_eq!(r[7..23], CHARACTERISTIC.to_le_bytes());
        // Descriptors.
        assert_eq!(att(&mut h, &[0x04, 10, 0, 10, 0]), vec![0x05, 1, 10, 0, 0x02, 0x29]);
        // Subscribe, then a write without response.
        assert_eq!(att(&mut h, &[0x12, 10, 0, 1, 0]), vec![0x13]);
        assert_eq!(att(&mut h, &[0x52, 9, 0, 0xaa, 0xbb]), Vec::<u8>::new());
        let ev: Vec<_> = h.events.drain(..).collect();
        assert!(matches!(ev[..], [Event::Connected(0x40), Event::Subscribed(0x40), Event::Data(0x40, ref d)] if d == &[0xaa, 0xbb]));
        assert_eq!(h.subscribed(), vec![(0x40, 514)]);
        // Unknown requests get an error, not silence.
        assert_eq!(att(&mut h, &[0x20, 1]), vec![0x01, 0x20, 0, 0, 0x06]);
    }

    #[test]
    fn notifications_are_cut_to_the_controller_buffer() {
        let mut h = connected();
        h.acl_len = 27;
        h.acl_free = 2;
        h.notify(0x40, &[7; 100]);
        assert_eq!(h.out.len(), 2);
        assert!(h.backlog() > 0);
        // The controller frees two buffers.
        h.receive(&[4, 0x13, 5, 1, 0x40, 0, 2, 0]);
        assert_eq!(h.out.len(), 4);
        let first = &h.out[0];
        assert_eq!(first[1..3], [0x40, 0x00]);
        assert_eq!(h.out[1][1..3], [0x40, 0x10], "continuation fragments set PB=01");
    }

    #[test]
    fn long_writes_arrive_whole() {
        let mut h = connected();
        att(&mut h, &[0x16, 9, 0, 0, 0, 1, 2, 3]);
        att(&mut h, &[0x16, 9, 0, 3, 0, 4, 5]);
        assert_eq!(att(&mut h, &[0x18, 1]), vec![0x19]);
        assert!(h.events.iter().any(|e| matches!(e, Event::Data(_, d) if d == &[1, 2, 3, 4, 5])));
    }
}
