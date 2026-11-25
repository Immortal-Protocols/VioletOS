//! The connection to the relay: a WebSocket client (ws:// or wss:// with
//! rustls) on its own thread that reconnects by itself, with waits growing
//! from 1 to 30 seconds, until the app drops its sender.
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

pub enum Net {
    /// Connected to the relay (not yet to the peer).
    Connected,
    PeerUp,
    PeerDown,
    Message(Vec<u8>),
    /// Lost the relay; the text says why.
    Lost(String),
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}
impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            Stream::Tls(s) => s.read(b),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

fn tls() -> Arc<rustls::ClientConfig> {
    static C: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    C.get_or_init(|| {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(c)
    })
    .clone()
}

/// "https://host[:port][/base]" or "http://…" (ws/wss are accepted too).
fn split(url: &str) -> Option<(bool, String, u16, String)> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://").or_else(|| url.strip_prefix("wss://")) {
        (true, r)
    } else {
        (false, url.strip_prefix("http://").or_else(|| url.strip_prefix("ws://"))?)
    };
    let (authority, base) = rest.split_once('/').map_or((rest, ""), |(a, b)| (a, b));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (authority.to_string(), if secure { 443 } else { 80 }),
    };
    let base = base.trim_end_matches('/');
    Some((secure, host, port, if base.is_empty() { String::new() } else { format!("/{base}") }))
}

fn connect(url: &str, path: &str) -> io::Result<Stream> {
    let bad = || io::Error::new(io::ErrorKind::InvalidInput, "bad relay address");
    let (secure, host, port, base) = split(url).ok_or_else(bad)?;
    let sock = TcpStream::connect((host.as_str(), port))?;
    let _ = sock.set_nodelay(true);
    sock.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut s = if secure {
        let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| bad())?;
        let conn = rustls::ClientConnection::new(tls(), name).map_err(io::Error::other)?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, sock)))
    } else {
        Stream::Plain(sock)
    };
    let key = crate::secure::b64url(&crate::secure::random::<16>()).replace('-', "+").replace('_', "/");
    let req = format!(
        "GET {base}{path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}==\r\nSec-WebSocket-Version: 13\r\nUser-Agent: VioletOS-Agent/1\r\n\r\n"
    );
    s.write_all(req.as_bytes())?;
    s.flush()?;
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b)? == 0 || head.len() > 8192 {
            return Err(io::Error::other("relay closed during upgrade"));
        }
        head.push(b[0]);
    }
    let status = String::from_utf8_lossy(&head);
    if !status.starts_with("HTTP/1.1 101") {
        return Err(io::Error::other(format!("relay refused: {}", status.lines().next().unwrap_or(""))));
    }
    Ok(s)
}

fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = crate::secure::random::<4>();
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n < 65536 => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
    f
}

/// Incoming bytes cut into WebSocket messages.
#[derive(Default)]
struct Frames {
    buf: Vec<u8>,
    partial: Vec<u8>,
    kind: u8,
}
impl Frames {
    fn next(&mut self) -> Option<(u8, Vec<u8>)> {
        loop {
            let b = &self.buf;
            if b.len() < 2 {
                return None;
            }
            let (fin, opcode, masked) = (b[0] & 0x80 != 0, b[0] & 15, b[1] & 0x80 != 0);
            let (mut at, mut n) = (2usize, (b[1] & 127) as usize);
            if n == 126 {
                if b.len() < 4 {
                    return None;
                }
                n = u16::from_be_bytes([b[2], b[3]]) as usize;
                at = 4;
            } else if n == 127 {
                if b.len() < 10 {
                    return None;
                }
                n = u64::from_be_bytes(b[2..10].try_into().unwrap()) as usize;
                at = 10;
            }
            let mask = if masked { at += 4; Some([b[at - 4], b[at - 3], b[at - 2], b[at - 1]]) } else { None };
            if b.len() < at + n {
                return None;
            }
            let mut data: Vec<u8> = self.buf[at..at + n].to_vec();
            if let Some(m) = mask {
                data.iter_mut().enumerate().for_each(|(i, v)| *v ^= m[i & 3]);
            }
            self.buf.drain(..at + n);
            if opcode >= 8 {
                return Some((opcode, data));
            }
            if opcode != 0 {
                self.kind = opcode;
            }
            self.partial.extend_from_slice(&data);
            if fin {
                return Some((self.kind, std::mem::take(&mut self.partial)));
            }
        }
    }
}

