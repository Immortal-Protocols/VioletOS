//! Socket system calls: IPv4 TCP and UDP through Radio (net.rs), loopback
//! TCP and UDP inside the runtime (loopback.rs), and AF_INET6 sockets for the
//! addresses those carry (sockaddr.rs). DNS questions to the runtime's
//! resolver are answered here (names::answer).
//!
//! Who may reach whom (loopback.rs `may_reach`, `serve_words`): a loopback
//! connection or datagram reaches only a socket of the same app (App::owner)
//! unless the receiving app's manifest says `serve: apps`; a socket on the
//! wildcard is offered to the network (a Radio listener, or a UDP port bound
//! there) only when its app's says `serve: lan`, never while it is
//! quarantined, and every connection taken from the network passes the
//! Shield as a connect to its peer.
use super::device::{Kind, OpenFile};
use super::loopback::{self, Listener, Lo};
use super::names;
use super::net;
use super::sockaddr::{self, AF_INET, AF_INET6, AF_UNIX, LOOPBACK};
use super::syscall::{Outcome, EAGAIN, EBADF, EFAULT, EINVAL, ENOSYS};
use super::{Runtime, Wait};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use Outcome::*;

const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_NONBLOCK: u32 = 0o4000;
const O_NONBLOCK: u32 = 0o4000;
const MSG_DONTWAIT: u32 = 0x40;
const EMFILE: i32 = -24;
const EAFNOSUPPORT: i32 = -97;
const EPROTONOSUPPORT: i32 = -93;
const EOPNOTSUPP: i32 = -95;
const EADDRINUSE: i32 = -98;
const EADDRNOTAVAIL: i32 = -99;
const ENOTCONN: i32 = -107;
const EISCONN: i32 = -106;
const EINPROGRESS: i32 = -115;
const EALREADY: i32 = -114;
const ENETUNREACH: i32 = -101;
const ECONNREFUSED: i32 = -111;
/// How long listen and bind wait for Radio to take a port on the network.
const OFFER_NS: u64 = 5_000_000_000;
/// DNS answers a UDP socket keeps until the program reads them.
const MAX_REPLIES: usize = 16;
/// Loopback datagrams waiting on the whole phone, and the largest one.
const LOOP_DATAGRAMS: usize = 256;
/// The resolver programs are told to ask (/etc/resolv.conf).
pub const NAMESERVER: u32 = 0x0a00_0035; // 10.0.0.53

/// A socket of some app still holds `ip` (as its peer, or as a destination the
/// Shield allowed): the resolver must not give the address to another name.
/// A file that is being changed right now counts as holding it.
fn address_live(apps: &[Option<super::App>], ip: u32) -> bool {
    apps.iter().flatten().flat_map(|a| a.fds.iter().flatten()).any(|f| match f.try_borrow() {
        Ok(file) => match &file.kind {
            Kind::Tcp { peer, .. } => peer.0 == ip,
            Kind::Udp { peer, allowed, .. } => peer.is_some_and(|p| p.0 == ip) || allowed.iter().any(|a| a.0 == ip),
            _ => false,
        },
        Err(_) => true,
    })
}

/// The TCP (or UDP) port a socket holds: a listener's, or one bound and
/// not yet listening or connected. Connections hold none (so a server can
/// listen again on its port while old connections drain).
fn holds_port(kind: &Kind, udp: bool) -> u16 {
    match kind {
        Kind::Tcp { local, lo: Some(Lo::Listen(_)), .. } | Kind::Tcp { local, lo: None, slot: None, .. } if !udp => local.1,
        Kind::Udp { local, .. } if udp => local.1,
        _ => 0,
    }
}

/// A new socket file.
fn tcp(local: (u32, u16), v6: bool, slot: Option<usize>, peer: (u32, u16), lo: Option<Lo>) -> Kind {
    Kind::Tcp { slot, peer, local, v6, lo }
}

impl Runtime {
    /// The owner of app `i`'s sockets, slot quota and resolved names: the
    /// pid at the root of its process tree (syscall.rs quota_owner), so forked
    /// children share one app's share instead of multiplying it.
    fn pid_of(&self, i: usize) -> u32 {
        self.quota_owner(i)
    }
    /// The app (App::owner) app `i` is part of; forked children share it.
    fn owner_of(&self, i: usize) -> &str {
        self.apps[i].as_ref().map_or("", |a| a.owner.as_str())
    }
    /// What app `i`'s manifest lets it serve (loopback::serve_words): the
    /// manifest it runs under (manifest.rs `profile`), none for a program
    /// that runs under none.
    fn serves(&self, i: usize) -> u8 {
        use super::policy::{self, Origin};
        let Some(app) = self.apps[i].as_ref() else { return 0 };
        let text = match self.profile(i).origin {
            Origin::Image => self.fs.image_contents(super::manifest::MANIFESTS).and_then(|d| core::str::from_utf8(d).ok()).and_then(|t| policy::image_manifest(t, &app.owner)),
            Origin::Installed => self.store.entries.iter().find(|e| e.name == app.owner && !e.package).map(|e| e.manifest.as_str()),
            Origin::Other => None,
        };
        text.map_or(0, loopback::serve_words)
    }
    /// May app `i` reach a socket that app `j` holds?
    fn reaches(&self, i: usize, j: usize) -> bool {
        let same = self.owner_of(i) == self.owner_of(j);
        loopback::may_reach(same, if same { 0 } else { self.serves(j) })
    }
    /// May app `i` offer a socket on the wildcard to the network now?
    fn offers_lan(&mut self, i: usize) -> Result<bool, i32> {
        if self.serves(i) & loopback::SERVE_LAN == 0 {
            return Ok(false);
        }
        if self.quarantined(i) {
            return Err(-13);
        }
        Ok(true)
    }
    /// For the lifecycle (mod.rs `life_facts`): a connection was offered to,
    /// or bytes sent to, this socket within loopback::WAKE_NS, or the network
    /// has a connection waiting on its listener. Its app is kept running.
    pub fn socket_wakes(&self, kind: &Kind, now: u64) -> bool {
        match kind {
            Kind::Tcp { lo: Some(lo), slot, .. } => lo.wakes(now) || (matches!(lo, Lo::Listen(_)) && slot.is_some_and(|s| self.net.waiting(s) > 0)),
            _ => false,
        }
    }

