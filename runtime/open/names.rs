//! The Open Runtime's resolver: a bounded table of stand-in addresses.
//!
//! A program's DNS question never leaves the phone. Each name it asks for
//! gets an address in 198.18.0.0/15, and a connect to that address hands the
//! name to Radio (net.rs). The table is what an app can make the runtime
//! remember, so it is bounded and it is per owner:
//! - an owner (an app's pid, or SYSTEM) sees only its own names, so one app
//!   cannot learn what another resolved, nor use its addresses;
//! - an owner keeps at most `PER_OWNER` names and the table `CAP`; past that
//!   the least recently used name goes, never one whose address a live socket
//!   still holds (`live`);
//! - addresses are handed out in rotation over the whole /15, and one a live
//!   socket holds is never given to another name, so an address means one
//!   name for as long as anything uses it;
//! - an address with no name behind it is `Stale`: callers refuse it and
//!   never treat it as a real address.
//! Pure: tested on the host (make test-unit).
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// Stand-in addresses: 198.18.0.0/15.
pub const BASE: u32 = 0xc612_0000;
const MASK: u32 = 0xfffe_0000;
/// Host parts 1..=HOSTS (not the network's own address nor its broadcast).
const HOSTS: u32 = 0x1_fffe;
/// Names one owner can keep.
pub const PER_OWNER: usize = 256;
/// Names in the whole table.
pub const CAP: usize = 2304;
/// The longest name (RFC 1035, without the root's dot).
pub const MAX_NAME: usize = 253;
/// The longest question answered.
pub const MAX_QUERY: usize = 512;
pub const SERVFAIL: u8 = 2;
pub const NXDOMAIN: u8 = 3;

pub fn is_stand_in(ip: u32) -> bool {
    ip & MASK == BASE
}

struct Entry {
    owner: u32,
    addr: u32,
    used: u64,
    name: Box<str>,
}

pub enum Lookup<'a> {
    /// The name this owner resolved to the address.
    Name(&'a str),
    /// A stand-in address with no name of this owner's behind it.
    Stale,
    /// Not a stand-in address.
    Literal,
}

pub struct Names {
    entries: Vec<Entry>,
    /// The host part handed out last.
    cursor: u32,
    /// Every address has been handed out once: check `live` before reuse.
    wrapped: bool,
    clock: u64,
}

/// A label that is a number as inet_aton reads one: decimal or octal digits,
/// or "0x" and hex digits.
fn numeric(label: &str) -> bool {
    let l = label.as_bytes();
    if l.len() >= 2 && l[0] == b'0' && (l[1] | 0x20) == b'x' {
        return l[2..].iter().all(|b| b.is_ascii_hexdigit());
    }
    l.iter().all(|b| b.is_ascii_digit())
}

