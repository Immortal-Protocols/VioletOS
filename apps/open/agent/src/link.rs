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

/// The socket under TLS, written with plain write(): rustls writes records
/// with writev, which Open Runtimes before November 2026 answered with EAGAIN
/// on a blocking socket whose send ring was full.
struct Sock(TcpStream);
impl Read for Sock {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}
impl Write for Sock {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Sock>>),
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

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "bad relay address")
}

/// A TCP (and, for https, TLS) connection to the relay.
fn open_stream(url: &str, timeout: Duration) -> io::Result<Stream> {
    let (secure, host, port, _) = split(url).ok_or_else(bad)?;
    let sock = TcpStream::connect((host.as_str(), port))?;
    let _ = sock.set_nodelay(true);
    sock.set_read_timeout(Some(timeout))?;
    Ok(if secure {
        let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| bad())?;
        let conn = rustls::ClientConnection::new(tls(), name).map_err(io::Error::other)?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, Sock(sock))))
    } else {
        Stream::Plain(sock)
    })
}

/// A relay that refused the WebSocket upgrade (a proxy in front of it).
const REFUSED: &str = "relay refused";

fn connect(url: &str, path: &str) -> io::Result<Stream> {
    let (_, host, _, base) = split(url).ok_or_else(bad)?;
    let mut s = open_stream(url, Duration::from_secs(15))?;
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
        return Err(io::Error::other(format!("{REFUSED}: {}", status.lines().next().unwrap_or(""))));
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
        Stream::Tls(t) => &t.get_ref().0,
    };
    let _ = t.set_read_timeout(Some(d));
}

/// One HTTP request on a kept-alive connection (reopened when the server closes it).
fn http(conn: &mut Option<Stream>, url: &str, timeout: Duration, method: &str, path: &str, body: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let started = Instant::now();
    let fresh = conn.is_none();
    http_once(conn, url, timeout, method, path, body)
        .map_err(|e| io::Error::new(e.kind(), format!("{method} after {} ms ({}): {e}", started.elapsed().as_millis(), if fresh { "new" } else { "kept" })))
}

fn http_once(conn: &mut Option<Stream>, url: &str, timeout: Duration, method: &str, path: &str, body: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let (_, host, _, base) = split(url).ok_or_else(bad)?;
    let fresh = conn.is_none();
    let t0 = Instant::now();
    if fresh {
        let mut st = open_stream(url, timeout).map_err(|e| io::Error::new(e.kind(), format!("connect: {e}")))?;
        let t1 = t0.elapsed().as_millis();
        if let Stream::Tls(t) = &mut st {
            while t.conn.is_handshaking() {
                t.conn.complete_io(&mut t.sock).map_err(|e| io::Error::new(e.kind(), format!("TLS after connect {t1} ms, {} ms: {e}", t0.elapsed().as_millis())))?;
            }
        }
        *conn = Some(st);
    }
    let s = conn.as_mut().unwrap();
    let head = format!(
        "{method} {base}{path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nUser-Agent: VioletOS-Agent/1\r\n\r\n",
        body.len()
    );
    let sent = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body)).and_then(|_| s.flush());
    if let Err(e) = sent {
        *conn = None;
        // A kept-alive connection the server dropped: try once more on a new one.
        return if fresh { Err(e) } else { http_once(conn, url, timeout, method, path, body) };
    }
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut b) {
            Ok(1) => head.push(b[0]),
            Ok(_) if head.is_empty() && !fresh => {
                *conn = None;
                return http_once(conn, url, timeout, method, path, body);
            }
            Ok(_) => {
                *conn = None;
                return Err(io::Error::other("relay closed the connection"));
            }
            Err(e) => {
                *conn = None;
                let e = io::Error::new(e.kind(), format!("reply, {} ms in: {e}", t0.elapsed().as_millis()));
                // A kept-alive connection a proxy dropped without a word: once more on a new one.
                return if fresh { Err(e) } else { http_once(conn, url, timeout, method, path, body) };
            }
        }
        if head.len() > 16384 {
            *conn = None;
            return Err(io::Error::other("bad reply from the relay"));
        }
    }
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let len = text.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0usize);
    let mut data = vec![0u8; len];
    if let Err(e) = s.read_exact(&mut data) {
        *conn = None;
        return Err(e);
    }
    if text.contains("connection: close") {
        *conn = None;
    }
    Ok((status, data))
}

fn poll_frames(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 5 <= data.len() {
        let n = u32::from_be_bytes(data[at + 1..at + 5].try_into().unwrap()) as usize;
        if at + 5 + n > data.len() {
            break;
        }
        out.push((data[at], data[at + 5..at + 5 + n].to_vec()));
        at += 5 + n;
    }
    out
}