    pub fn socket_call(&mut self, i: usize, n: u32, a: [u32; 7]) -> Outcome {
        match n {
            281 => Done(self.socket(i, a[0], a[1])),
            282 => self.bind(i, a[0] as i32, a[1], a[2]),
            283 => self.connect(i, a[0] as i32, a[1], a[2]),
            284 => self.listen(i, a[0] as i32, a[1] as i32),
            285 | 366 => self.accept(i, a[0] as i32, a[1], a[2], if n == 366 { a[3] } else { 0 }),
            286 | 287 => self.socket_name(i, a[0] as i32, a[1], a[2], n == 287),
            289 | 290 => {
                let to = if n == 290 && a[4] != 0 { Some(self.read_sockaddr(i, a[0] as i32, a[4], a[5])) } else { None };
                match to {
                    Some(Err(e)) => Done(e),
                    to => self.send_call(i, a[0] as i32, a[1], a[2], a[3], to.and_then(|t| t.ok())),
                }
            }
            291 | 292 => {
                let (name, name_len) = if n == 292 { (a[4], a[5]) } else { (0, 0) };
                self.recv_call(i, a[0] as i32, a[1], a[2], a[3], name, name_len)
            }
            293 => {
                let Some(f) = self.socket_file(i, a[0] as i32) else { return Done(EBADF) };
                match &f.borrow().kind {
                    Kind::Tcp { lo: Some(Lo::Conn(end)), .. } if a[1] >= 1 => end.shutdown(),
                    Kind::Tcp { slot: Some(slot), lo: None, .. } if a[1] >= 1 => self.net.shutdown_send(*slot),
                    _ => {}
                }
                Done(0)
            }
            294 => {
                let Some(f) = self.socket_file(i, a[0] as i32) else { return Done(EBADF) };
                // SOL_SOCKET SO_RCVTIMEO: struct timeval with 32-bit (20) or
                // 64-bit (66, musl's time64 form) fields. Other options are accepted.
                if a[1] == 1 && (a[2] == 20 || a[2] == 66) {
                    let wide = a[2] == 66;
                    let mut tv = [0u8; 16];
                    let n = if wide { 16 } else { 8 };
                    let app = self.apps[i].as_mut().unwrap();
                    if !app.space.read(&mut self.k, &mut self.pool, a[3], &mut tv[..n]) {
                        return Done(EFAULT);
                    }
                    let (sec, usec) = if wide {
                        (i64::from_le_bytes(tv[..8].try_into().unwrap()), i64::from_le_bytes(tv[8..16].try_into().unwrap()))
                    } else {
                        (i32::from_le_bytes(tv[..4].try_into().unwrap()) as i64, i32::from_le_bytes(tv[4..8].try_into().unwrap()) as i64)
                    };
                    f.borrow_mut().recv_timeout = (sec.max(0) as u64) * 1_000_000_000 + (usec.max(0) as u64) * 1000;
                }
                Done(0)
            }
            295 => self.getsockopt(i, a[0] as i32, a[1], a[2], a[3], a[4]),
            296 | 297 => self.message_call(i, a[0] as i32, a[1], a[2], n == 296),
            288 => Done(self.socketpair(i, a[0], a[1], a[3])),
            _ => Done(ENOSYS),
        }
    }