/// A name as the table keeps it: lower case, no trailing dot; None unless it
/// is 1..=253 bytes of labels of 1..=63 letters, digits, '-' and '_'.
///
/// None, too, for a name whose last label is a number ("10.0.0.9",
/// "167772169", "0x0a000009", "10.9"): no top-level domain is one, and every
/// form of an IPv4 address that a resolver (lwIP's, a phone's, the Mac's)
/// takes for the address itself ends in one. Such a "name" would be checked
/// by the Shield as a name and then dialled as an address.
pub fn normalise(name: &str) -> Option<String> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    let mut last = "";
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return None;
        }
        last = label;
    }
    if numeric(last) {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

impl Names {
    pub const fn new() -> Names {
        Names { entries: Vec::new(), cursor: 0, wrapped: false, clock: 0 }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn count(&self, owner: u32) -> usize {
        self.entries.iter().filter(|e| e.owner == owner).count()
    }
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
    /// Drop the least recently used entry (`owner`'s, or anyone's for None)
    /// that no live socket holds; false when there is none.
    fn evict(&mut self, owner: Option<u32>, live: &mut dyn FnMut(u32) -> bool) -> bool {
        let mut after = 0;
        loop {
            let mut best: Option<usize> = None;
            for (i, e) in self.entries.iter().enumerate() {
                if owner.map_or(true, |o| e.owner == o) && e.used > after && best.map_or(true, |b| e.used < self.entries[b].used) {
                    best = Some(i);
                }
            }
            let Some(i) = best else { return false };
            if !live(self.entries[i].addr) {
                self.entries.swap_remove(i);
                return true;
            }
            after = self.entries[i].used;
        }
    }
    /// The next address no name has and no live socket holds.
    fn fresh(&mut self, live: &mut dyn FnMut(u32) -> bool) -> Option<u32> {
        for _ in 0..HOSTS {
            if self.cursor >= HOSTS {
                self.cursor = 0;
                self.wrapped = true;
            }
            self.cursor += 1;
            let addr = BASE + self.cursor;
            if self.entries.iter().any(|e| e.addr == addr) || (self.wrapped && live(addr)) {
                continue;
            }
            return Some(addr);
        }
        None
    }
    /// The stand-in address for `name`, allocating one the first time.
    /// `live(address)`: a socket still holds the address. Err is the DNS
    /// answer code: NXDOMAIN for a name that cannot be one, SERVFAIL when
    /// the table is full of names in use.
    pub fn resolve(&mut self, owner: u32, name: &str, live: &mut dyn FnMut(u32) -> bool) -> Result<u32, u8> {
        let name = normalise(name).ok_or(NXDOMAIN)?;
        let now = self.tick();
        if let Some(e) = self.entries.iter_mut().find(|e| e.owner == owner && *e.name == *name) {
            e.used = now;
            return Ok(e.addr);
        }
        if self.count(owner) >= PER_OWNER {
            if !self.evict(Some(owner), live) {
                return Err(SERVFAIL);
            }
        } else if self.entries.len() >= CAP && !self.evict(None, live) {
            return Err(SERVFAIL);
        }
        let addr = self.fresh(live).ok_or(SERVFAIL)?;
        if self.entries.try_reserve(1).is_err() {
            return Err(SERVFAIL);
        }
        self.entries.push(Entry { owner, addr, used: now, name: name.into_boxed_str() });
        Ok(addr)
    }
    /// What `ip` is to `owner`. Using a name keeps it from being evicted next.
    pub fn lookup(&mut self, owner: u32, ip: u32) -> Lookup<'_> {
        if !is_stand_in(ip) {
            return Lookup::Literal;
        }
        let now = self.tick();
        match self.entries.iter_mut().find(|e| e.addr == ip && e.owner == owner) {
            Some(e) => {
                e.used = now;
                Lookup::Name(&e.name)
            }
            None => Lookup::Stale,
        }
    }
    /// Forget an owner's names (its app exited).
    pub fn purge(&mut self, owner: u32) {
        self.entries.retain(|e| e.owner != owner);
    }
}

/// Answer a DNS question (RFC 1035): an A record gets the name's stand-in
/// address, every other type no records (so programs fall back from AAAA:
/// there is no IPv6 route; sockaddr.rs takes IPv4-mapped addresses), and
/// localhost its loopback addresses (127.0.0.1, ::1).
/// None for anything that is not one well-formed question.
pub fn answer(names: &mut Names, owner: u32, query: &[u8], live: &mut dyn FnMut(u32) -> bool) -> Option<Vec<u8>> {
    if query.len() < 12 || query.len() > MAX_QUERY || query[2] & 0x80 != 0 || u16::from_be_bytes([query[4], query[5]]) != 1 {
        return None;
    }
    let mut at = 12;
    let mut name = String::new();
    loop {
        let len = *query.get(at)? as usize;
        at += 1;
        if len == 0 {
            break;
        }
        // A question's name is never compressed.
        if len > 63 {
            return None;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(core::str::from_utf8(query.get(at..at + len)?).ok()?);
        at += len;
    }
    let qtype = u16::from_be_bytes([*query.get(at)?, *query.get(at + 1)?]);
    let question_end = at + 4;
    let question = query.get(12..question_end)?;
    let mut r = Vec::with_capacity(question_end + 16);
    r.extend_from_slice(&query[..2]);
    // A response, recursion desired and available, one question.
    r.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0]);
    r.extend_from_slice(question);
    // "localhost" and names under it (RFC 6761) are this phone: the loopback
    // addresses, never a stand-in, never a question for Radio or a relay.
    let normal = normalise(&name);
    let local = normal.as_deref().is_some_and(|n| n == "localhost" || n.ends_with(".localhost"));
    if local && (qtype == 1 || qtype == 28) {
        r[7] = 1;
        let v4 = qtype == 1;
        r.extend_from_slice(&[0xc0, 12, 0, qtype as u8, 0, 1, 0, 0, 0, 60, 0, if v4 { 4 } else { 16 }]);
        if v4 {
            r.extend_from_slice(&[127, 0, 0, 1]);
        } else {
            r.extend_from_slice(&[0; 15]);
            r.push(1);
        }
    } else if qtype != 1 && !name.is_empty() && normal.is_none() {
        // AAAA and every other type: no records (programs fall back to A),
        // but a name that cannot be one is NXDOMAIN, as its A question is.
        r[3] |= NXDOMAIN;
    } else if qtype == 1 && !name.is_empty() {
        match names.resolve(owner, &name, live) {
            Ok(ip) => {
                r[7] = 1;
                r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
                r.extend_from_slice(&ip.to_be_bytes());
            }
            Err(code) => r[3] |= code,
        }
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    fn none() -> impl FnMut(u32) -> bool {
        |_| false
    }
    fn name_of(names: &mut Names, owner: u32, ip: u32) -> Option<String> {
        match names.lookup(owner, ip) {
            Lookup::Name(n) => Some(String::from(n)),
            _ => None,
        }
    }
    fn question(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.').filter(|l| !l.is_empty()) {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&[0, 1]);
        q
    }

    #[test]
    fn a_name_keeps_its_address() {
        let mut n = Names::new();
        let a = n.resolve(100, "Example.COM.", &mut none()).unwrap();
        assert!(is_stand_in(a));
        assert_eq!(n.resolve(100, "example.com", &mut none()), Ok(a));
        assert_ne!(n.resolve(100, "example.org", &mut none()).unwrap(), a);
        assert_eq!(name_of(&mut n, 100, a).as_deref(), Some("example.com"));
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn names_that_cannot_be_names_are_refused() {
        let mut n = Names::new();
        let long_label = "a".repeat(64);
        let long = format!("{}.{}.{}.{}", "a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(62));
        assert_eq!(long.len(), 254);
        for bad in ["", ".", "a..b", ".a", "a b", "a/b", "caf\u{e9}.fr", "a\0b", long_label.as_str(), long.as_str()] {
            assert_eq!(n.resolve(1, bad, &mut none()), Err(NXDOMAIN), "{:?}", bad);
        }
        // An address written out as a name, in every form a resolver reads
        // as the address: it would be checked as a name and dialled as 10.0.0.9.
        for bad in [
            "10.0.0.9", "167772169", "0x0a000009", "0X0A000009", "10.9", "10.0.9", "012.0.0.011", "0", "0x", "1.", "example.123", "a.0x7f", "0x0a.0.0.9",
            "4294967296", "1.2.3.4.5", "host.08",
        ] {
            assert_eq!(n.resolve(1, bad, &mut none()), Err(NXDOMAIN), "{:?}", bad);
            assert!(normalise(bad).is_none());
        }
        assert_eq!(n.len(), 0);
        // Digits elsewhere, or beside letters, are names.
        for good in ["1.example", "10.0.0.9.example", "0x10.net", "a.b0", "a.0xg", "3com", "x.123a", "9-9", "0x-1"] {
            assert!(n.resolve(1, good, &mut none()).is_ok(), "{:?}", good);
        }
        let mut n = Names::new();
        // The longest legal name is kept whole, never cut short.
        let longest = &long[..253];
        let a = n.resolve(1, longest, &mut none()).unwrap();
        assert_eq!(name_of(&mut n, 1, a).as_deref(), Some(longest));
        assert!(n.resolve(1, "_srv-1.x_y.example", &mut none()).is_ok());
    }

    #[test]
    fn the_table_is_bounded_whatever_one_owner_asks() {
        let mut n = Names::new();
        let mut seen = alloc::collections::BTreeSet::new();
        for i in 0..200_000u32 {
            let a = n.resolve(100, &format!("h{}.example", i), &mut none()).unwrap();
            assert!(is_stand_in(a) && a != BASE && a != BASE + 0x1_ffff);
            assert!(n.len() <= PER_OWNER);
            // An address is not reused until the whole /15 has been used.
            if i < HOSTS {
                assert!(seen.insert(a), "address reused early at {}", i);
            }
        }
        assert_eq!(n.count(100), PER_OWNER);
    }

    #[test]
    fn eviction_is_least_recently_used_and_leaves_no_alias() {
        let mut n = Names::new();
        let first = n.resolve(100, "first.example", &mut none()).unwrap();
        let second = n.resolve(100, "second.example", &mut none()).unwrap();
        for i in 2..PER_OWNER {
            n.resolve(100, &format!("h{}.example", i), &mut none()).unwrap();
        }
        // Using the first name (a lookup, as a connect does) spares it.
        assert!(name_of(&mut n, 100, first).is_some());
        let newest = n.resolve(100, "newest.example", &mut none()).unwrap();
        assert!(name_of(&mut n, 100, first).is_some());
        assert!(matches!(n.lookup(100, second), Lookup::Stale));
        assert_ne!(newest, second);
        // Asking again gives the evicted name a new address, not its old one.
        let again = n.resolve(100, "second.example", &mut none()).unwrap();
        assert_ne!(again, second);
        assert!(matches!(n.lookup(100, second), Lookup::Stale));
    }

    #[test]
    fn a_live_address_is_never_evicted_or_remapped() {
        let mut n = Names::new();
        let held = n.resolve(100, "held.example", &mut none()).unwrap();
        let mut live = |ip: u32| ip == held;
        // Far more names than the /15 has addresses: the cursor wraps.
        for i in 0..150_000u32 {
            let a = n.resolve(100, &format!("h{}.example", i), &mut live).unwrap();
            assert_ne!(a, held);
        }
        assert_eq!(name_of(&mut n, 100, held).as_deref(), Some("held.example"));
        // An address whose name is gone but which a socket still holds stays unused.
        let mut m = Names::new();
        let gone = m.resolve(100, "gone.example", &mut none()).unwrap();
        m.purge(100);
        let mut live = |ip: u32| ip == gone;
        for i in 0..150_000u32 {
            assert_ne!(m.resolve(100, &format!("h{}.example", i), &mut live).unwrap(), gone);
        }
        assert!(matches!(m.lookup(100, gone), Lookup::Stale));
    }

    #[test]
    fn a_table_full_of_live_names_fails_closed() {
        let mut n = Names::new();
        let mut all = |_: u32| true;
        for i in 0..PER_OWNER {
            n.resolve(100, &format!("h{}.example", i), &mut all).unwrap();
        }
        assert_eq!(n.resolve(100, "one-more.example", &mut all), Err(SERVFAIL));
        assert_eq!(n.count(100), PER_OWNER);
        // A known name still answers.
        assert!(n.resolve(100, "h5.example", &mut all).is_ok());
        // Another owner is not affected.
        assert!(n.resolve(101, "one-more.example", &mut all).is_ok());
    }

    #[test]
    fn owners_do_not_see_each_other() {
        let mut n = Names::new();
        let a = n.resolve(100, "secret.example", &mut none()).unwrap();
        // Another app asking the same name learns nothing from the address.
        let b = n.resolve(101, "secret.example", &mut none()).unwrap();
        assert_ne!(a, b);
        assert!(matches!(n.lookup(101, a), Lookup::Stale));
        assert!(matches!(n.lookup(100, b), Lookup::Stale));
        assert!(matches!(n.lookup(100, 0x0808_0808), Lookup::Literal));
        // One owner filling its share evicts nothing of another's.
        for i in 0..5 * PER_OWNER {
            n.resolve(101, &format!("h{}.example", i), &mut none()).unwrap();
        }
        assert_eq!(name_of(&mut n, 100, a).as_deref(), Some("secret.example"));
        assert_eq!(n.count(100), 1);
        n.purge(101);
        assert_eq!(n.len(), 1);
        assert!(matches!(n.lookup(101, b), Lookup::Stale));
    }

    #[test]
    fn the_whole_table_is_bounded_across_owners() {
        let mut n = Names::new();
        for owner in 0..40u32 {
            for i in 0..PER_OWNER {
                n.resolve(owner, &format!("h{}.example", i), &mut none()).unwrap();
                assert!(n.len() <= CAP);
            }
        }
        assert_eq!(n.len(), CAP);
    }

    #[test]
    fn dns_a_question_gets_one_address() {
        let mut n = Names::new();
        let q = question("www.Example.com", 1);
        let r = answer(&mut n, 100, &q, &mut none()).unwrap();
        assert_eq!(&r[..2], &q[..2]);
        assert_eq!(r[2] & 0x80, 0x80);
        assert_eq!(r[3] & 15, 0);
        assert_eq!(&r[4..8], &[0, 1, 0, 1]);
        assert_eq!(&r[12..q.len()], &q[12..]);
        let rr = &r[q.len()..];
        assert_eq!(rr.len(), 16);
        assert_eq!(&rr[..12], &[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        let ip = u32::from_be_bytes(rr[12..16].try_into().unwrap());
        assert_eq!(name_of(&mut n, 100, ip).as_deref(), Some("www.example.com"));
    }

    #[test]
    fn dns_other_types_get_no_records() {
        let mut n = Names::new();
        for qtype in [28u16, 5, 15, 16, 33, 65, 255] {
            let q = question("www.example.com", qtype);
            let r = answer(&mut n, 100, &q, &mut none()).unwrap();
            assert_eq!(r.len(), q.len());
            assert_eq!((r[3] & 15, r[7]), (0, 0));
        }
        assert_eq!(n.len(), 0);
    }

    #[test]
    fn dns_aaaa_falls_back_cleanly() {
        let mut n = Names::new();
        // A name that can be one: no records and no error, so a resolver that
        // asked A and AAAA together keeps the A answer; nothing is allocated.
        let r = answer(&mut n, 100, &question("v6first.example", 28), &mut none()).unwrap();
        assert_eq!((r[3] & 15, r[7], r.len()), (0, 0, question("v6first.example", 28).len()));
        assert_eq!(n.len(), 0);
        // One that cannot be: NXDOMAIN, as its A question gets.
        for bad in ["10.0.0.9", "bad name.example", "0x7f000001"] {
            for qtype in [1u16, 28] {
                let r = answer(&mut n, 100, &question(bad, qtype), &mut none()).unwrap();
                assert_eq!((r[3] & 15, r[7]), (NXDOMAIN, 0), "{} {}", bad, qtype);
            }
        }
        // The A answer for the same name still works and is unchanged by the AAAA.
        let a = answer(&mut n, 100, &question("v6first.example", 1), &mut none()).unwrap();
        assert_eq!(a[7], 1);
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn dns_localhost_is_loopback_not_a_stand_in() {
        let mut n = Names::new();
        for name in ["localhost", "LocalHost.", "app.localhost"] {
            let q = question(name, 1);
            let r = answer(&mut n, 100, &q, &mut none()).unwrap();
            assert_eq!((r[3] & 15, r[7]), (0, 1), "{}", name);
            assert_eq!(&r[q.len()..], &[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
            let q6 = question(name, 28);
            let r6 = answer(&mut n, 100, &q6, &mut none()).unwrap();
            assert_eq!((r6[3] & 15, r6[7]), (0, 1));
            let rr = &r6[q6.len()..];
            assert_eq!(&rr[..12], &[0xc0, 12, 0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
            assert_eq!(&rr[12..], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        }
        // Not a name lookalike.
        let r = answer(&mut n, 100, &question("notlocalhost", 1), &mut none()).unwrap();
        let ip = u32::from_be_bytes(r[r.len() - 4..].try_into().unwrap());
        assert!(is_stand_in(ip));
        assert_eq!(n.len(), 1);
    }

    #[test]
    fn dns_errors_are_answers_not_silence() {
        let mut n = Names::new();
        let r = answer(&mut n, 100, &question("bad name.example", 1), &mut none()).unwrap();
        assert_eq!((r[3] & 15, r[7]), (NXDOMAIN, 0));
        let mut all = |_: u32| true;
        for i in 0..PER_OWNER {
            n.resolve(100, &format!("h{}.example", i), &mut all).unwrap();
        }
        let r = answer(&mut n, 100, &question("more.example", 1), &mut all).unwrap();
        assert_eq!((r[3] & 15, r[7]), (SERVFAIL, 0));
    }

    #[test]
    fn dns_malformed_questions_are_dropped() {
        let mut n = Names::new();
        let good = question("www.example.com", 1);
        assert!(answer(&mut n, 1, &good[..11], &mut none()).is_none());
        // Cut anywhere, it is not a question.
        for cut in 12..good.len() {
            assert!(answer(&mut n, 1, &good[..cut], &mut none()).is_none(), "cut at {}", cut);
        }
        let mut response = good.clone();
        response[2] |= 0x80;
        assert!(answer(&mut n, 1, &response, &mut none()).is_none());
        let mut two = good.clone();
        two[5] = 2;
        assert!(answer(&mut n, 1, &two, &mut none()).is_none());
        // A compression pointer where a label should be.
        let mut pointer = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1];
        assert!(answer(&mut n, 1, &pointer, &mut none()).is_none());
        pointer[12] = 0x40;
        assert!(answer(&mut n, 1, &pointer, &mut none()).is_none());
        // A label that runs past the end.
        assert!(answer(&mut n, 1, &[0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 5, b'a', b'b'], &mut none()).is_none());
        let mut huge = good.clone();
        huge.resize(MAX_QUERY + 1, 0);
        assert!(answer(&mut n, 1, &huge, &mut none()).is_none());
        // The root's own A record: no records, no table entry.
        let r = answer(&mut n, 1, &question("", 1), &mut none()).unwrap();
        assert_eq!(r[7], 0);
        assert_eq!(n.len(), 0);
    }
}
