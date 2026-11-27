//! Streaming HTTPS download of a system update bundle, read piece by piece
//! so a bundle of tens of megabytes never sits in memory whole.
//!
//! Trust: the Mozilla roots (webpki-roots), plus, only in an image built with
//! VIOLET_UPDATE_TEST_ROOT (the DER of a test CA, in hex), that one extra
//! root, for `make test-ota`'s local server. TLS protects the download; what
//! decides whether the phone boots it is the bundle's own signature, which
//! the supervisor checks (system/package-manager/ota.rs).
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// The largest bundle accepted: a slot is 64 MiB in QEMU and 16 MiB on the handset.
pub const MAX_BUNDLE: u64 = 64 << 20;

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn config() -> io::Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    if let Some(hex) = option_env!("VIOLET_UPDATE_TEST_ROOT") {
        let der = unhex(hex).ok_or_else(|| io::Error::other("VIOLET_UPDATE_TEST_ROOT is not hex"))?;
        roots.add(rustls::pki_types::CertificateDer::from(der)).map_err(io::Error::other)?;
    }
    let c = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(c))
}

/// "https://host[:port]/path" as (host, port, path).
pub fn split(url: &str) -> io::Result<(&str, u16, &str)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "https only"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad port"))?),
        None => (authority, 443),
    };
    if host.is_empty() || path.contains(['\r', '\n', ' ']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad URL"));
    }
    Ok((host, port, path))
}

pub struct Download {
    body: BufReader<Tls>,
    pub length: u64,
    pub done: u64,
}

impl Download {
    /// GET `url` and read the status and headers; the body follows with `read`.
    pub fn open(url: &str) -> io::Result<Download> {
        let (host, port, path) = split(url)?;
        Download::over(TcpStream::connect((host, port))?, host, path)
    }
    /// The same over a connection already made (tests reach a local server).
    pub fn over(sock: TcpStream, host: &str, path: &str) -> io::Result<Download> {
        let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let conn = rustls::ClientConnection::new(config()?, name).map_err(io::Error::other)?;
        let mut body = BufReader::with_capacity(32 << 10, rustls::StreamOwned::new(conn, sock));
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: VioletOS\r\nAccept: */*\r\nConnection: close\r\n\r\n");
        body.get_mut().write_all(request.as_bytes())?;
        body.get_mut().flush()?;
        let mut line = String::new();
        body.read_line(&mut line)?;
        let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| io::Error::other("bad status line"))?;
        let mut length = None;
        loop {
            line.clear();
            if body.read_line(&mut line)? == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "headers cut off"));
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse::<u64>().ok();
                }
            }
        }
        if status != 200 {
            return Err(io::Error::other(format!("the server answered {status}")));
        }
        let length = length.ok_or_else(|| io::Error::other("no content length"))?;
        if length > MAX_BUNDLE {
            return Err(io::Error::other("the bundle is too large"));
        }
        Ok(Download { body, length, done: 0 })
    }
    /// Fill `buf` from the body, or less at its end. Ok(0) once it is all read.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf.len().min((self.length - self.done) as usize);
        let mut got = 0;
        while got < want {
            let n = self.body.read(&mut buf[got..want])?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "download cut off"));
            }
            got += n;
        }
        self.done += got as u64;
        Ok(got)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(split("https://updates.violet.test/latest.vota").unwrap(), ("updates.violet.test", 443, "/latest.vota"));
        assert_eq!(split("https://h:8443").unwrap(), ("h", 8443, "/"));
        assert!(split("http://h/x").is_err());
        assert!(split("https://h/a b").is_err());
        assert!(split("https://:1/").is_err());
    }

    /// OTA_FETCH_ADDR=127.0.0.1:PORT OTA_FETCH_URL=https://name/path with an
    /// image built with VIOLET_UPDATE_TEST_ROOT: download through TLS from
    /// targets/qemu/test_ota.py's server and print the digest.
    #[test]
    fn fetch_from_a_test_server() {
        let (Ok(addr), Ok(url)) = (std::env::var("OTA_FETCH_ADDR"), std::env::var("OTA_FETCH_URL")) else { return };
        let (host, _, path) = split(&url).unwrap();
        let mut d = Download::over(TcpStream::connect(addr.as_str()).unwrap(), host, path).unwrap();
        let mut all = vec![];
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let n = d.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        assert_eq!(all.len() as u64, d.length);
        println!("fetched {} bytes, first {:02x?}", all.len(), &all[..8]);
    }
}
