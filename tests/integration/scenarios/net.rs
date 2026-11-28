//! Open apps' sockets: what targets/qemu/test_net.py checked with the
//! nethog, netpeer and netserve apps, against the shipped slots.rs,
//! names.rs, loopback.rs, sockaddr.rs and the policy. Radio's own side
//! (lwIP: a connect that never answers ends in ETIMEDOUT, a slot reused
//! mid-stream) is `make test-net-lwip`; a real TCP stream through the relay
//! is `make test-open` (pynet, gonet).
use crate::kernel::clock;
use crate::open::loopback::{self, Budget, Listener, SERVE_APPS, SERVE_LAN};
use crate::open::names::{self, Lookup};
use crate::open::{slots, sockaddr};
use crate::world::*;

const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const STREAM: u32 = 1;
const DGRAM: u32 = 2;
const NET: &str = "caps: net\n";

fn phone() -> (World, usize, usize) {
    let mut w = World::boot(Image::new().app("nethog", NET).app("netpeer", NET).app("netserve", "caps: net\nserve: apps lan\n"));
    let hog = w.open_app("nethog");
    let peer = w.open_app("netpeer");
    (w, hog, peer)
}
fn v4(ip: [u8; 4], port: u16) -> Vec<u8> {
    let mut b = vec![2, 0];
    b.extend_from_slice(&port.to_be_bytes());
    b.extend_from_slice(&ip);
    b.extend_from_slice(&[0; 8]);
    b
}
fn v6(addr: [u8; 16], port: u16) -> Vec<u8> {
    let mut b = vec![10, 0];
    b.extend_from_slice(&port.to_be_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&addr);
    b.extend_from_slice(&[0; 4]);
    b
}
fn mapped(ip: [u8; 4]) -> [u8; 16] {
    let mut a = [0u8; 16];
    a[10] = 0xff;
    a[11] = 0xff;
    a[12..].copy_from_slice(&ip);
    a
}
const LOCALHOST6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

#[test]
fn the_resolver_is_bounded_and_a_stale_address_goes_nowhere() {
    // nethog: 300 names get 300 addresses in 198.18.0.0/15; the table keeps
    // 256 an app; an address whose name was dropped is no host at all.
    let (mut w, hog, peer) = phone();
    let (hog_pid, peer_pid) = (w.app(hog).pid(), w.app(peer).pid());
    let n = &mut w.rt.names;
    let mut addrs = Vec::new();
    for k in 0..300 {
        let ip = n.resolve(hog_pid, &format!("host{k}.violet.test"), &mut |_| false).expect("resolved");
        assert!(names::is_stand_in(ip) && ip >> 17 == names::BASE >> 17, "{ip:#x} is outside 198.18.0.0/15");
        addrs.push(ip);
    }
    addrs.sort();
    addrs.dedup();
    assert_eq!(addrs.len(), 300);
    assert_eq!(n.count(hog_pid), names::PER_OWNER);
    // The first names were dropped: their addresses are stale (EHOSTUNREACH), the last still name a host.
    let first = n.resolve(peer_pid, "elsewhere.test", &mut |_| false).unwrap();
    assert!(matches!(n.lookup(hog_pid, addrs[0]), Lookup::Stale));
    assert!(matches!(n.lookup(hog_pid, *addrs.last().unwrap()), Lookup::Name("host299.violet.test")));
    // Another app's address means nothing to this one, and a literal is a literal.
    assert!(matches!(n.lookup(hog_pid, first), Lookup::Stale));
    assert!(matches!(n.lookup(hog_pid, 0x0808_0808), Lookup::Literal));
    // A name that cannot be one is refused; the hog's churn cost the peer nothing.
    assert_eq!(n.resolve(hog_pid, "bad name!", &mut |_| false), Err(names::NXDOMAIN));
    assert_eq!(n.resolve(hog_pid, &"a".repeat(300), &mut |_| false), Err(names::NXDOMAIN));
    assert_eq!(n.count(peer_pid), 1);
    assert!(n.len() <= names::CAP);
    // An app that ends takes its names with it.
    w.rt.end(hog, 0);
    assert_eq!(w.rt.names.count(hog_pid), 0);
}

