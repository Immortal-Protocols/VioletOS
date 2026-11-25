//! The direct path to the Mac (docs/agent-protocol.md, "Direct path"): one
//! UDP socket, its public address learned from STUN servers, opened by both
//! sides probing each other's candidates (hole punching). Once a probe is
//! answered, records travel here instead of through the relay, in fragments
//! delivered in order with acknowledgements and retransmission. Every
//! datagram carries an HMAC under a key the phone sent through the encrypted
//! session, so nothing else can inject into or acknowledge on the path.
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use ring::hmac;

pub const PROBE: u8 = 1;
pub const PROBE_ACK: u8 = 2;
pub const DATA: u8 = 3;
pub const ACK: u8 = 4;
pub const PING: u8 = 5;
const MAGIC: u8 = b'V';
const HEADER: usize = 11;
const TAG: usize = 16;
/// Fragment payload: well under any path's MTU, and the Open Runtime's datagram limit.
pub const FRAGMENT: usize = 1200;
/// Fragments in flight: the Moto's socket rings hold about a dozen datagrams.
const WINDOW: u32 = 12;
const PUNCH_FOR: Duration = Duration::from_secs(12);
const SILENT: Duration = Duration::from_secs(8);
const PING_EVERY: Duration = Duration::from_millis(1500);

pub enum Event {
    /// Our candidates ("ip:port"), for the Mac.
    Gathered(Vec<String>),
    /// The path is open to this address.
    Open(String),
    /// A record that came over the path.
    Message(Vec<u8>),
    /// The path failed or never opened; the records not known delivered, to
    /// send through the relay instead.
    Down(String, Vec<Vec<u8>>),
}

pub enum Cmd {
    /// Probe these candidates of the Mac's.
    Punch(Vec<SocketAddrV4>),
    Send(Vec<u8>),
}

/// A datagram: magic, type, seq, ack, flags, payload, then the tag.
pub fn packet(key: &hmac::Key, kind: u8, seq: u32, ack: u32, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(HEADER + payload.len() + TAG);
    p.push(MAGIC);
    p.push(kind);
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&ack.to_be_bytes());
    p.push(flags);
    p.extend_from_slice(payload);
    let tag = hmac::sign(key, &p);
    p.extend_from_slice(&tag.as_ref()[..TAG]);
    p
}

pub struct Packet<'a> {
    pub kind: u8,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub payload: &'a [u8],
}

pub fn parse<'a>(key: &hmac::Key, p: &'a [u8]) -> Option<Packet<'a>> {
    if p.len() < HEADER + TAG || p[0] != MAGIC {
        return None;
    }
    let (body, tag) = p.split_at(p.len() - TAG);
    let want = hmac::sign(key, body);
    // Constant time is not needed for a truncated tag checked once per packet,
    // but compare all of it.
    if want.as_ref()[..TAG].iter().zip(tag).fold(0, |d, (a, b)| d | (a ^ b)) != 0 {
        return None;
    }
    Some(Packet {
        kind: body[1],
        seq: u32::from_be_bytes(body[2..6].try_into().unwrap()),
        ack: u32::from_be_bytes(body[6..10].try_into().unwrap()),
        flags: body[10],
        payload: &body[HEADER..],
    })
}

/// A STUN binding request (RFC 5389) with this transaction id.
pub fn stun_request(id: &[u8; 12]) -> Vec<u8> {
    let mut r = vec![0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42];
    r.extend_from_slice(id);
    r
}

/// The mapped address in a STUN binding response.
pub fn stun_mapped(r: &[u8], id: &[u8; 12]) -> Option<SocketAddrV4> {
    if r.len() < 20 || r[0..2] != [1, 1] || r[4..8] != [0x21, 0x12, 0xa4, 0x42] || &r[8..20] != id {
        return None;
    }
    let end = (20 + u16::from_be_bytes([r[2], r[3]]) as usize).min(r.len());
    let mut at = 20;
    let mut plain = None;
    while at + 4 <= end {
        let (kind, len) = (u16::from_be_bytes([r[at], r[at + 1]]), u16::from_be_bytes([r[at + 2], r[at + 3]]) as usize);
        let v = r.get(at + 4..at + 4 + len)?;
        if len >= 8 && v[1] == 1 {
            let port = u16::from_be_bytes([v[2], v[3]]);
            let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
            match kind {
                0x0020 => return Some(SocketAddrV4::new(Ipv4Addr::from(ip ^ 0x2112_a442), port ^ 0x2112)),
                0x0001 => plain = Some(SocketAddrV4::new(Ipv4Addr::from(ip), port)),
                _ => {}
            }
        }
        at += 4 + (len + 3) / 4 * 4;
    }
    plain
}