/// The relay over plain HTTP, for proxies that refuse WebSockets (a Chariot's):
/// messages go up in POSTs, batched for 200 ms, and come down in GETs the relay
/// holds for up to 8 seconds, on a thread of their own.
fn poll_session(url: &str, room: &str, role: &str, events: &Sender<Net>, outgoing: &Receiver<Vec<u8>>) -> io::Result<()> {
    let id = crate::secure::hex(&crate::secure::random::<8>());
    let send_path = format!("/v1/send/{room}?role={role}&id={id}");
    let recv_path = format!("/v1/recv/{room}?role={role}&id={id}");
    let mut up: Option<Stream> = None;
    http(&mut up, url, Duration::from_secs(15), "POST", &send_path, &[])?;
    if events.send(Net::Connected).is_err() {
        return Ok(());
    }
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let failure = Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let (url, events, alive, failure) = (url.to_string(), events.clone(), alive.clone(), failure.clone());
        std::thread::spawn(move || {
            let mut down: Option<Stream> = None;
            let mut misses = 0;
            while alive.load(std::sync::atomic::Ordering::Relaxed) {
                match http(&mut down, &url, Duration::from_secs(20), "GET", &recv_path, &[]) {
                    Ok((200, data)) => {
                        misses = 0;
                        for (op, m) in poll_frames(&data) {
                            let e = match op {
                                1 if m.windows(4).any(|w| w == b"\"up\"") => Net::PeerUp,
                                1 if m.windows(6).any(|w| w == b"\"down\"") => Net::PeerDown,
                                2 if !m.is_empty() => Net::Message(m),
                                _ => continue,
                            };
                            if events.send(e).is_err() {
                                alive.store(false, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                    // A proxy's own timeout or hiccup: ask again.
                    Ok((code, _)) if (500..600).contains(&code) && code != 501 => {
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    Ok((code, _)) => {
                        *failure.lock().unwrap() = Some(format!("relay answered {code}"));
                        break;
                    }
                    // A blip: try again a few times before giving up on the relay.
                    Err(e) => {
                        println!("agent: relay: {e}");
                        misses += 1;
                        if misses >= 4 {
                            *failure.lock().unwrap() = Some(e.to_string());
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            }
            alive.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    }
    let result = loop {
        if !alive.load(std::sync::atomic::Ordering::Relaxed) {
            let why = failure.lock().unwrap().take().unwrap_or_else(|| "relay lost".into());
            break Err(io::Error::other(why));
        }
        let first = match outgoing.recv_timeout(Duration::from_millis(200)) {
            Ok(m) => m,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break Ok(()),
        };
        let mut batch = Vec::new();
        let add = |b: &mut Vec<u8>, m: &[u8]| {
            b.push(2);
            b.extend_from_slice(&(m.len() as u32).to_be_bytes());
            b.extend_from_slice(m);
        };
        add(&mut batch, &first);
        // Five requests a second at most: each costs headers and a proxy's attention.
        let until = Instant::now() + Duration::from_millis(200);
        while batch.len() < 256 << 10 {
            match outgoing.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(m) => add(&mut batch, &m),
                Err(_) => break,
            }
        }
        let mut tries = 0;
        let sent = loop {
            match http(&mut up, url, Duration::from_secs(6), "POST", &send_path, &batch) {
                Ok(_) => break Ok(()),
                Err(e) if tries < 3 => {
                    tries += 1;
                    up = None;
                    println!("agent: relay: {e}; again");
                }
                Err(e) => break Err(e),
            }
        };
        if let Err(e) = sent {
            break Err(e);
        }
    };
    alive.store(false, std::sync::atomic::Ordering::Relaxed);
    result
}

/// Keep a connection to `url` for `room` as `role` until the returned sender is dropped.
pub fn spawn(url: String, room: [u8; 16], role: &'static str, events: Sender<Net>) -> Sender<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let path = format!("/v1/pipe/{}?role={role}", crate::secure::hex(&room));
        let mut wait = Duration::from_secs(1);
        let mut polling = false;
        loop {
            let started = Instant::now();
            let r = if polling {
                poll_session(&url, &crate::secure::hex(&room), role, &events, &rx)
            } else {
                session(&url, &path, &events, &rx)
            };
            match r {
                Ok(()) => return,
                Err(e) if !polling && e.to_string().starts_with(REFUSED) => {
                    println!("agent: the relay refuses WebSockets; polling over HTTP");
                    polling = true;
                    continue;
                }
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

#[cfg(test)]
mod live {
    /// VIOLET_RELAY_TEST=https://… cargo test -p agent live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn poll_holds_up() {
        let Ok(url) = std::env::var("VIOLET_RELAY_TEST") else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let out = super::spawn(url, [7u8; 16], "phone", tx);
        let start = std::time::Instant::now();
        let mut lost = 0;
        while start.elapsed() < std::time::Duration::from_secs(150) {
            let _ = out.send(vec![0x55; 700]);
            while let Ok(e) = rx.try_recv() {
                match e {
                    super::Net::Lost(why) => {
                        lost += 1;
                        println!("{:?} lost: {why}", start.elapsed());
                    }
                    super::Net::Connected => println!("{:?} connected", start.elapsed()),
                    _ => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        assert_eq!(lost, 0);
    }
}