    fn socket_file(&self, i: usize, fd: i32) -> Option<super::FileRef> {
        let f = self.apps[i].as_ref()?.fds.get(fd as usize)?.clone()?;
        let socket = matches!(f.borrow().kind, Kind::Tcp { .. } | Kind::Udp { .. });
        socket.then_some(f)
    }
    fn is_socket(&self, i: usize, fd: i32) -> bool {
        self.socket_file(i, fd).is_some()
    }
    /// The socket is AF_INET6: addresses go to and from it in that form.
    fn is_v6(&self, i: usize, fd: i32) -> bool {
        self.socket_file(i, fd).is_some_and(|f| matches!(f.borrow().kind, Kind::Tcp { v6: true, .. } | Kind::Udp { v6: true, .. }))
    }
    fn peer_of(&self, i: usize, fd: i32) -> Option<(u32, u16)> {
        let f = self.socket_file(i, fd)?;
        let kind = &f.borrow().kind;
        match kind {
            Kind::Tcp { peer, .. } => Some(*peer),
            Kind::Udp { peer, .. } => *peer,
            _ => None,
        }
    }
    /// The address an app gave for socket `fd`, as IPv4.
    fn read_sockaddr(&mut self, i: usize, fd: i32, ptr: u32, len: u32) -> Result<(u32, u16), i32> {
        let v6 = self.is_v6(i, fd);
        let n = (len as usize).min(sockaddr::MAX);
        let app = self.apps[i].as_mut().ok_or(EBADF)?;
        let mut b = [0u8; sockaddr::MAX];
        if n < 2 {
            return Err(EINVAL);
        }
        if !app.space.read(&mut self.k, &mut self.pool, ptr, &mut b[..n]) {
            return Err(EFAULT);
        }
        sockaddr::parse(&b[..n], v6)
    }
    fn put_sockaddr(&mut self, i: usize, ptr: u32, len_ptr: u32, addr: (u32, u16), v6: bool) {
        let app = self.apps[i].as_mut().unwrap();
        let (b, n) = sockaddr::encode(addr, v6);
        let room = if len_ptr != 0 { app.space.read_u32(&mut self.k, &mut self.pool, len_ptr).unwrap_or(0) } else { n as u32 };
        app.space.write(&mut self.k, &mut self.pool, ptr, &b[..(room as usize).min(n)]);
        if len_ptr != 0 {
            app.space.write_u32(&mut self.k, &mut self.pool, len_ptr, n as u32);
        }
    }
    /// A new descriptor for `file` (at most 1024, as `install` keeps).
    fn add_fd(&mut self, i: usize, file: OpenFile) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        let pos = app.fds.iter().position(|f| f.is_none()).unwrap_or(app.fds.len());
        if pos >= 1024 {
            return EMFILE;
        }
        if pos == app.fds.len() {
            app.fds.push(None);
        }
        app.fds[pos] = Some(alloc::rc::Rc::new(core::cell::RefCell::new(file)));
        pos as i32
    }

    fn socket(&mut self, i: usize, domain: u32, kind: u32) -> i32 {
        if domain != AF_INET && domain != AF_INET6 {
            return EAFNOSUPPORT;
        }
        let v6 = domain == AF_INET6;
        let flags = 2 | if kind & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
        let file = match kind & 0xf {
            SOCK_STREAM => OpenFile::new(tcp((0, 0), v6, None, (0, 0), None), flags),
            SOCK_DGRAM => OpenFile::new(Kind::Udp { peer: None, replies: VecDeque::new(), slot: None, allowed: Vec::new(), local: (0, 0), v6 }, flags),
            _ => return EPROTONOSUPPORT,
        };
        self.add_fd(i, file)
    }

    /// socketpair(2): AF_UNIX stream sockets, as a loopback connection.
    fn socketpair(&mut self, i: usize, domain: u32, kind: u32, sv: u32) -> i32 {
        if domain != AF_UNIX || kind & 0xf != SOCK_STREAM {
            return EOPNOTSUPP;
        }
        let (a, b) = match self.net.pipes.open(self.owner_of(i)) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let flags = 2 | if kind & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
        let x = self.add_fd(i, OpenFile::new(tcp((0, 0), false, None, (0, 0), Some(Lo::Conn(a))), flags));
        if x < 0 {
            return x;
        }
        let y = self.add_fd(i, OpenFile::new(tcp((0, 0), false, None, (0, 0), Some(Lo::Conn(b))), flags));
        let app = self.apps[i].as_mut().unwrap();
        if y < 0 || !app.space.write(&mut self.k, &mut self.pool, sv, &[(x as u32).to_le_bytes(), (y as u32).to_le_bytes()].concat()) {
            app.fds[x as usize] = None;
            if y >= 0 {
                app.fds[y as usize] = None;
            }
            return if y < 0 { y } else { EFAULT };
        }
        0
    }

    /// Every socket of every app (and the app's index), until `f` says stop
    /// (true). A file being changed right now is passed as None.
    fn scan(&self, f: &mut dyn FnMut(usize, &super::FileRef, Option<&Kind>) -> bool) -> bool {
        let all = self.apps.iter().enumerate().flat_map(|(j, a)| a.iter().flat_map(move |a| a.fds.iter().flatten().map(move |g| (j, g))));
        for (j, g) in all {
            let stop = match g.try_borrow() {
                Ok(file) => f(j, g, Some(&file.kind)),
                Err(_) => f(j, g, None),
            };
            if stop {
                return true;
            }
        }
        false
    }
    /// A port nobody holds for this protocol (a file being changed counts as holding).
    fn port_free(&self, udp: bool, port: u16) -> bool {
        !self.scan(&mut |_, _, k| k.map_or(true, |k| holds_port(k, udp) == port))
    }
    /// A free ephemeral port, in rotation.
    fn ephemeral(&mut self, udp: bool) -> Option<u16> {
        let p = loopback::ephemeral(self.net.next_port, &mut |p| !self.port_free(udp, p))?;
        self.net.next_port = p.wrapping_add(1);
        Some(p)
    }
    /// The Shield's say on an app taking a port.
    fn gate_bind(&mut self, i: usize, port: u16) -> Option<Outcome> {
        match self.gate(i, &super::policy::Action::Bind { port }) {
            super::Gate::Allow => None,
            super::Gate::Deny => Some(Done(-13)),
            super::Gate::Frozen => Some(Block(Wait::Frozen, None)),
            super::Gate::Killed => Some(Outcome::Gone),
        }
    }

    /// bind(2): the wildcard or a loopback address; another of the phone's
    /// addresses is not offered (EADDRNOTAVAIL). A UDP socket bound to the
    /// wildcard also takes its port on the network when the phone is online.
    fn bind(&mut self, i: usize, fd: i32, addr: u32, len: u32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let (udp, bound) = match &f.borrow().kind {
            Kind::Tcp { local, slot, lo, .. } => (false, local.1 != 0 || slot.is_some() || lo.is_some()),
            Kind::Udp { local, slot, .. } => (true, local.1 != 0 || slot.is_some()),
            _ => return Done(EBADF),
        };
        let (ip, port) = match self.read_sockaddr(i, fd, addr, len) {
            Ok(a) => a,
            Err(ENETUNREACH) => return Done(EADDRNOTAVAIL),
            Err(e) => return Done(e),
        };
        if bound {
            return Done(EINVAL);
        }
        if ip != 0 && !loopback::is_loopback(ip) {
            return Done(EADDRNOTAVAIL);
        }
        if let Err(e) = loopback::may_bind(port, false) {
            return Done(e);
        }
        let port = if port != 0 {
            if !self.port_free(udp, port) {
                return Done(EADDRINUSE);
            }
            port
        } else {
            match self.ephemeral(udp) {
                Some(p) => p,
                None => return Done(EADDRINUSE),
            }
        };
        let mut slot = None;
        if udp && ip == 0 && self.net.online() != 0 {
            match self.offers_lan(i) {
                Ok(true) => {
                    if let Some(o) = self.gate_bind(i, port) {
                        return o;
                    }
                    // Datagrams to the port from the network: Radio binds it,
                    // and bind waits for its answer (connect_result).
                    match self.net.udp_bind_for(self.pid_of(i), super::MAX_APPS, port) {
                        Ok(s) => slot = Some(s),
                        Err(e) => return Done(e),
                    }
                }
                Ok(false) => {}
                Err(e) => return Done(e),
            }
        }
        match &mut f.borrow_mut().kind {
            Kind::Tcp { local, .. } => *local = (ip, port),
            Kind::Udp { local, slot: s, .. } => {
                *local = (ip, port);
                *s = slot;
            }
            _ => {}
        }
        if slot.is_some() {
            return Block(Wait::Connect { fd }, Some(super::sys::now_ns() + OFFER_NS));
        }
        Done(0)
    }

    /// listen(2): connections from apps on the phone, and, for a socket on
    /// the wildcard while the phone is on a network, from the network through
    /// Radio (one of the app's slots; not through the phone's relay).
    fn listen(&mut self, i: usize, fd: i32, backlog: i32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let mut local = match &mut f.borrow_mut().kind {
            Kind::Tcp { lo: Some(Lo::Listen(l)), .. } => {
                l.set_backlog(backlog);
                return Done(0);
            }
            Kind::Tcp { lo: Some(_), .. } | Kind::Tcp { slot: Some(_), .. } => return Done(EINVAL),
            Kind::Tcp { local, .. } => *local,
            _ => return Done(EOPNOTSUPP),
        };
        if local.1 == 0 {
            local = match self.ephemeral(false) {
                Some(p) => (0, p),
                None => return Done(EADDRINUSE),
            };
        }
        if let Some(o) = self.gate_bind(i, local.1) {
            return o;
        }
        let listener = Listener::new(backlog);
        let mut slot = None;
        if local.0 == 0 && matches!(self.net.online(), 1 | 3) {
            match self.offers_lan(i) {
                // Through the network too: listen waits for Radio's answer
                // (connect_result), so a port Radio cannot take is an error here.
                Ok(true) => match self.net.listen_for(self.pid_of(i), super::MAX_APPS, local.1, listener.backlog() as u32) {
                    Ok(s) => {
                        crate::log!("net: {} (pid {}) listens on the network, port {}", self.owner_of(i), self.pid_of(i), local.1);
                        slot = Some(s)
                    }
                    Err(e) => return Done(e),
                },
                // Its manifest does not say `serve: lan`: loopback only.
                Ok(false) => {}
                Err(e) => return Done(e),
            }
        }
        if let Kind::Tcp { local: l, slot: s, lo, .. } = &mut f.borrow_mut().kind {
            *l = local;
            *s = slot;
            *lo = Some(Lo::Listen(listener));
        }
        if slot.is_some() {
            return Block(Wait::Connect { fd }, Some(super::sys::now_ns() + OFFER_NS));
        }
        Done(0)
    }

    fn accept(&mut self, i: usize, fd: i32, addr: u32, len: u32, flags: u32) -> Outcome {
        let r = self.accept_now(i, fd, addr, len, flags);
        if self.apps[i].is_none() {
            return Outcome::Gone; // the Shield ended it over the connection
        }
        let nonblock = self.socket_file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if r == EAGAIN && !nonblock {
            return Block(Wait::Accept { fd, addr, len, flags }, self.recv_deadline(i, fd));
        }
        Done(r)
    }

    /// One accept: a loopback connection waiting, else one Radio has; EAGAIN
    /// when there is none yet (a network one may then be on its way).
    pub fn accept_now(&mut self, i: usize, fd: i32, addr: u32, len: u32, flags: u32) -> i32 {
        let Some(f) = self.socket_file(i, fd) else { return EBADF };
        let pid = self.pid_of(i);
        let quarantined = self.quarantined(i);
        let mut file = f.borrow_mut();
        let Kind::Tcp { slot, local, v6, lo: Some(Lo::Listen(l)), .. } = &mut file.kind else { return EINVAL };
        let (local, v6, listener) = (*local, *v6, *slot);
        let mut host = None;
        let kind = if let Some((end, peer)) = l.take() {
            Some(tcp((if local.0 == 0 { LOOPBACK } else { local.0 }, local.1), v6, None, peer, Some(Lo::Conn(end))))
        } else if let Some(s) = l.accepting {
            match self.net.state(s) {
                (net::CONNECTING, _) => None,
                (net::OPEN | net::PEER_CLOSED, _) => {
                    l.accepting = None;
                    let peer = self.net.local(s);
                    host = Some((s, peer));
                    Some(tcp(local, v6, Some(s), peer, None))
                }
                (_, e) if e != 11 && e != 0 => {
                    // Radio refused the accept itself: say so, rather than
                    // reporting nothing yet while poll says readable.
                    l.accepting = None;
                    net::close(s);
                    return -e;
                }
                _ => {
                    // The connection went before it was taken (or another
                    // process sharing the listener took it): look again next time.
                    l.accepting = None;
                    net::close(s);
                    None
                }
            }
        } else if let Some(ls) = listener.filter(|&ls| self.net.waiting(ls) > 0) {
            if quarantined {
                return -13; // a quarantine also stops what comes in
            }
            // The listening slot's owner (its pid) takes it: Radio hands a
            // listener's connections only to the owner, and a forked child
            // sharing the listener accepts for it.
            match self.net.accept_for(net::owner(ls).unwrap_or(pid), super::MAX_APPS, ls) {
                Ok(s) => l.accepting = Some(s),
                Err(e) => return e,
            }
            None
        } else {
            None
        };
        drop(file);
        let Some(kind) = kind else { return EAGAIN };
        if let Some((s, (ip, port))) = host {
            // The peer passes the Shield as a connect to it would: a host the
            // app may not reach cannot reach the app by dialling in.
            // (Its address as it is: a peer's is never one of the resolver's stand-ins.)
            let h = net::literal(ip);
            let allowed = !quarantined && matches!(self.gate(i, &super::policy::Action::Connect { host: &h, port }), super::Gate::Allow);
            if !allowed || self.apps[i].is_none() {
                net::close(s);
                return EAGAIN;
            }
            self.track_connection(i, s, &h, port);
        }
        let peer = match &kind {
            Kind::Tcp { peer, .. } => *peer,
            _ => (0, 0),
        };
        let new = self.add_fd(i, OpenFile::new(kind, 2 | if flags & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 }));
        if new >= 0 && addr != 0 {
            self.put_sockaddr(i, addr, len, peer, v6);
        }
        new
    }

    /// A loopback connect: to the app listening on the port, at once. None
    /// when no app listens there and the emulator's relay may have the port
    /// on the Mac's loopback (tests serve there); elsewhere ECONNREFUSED.
    fn loop_connect(&mut self, i: usize, f: &super::FileRef, ip: u32, port: u16) -> Option<i32> {
        let mut listener = None;
        self.scan(&mut |j, g, k| match k {
            Some(Kind::Tcp { lo: Some(Lo::Listen(_)), local, .. }) if local.1 == port && (local.0 == 0 || local.0 == ip) => {
                listener = Some((j, g.clone()));
                true
            }
            _ => false,
        });
        let Some((j, g)) = listener else { return (self.net.online() != 3).then_some(ECONNREFUSED) };
        // Another app's listener: refused unless its manifest serves apps.
        if !self.reaches(i, j) {
            return Some(ECONNREFUSED);
        }
        // The port it was bound to, if it was.
        let bound = match f.borrow().kind {
            Kind::Tcp { local, .. } => local.1,
            _ => 0,
        };
        let Some(me) = (if bound != 0 { Some(bound) } else { self.ephemeral(false) }) else { return Some(EADDRNOTAVAIL) };
        let (mine, theirs) = match self.net.pipes.open(self.owner_of(i)) {
            Ok(p) => p,
            Err(e) => return Some(e),
        };
        if let Kind::Tcp { lo: Some(Lo::Listen(l)), .. } = &mut g.borrow_mut().kind {
            if let Err(e) = l.offer(theirs, (ip, me), super::sys::now_ns()) {
                return Some(e);
            }
        }
        if let Kind::Tcp { peer, local, lo, .. } = &mut f.borrow_mut().kind {
            *peer = (ip, port);
            *local = (ip, me);
            *lo = Some(Lo::Conn(mine));
        }
        Some(0)
    }

    fn connect(&mut self, i: usize, fd: i32, addr: u32, len: u32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let (ip, port) = match self.read_sockaddr(i, fd, addr, len) {
            Ok(a) => a,
            Err(EFAULT) => return Done(EFAULT),
            Err(ENETUNREACH) => return Done(ENETUNREACH),
            Err(EAFNOSUPPORT) => return Done(EAFNOSUPPORT),
            Err(_) => return Done(EINVAL),
        };
        // Linux takes 0.0.0.0 as this host.
        let ip = if ip == 0 { LOOPBACK } else { ip };
        let nonblock = f.borrow().flags & O_NONBLOCK != 0;
        let mut file = f.borrow_mut();
        match &mut file.kind {
            Kind::Udp { .. } => {
                drop(file);
                if let Some(o) = self.udp_gate(i, fd, (ip, port)) {
                    return o;
                }
                if let Kind::Udp { peer, .. } = &mut f.borrow_mut().kind {
                    *peer = Some((ip, port));
                }
                Done(0)
            }
            Kind::Tcp { lo: Some(Lo::Listen(_)), .. } => Done(EINVAL),
            Kind::Tcp { lo: Some(Lo::Conn(_)), .. } => Done(EISCONN),
            Kind::Tcp { slot: Some(slot), .. } => {
                let state = self.net.state(*slot);
                Done(match state.0 {
                    net::CONNECTING => EALREADY,
                    net::FAILED => -state.1.max(1),
                    _ => EISCONN,
                })
            }
            Kind::Tcp { .. } => {
                if ip == 0x7f00_0001 && port == 0 {
                    return Done(EINVAL);
                }
                // A stand-in address this app has no name for goes nowhere.
                let Some(host) = self.net.host_of(self.pid_of(i), ip) else { return Done(net::EHOSTUNREACH) };
                if matches!(host.as_str(), "sentinel.violet" | "shield-flows.violet")
                    || (matches!(host.as_str(), "agent-pair.violet" | "agent-relay.violet")
                        && (cfg!(moto_board) || self.apps[i].as_ref().map(|a| a.path.as_str()) != Some(super::appstore::INSTALLER)))
                {
                    return Done(-13);
                }
                drop(file);
                match self.gate(i, &super::policy::Action::Connect { host: &host, port }) {
                    super::Gate::Allow => {}
                    super::Gate::Deny => return Done(-13),
                    super::Gate::Frozen => return Block(Wait::Frozen, None),
                    super::Gate::Killed => return Outcome::Gone,
                }
                // 127.0.0.0/8 is this phone: never Radio. (In QEMU a port no
                // app listens on is the Mac's, as before.)
                if loopback::is_loopback(ip) {
                    if let Some(r) = self.loop_connect(i, &f, ip, port) {
                        return Done(r);
                    }
                }
                let mut file = f.borrow_mut();
                let Kind::Tcp { slot, peer, .. } = &mut file.kind else { return Done(EBADF) };
                match self.net.connect_for(self.pid_of(i), super::MAX_APPS, ip, port) {
                    Ok(s) => {
                        *slot = Some(s);
                        *peer = (ip, port);
                    }
                    Err(e) => return Done(e),
                }
                let s = slot.unwrap();
                drop(file);
                self.track_connection(i, s, &host, port);
                if nonblock {
                    Done(EINPROGRESS)
                } else {
                    Block(Wait::Connect { fd }, Some(super::sys::now_ns() + 30_000_000_000))
                }
            }
            _ => Done(EBADF),
        }
    }

    /// The result of a blocking connect, once it is known.
    /// Also of a listen or a UDP bind Radio was asked to take on the network:
    /// one it refused leaves the socket as it was before the call (a
    /// listener through the phone's relay, which cannot listen, stays a
    /// loopback listener).
    pub fn connect_result(&mut self, i: usize, fd: i32) -> Option<i32> {
        let f = self.socket_file(i, fd)?;
        let mut file = f.borrow_mut();
        let (slot, offered) = match &file.kind {
            Kind::Tcp { slot: Some(slot), lo, .. } => (*slot, lo.is_some()),
            Kind::Udp { slot: Some(slot), .. } => (*slot, true),
            _ => return Some(EBADF),
        };
        let r = match self.net.state(slot) {
            (net::CONNECTING, _) => return None,
            (net::FAILED, e) => -e.max(1),
            _ => return Some(0),
        };
        if offered {
            net::close(slot);
            match &mut file.kind {
                Kind::Tcp { slot: s, lo, .. } => {
                    *s = None;
                    if r == EOPNOTSUPP {
                        return Some(0);
                    }
                    *lo = None;
                }
                Kind::Udp { slot: s, local, .. } => {
                    *s = None;
                    *local = (0, 0);
                }
                _ => {}
            }
        }
        Some(r)
    }

    pub fn socket_send(&mut self, i: usize, fd: i32, buf: u32, len: u32, to: Option<(u32, u16)>) -> i32 {
        let r = self.socket_send_now(i, fd, buf, len, to);
        if r == EAGAIN {
            self.epoll_rearm(i, fd);
        }
        r
    }

    /// A datagram to a loopback port: into the bound socket's queue (none
    /// bound: dropped, as on a network; None in QEMU, where the port may be
    /// the Mac's). From the sender's port, bound now if it had none.
    fn loop_datagram(&mut self, i: usize, f: &super::FileRef, port: u16, data: &[u8]) -> Option<i32> {
        if data.len() > net::MAX_DATAGRAM {
            return Some(-90); // EMSGSIZE
        }
        let unbound = matches!(f.borrow().kind, Kind::Udp { local: (_, 0), .. });
        if unbound {
            let Some(p) = self.ephemeral(true) else { return Some(EADDRNOTAVAIL) };
            if let Kind::Udp { local, .. } = &mut f.borrow_mut().kind {
                *local = (0, p);
            }
        }
        let from = match f.borrow().kind {
            Kind::Udp { local, .. } => (LOOPBACK, local.1),
            _ => return Some(EBADF),
        };
        let mut queued = 0;
        let mut target = None;
        self.scan(&mut |j, g, k| {
            if let Some(Kind::Udp { replies, local, peer, .. }) = k {
                queued += replies.iter().filter(|r| loopback::is_loopback(r.0 .0)).count();
                if target.is_none() && loopback::takes_datagram(*local, *peer, port, from.1) {
                    target = Some((j, g.clone()));
                }
            }
            false
        });
        let Some((j, g)) = target else { return (self.net.online() != 3).then_some(data.len() as i32) };
        // Another app's socket: dropped, as by a firewall, unless its manifest serves apps.
        if !self.reaches(i, j) {
            return Some(data.len() as i32);
        }
        if let Kind::Udp { replies, .. } = &mut g.borrow_mut().kind {
            // A full queue drops the datagram, as a full socket buffer would.
            if queued < LOOP_DATAGRAMS && replies.len() < MAX_REPLIES && replies.try_reserve(1).is_ok() {
                replies.push_back((from, data.to_vec()));
            }
        }
        Some(data.len() as i32)
    }

    fn socket_send_now(&mut self, i: usize, fd: i32, buf: u32, len: u32, to: Option<(u32, u16)>) -> i32 {
        let Some(f) = self.socket_file(i, fd) else { return EBADF };
        let data = {
            let app = self.apps[i].as_mut().unwrap();
            let mut v = alloc::vec![0u8; len.min(64 << 10) as usize];
            if !app.space.read(&mut self.k, &mut self.pool, buf, &mut v) {
                return EFAULT;
            }
            v
        };
        let pid = self.pid_of(i);
        // A question for the runtime's resolver. No file is borrowed while it
        // is answered: the resolver looks at every socket's addresses.
        let question = match &f.borrow().kind {
            Kind::Udp { peer, .. } => to.or(*peer).filter(|&(ip, port)| port == 53 && (ip == NAMESERVER || self.net.is_stand_in(ip) || !self.net.present)),
            _ => None,
        };
        if let Some(from) = question {
            let apps = &self.apps;
            let answer = names::answer(&mut self.net.names, pid, &data, &mut |ip| address_live(apps, ip));
            if let (Some(answer), Kind::Udp { replies, .. }) = (answer, &mut f.borrow_mut().kind) {
                // A program that asks and never reads loses the oldest answers.
                if replies.len() >= MAX_REPLIES {
                    replies.pop_front();
                }
                replies.push_back((from, answer));
            }
            return data.len() as i32;
        }
        let local_dest = match &f.borrow().kind {
            Kind::Udp { peer, allowed, local, .. } => match to.or(*peer) {
                Some(d) if !allowed.contains(&d) => return -13, // EACCES: send_call asks the Shield first
                Some((_, 0)) => return EINVAL, // as Linux: port 0 is no destination
                Some((ip, port)) if loopback::is_loopback(ip) || ip == 0 => Some(port),
                // Bound to a loopback address: the network is not reachable from it.
                Some(_) if loopback::is_loopback(local.0) => return ENETUNREACH,
                _ => None,
            },
            _ => None,
        };
        if let Some(r) = local_dest.and_then(|port| self.loop_datagram(i, &f, port, &data)) {
            return r;
        }
        let mut file = f.borrow_mut();
        match &mut file.kind {
            Kind::Udp { peer, slot, allowed, local, .. } => {
                let dest = to.or(*peer);
                match dest {
                    Some(d) if !allowed.contains(&d) => -13, // EACCES: send_call asks the Shield first
                    Some((ip, port)) => {
                        if data.len() > net::MAX_DATAGRAM {
                            return -90; // EMSGSIZE
                        }
                        let s = match *slot {
                            Some(s) => s,
                            None => match self.net.udp_bind_for(pid, super::MAX_APPS, local.1) {
                                Ok(s) => {
                                    *slot = Some(s);
                                    s
                                }
                                Err(e) => return e,
                            },
                        };
                        match self.net.state(s) {
                            (net::FAILED, e) => return -e.max(1),
                            (net::IDLE, _) | (net::PEER_CLOSED, _) => return ENETUNREACH,
                            _ => {}
                        }
                        // A full ring drops the datagram, as a full interface queue would.
                        match self.net.send_datagram(s, pid, ip, port, &data) {
                            Ok(_) => data.len() as i32,
                            Err(e) => e,
                        }
                    }
                    None => -89, // EDESTADDRREQ
                }
            }
            Kind::Tcp { lo: Some(Lo::Conn(end)), .. } => match end.write(&data, super::sys::now_ns()) {
                Ok(n) => n as i32,
                Err(e) => e,
            },
            Kind::Tcp { lo: Some(Lo::Listen(_)), .. } => ENOTCONN,
            Kind::Tcp { slot: Some(slot), .. } => {
                let slot = *slot;
                match self.net.state(slot) {
                    (net::OPEN, _) => {}
                    (net::CONNECTING, _) => return EAGAIN,
                    (net::FAILED, e) => return -e.max(1),
                    _ => return -32, // EPIPE
                }
                let n = self.net.send(slot, &data);
                drop(file);
                self.count_sent(slot, n);
                if n == 0 { EAGAIN } else { n as i32 }
            }
            Kind::Tcp { slot: None, .. } => ENOTCONN,
            _ => EBADF,
        }
    }

    /// The Shield's say on a UDP destination the first time a socket uses it.
    /// None: allowed (and remembered); otherwise the outcome to return.
    fn udp_gate(&mut self, i: usize, fd: i32, dest: (u32, u16)) -> Option<Outcome> {
        let f = self.socket_file(i, fd)?;
        let known = match &f.borrow().kind {
            Kind::Udp { allowed, .. } => allowed.contains(&dest),
            _ => return None,
        };
        if known || dest.1 == 53 {
            return None;
        }
        let Some(host) = self.net.host_of(self.pid_of(i), dest.0) else { return Some(Done(net::EHOSTUNREACH)) };
        match self.gate_with_flags(i, &super::policy::Action::Connect { host: &host, port: dest.1 }, super::shield::FLAG_UDP) {
            super::Gate::Allow => {}
            super::Gate::Deny => return Some(Done(-13)),
            super::Gate::Frozen => return Some(Block(Wait::Frozen, None)),
            super::Gate::Killed => return Some(Outcome::Gone),
        }
        if let Kind::Udp { allowed, .. } = &mut f.borrow_mut().kind {
            if allowed.len() >= 64 {
                allowed.remove(0);
            }
            allowed.push(dest);
        }
        None
    }

    fn send_call(&mut self, i: usize, fd: i32, buf: u32, len: u32, flags: u32, to: Option<(u32, u16)>) -> Outcome {
        // A quarantine cuts send and sendto as it cuts write (syscall.rs).
        if self.quarantined(i) {
            return Done(-13);
        }
        if let Some(d) = to {
            if let Some(o) = self.udp_gate(i, fd, d) {
                return o;
            }
        }
        let r = self.socket_send(i, fd, buf, len, to);
        let nonblock = flags & MSG_DONTWAIT != 0
            || self.socket_file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if r == EAGAIN && !nonblock {
            return Block(Wait::Send { fd, buf, len }, None);
        }
        Done(r)
    }

    /// recv, recvfrom and recvmsg: `name` and `name_len` (pointers) take the
    /// source address when given.
    fn recv_call(&mut self, i: usize, fd: i32, buf: u32, len: u32, flags: u32, name: u32, name_len: u32) -> Outcome {
        if !self.is_socket(i, fd) {
            return Done(EBADF);
        }
        let r = self.recv_from(i, fd, buf, len, name, name_len);
        let nonblock = flags & MSG_DONTWAIT != 0
            || self.socket_file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if r == EAGAIN && !nonblock {
            return Block(Wait::RecvFrom { fd, buf, len, name, name_len }, self.recv_deadline(i, fd));
        }
        Done(r)
    }

    /// One read, and the sender's address to `name` when something came.
    pub fn recv_from(&mut self, i: usize, fd: i32, buf: u32, len: u32, name: u32, name_len: u32) -> i32 {
        self.net.last_from = (0, 0);
        let r = self.read_now(i, fd, buf, len);
        if r >= 0 && name != 0 {
            let addr = match self.net.last_from {
                (0, 0) => self.peer_of(i, fd).unwrap_or((NAMESERVER, 53)),
                a => a,
            };
            let v6 = self.is_v6(i, fd);
            self.put_sockaddr(i, name, name_len, addr, v6);
        }
        r
    }

    /// sendmsg and recvmsg: the address and the first buffer of the vector.
    fn message_call(&mut self, i: usize, fd: i32, msg: u32, flags: u32, send: bool) -> Outcome {
        // struct msghdr and struct iovec in the app's ABI (../arch/compat.rs):
        // an AArch64 app's are 64-bit, its addresses below 4 GiB.
        use super::arch::compat;
        let abi = super::arch::NATIVE;
        let mut hb = alloc::vec![0u8; compat::msghdr_bytes(abi)];
        let Some(h) = self.app_bytes(i, msg, &mut hb).then(|| compat::msghdr(abi, &hb)).flatten() else { return Done(EFAULT) };
        let (Ok(name), Ok(iov)) = (u32::try_from(h.name), u32::try_from(h.iov)) else { return Done(EFAULT) };
        let namelen = h.namelen;
        if h.iovlen == 0 {
            return Done(0);
        }
        let mut vb = alloc::vec![0u8; compat::iovec_bytes(abi)];
        let Some((base, len)) = self.app_bytes(i, iov, &mut vb).then(|| compat::iovec(abi, &vb, 0)).flatten() else { return Done(EFAULT) };
        let (Ok(base), len) = (u32::try_from(base), len.min(u64::from(u32::MAX)) as u32) else { return Done(EFAULT) };
        let v = [base, len];
        let w = abi.word_bytes() as u32;
        if send {
            let to = if name != 0 {
                match self.read_sockaddr(i, fd, name, namelen) {
                    Ok(a) => Some(a),
                    Err(e) => return Done(e),
                }
            } else {
                None
            };
            self.send_call(i, fd, v[0], v[1], flags, to)
        } else {
            // msg_namelen follows msg_name (a word); msg_controllen and
            // msg_flags are words 5 and 6 (AArch32), or at 40 and 48 (AArch64).
            let r = self.recv_call(i, fd, v[0], v[1], flags, name, if name != 0 { msg + w } else { 0 });
            if let Done(k) = r {
                if k >= 0 {
                    // No control data; flags cleared.
                    let (controllen, msg_flags) = if w == 8 { (40, 48) } else { (20, 24) };
                    let app = self.apps[i].as_mut().unwrap();
                    app.space.write(&mut self.k, &mut self.pool, msg + controllen, &[0u8; 8][..w as usize]);
                    app.space.write_u32(&mut self.k, &mut self.pool, msg + msg_flags, 0);
                }
            }
            r
        }
    }

    fn app_bytes(&mut self, i: usize, at: u32, out: &mut [u8]) -> bool {
        let Some(app) = self.apps[i].as_mut() else { return false };
        app.space.read(&mut self.k, &mut self.pool, at, out)
    }

    fn getsockopt(&mut self, i: usize, fd: i32, level: u32, name: u32, val: u32, len_ptr: u32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let value = match (level, name) {
            // SO_ERROR: the outcome of a non-blocking connect.
            (1, 4) => match f.borrow().kind {
                Kind::Tcp { slot: Some(slot), lo: None, .. } => match self.net.state(slot) {
                    (net::FAILED, e) => e.max(1) as u32,
                    _ => 0,
                },
                _ => 0,
            },
            (1, 3) => match f.borrow().kind {
                Kind::Tcp { .. } => SOCK_STREAM,
                _ => SOCK_DGRAM,
            },
            (1, 7) | (1, 8) => 65536, // SO_SNDBUF, SO_RCVBUF
            // SO_ACCEPTCONN
            (1, 30) => matches!(f.borrow().kind, Kind::Tcp { lo: Some(Lo::Listen(_)), .. }) as u32,
            _ => 0,
        };
        let app = self.apps[i].as_mut().unwrap();
        app.space.write_u32(&mut self.k, &mut self.pool, val, value);
        if len_ptr != 0 {
            app.space.write_u32(&mut self.k, &mut self.pool, len_ptr, 4);
        }
        Done(0)
    }

    /// A UDP socket's address on the network, opening its slot if need be
    /// (port 0 until Radio has bound it).
    fn udp_local(&mut self, i: usize, fd: i32) -> Option<(u32, u16)> {
        let f = self.socket_file(i, fd)?;
        let mut file = f.borrow_mut();
        let Kind::Udp { slot, .. } = &mut file.kind else { return None };
        if slot.is_none() {
            let pid = self.pid_of(i);
            *slot = self.net.udp_open_for(pid, super::MAX_APPS).ok();
        }
        let (ip, port) = self.net.local((*slot)?);
        Some((if ip == 0 { 0x0a00_0002 } else { ip }, port))
    }

    fn socket_name(&mut self, i: usize, fd: i32, ptr: u32, len_ptr: u32, peer: bool) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let (bound, v6) = match &f.borrow().kind {
            Kind::Tcp { local, v6, .. } | Kind::Udp { local, v6, .. } => (*local, *v6),
            _ => ((0, 0), false),
        };
        let addr = if peer {
            match self.peer_of(i, fd) {
                Some(p) if p.0 != 0 => p,
                _ => return Done(ENOTCONN),
            }
        } else if bound.1 != 0 {
            bound
        } else {
            match self.udp_local(i, fd) {
                Some(a) => a,
                None => (0x0a00_0002, 40000 + fd as u16),
            }
        };
        self.put_sockaddr(i, ptr, len_ptr, addr, v6);
        Done(0)
    }
}
