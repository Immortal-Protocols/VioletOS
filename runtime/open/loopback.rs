//! Loopback TCP for open apps: an app can serve itself or another app on
//! 127.0.0.0/8 (and ::1, sockaddr.rs) without Radio, and socketpair works.
//!
//! A connection is a `Pipe` of two byte queues, one each way, held by its two
//! `End`s; a listening socket holds the `Listener` its connections wait in
//! until accepted. What apps can make the runtime keep is bounded, because a
//! runtime heap that runs out ends every app:
//! - each direction of a pipe holds at most `CAP` bytes (then EAGAIN);
//! - at most `MAX_PIPES` pipes are open on the phone (ENFILE) and
//!   `APP_PIPES` of them opened by one app (EMFILE), counted until both ends
//!   are closed, wherever they are (an app's descriptors or a backlog). An app
//!   is its owner (App::owner, which forked children and the programs it
//!   starts share), not a pid; and the last `FLOOR_POOL` pipes go only to
//!   apps holding fewer than `PIPE_FLOOR`, so busy apps never leave another
//!   app without its first socketpair (asyncio needs one);
//! - a listener keeps at most its backlog (`MAX_BACKLOG`) waiting; a connect
//!   past that is refused (ECONNREFUSED).
//! Ports below 1024 are the system's (EACCES); a port 0 bind takes one of
//! the ephemeral range in rotation (above lwIP's own local ports, lwipopts.h).
//!
//! Between apps: a connection or datagram reaches only a socket of the same
//! app, unless the receiving app's manifest says `serve: apps` (`serve_words`);
//! `serve: lan` lets a socket on the wildcard take connections and datagrams
//! from the network. A connection offered or bytes sent to an app recently
//! keep it running (`wakes`: the lifecycle would freeze a server in the
//! background, and its clients would hang). Pure: tested on the host (make test-unit).
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

/// Bytes one direction of a connection holds.
pub const CAP: usize = 32 << 10;
/// Pipes on the phone, and pipes one app may have opened.
pub const MAX_PIPES: usize = 32;
pub const APP_PIPES: usize = 8;
/// What every app can open however busy the others are, and the pipes kept for that.
pub const PIPE_FLOOR: usize = 2;
pub const FLOOR_POOL: usize = 6;
/// How long a connection offered, or bytes sent, keep the receiving app running (ns).
pub const WAKE_NS: u64 = 10_000_000_000;
/// Manifest `serve:` words.
pub const SERVE_LAN: u8 = 1;
pub const SERVE_APPS: u8 = 2;
/// The longest backlog a listener keeps.
pub const MAX_BACKLOG: usize = 8;
/// The first port a port 0 bind may take (IANA's dynamic range).
/// (The upper half of IANA's dynamic range: lwIP picks its own local ports
/// from the lower half, so a port given here is never one Radio already uses.)
pub const EPHEMERAL: u16 = 57344;
const EAGAIN: i32 = -11;
const EACCES: i32 = -13;
const ENFILE: i32 = -23;
const EMFILE: i32 = -24;
const EPIPE: i32 = -32;
const ENOBUFS: i32 = -105;
const ENOTCONN: i32 = -107;
const ECONNREFUSED: i32 = -111;
// poll(2) bits.
const POLLIN: u32 = 1;
const POLLOUT: u32 = 4;
const POLLHUP: u32 = 0x10;

pub fn is_loopback(ip: u32) -> bool {
    ip >> 24 == 127
}

/// The `serve:` words of a manifest (SERVE_LAN, SERVE_APPS). Unknown words
/// grant nothing.
pub fn serve_words(manifest: &str) -> u8 {
    let mut m = 0;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or("");
        if let Some(("serve", rest)) = line.split_once(':').map(|(k, r)| (k.trim(), r)) {
            for w in rest.split(|c: char| c.is_whitespace() || c == ',') {
                m |= match w {
                    "lan" => SERVE_LAN,
                    "apps" => SERVE_APPS,
                    _ => 0,
                };
            }
        }
    }
    m
}