struct Fragment {
    seq: u32,
    last: bool,
    data: Vec<u8>,
}

/// Ordered, acknowledged delivery of records over the path. Pure state, so it
/// is tested without sockets: `outgoing` gives what to send now.
pub struct Transport {
    key: hmac::Key,
    next: u32,
    /// Every fragment below this is acknowledged.
    acked: u32,
    /// Fragments below this were sent at least once.
    sent: u32,
    queue: VecDeque<Fragment>,
    /// Each record queued and its last fragment.
    records: VecDeque<(u32, Vec<u8>)>,
    timer: Instant,
    pub rto: Duration,
    expect: u32,
    partial: Vec<u8>,
    ack_due: bool,
}

impl Transport {
    pub fn new(key: &[u8; 32]) -> Transport {
        Transport {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
            next: 1,
            acked: 1,
            sent: 1,
            queue: VecDeque::new(),
            records: VecDeque::new(),
            timer: Instant::now(),
            rto: Duration::from_millis(300),
            expect: 1,
            partial: Vec::new(),
            ack_due: false,
        }
    }
    pub fn key(&self) -> &hmac::Key {
        &self.key
    }
    pub fn queue(&mut self, record: Vec<u8>) {
        let chunks: Vec<&[u8]> = if record.is_empty() { vec![&[][..]] } else { record.chunks(FRAGMENT).collect() };
        let n = chunks.len();
        for (k, c) in chunks.into_iter().enumerate() {
            self.queue.push_back(Fragment { seq: self.next, last: k + 1 == n, data: c.to_vec() });
            self.next += 1;
        }
        self.records.push_back((self.next - 1, record));
    }
    /// Records not yet known delivered.
    pub fn unsent(&mut self) -> Vec<Vec<u8>> {
        self.queue.clear();
        self.records.drain(..).map(|(_, r)| r).collect()
    }
    pub fn idle(&self) -> bool {
        self.queue.is_empty()
    }
    fn on_ack(&mut self, ack: u32) {
        if ack <= self.acked || ack > self.sent {
            return;
        }
        self.acked = ack;
        self.timer = Instant::now();
        while self.queue.front().is_some_and(|f| f.seq < ack) {
            self.queue.pop_front();
        }
        while self.records.front().is_some_and(|r| r.0 < ack) {
            self.records.pop_front();
        }
    }
    /// A datagram from the peer (already checked); records completed by it.
    pub fn receive(&mut self, p: &Packet) -> Vec<Vec<u8>> {
        self.on_ack(p.ack);
        let mut done = Vec::new();
        if p.kind == DATA {
            self.ack_due = true;
            if p.seq == self.expect {
                self.expect += 1;
                self.partial.extend_from_slice(p.payload);
                if p.flags & 1 != 0 {
                    done.push(std::mem::take(&mut self.partial));
                }
            }
        }
        done
    }
    /// The datagrams to send now: new fragments while the window allows,
    /// the oldest unacknowledged again when the timer runs out, and an ack.
    pub fn outgoing(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if self.acked < self.sent && now.duration_since(self.timer) > self.rto {
            for f in self.queue.iter().take_while(|f| f.seq < self.sent).take(4) {
                out.push(packet(&self.key, DATA, f.seq, self.expect, f.last as u8, &f.data));
            }
            self.timer = now;
        }
        if self.acked == self.sent {
            self.timer = now;
        }
        while self.sent < self.next && self.sent - self.acked < WINDOW {
            let at = (self.sent - self.acked) as usize;
            let f = &self.queue[at];
            out.push(packet(&self.key, DATA, f.seq, self.expect, f.last as u8, &f.data));
            self.sent += 1;
        }
        if self.ack_due {
            self.ack_due = false;
            if out.is_empty() {
                out.push(packet(&self.key, ACK, 0, self.expect, 0, &[]));
            }
        }
        out
    }
    pub fn ping(&self) -> Vec<u8> {
        packet(&self.key, PING, 0, self.expect, 0, &[])
    }
}