#[test]
fn one_app_holds_its_share_of_the_slots_and_no_more() {
    // nethog opens sockets until refused: 9 of 32 with the emulator's eight
    // apps (slots.rs `share`), then EMFILE; netpeer still gets its own at once.
    let (mut w, hog, peer) = phone();
    assert_eq!(slots::share(slots::SLOTS, w.rt.app_limit()), 9);
    let mut held = Vec::new();
    let error = loop {
        match w.app(hog).socket(AF_INET, STREAM) {
            Ok(fd) => held.push(fd),
            Err(e) => break e,
        }
    };
    assert_eq!((held.len(), error), (9, slots::EMFILE));
    // A connect that never answers keeps one of them busy: nobody else waits on it.
    let before = clock::now_ns();
    for _ in 0..slots::APP_FLOOR {
        assert!(w.app(peer).socket(AF_INET, STREAM).is_ok(), "netpeer's sockets beside a hog");
    }
    assert!(w.app(peer).socket(AF_INET, DGRAM).is_ok());
    assert_eq!(clock::now_ns(), before, "no call of netpeer's waited");
    // Closing one gives the hog exactly one more.
    assert_eq!(w.app(hog).close(held.pop().unwrap()), Ok(0));
    assert!(w.app(hog).socket(AF_INET, STREAM).is_ok());
    assert_eq!(w.app(hog).socket(AF_INET, STREAM), Err(slots::EMFILE));
    // Every app that may run keeps its floor however many the others take.
    let (mut total, mut apps) = (0, vec![hog, peer]);
    for name in ["netserve", "nethog", "netpeer", "netserve", "nethog"] {
        apps.push(w.open_app(name));
    }
    for &a in &apps {
        let pid = w.app(a).pid();
        let mut mine = w.rt.slots.held(pid);
        while w.app(a).socket(AF_INET, STREAM).is_ok() {
            mine += 1;
        }
        assert!(mine >= slots::APP_FLOOR, "an app was left below its floor");
        total += mine;
    }
    assert!(total <= slots::SLOTS - slots::RESERVED, "the system's reserve was taken: {total}");
}

/// The loopback join of sockets.rs, for these tests: listeners by port, and
/// a connect that goes through sockaddr.rs and loopback.rs's rules.
struct Lo {
    budget: Budget,
    listeners: Vec<(String, u16, u8, Listener)>,
}
impl Lo {
    fn listen(&mut self, owner: &str, manifest: &str, port: u16, system: bool) -> Result<u16, i32> {
        loopback::may_bind(port, system)?;
        let port = if port == 0 { loopback::ephemeral(0, &mut |p| self.listeners.iter().any(|l| l.1 == p)).unwrap() } else { port };
        self.listeners.push((owner.to_string(), port, loopback::serve_words(manifest), Listener::new(8)));
        Ok(port)
    }
    /// connect(): Ok(the client's end), or the errno.
    fn connect(&mut self, owner: &str, addr: &[u8], v6: bool, from: u16) -> Result<loopback::End, i32> {
        let (ip, port) = sockaddr::parse(addr, v6)?;
        if !loopback::is_loopback(ip) {
            return Err(ENETUNREACH); // not this test's: the network, through Radio
        }
        let now = clock::now_ns();
        let Some(l) = self.listeners.iter_mut().find(|l| l.1 == port && loopback::may_reach(l.0 == owner, l.2)) else { return Err(ECONNREFUSED) };
        let (mine, theirs) = self.budget.open(owner)?;
        l.3.offer(theirs, (sockaddr::LOOPBACK, from), now)?;
        Ok(mine)
    }
}