/// May a connection or datagram from one app reach a socket of another?
/// `same`: the same app; `serves`: the receiving app's `serve_words`.
pub fn may_reach(same: bool, serves: u8) -> bool {
    same || serves & SERVE_APPS != 0
}

/// Does a UDP socket bound to `local`, connected to `peer` if it is, take a
/// loopback datagram to `port` from `from_port`? Never one not bound to a
/// port (port 0 is no destination), and a connected socket only its peer's
/// (as Linux filters).
pub fn takes_datagram(local: (u32, u16), peer: Option<(u32, u16)>, port: u16, from_port: u16) -> bool {
    port != 0
        && local.1 == port
        && (local.0 == 0 || is_loopback(local.0))
        && peer.map_or(true, |p| (p.0 == 0 || is_loopback(p.0)) && p.1 == from_port)
}

/// Whether an app (`system`: the runtime itself) may bind `port`.
pub fn may_bind(port: u16, system: bool) -> Result<(), i32> {
    if port != 0 && port < 1024 && !system { Err(EACCES) } else { Ok(()) }
}

/// The first port from `from` on, in the ephemeral range and wrapping
/// within it, that `used` says is free; None when every one is taken.
pub fn ephemeral(from: u16, used: &mut dyn FnMut(u16) -> bool) -> Option<u16> {
    let span = 65536 - EPHEMERAL as u32;
    let start = u32::from(from.max(EPHEMERAL) - EPHEMERAL);
    (0..span).map(|k| EPHEMERAL + ((start + k) % span) as u16).find(|&p| !used(p))
}

/// Who (App::owner) opened each pipe still open (one entry a pipe).
#[derive(Clone)]
pub struct Budget(Rc<RefCell<Vec<String>>>);

impl Budget {
    pub fn new() -> Budget {
        Budget(Rc::new(RefCell::new(Vec::new())))
    }
    pub fn live(&self) -> usize {
        self.0.borrow().len()
    }
    pub fn held(&self, owner: &str) -> usize {
        self.0.borrow().iter().filter(|o| *o == owner).count()
    }
    /// A new connection opened by app `owner`: its two ends (0 the opener's).
    pub fn open(&self, owner: &str) -> Result<(End, End), i32> {
        let mut v = self.0.borrow_mut();
        let mine = v.iter().filter(|o| *o == owner).count();
        if v.len() >= MAX_PIPES || (mine >= PIPE_FLOOR && v.len() >= MAX_PIPES - FLOOR_POOL) {
            return Err(ENFILE);
        }
        if mine >= APP_PIPES {
            return Err(EMFILE);
        }
        if v.try_reserve(1).is_err() {
            return Err(ENOBUFS);
        }
        v.push(String::from(owner));
        drop(v);
        let pipe = Pipe { to: [VecDeque::new(), VecDeque::new()], open: [true; 2], shut: [false; 2], fed: [0; 2], owner: String::from(owner), budget: self.clone() };
        let pipe = Rc::new(RefCell::new(pipe));
        Ok((End { pipe: pipe.clone(), side: 0 }, End { pipe, side: 1 }))
    }
}

struct Pipe {
    /// Bytes on their way to side 0, and to side 1.
    to: [VecDeque<u8>; 2],
    open: [bool; 2],
    /// The side has shut down sending.
    shut: [bool; 2],
    /// When bytes were last queued to side 0, and to side 1 (ns).
    fed: [u64; 2],
    owner: String,
    budget: Budget,
}
impl Drop for Pipe {
    fn drop(&mut self) {
        let mut v = self.budget.0.borrow_mut();
        if let Some(k) = v.iter().position(|o| *o == self.owner) {
            v.swap_remove(k);
        }
    }
}