fn gather(sock: &UdpSocket, stun: &[String]) -> Vec<String> {
    let mut cands = Vec::new();
    let mut ids = Vec::new();
    for s in stun {
        let Ok(mut addrs) = s.to_socket_addrs() else {
            println!("agent: direct: no address for {s}");
            continue;
        };
        let Some(a) = addrs.find(|a| a.is_ipv4()) else { continue };
        let id = crate::secure::random::<12>();
        let _ = sock.send_to(&stun_request(&id), a);
        ids.push((id, a));
    }
    let until = Instant::now() + Duration::from_millis(1500);
    let mut buf = [0u8; 1500];
    let mut answered = 0;
    while Instant::now() < until && answered < ids.len() {
        let Ok((n, _)) = sock.recv_from(&mut buf) else { continue };
        if let Some(m) = ids.iter().find_map(|(id, _)| stun_mapped(&buf[..n], id)) {
            answered += 1;
            let c = m.to_string();
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
    }
    // The address on the local network (known once Radio has bound the socket).
    if let Ok(SocketAddr::V4(l)) = sock.local_addr() {
        if !l.ip().is_unspecified() && l.port() != 0 && !cands.contains(&l.to_string()) {
            cands.insert(0, l.to_string());
        }
    }
    cands
}

/// Open a path with key `key`: gather candidates, report them, and punch once
/// the Mac's arrive. Runs until the path fails or the sender is dropped.
pub fn spawn(key: [u8; 32], stun: Vec<String>, events: Sender<Event>) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (why, unsent) = run(key, &stun, &events, &rx);
        let _ = events.send(Event::Down(why, unsent));
        // Anything still sent our way goes back through the relay.
        for c in rx {
            if let Cmd::Send(m) = c {
                if events.send(Event::Down(String::new(), vec![m])).is_err() {
                    return;
                }
            }
        }
    });
    tx
}

