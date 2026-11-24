//! Loading a page: HTTPS through the shared violet::http client (rustls,
//! verified against the Mozilla roots), plain HTTP over a TCP socket of our
//! own, following redirects.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::url::Url;

const USER_AGENT: &str = "Mozilla/5.0 (VioletOS; ARMv7) Ladybird/1.0 Mobile";
const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5";
const MAX_REDIRECTS: usize = 8;

pub struct Page {
    /// Where the page came from, after redirects.
    pub url: Url,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub fn load(start: &Url) -> io::Result<Page> {
    let mut url = start.clone();
    for _ in 0..=MAX_REDIRECTS {
        let reply = if url.https { https(&url)? } else { http(&url)? };
        if matches!(reply.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = reply.header("location")
        {
            let fragment = url.fragment.clone();
            url = url.join(location).ok_or_else(|| io::Error::other(format!("cannot follow a redirect to {location}")))?;
            if url.fragment.is_none() {
                url.fragment = fragment;
            }
            continue;
        }
        let content_type = reply.header("content-type").unwrap_or("text/html").to_ascii_lowercase();
        return Ok(Page { url, status: reply.status, content_type, body: reply.body });
    }
    Err(io::Error::other("too many redirects"))
}

fn https(url: &Url) -> io::Result<Reply> {
    if url.port() != 443 {
        return Err(io::Error::other("HTTPS is supported on port 443 only"));
    }
    let r = violet::http::get(&url.document(), &[("User-Agent", USER_AGENT), ("Accept", ACCEPT), ("Accept-Language", "en")])?;
    Ok(Reply { status: r.status, headers: r.headers, body: r.body })
}

fn http(url: &Url) -> io::Result<Reply> {
    let socket = TcpStream::connect((url.host(), url.port()))?;
    let _ = socket.set_read_timeout(Some(Duration::from_secs(30)));
    let mut conn = BufReader::with_capacity(32 << 10, socket);
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {USER_AGENT}\r\nAccept: {ACCEPT}\r\nAccept-Language: en\r\nConnection: close\r\n\r\n",
        url.path, url.authority
    );
    conn.get_mut().write_all(request.as_bytes())?;
    let mut line = String::new();
    conn.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other(format!("bad status line {:?}", line.trim())))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        if conn.read_line(&mut line)? == 0 {
            break;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let reply = Reply { status, headers, body: Vec::new() };
    let mut body = Vec::new();
    if reply.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            line.clear();
            conn.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16).map_err(io::Error::other)?;
            if size == 0 {
                break;
            }
            let at = body.len();
            body.resize(at + size, 0);
            conn.read_exact(&mut body[at..])?;
            line.clear();
            conn.read_line(&mut line)?;
        }
    } else if let Some(n) = reply.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.resize(n, 0);
        conn.read_exact(&mut body)?;
    } else {
        conn.read_to_end(&mut body)?;
    }
    Ok(Reply { body, ..reply })
}

/// The page's text: UTF-8 when it is valid UTF-8 (or says so), otherwise
/// Windows-1252, the web's default for unlabelled pages.
pub fn decode(body: &[u8], content_type: &str) -> String {
    let labelled_utf8 = content_type.contains("utf-8") || content_type.contains("utf8");
    match std::str::from_utf8(body) {
        Ok(s) => s.to_string(),
        Err(_) if labelled_utf8 => String::from_utf8_lossy(body).into_owned(),
        Err(_) => body.iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn windows_1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D,
        0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D,
        0x017E, 0x0178,
    ];
    match b {
        0x80..=0x9f => char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('\u{fffd}'),
        _ => b as char,
    }
}
