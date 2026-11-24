//! Just enough of URL parsing and resolution for following links.
//! http and https only; other schemes (mailto:, javascript:, data:) are not
//! followed.

#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub https: bool,
    /// Host, with ":port" if one was given.
    pub authority: String,
    /// Path and query, always starting with '/'.
    pub path: String,
    pub fragment: Option<String>,
}

impl Url {
    pub fn parse(text: &str) -> Option<Url> {
        let text = text.trim();
        let (https, rest) = if let Some(r) = strip_prefix_ignore_case(text, "https://") {
            (true, r)
        } else if let Some(r) = strip_prefix_ignore_case(text, "http://") {
            (false, r)
        } else {
            return None;
        };
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, Some(f.to_string())),
            None => (rest, None),
        };
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = rest[..end].to_ascii_lowercase();
        let authority = authority.rsplit_once('@').map_or(authority.as_str(), |(_, a)| a).to_string();
        if authority.is_empty() {
            return None;
        }
        let mut path = rest[end..].to_string();
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        Some(Url { https, authority, path: encode(&remove_dot_segments(&path)), fragment })
    }

    pub fn host(&self) -> &str {
        self.authority.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(&self.authority, |(h, _)| h)
    }

    pub fn port(&self) -> u16 {
        self.authority
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(if self.https { 443 } else { 80 })
    }

    /// The URL without its fragment.
    pub fn document(&self) -> String {
        format!("{}://{}{}", if self.https { "https" } else { "http" }, self.authority, self.path)
    }

    /// Resolve a link against this URL.
    pub fn join(&self, href: &str) -> Option<Url> {
        let href = href.trim().replace(['\n', '\r', '\t'], "");
        if href.is_empty() {
            return Some(Url { fragment: None, ..self.clone() });
        }
        let scheme_end = href.find(':').filter(|&i| {
            href[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) && i > 0 && !href[..i].contains('/')
        });
        if let Some(i) = scheme_end {
            let scheme = href[..i].to_ascii_lowercase();
            return if scheme == "http" || scheme == "https" { Url::parse(&href) } else { None };
        }
        let origin = format!("{}://{}", if self.https { "https" } else { "http" }, self.authority);
        if let Some(rest) = href.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        if let Some(fragment) = href.strip_prefix('#') {
            return Some(Url { fragment: Some(fragment.to_string()), ..self.clone() });
        }
        let base_path = self.path.split('?').next().unwrap_or("/");
        let path = if href.starts_with('/') {
            href.to_string()
        } else if href.starts_with('?') {
            format!("{base_path}{href}")
        } else {
            let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
            format!("{dir}{href}")
        };
        Url::parse(&format!("{origin}{path}"))
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    (text.len() >= prefix.len() && text.is_char_boundary(prefix.len()) && text[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &text[prefix.len()..])
}

/// RFC 3986 section 5.2.4, applied to the path but not the query.
fn remove_dot_segments(path: &str) -> String {
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path, None),
    };
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    for (i, s) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *s {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut result = format!("/{}", out.join("/"));
    if let Some(q) = query {
        result.push('?');
        result.push_str(q);
    }
    result
}

/// Percent-encode what may not appear in a request line.
fn encode(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b <= 0x20 || b >= 0x7f || b == b'"' || b == b'<' || b == b'>' || b == b'`' {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Url;

    #[test]
    fn resolves_links() {
        let base = Url::parse("https://example.com/a/b/c.html?x=1").unwrap();
        let j = |h: &str| base.join(h).map(|u| u.document());
        assert_eq!(j("d.html").as_deref(), Some("https://example.com/a/b/d.html"));
        assert_eq!(j("../d").as_deref(), Some("https://example.com/a/d"));
        assert_eq!(j("/e").as_deref(), Some("https://example.com/e"));
        assert_eq!(j("//other.org/f").as_deref(), Some("https://other.org/f"));
        assert_eq!(j("?y=2").as_deref(), Some("https://example.com/a/b/c.html?y=2"));
        assert_eq!(j("http://h:8080/p q").as_deref(), Some("http://h:8080/p%20q"));
        assert_eq!(j("mailto:x@y"), None);
        assert_eq!(base.join("#top").unwrap().fragment.as_deref(), Some("top"));
        let port = Url::parse("http://Localhost:8088").unwrap();
        assert_eq!((port.host(), port.port(), port.path.as_str()), ("localhost", 8088, "/"));
    }
}
