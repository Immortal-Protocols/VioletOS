//! Socket system calls: IPv4 TCP through Radio (net.rs), and UDP only for the
//! DNS questions the Open Runtime answers itself.
use super::device::{Kind, OpenFile};
use super::net;
use super::syscall::{Outcome, EAGAIN, EBADF, EFAULT, EINVAL, ENOSYS};
use super::{Runtime, Wait};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use Outcome::*;

const AF_INET: u32 = 2;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_NONBLOCK: u32 = 0o4000;
const O_NONBLOCK: u32 = 0o4000;
const MSG_DONTWAIT: u32 = 0x40;
const EAFNOSUPPORT: i32 = -97;
const EPROTONOSUPPORT: i32 = -93;
const EOPNOTSUPP: i32 = -95;
const ENOTCONN: i32 = -107;
const EISCONN: i32 = -106;
const EINPROGRESS: i32 = -115;
const EALREADY: i32 = -114;
const ENETUNREACH: i32 = -101;
/// The resolver programs are told to ask (/etc/resolv.conf).
pub const NAMESERVER: u32 = 0x0a00_0035; // 10.0.0.53

fn sockaddr(ip: u32, port: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&ip.to_be_bytes());
    b
}

impl Runtime {
    pub fn socket_call(&mut self, i: usize, n: u32, a: [u32; 7]) -> Outcome {
        match n {
            281 => Done(self.socket(i, a[0], a[1])),
            282 => Done(if self.is_socket(i, a[0] as i32) { 0 } else { EBADF }),
            283 => self.connect(i, a[0] as i32, a[1], a[2]),
            284 | 285 | 366 => Done(EOPNOTSUPP),
            286 | 287 => self.socket_name(i, a[0] as i32, a[1], a[2], n == 287),
            289 | 290 => {
                let to = if n == 290 && a[4] != 0 { self.read_sockaddr(i, a[4], a[5]) } else { None };
                self.send_call(i, a[0] as i32, a[1], a[2], a[3], to)
            }
            291 | 292 => {
                let r = self.recv_call(i, a[0] as i32, a[1], a[2], a[3]);
                if n == 292 && a[4] != 0 {
                    if let Done(k) = r {
                        if k >= 0 {
                            let addr = self.peer_of(i, a[0] as i32).unwrap_or((NAMESERVER, 53));
                            self.put_sockaddr(i, a[4], a[5], addr);
                        }
                    }
                }
                r
            }
            293 => {
                let Some(f) = self.socket_file(i, a[0] as i32) else { return Done(EBADF) };
                if let Kind::Tcp { slot: Some(slot), .. } = f.borrow().kind {
                    if a[1] >= 1 {
                        self.net.shutdown_send(slot);
                    }
                }
                Done(0)
            }
            294 => Done(if self.is_socket(i, a[0] as i32) { 0 } else { EBADF }),
            295 => self.getsockopt(i, a[0] as i32, a[1], a[2], a[3], a[4]),
            296 | 297 => self.message_call(i, a[0] as i32, a[1], a[2], n == 296),
            288 => Done(EOPNOTSUPP),
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
    fn peer_of(&self, i: usize, fd: i32) -> Option<(u32, u16)> {
        let f = self.socket_file(i, fd)?;
        let kind = &f.borrow().kind;
        match kind {
            Kind::Tcp { peer, .. } => Some(*peer),
            Kind::Udp { peer, .. } => *peer,
            _ => None,
        }
    }
    fn read_sockaddr(&mut self, i: usize, ptr: u32, len: u32) -> Option<(u32, u16)> {
        if len < 8 {
            return None;
        }
        let app = self.apps[i].as_mut()?;
        let mut b = [0u8; 8];
        if !app.space.read(&mut self.k, &mut self.pool, ptr, &mut b) {
            return None;
        }
        if u16::from_le_bytes([b[0], b[1]]) as u32 != AF_INET {
            return None;
        }
        Some((u32::from_be_bytes([b[4], b[5], b[6], b[7]]), u16::from_be_bytes([b[2], b[3]])))
    }
    fn put_sockaddr(&mut self, i: usize, ptr: u32, len_ptr: u32, addr: (u32, u16)) {
        let app = self.apps[i].as_mut().unwrap();
        let room = if len_ptr != 0 { app.space.read_u32(&mut self.k, &mut self.pool, len_ptr).unwrap_or(0) } else { 16 };
        let b = sockaddr(addr.0, addr.1);
        app.space.write(&mut self.k, &mut self.pool, ptr, &b[..(room as usize).min(16)]);
        if len_ptr != 0 {
            app.space.write_u32(&mut self.k, &mut self.pool, len_ptr, 16);
        }
    }

    fn socket(&mut self, i: usize, domain: u32, kind: u32) -> i32 {
        if domain != AF_INET {
            return EAFNOSUPPORT;
        }
        let flags = 2 | if kind & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
        let file = match kind & 0xf {
            SOCK_STREAM => OpenFile::new(Kind::Tcp { slot: None, peer: (0, 0) }, flags),
            SOCK_DGRAM => OpenFile::new(Kind::Udp { peer: None, replies: VecDeque::new() }, flags),
            _ => return EPROTONOSUPPORT,
        };
        let app = self.apps[i].as_mut().unwrap();
        let pos = app.fds.iter().position(|f| f.is_none()).unwrap_or(app.fds.len());
        if pos == app.fds.len() {
            app.fds.push(None);
        }
        app.fds[pos] = Some(alloc::rc::Rc::new(core::cell::RefCell::new(file)));
        pos as i32
    }

    fn connect(&mut self, i: usize, fd: i32, addr: u32, len: u32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let Some((ip, port)) = self.read_sockaddr(i, addr, len) else { return Done(EINVAL) };
        let nonblock = f.borrow().flags & O_NONBLOCK != 0;
        let mut file = f.borrow_mut();
        match &mut file.kind {
            Kind::Udp { peer, .. } => {
                *peer = Some((ip, port));
                Done(0)
            }
            Kind::Tcp { slot: Some(slot), .. } => {
                let state = self.net.state(*slot);
                Done(match state.0 {
                    net::CONNECTING => EALREADY,
                    net::FAILED => -state.1.max(1),
                    _ => EISCONN,
                })
            }
            Kind::Tcp { slot, peer } => {
                if ip == 0x7f00_0001 && port == 0 {
                    return Done(EINVAL);
                }
                match self.net.connect(ip, port) {
                    Ok(s) => {
                        *slot = Some(s);
                        *peer = (ip, port);
                    }
                    Err(e) => return Done(e),
                }
                drop(file);
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
    pub fn connect_result(&mut self, i: usize, fd: i32) -> Option<i32> {
        let f = self.socket_file(i, fd)?;
        let Kind::Tcp { slot: Some(slot), .. } = f.borrow().kind else { return Some(EBADF) };
        match self.net.state(slot) {
            (net::CONNECTING, _) => None,
            (net::FAILED, e) => Some(-e.max(1)),
            _ => Some(0),
        }
    }

    pub fn socket_send(&mut self, i: usize, fd: i32, buf: u32, len: u32, to: Option<(u32, u16)>) -> i32 {
        let Some(f) = self.socket_file(i, fd) else { return EBADF };
        let data = {
            let app = self.apps[i].as_mut().unwrap();
            let mut v = alloc::vec![0u8; len.min(64 << 10) as usize];
            if !app.space.read(&mut self.k, &mut self.pool, buf, &mut v) {
                return EFAULT;
            }
            v
        };
        let mut file = f.borrow_mut();
        match &mut file.kind {
            Kind::Udp { peer, replies } => {
                let dest = to.or(*peer);
                match dest {
                    Some((_, 53)) => {
                        if let Some(answer) = net::answer_dns(&mut self.net, &data) {
                            replies.push_back(answer);
                        }
                        data.len() as i32
                    }
                    Some(_) => ENETUNREACH,
                    None => -89, // EDESTADDRREQ
                }
            }
            Kind::Tcp { slot: Some(slot), .. } => {
                let slot = *slot;
                match self.net.state(slot) {
                    (net::OPEN, _) => {}
                    (net::CONNECTING, _) => return EAGAIN,
                    (net::FAILED, e) => return -e.max(1),
                    _ => return -32, // EPIPE
                }
                let n = self.net.send(slot, &data);
                if n == 0 { EAGAIN } else { n as i32 }
            }
            Kind::Tcp { slot: None, .. } => ENOTCONN,
            _ => EBADF,
        }
    }

    fn send_call(&mut self, i: usize, fd: i32, buf: u32, len: u32, flags: u32, to: Option<(u32, u16)>) -> Outcome {
        let r = self.socket_send(i, fd, buf, len, to);
        let nonblock = flags & MSG_DONTWAIT != 0
            || self.socket_file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if r == EAGAIN && !nonblock {
            return Block(Wait::Send { fd, buf, len }, None);
        }
        Done(r)
    }

    fn recv_call(&mut self, i: usize, fd: i32, buf: u32, len: u32, flags: u32) -> Outcome {
        if !self.is_socket(i, fd) {
            return Done(EBADF);
        }
        let r = self.read_now(i, fd, buf, len);
        let nonblock = flags & MSG_DONTWAIT != 0
            || self.socket_file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if r == EAGAIN && !nonblock {
            return Block(Wait::Read { fd, buf, len }, None);
        }
        Done(r)
    }

    /// sendmsg and recvmsg: the address and the first buffer of the vector.
    fn message_call(&mut self, i: usize, fd: i32, msg: u32, flags: u32, send: bool) -> Outcome {
        let Some(h) = self.rd_words(i, msg, 7) else { return Done(EFAULT) };
        let (name, namelen, iov, iovlen) = (h[0], h[1], h[2], h[3]);
        if iovlen == 0 {
            return Done(0);
        }
        let Some(v) = self.rd_words(i, iov, 2) else { return Done(EFAULT) };
        if send {
            let to = if name != 0 { self.read_sockaddr(i, name, namelen) } else { None };
            self.send_call(i, fd, v[0], v[1], flags, to)
        } else {
            let r = self.recv_call(i, fd, v[0], v[1], flags);
            if let Done(k) = r {
                if k >= 0 && name != 0 {
                    let addr = self.peer_of(i, fd).unwrap_or((NAMESERVER, 53));
                    let app = self.apps[i].as_mut().unwrap();
                    app.space.write(&mut self.k, &mut self.pool, name, &sockaddr(addr.0, addr.1));
                    app.space.write_u32(&mut self.k, &mut self.pool, msg + 4, 16);
                }
                if k >= 0 {
                    // No control data; flags cleared.
                    let app = self.apps[i].as_mut().unwrap();
                    app.space.write_u32(&mut self.k, &mut self.pool, msg + 20, 0);
                    app.space.write_u32(&mut self.k, &mut self.pool, msg + 24, 0);
                }
            }
            r
        }
    }
    fn rd_words(&mut self, i: usize, at: u32, n: usize) -> Option<Vec<u32>> {
        let app = self.apps[i].as_mut()?;
        (0..n).map(|k| app.space.read_u32(&mut self.k, &mut self.pool, at + 4 * k as u32)).collect()
    }

    fn getsockopt(&mut self, i: usize, fd: i32, level: u32, name: u32, val: u32, len_ptr: u32) -> Outcome {
        let Some(f) = self.socket_file(i, fd) else { return Done(EBADF) };
        let value = match (level, name) {
            // SO_ERROR: the outcome of a non-blocking connect.
            (1, 4) => match f.borrow().kind {
                Kind::Tcp { slot: Some(slot), .. } => match self.net.state(slot) {
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
            _ => 0,
        };
        let app = self.apps[i].as_mut().unwrap();
        app.space.write_u32(&mut self.k, &mut self.pool, val, value);
        if len_ptr != 0 {
            app.space.write_u32(&mut self.k, &mut self.pool, len_ptr, 4);
        }
        Done(0)
    }

    fn socket_name(&mut self, i: usize, fd: i32, ptr: u32, len_ptr: u32, peer: bool) -> Outcome {
        if !self.is_socket(i, fd) {
            return Done(EBADF);
        }
        let addr = if peer {
            match self.peer_of(i, fd) {
                Some(p) if p.0 != 0 => p,
                _ => return Done(ENOTCONN),
            }
        } else {
            (0x0a00_0002, 40000 + fd as u16)
        };
        self.put_sockaddr(i, ptr, len_ptr, addr);
        Done(0)
    }
}