/// One side of a connection.
pub struct End {
    pipe: Rc<RefCell<Pipe>>,
    side: usize,
}
impl End {
    /// Up to `max` bytes; empty at the end of the stream (the other side
    /// closed or shut down sending), EAGAIN when nothing has come yet.
    pub fn read(&self, max: usize) -> Result<Vec<u8>, i32> {
        let mut p = self.pipe.borrow_mut();
        let o = 1 - self.side;
        let ended = !p.open[o] || p.shut[o];
        let q = &mut p.to[self.side];
        if q.is_empty() {
            return if ended || max == 0 { Ok(Vec::new()) } else { Err(EAGAIN) };
        }
        let n = q.len().min(max);
        Ok(q.drain(..n).collect())
    }
    /// What of `data` fits; EPIPE once this side shut down or the other
    /// closed. `now` (ns) is when, for `wakes`.
    pub fn write(&self, data: &[u8], now: u64) -> Result<usize, i32> {
        let mut p = self.pipe.borrow_mut();
        let o = 1 - self.side;
        if p.shut[self.side] || !p.open[o] {
            return Err(EPIPE);
        }
        let q = &mut p.to[o];
        let n = (CAP - q.len()).min(data.len());
        if n == 0 {
            return if data.is_empty() { Ok(0) } else { Err(EAGAIN) };
        }
        if q.try_reserve(n).is_err() {
            return Err(ENOBUFS);
        }
        q.extend(data[..n].iter().copied());
        p.fed[o] = now;
        Ok(n)
    }
    /// Bytes wait to be read here that were sent within WAKE_NS of `now`.
    pub fn wakes(&self, now: u64) -> bool {
        let p = self.pipe.borrow();
        !p.to[self.side].is_empty() && now.saturating_sub(p.fed[self.side]) < WAKE_NS
    }
    pub fn shutdown(&self) {
        self.pipe.borrow_mut().shut[self.side] = true;
    }
    pub fn poll(&self) -> u32 {
        let p = self.pipe.borrow();
        let o = 1 - self.side;
        let mut m = 0;
        if !p.to[self.side].is_empty() || !p.open[o] || p.shut[o] {
            m |= POLLIN;
        }
        if !p.open[o] {
            m |= POLLHUP | POLLOUT;
        } else if !p.shut[self.side] && p.to[o].len() < CAP {
            m |= POLLOUT;
        }
        m
    }
}
impl Drop for End {
    fn drop(&mut self) {
        if let Ok(mut p) = self.pipe.try_borrow_mut() {
            p.open[self.side] = false;
            // Nobody will read what was on its way here.
            p.to[self.side] = VecDeque::new();
        }
    }
}