fn run(key: [u8; 32], stun: &[String], events: &Sender<Event>, rx: &Receiver<Cmd>) -> (String, Vec<Vec<u8>>) {
    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => return (format!("no UDP socket: {e}"), Vec::new()),
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let cands = gather(&sock, stun);
    if cands.is_empty() {
        return ("this network gives UDP no way out".into(), Vec::new());
    }
    println!("agent: direct: candidates {}", cands.join(" "));
    if events.send(Event::Gathered(cands)).is_err() {
        return (String::new(), Vec::new());
    }
    let _ = sock.set_read_timeout(Some(Duration::from_millis(10)));
    let mut t = Transport::new(&key);
    let mut peers: Vec<SocketAddrV4> = Vec::new();
    let mut path: Option<SocketAddr> = None;
    let mut punch_until: Option<Instant> = None;
    let (mut probed, mut heard, mut pinged) = (Instant::now() - PUNCH_FOR, Instant::now(), Instant::now());
    let mut probes_out: Vec<(u32, Instant)> = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Punch(p)) => {
                    peers = p;
                    punch_until = Some(Instant::now() + PUNCH_FOR);
                    probed = Instant::now() - PUNCH_FOR;
                }
                Ok(Cmd::Send(m)) => t.queue(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return (String::new(), t.unsent()),
            }
        }
        let now = Instant::now();
        match path {
            None => {
                if let Some(u) = punch_until {
                    if now > u {
                        return ("no answer to the probes (a firewall or a strict NAT)".into(), t.unsent());
                    }
                    if now.duration_since(probed) > Duration::from_millis(200) {
                        probed = now;
                        let n = u32::from_be_bytes(crate::secure::random::<4>());
                        probes_out.push((n, now));
                        if probes_out.len() > 64 {
                            probes_out.remove(0);
                        }
                        for p in &peers {
                            let _ = sock.send_to(&packet(t.key(), PROBE, n, 0, 0, &[]), p);
                        }
                    }
                }
            }
            Some(to) => {
                for d in t.outgoing(now) {
                    if sock.send_to(&d, to).is_ok() {
                        pinged = now;
                    }
                }
                if now.duration_since(pinged) > PING_EVERY {
                    let _ = sock.send_to(&t.ping(), to);
                    pinged = now;
                }
                if now.duration_since(heard) > SILENT {
                    return (format!("nothing from {to} for {} s", SILENT.as_secs()), t.unsent());
                }
            }
        }
        // Everything that has arrived, then the acks and data it calls for at once.
        let mut got = false;
        loop {
            let (n, from) = match sock.recv_from(&mut buf) {
                Ok(x) => x,
                Err(_) => break,
            };
            got = true;
            let Some(p) = parse(t.key(), &buf[..n]) else { continue };
            heard = Instant::now();
            match p.kind {
                PROBE => {
                    let _ = sock.send_to(&packet(t.key(), PROBE_ACK, p.seq, 0, 0, &[]), from);
                }
                PROBE_ACK if path.is_none() => {
                    if let Some((_, at)) = probes_out.iter().find(|(s, _)| *s == p.seq) {
                        let rtt = at.elapsed();
                        t.rto = (rtt * 3).clamp(Duration::from_millis(150), Duration::from_millis(1000));
                        println!("agent: direct: open to {from}, {} ms", rtt.as_millis());
                        path = Some(from);
                        if events.send(Event::Open(from.to_string())).is_err() {
                            return (String::new(), t.unsent());
                        }
                    }
                }
                _ => {
                    if path.is_none() && p.kind == DATA {
                        path = Some(from);
                        let _ = events.send(Event::Open(from.to_string()));
                    }
                    // The Mac's address may change (another network): follow it.
                    if path.is_some() && p.kind != PROBE_ACK {
                        path = Some(from);
                    }
                    for r in t.receive(&p) {
                        if events.send(Event::Message(r)).is_err() {
                            return (String::new(), t.unsent());
                        }
                    }
                }
            }
        }
        if let (true, Some(to)) = (got, path) {
            for d in t.outgoing(Instant::now()) {
                if sock.send_to(&d, to).is_ok() {
                    pinged = Instant::now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Transport, Transport) {
        (Transport::new(&[3; 32]), Transport::new(&[3; 32]))
    }

    /// Deliver datagrams from one side to the other, dropping those `lose` picks.
    fn carry(from: &mut Transport, to: &mut Transport, now: Instant, lose: &mut dyn FnMut(usize) -> bool, got: &mut Vec<Vec<u8>>, k: &mut usize) {
        for d in from.outgoing(now) {
            *k += 1;
            if lose(*k) {
                continue;
            }
            let key = hmac::Key::new(hmac::HMAC_SHA256, &[3; 32]);
            let p = parse(&key, &d).expect("tagged");
            got.extend(to.receive(&p));
        }
    }

    #[test]
    fn records_arrive_in_order_through_loss() {
        let (mut a, mut b) = pair();
        let records: Vec<Vec<u8>> = (0..40u32).map(|i| (0..(i * 397 % 5000) as usize).map(|j| (i as usize + j) as u8).collect()).collect();
        for r in &records {
            a.queue(r.clone());
        }
        let mut now = Instant::now();
        let (mut got, mut none) = (Vec::new(), Vec::new());
        let (mut ka, mut kb) = (0, 0);
        let mut drop_a = |k: usize| k % 7 == 3;
        let mut drop_b = |k: usize| k % 5 == 1;
        for _ in 0..2000 {
            carry(&mut a, &mut b, now, &mut drop_a, &mut got, &mut ka);
            carry(&mut b, &mut a, now, &mut drop_b, &mut none, &mut kb);
            now += Duration::from_millis(50);
            if a.idle() && got.len() == records.len() {
                break;
            }
        }
        assert_eq!(got, records);
        assert!(a.idle());
        assert!(a.unsent().is_empty());
    }

    #[test]
    fn unsent_records_come_back() {
        let (mut a, _) = pair();
        a.queue(vec![1; 3000]);
        a.queue(vec![2; 10]);
        let _ = a.outgoing(Instant::now());
        assert_eq!(a.unsent(), vec![vec![1; 3000], vec![2; 10]]);
    }

    #[test]
    fn tags_and_stun() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &[1; 32]);
        let other = hmac::Key::new(hmac::HMAC_SHA256, &[2; 32]);
        let p = packet(&key, PROBE, 7, 0, 0, b"");
        assert_eq!(parse(&key, &p).unwrap().seq, 7);
        assert!(parse(&other, &p).is_none());
        let mut bad = p.clone();
        bad[3] ^= 1;
        assert!(parse(&key, &bad).is_none());
        let id = [9u8; 12];
        assert_eq!(stun_request(&id).len(), 20);
        // A response with XOR-MAPPED-ADDRESS 203.0.113.7:40000.
        let mut r = vec![1, 1, 0, 12, 0x21, 0x12, 0xa4, 0x42];
        r.extend_from_slice(&id);
        r.extend_from_slice(&[0, 0x20, 0, 8, 0, 1]);
        r.extend_from_slice(&(40000u16 ^ 0x2112).to_be_bytes());
        r.extend_from_slice(&(u32::from(Ipv4Addr::new(203, 0, 113, 7)) ^ 0x2112_a442).to_be_bytes());
        assert_eq!(stun_mapped(&r, &id), Some("203.0.113.7:40000".parse().unwrap()));
        assert_eq!(stun_mapped(&r, &[0; 12]), None);
    }
}
