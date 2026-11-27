//! HTTPS GET with rustls and the Mozilla roots (webpki-roots). Connections
//! are kept alive per host and reopened when they fail.
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// The largest response body read into memory.
pub const MAX_BODY: usize = 32 << 20;

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(c)
        })
        .clone()
}

fn pool() -> &'static Mutex<HashMap<String, BufReader<Tls>>> {
    static POOL: OnceLock<Mutex<HashMap<String, BufReader<Tls>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn connect(authority: &str) -> io::Result<BufReader<Tls>> {
    // "host" or "host:port".
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad port"))?),
        None => (authority, 443),
    };
    let sock = TcpStream::connect((host, port))?;
    // Not every system call is supported; a missing timeout is not fatal.
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let conn = rustls::ClientConnection::new(config(), name).map_err(io::Error::other)?;
    Ok(BufReader::with_capacity(32 << 10, rustls::StreamOwned::new(conn, sock)))
}

fn split(url: &str) -> io::Result<(&str, &str)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "https only"))?;
    Ok(match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    })
}

/// GET a URL with extra headers. The whole body is read.
pub fn get(url: &str, headers: &[(&str, &str)]) -> io::Result<Response> {
    let (host, path) = split(url)?;
    // A kept connection may have been closed by the server: retry once fresh.
    for attempt in 0..2 {
        let kept = if attempt == 0 { pool().lock().unwrap().remove(host) } else { None };
        let fresh = kept.is_none();
        let mut conn = match kept {
            Some(c) => c,
            None => connect(host)?,
        };
        match exchange(&mut conn, host, path, headers) {
            Ok((response, reusable)) => {
                if reusable {
                    pool().lock().unwrap().insert(host.to_string(), conn);
                }
                return Ok(response);
            }
            Err(e) if fresh => return Err(e),
            Err(_) => continue,
        }
    }
    Err(io::Error::other("unreachable"))
}

fn exchange(conn: &mut BufReader<Tls>, host: &str, path: &str, headers: &[(&str, &str)]) -> io::Result<(Response, bool)> {
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: VioletOS\r\nAccept: */*\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    conn.get_mut().write_all(req.as_bytes())?;
    conn.get_mut().flush()?;

    let mut line = String::new();
    if conn.read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    let status = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| io::Error::other(format!("bad status line {line:?}")))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        conn.read_line(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let find = |name: &str| headers.iter().find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
    let close = find("connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
    let mut body = Vec::new();
    let reusable;
    if let Some(n) = find("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if n > MAX_BODY {
            return Err(io::Error::other(format!("response of {n} bytes is too large")));
        }
        body.resize(n, 0);
        conn.read_exact(&mut body)?;
        reusable = !close;
    } else if find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            line.clear();
            conn.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16).map_err(io::Error::other)?;
            if size == 0 {
                // Trailer fields, if any, end with an empty line.
                loop {
                    line.clear();
                    if conn.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
                        break;
                    }
                }
                break;
            }
            if size > MAX_BODY - body.len() {
                return Err(io::Error::other("response is too large"));
            }
            let at = body.len();
            body.resize(at + size, 0);
            conn.read_exact(&mut body[at..])?;
            line.clear();
            conn.read_line(&mut line)?;
        }
        reusable = !close;
    } else {
        // Until the server closes the connection; TLS close_notify may be missing.
        match conn.take(MAX_BODY as u64 + 1).read_to_end(&mut body) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && !body.is_empty() => {}
            Err(e) => return Err(e),
        }
        if body.len() > MAX_BODY {
            return Err(io::Error::other("response is too large"));
        }
        reusable = false;
    }
    Ok((Response { status, headers, body }, reusable))
}