/// One connection, until it fails or the app hangs up (Ok).
fn session(url: &str, path: &str, events: &Sender<Net>, outgoing: &Receiver<Vec<u8>>) -> io::Result<()> {
    let mut s = connect(url, path)?;
    set_timeout(&s, Duration::from_millis(10));
    if events.send(Net::Connected).is_err() {
        return Ok(());
    }
    let mut frames = Frames::default();
    let mut buf = vec![0u8; 32 << 10];
    let (mut pinged, mut heard) = (Instant::now(), Instant::now());
    loop {
        match s.read(&mut buf) {
            Ok(0) => return Err(io::Error::other("relay closed the connection")),
            Ok(n) => {
                heard = Instant::now();
                frames.buf.extend_from_slice(&buf[..n]);
                while let Some((op, data)) = frames.next() {
                    let e = match op {
                        1 if data.windows(4).any(|w| w == b"\"up\"") => Net::PeerUp,
                        1 if data.windows(6).any(|w| w == b"\"down\"") => Net::PeerDown,
                        2 => Net::Message(data),
                        8 => return Err(io::Error::other("relay closed the connection")),
                        9 => {
                            s.write_all(&frame(10, &data))?;
                            continue;
                        }
                        _ => continue,
                    };
                    if events.send(e).is_err() {
                        return Ok(());
                    }
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
        let mut wrote = false;
        loop {
            match outgoing.try_recv() {
                Ok(m) => {
                    s.write_all(&frame(2, &m))?;
                    wrote = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = s.write_all(&frame(8, &[]));
                    return Ok(());
                }
            }
        }
        if pinged.elapsed() > Duration::from_secs(20) {
            s.write_all(&frame(9, b"violet"))?;
            pinged = Instant::now();
            wrote = true;
        }
        if wrote {
            s.flush()?;
        }
        if heard.elapsed() > Duration::from_secs(60) {
            return Err(io::Error::other("relay went quiet"));
        }
    }
}

fn set_timeout(s: &Stream, d: Duration) {
    let t = match s {
        Stream::Plain(t) => t,
        Stream::Tls(t) => t.get_ref(),
    };
    let _ = t.set_read_timeout(Some(d));
}

/// Keep a connection to `url` for `room` as `role` until the returned sender is dropped.
pub fn spawn(url: String, room: [u8; 16], role: &'static str, events: Sender<Net>) -> Sender<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let path = format!("/v1/pipe/{}?role={role}", crate::secure::hex(&room));
        let mut wait = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            match session(&url, &path, &events, &rx) {
                Ok(()) => return,
                Err(e) => {
                    if events.send(Net::Lost(e.to_string())).is_err() {
                        return;
                    }
                }
            }
            if started.elapsed() > Duration::from_secs(30) {
                wait = Duration::from_secs(1);
            }
            // Wait, but notice the app hanging up.
            let until = Instant::now() + wait;
            while Instant::now() < until {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(_) => {} // nothing to send to while offline
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => return,
                }
            }
            wait = (wait * 2).min(Duration::from_secs(30));
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls() {
        assert_eq!(split("https://a.example.com").unwrap(), (true, "a.example.com".into(), 443, "".into()));
        assert_eq!(split("http://192.168.1.4:8787/").unwrap(), (false, "192.168.1.4".into(), 8787, "".into()));
        assert_eq!(split("ws://h:1/base/").unwrap(), (false, "h".into(), 1, "/base".into()));
    }
    #[test]
    fn frames_parse() {
        let mut f = Frames::default();
        f.buf.extend_from_slice(&[0x82, 3, 1, 2, 3, 0x81, 2]);
        assert_eq!(f.next().unwrap(), (2, vec![1, 2, 3]));
        assert!(f.next().is_none());
        f.buf.extend_from_slice(b"hi");
        assert_eq!(f.next().unwrap(), (1, b"hi".to_vec()));
    }
}