#[test]
fn an_app_serves_on_loopback_over_ipv4_and_ipv6() {
    // netserve: a server on the phone's loopback, reached at 127.0.0.1, ::1
    // and ::ffff:127.0.0.1; a refused port; ENETUNREACH for a real IPv6 address.
    let mut lo = Lo { budget: Budget::new(), listeners: Vec::new() };
    let port = lo.listen("netserve", "caps: net\nserve: apps\n", 8080, false).unwrap();
    for (addr, six) in [(v4([127, 0, 0, 1], port), false), (v6(LOCALHOST6, port), true), (v6(mapped([127, 0, 0, 1]), port), true), (v4([127, 0, 0, 1], port), true)] {
        let client = lo.connect("netserve", &addr, six, 57344).expect("connects");
        let (server, peer) = lo.listeners[0].3.take().expect("accepted");
        assert_eq!(peer, (sockaddr::LOOPBACK, 57344), "accept reports the client's address");
        assert_eq!(client.write(b"GET / HTTP/1.0\r\n\r\n", 0), Ok(18));
        assert_eq!(server.read(64).unwrap(), b"GET / HTTP/1.0\r\n\r\n");
        assert_eq!(server.write(b"HTTP/1.0 200 OK\r\n\r\nhello", 0), Ok(24));
        assert_eq!(client.read(64).unwrap(), b"HTTP/1.0 200 OK\r\n\r\nhello");
        server.shutdown();
        assert_eq!(client.read(64).unwrap(), b"", "end of stream");
    }
    assert_eq!(lo.connect("netserve", &v4([127, 0, 0, 1], 8081), false, 1).err(), Some(ECONNREFUSED), "nobody listens there");
    // An AF_INET socket takes no AF_INET6 address; a real IPv6 address has no route.
    assert_eq!(sockaddr::parse(&v6(LOCALHOST6, 80), false), Err(sockaddr::EAFNOSUPPORT));
    let real = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    assert_eq!(sockaddr::parse(&v6(real, 80), true), Err(sockaddr::ENETUNREACH));
    assert_eq!(sockaddr::parse(&v6([0; 16], 80), true), Ok((0, 80)), "the dual-stack wildcard");
    // An address reads back in its socket's family.
    let (b, len) = sockaddr::encode((sockaddr::LOOPBACK, 8080), true);
    assert_eq!((len, &b[8..24]), (28, &LOCALHOST6[..]));
    assert_eq!(sockaddr::parse(&b[..len], true), Ok((sockaddr::LOOPBACK, 8080)));
}

#[test]
fn another_app_reaches_a_loopback_server_only_if_its_manifest_serves_apps() {
    let mut lo = Lo { budget: Budget::new(), listeners: Vec::new() };
    let private = lo.listen("notes", "caps: net\n", 9000, false).unwrap();
    let public = lo.listen("netserve", "caps: net\nserve: apps\n", 9001, false).unwrap();
    let lan_only = lo.listen("printer", "caps: net\nserve: lan\n", 9002, false).unwrap();
    assert!(lo.connect("notes", &v4([127, 0, 0, 1], private), false, 1).is_ok(), "its own server");
    assert_eq!(lo.connect("probe", &v4([127, 0, 0, 1], private), false, 1).err(), Some(ECONNREFUSED), "looks closed to another app");
    assert!(lo.connect("probe", &v4([127, 0, 0, 1], public), false, 1).is_ok());
    assert_eq!(lo.connect("probe", &v4([127, 0, 0, 1], lan_only), false, 1).err(), Some(ECONNREFUSED));
    assert_eq!(loopback::serve_words("serve: lan apps everything\n"), SERVE_LAN | SERVE_APPS);
    assert_eq!(loopback::serve_words("caps: net\nserving: apps\n# serve: apps\n"), 0);
    // Ports below 1024 are the system's; port 0 gets one from the upper dynamic range.
    assert_eq!(lo.listen("probe", "", 80, false), Err(EACCES));
    assert!(lo.listen("probe", "", 0, false).unwrap() >= loopback::EPHEMERAL);
    // The backlog is bounded: with eight connections waiting, the next is refused.
    let mut waiting = Vec::new();
    for (k, client) in ["a", "b", "c", "d"].iter().cycle().take(loopback::MAX_BACKLOG - lo.listeners[1].3.waiting()).enumerate() {
        waiting.push(lo.connect(client, &v4([127, 0, 0, 1], public), false, 100 + k as u16).expect("room in the backlog"));
    }
    assert_eq!(lo.listeners[1].3.waiting(), loopback::MAX_BACKLOG);
    assert!(lo.connect("e", &v4([127, 0, 0, 1], public), false, 99).is_err());
}