/// A listening socket's connections waiting to be accepted, and the peer
/// (address, port) of each.
pub struct Listener {
    backlog: usize,
    waiting: VecDeque<(End, (u32, u16))>,
    /// A network connection being taken from Radio for this listener (net.rs CMD_ACCEPT).
    pub accepting: Option<usize>,
    /// When the newest connection was offered (ns).
    offered: u64,
}
impl Listener {
    pub fn new(backlog: i32) -> Listener {
        let mut l = Listener { backlog: 1, waiting: VecDeque::new(), accepting: None, offered: 0 };
        l.set_backlog(backlog);
        l
    }
    pub fn set_backlog(&mut self, backlog: i32) {
        self.backlog = (backlog.max(1) as usize).min(MAX_BACKLOG);
    }
    pub fn backlog(&self) -> usize {
        self.backlog
    }
    /// A connection for this listener at `now` (ns); refused when its backlog is full.
    pub fn offer(&mut self, end: End, peer: (u32, u16), now: u64) -> Result<(), i32> {
        if self.waiting.len() >= self.backlog {
            return Err(ECONNREFUSED);
        }
        self.waiting.push_back((end, peer));
        self.offered = now;
        Ok(())
    }
    /// Connections wait, the newest offered within WAKE_NS of `now`.
    pub fn wakes(&self, now: u64) -> bool {
        !self.waiting.is_empty() && now.saturating_sub(self.offered) < WAKE_NS
    }
    pub fn take(&mut self) -> Option<(End, (u32, u16))> {
        self.waiting.pop_front()
    }
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// What a TCP socket is when it is not a Radio connection.
pub enum Lo {
    Listen(Listener),
    Conn(End),
}
impl Lo {
    pub fn read(&self, max: usize) -> Result<Vec<u8>, i32> {
        match self {
            Lo::Conn(e) => e.read(max),
            Lo::Listen(_) => Err(ENOTCONN),
        }
    }
    /// Something came for this socket's app within WAKE_NS of `now`.
    pub fn wakes(&self, now: u64) -> bool {
        match self {
            Lo::Conn(e) => e.wakes(now),
            Lo::Listen(l) => l.wakes(now),
        }
    }
    /// poll(2) bits; `network`: Radio has a connection for this listener.
    pub fn poll(&self, network: bool) -> u32 {
        match self {
            Lo::Conn(e) => e.poll(),
            Lo::Listen(l) => (l.waiting() > 0 || network) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_cross_both_ways_and_end() {
        let b = Budget::new();
        let (c, s) = b.open("7").unwrap();
        assert_eq!(s.read(10), Err(EAGAIN));
        assert_eq!(s.poll() & POLLIN, 0);
        assert_eq!(c.write(b"hello", 0), Ok(5));
        assert_eq!(s.poll() & POLLIN, POLLIN);
        assert_eq!(s.read(3).unwrap(), b"hel");
        assert_eq!(s.read(10).unwrap(), b"lo");
        assert_eq!(s.write(b"back", 0), Ok(4));
        assert_eq!(c.read(10).unwrap(), b"back");
        // Half-close: the client is done sending, the server still may.
        c.shutdown();
        assert_eq!(c.write(b"x", 0), Err(EPIPE));
        assert_eq!(s.read(10).unwrap(), b"");
        assert_eq!(s.poll() & POLLIN, POLLIN);
        assert_eq!(s.write(b"more", 0), Ok(4));
        assert_eq!(c.read(10).unwrap(), b"more");
        // Closing: what was sent is still read, then the end of the stream.
        assert_eq!(s.write(b"last", 0), Ok(4));
        drop(s);
        assert_eq!(c.poll() & (POLLIN | POLLHUP), POLLIN | POLLHUP);
        assert_eq!(c.read(10).unwrap(), b"last");
        assert_eq!(c.read(10).unwrap(), b"");
    }

    #[test]
    fn writing_to_a_closed_peer_is_epipe() {
        let b = Budget::new();
        let (c, s) = b.open("1").unwrap();
        drop(s);
        assert_eq!(c.write(b"x", 0), Err(EPIPE));
        assert_eq!(c.poll() & POLLHUP, POLLHUP);
    }

    #[test]
    fn a_full_direction_pushes_back() {
        let b = Budget::new();
        let (c, s) = b.open("1").unwrap();
        let big = alloc::vec![0x5a; CAP + 100];
        assert_eq!(c.write(&big, 0), Ok(CAP));
        assert_eq!(c.poll() & POLLOUT, 0);
        assert_eq!(c.write(b"x", 0), Err(EAGAIN));
        assert_eq!(c.write(b"", 0), Ok(0));
        // The other direction is not held up.
        assert_eq!(s.write(b"y", 0), Ok(1));
        assert_eq!(s.read(1000).unwrap().len(), 1000);
        assert_eq!(c.poll() & POLLOUT, POLLOUT);
        assert_eq!(c.write(&big, 0), Ok(1000));
    }

    #[test]
    fn closing_frees_what_was_never_read() {
        let b = Budget::new();
        let (c, s) = b.open("1").unwrap();
        c.write(&alloc::vec![1; CAP], 0).unwrap();
        drop(s);
        // The client cannot send more, and the server's bytes are gone with it.
        assert_eq!(c.pipe.borrow().to[1].len(), 0);
        assert_eq!(b.live(), 1);
        drop(c);
        assert_eq!(b.live(), 0);
    }

    #[test]
    fn pipes_are_bounded_per_app_and_in_all() {
        let b = Budget::new();
        let mut mine = Vec::new();
        for _ in 0..APP_PIPES {
            mine.push(b.open("one").unwrap());
        }
        assert_eq!(b.open("one").err(), Some(EMFILE));
        // Other busy apps are not held up by the first, until only the
        // floor pool is left.
        let mut others = Vec::new();
        for owner in ["two", "three"] {
            for _ in 0..APP_PIPES {
                others.push(b.open(owner).unwrap());
            }
        }
        for _ in 0..MAX_PIPES - FLOOR_POOL - 3 * APP_PIPES {
            others.push(b.open("four").unwrap());
        }
        assert_eq!(b.open("four").err(), Some(ENFILE));
        // The pool is for apps below their floor: each new app still gets its first pipes.
        for owner in ["five", "six", "seven"] {
            for _ in 0..PIPE_FLOOR {
                others.push(b.open(owner).unwrap());
            }
            assert_eq!(b.open(owner).err(), Some(ENFILE));
        }
        assert_eq!(b.live(), MAX_PIPES);
        assert_eq!(b.open("eight").err(), Some(ENFILE));
        // A pipe counts until both its ends are gone.
        let (c, s) = mine.pop().unwrap();
        drop(c);
        assert_eq!(b.open("eight").err(), Some(ENFILE));
        drop(s);
        assert_eq!(b.held("one"), APP_PIPES - 1);
        assert!(b.open("eight").is_ok());
    }

    #[test]
    fn forked_processes_share_their_apps_quota() {
        // The quota is the app's (App::owner, which a fork keeps), so four
        // processes of one app hold what one would: the rest of the phone
        // keeps the remaining pipes (it once could fill all 32 by forking).
        let b = Budget::new();
        let mut held = Vec::new();
        for _process in 0..4 {
            for _ in 0..APP_PIPES {
                if let Ok(p) = b.open("forker") {
                    held.push(p);
                }
            }
        }
        assert_eq!(held.len(), APP_PIPES);
        assert_eq!(b.live(), APP_PIPES);
        for _ in 0..APP_PIPES {
            held.push(b.open("asyncio-app").unwrap());
        }
    }

    #[test]
    fn manifests_say_who_may_be_served() {
        assert_eq!(serve_words("caps: net\n"), 0);
        assert_eq!(serve_words("caps: net\nserve: lan\n"), SERVE_LAN);
        assert_eq!(serve_words("serve: apps, lan # both\n"), SERVE_LAN | SERVE_APPS);
        assert_eq!(serve_words("serve: everything\n# serve: apps\nserves: apps\n"), 0);
        assert_eq!(serve_words(" serve : apps"), SERVE_APPS);
        // Another app's socket only by its own manifest's leave.
        assert!(may_reach(true, 0));
        assert!(!may_reach(false, 0));
        assert!(!may_reach(false, SERVE_LAN));
        assert!(may_reach(false, SERVE_APPS));
    }

    #[test]
    fn datagrams_reach_only_bound_and_matching_sockets() {
        const LO: u32 = 0x7f00_0001;
        // Bound to the port, on loopback or the wildcard.
        assert!(takes_datagram((LO, 60000), None, 60000, 58000));
        assert!(takes_datagram((0, 60000), None, 60000, 58000));
        assert!(!takes_datagram((LO, 60001), None, 60000, 58000));
        // Port 0 is nobody's: unbound sockets (local port 0 in the runtime,
        // whatever port Radio gave them) never take one.
        assert!(!takes_datagram((0, 0), None, 0, 58000));
        assert!(!takes_datagram((0, 0), Some((0x5db8_d822, 443)), 0, 58000));
        // A connected socket takes only its peer's.
        assert!(takes_datagram((LO, 60000), Some((LO, 58000)), 60000, 58000));
        assert!(!takes_datagram((LO, 60000), Some((LO, 58001)), 60000, 58000));
        assert!(!takes_datagram((0, 60000), Some((0x5db8_d822, 58000)), 60000, 58000));
    }

    #[test]
    fn what_came_recently_keeps_an_app_running() {
        let b = Budget::new();
        let mut l = Listener::new(4);
        assert!(!l.wakes(0));
        let (c, s) = b.open("client").unwrap();
        l.offer(s, (0x7f00_0001, 60000), 1_000).unwrap();
        assert!(l.wakes(1_000 + WAKE_NS - 1));
        // A connection never accepted keeps it only for so long.
        assert!(!l.wakes(1_000 + WAKE_NS));
        let (s, _) = l.take().unwrap();
        assert!(!l.wakes(2_000));
        assert!(!s.wakes(2_000));
        c.write(b"GET /", 5_000).unwrap();
        assert!(s.wakes(5_000 + WAKE_NS - 1) && !s.wakes(5_000 + WAKE_NS));
        assert!(Lo::Conn(s).wakes(6_000));
        // Read, nothing keeps it.
        let (c2, s2) = b.open("client").unwrap();
        c2.write(b"x", 7_000).unwrap();
        s2.read(10).unwrap();
        assert!(!s2.wakes(7_001));
        drop(c);
    }

    #[test]
    fn a_backlog_holds_what_it_may() {
        let b = Budget::new();
        let mut l = Listener::new(2);
        assert_eq!(l.backlog(), 2);
        let mut clients = Vec::new();
        for k in 0..3u16 {
            let (c, s) = b.open("1").unwrap();
            let r = l.offer(s, (0x7f00_0001, 50000 + k), 0);
            assert_eq!(r.is_ok(), k < 2, "{}", k);
            clients.push(c);
        }
        // The refused connection's server end is gone: its client sees the end.
        assert_eq!(clients[2].write(b"x", 0), Err(EPIPE));
        let (s, peer) = l.take().unwrap();
        assert_eq!(peer, (0x7f00_0001, 50000));
        clients[0].write(b"hi", 0).unwrap();
        assert_eq!(s.read(10).unwrap(), b"hi");
        assert_eq!(l.waiting(), 1);
        // A listener that goes closes what waits in it.
        drop(l);
        assert_eq!(clients[1].read(10).unwrap(), b"");
        assert_eq!(Listener::new(0).backlog(), 1);
        assert_eq!(Listener::new(-5).backlog(), 1);
        assert_eq!(Listener::new(4096).backlog(), MAX_BACKLOG);
    }

    #[test]
    fn listener_readiness() {
        let b = Budget::new();
        let mut l = Lo::Listen(Listener::new(4));
        assert_eq!(l.poll(false), 0);
        assert_eq!(l.poll(true), POLLIN);
        assert_eq!(l.read(1).err(), Some(ENOTCONN));
        let (_c, s) = b.open("1").unwrap();
        if let Lo::Listen(x) = &mut l {
            x.offer(s, (0, 0), 0).unwrap();
        }
        assert_eq!(l.poll(false), POLLIN);
    }

    #[test]
    fn ports() {
        assert_eq!(may_bind(80, false), Err(EACCES));
        assert_eq!(may_bind(1023, false), Err(EACCES));
        assert_eq!(may_bind(80, true), Ok(()));
        assert_eq!(may_bind(0, false), Ok(()));
        assert_eq!(may_bind(8080, false), Ok(()));
        assert_eq!(ephemeral(0, &mut |_| false), Some(EPHEMERAL));
        assert_eq!(ephemeral(60000, &mut |p| p == 60000), Some(60001));
        assert_eq!(ephemeral(65535, &mut |p| p == 65535), Some(EPHEMERAL));
        assert_eq!(ephemeral(EPHEMERAL, &mut |_| true), None);
        assert!(is_loopback(0x7f00_0001) && is_loopback(0x7fff_fffe) && !is_loopback(0x0a00_0002));
    }
}