#[test]
fn loopback_pipes_are_budgeted_per_app() {
    // One app cannot take every pipe: APP_PIPES each, MAX_PIPES in all, with a floor kept for the others.
    let budget = Budget::new();
    let mut held = Vec::new();
    let error = loop {
        match budget.open("nethog") {
            Ok(pair) => held.push(pair),
            Err(e) => break e,
        }
    };
    assert_eq!(held.len(), loopback::APP_PIPES);
    assert!(error < 0);
    assert!(budget.open("netpeer").is_ok(), "another app still gets a pipe");
    drop(held);
    assert_eq!(budget.held("nethog"), 0);
    // A full pipe pushes back instead of growing: the writer waits (EAGAIN).
    let (a, b) = budget.open("nethog").unwrap();
    let mut sent = 0;
    while let Ok(n) = a.write(&[7; 4096], 0) {
        sent += n;
    }
    assert_eq!(sent, loopback::CAP);
    assert_eq!(b.read(loopback::CAP).unwrap().len(), loopback::CAP);
    assert!(a.write(b"more", 0).is_ok());
}

#[test]
fn udp_on_loopback_goes_only_to_the_socket_bound_to_the_port() {
    // netserve: UDP on loopback, with the sender's port.
    let bound = (0, 5353);
    assert!(loopback::takes_datagram(bound, None, 5353, 60000));
    assert!(!loopback::takes_datagram(bound, None, 5354, 60000));
    assert!(!loopback::takes_datagram((0, 0), None, 0, 60000), "an unbound socket is no destination");
    assert!(!loopback::takes_datagram((0x0a00_0001, 5353), None, 5353, 60000), "bound to another interface");
    // A connected socket takes only its peer's datagrams, as Linux filters.
    let peer = Some((sockaddr::LOOPBACK, 60000));
    assert!(loopback::takes_datagram(bound, peer, 5353, 60000));
    assert!(!loopback::takes_datagram(bound, peer, 5353, 60001));
}

#[test]
fn only_ipv4_answers_for_a_name() {
    // netserve: AAAA has no records (so programs fall back to A); localhost has both.
    let (mut w, hog, _) = phone();
    let pid = w.app(hog).pid();
    let question = |name: &str, qtype: u16| {
        let mut q = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&[0, 1]);
        q
    };
    let answers = |reply: &[u8]| u16::from_be_bytes([reply[6], reply[7]]);
    let a = names::answer(&mut w.rt.names, pid, &question("example.test", 1), &mut |_| false).unwrap();
    let aaaa = names::answer(&mut w.rt.names, pid, &question("example.test", 28), &mut |_| false).unwrap();
    assert_eq!((answers(&a), answers(&aaaa)), (1, 0));
    assert_eq!(a[3] & 0xf, 0);
    let ip = u32::from_be_bytes(a[a.len() - 4..].try_into().unwrap());
    assert!(names::is_stand_in(ip));
    let local6 = names::answer(&mut w.rt.names, pid, &question("localhost", 28), &mut |_| false).unwrap();
    assert_eq!(answers(&local6), 1);
    assert_eq!(&local6[local6.len() - 16..], &LOCALHOST6);
    assert!(names::answer(&mut w.rt.names, pid, &[0; 5], &mut |_| false).is_none(), "not a DNS question");
}
