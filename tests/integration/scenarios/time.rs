//! Network time: what targets/qemu/test_time.py checked against public NTP
//! servers, against the shipped runtime/open/sntp.rs with time servers
//! scripted on the fake network link (no network, and no dependence on
//! this Mac reaching UDP port 123). timesync.rs, which joins the client to
//! Radio's sockets, is not host-buildable (net.rs); the datagrams a real
//! socket carries are covered by `make test-open`.
use crate::kernel::{clock, Link};
use crate::open::sntp::{self, Client, Sent, To, Wire, PACKET, PORT, SERVERS};

const SECOND: u64 = 1_000_000_000;
const BUILD: u64 = 1_795_000_000; // the image's build time
const TRUE_NOW: u64 = BUILD + 45 * 86_400; // real UTC when the phone boots
const NTP_UNIX: u64 = 2_208_988_800;
const ADDRESS: [u32; 3] = [0x0a00_0001, 0x0a00_0002, 0x0a00_0003];

/// A reply as a time server builds it: the request's nonce as origin, its clock as receive and transmit.
fn reply(nonce: u64, unix_ns: u64) -> Vec<u8> {
    let mut p = vec![0u8; PACKET];
    p[0] = 4 << 3 | 4;
    p[1] = 2;
    p[24..32].copy_from_slice(&nonce.to_be_bytes());
    let secs = (unix_ns / SECOND + NTP_UNIX) as u32;
    let frac = (((unix_ns % SECOND) << 32) / SECOND) as u32;
    for at in [32, 40] {
        p[at..at + 4].copy_from_slice(&secs.to_be_bytes());
        p[at + 4..at + 8].copy_from_slice(&frac.to_be_bytes());
    }
    p
}

/// The UDP socket timesync.rs opens, over the fake link: a frame is the
/// destination (4 bytes) and the packet; the peer answers with the source
/// (4 bytes) and the reply.
struct Socket<'a> {
    link: &'a mut Link,
    open: bool,
    nonces: u64,
}
impl Wire for Socket<'_> {
    fn open(&mut self) -> bool {
        self.open = true;
        true
    }
    fn close(&mut self) {
        self.open = false;
        self.link.received.clear();
    }
    fn broken(&mut self) -> bool {
        false
    }
    fn random(&mut self) -> Option<u64> {
        self.nonces += 1;
        Some((self.nonces.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5851_f42d_4c95_7f2d).rotate_left(29))
    }
    fn send(&mut self, to: To, packet: &[u8; PACKET]) -> Sent {
        assert!(self.open, "sent on a closed socket");
        // Radio resolves the name; an unknown one goes nowhere.
        let address = match to {
            To::Name(name) => ADDRESS[SERVERS.iter().position(|s| *s == name).unwrap()],
            To::Address(a) => a,
        };
        let mut frame = address.to_be_bytes().to_vec();
        frame.extend_from_slice(packet);
        self.link.send(&frame);
        Sent::Yes
    }
    fn receive(&mut self) -> Option<((u32, u16), Vec<u8>)> {
        let frame = self.link.receive()?;
        Some(((u32::from_be_bytes(frame[..4].try_into().unwrap()), PORT), frame[4..].to_vec()))
    }
}
/// Time servers on the wire: each one's clock error in seconds (None: silent).
fn servers(wrong: [Option<i64>; 3]) -> Link {
    let mut link = Link::default();
    link.peer = Some(Box::new(move |frame: &[u8]| {
        let to = u32::from_be_bytes(frame[..4].try_into().unwrap());
        let k = ADDRESS.iter().position(|a| *a == to)?;
        let nonce = u64::from_be_bytes(frame[4 + 40..4 + 48].try_into().unwrap());
        let says = ((TRUE_NOW * SECOND + clock::now_ns()) as i64 + wrong[k]? * SECOND as i64) as u64;
        let mut answer = to.to_be_bytes().to_vec();
        answer.extend_from_slice(&reply(nonce, says));
        Some(answer)
    }));
    link
}
/// The runtime's loop for `secs`: the client stepped every 10 ms.
fn run(client: &mut Client, link: &mut Link, secs: u64, online: bool) -> u32 {
    let mut rounds = 0;
    let mut socket = Socket { link, open: false, nonces: clock::now_ns() };
    for _ in 0..secs * 100 {
        clock::advance_ns(10_000_000);
        rounds += client.step(&mut socket, clock::now_ns(), online) as u32;
    }
    rounds
}
fn error_ns(client: &Client) -> u64 {
    client.wall.now_ns(clock::now_ns()).abs_diff(TRUE_NOW * SECOND + clock::now_ns())
}

#[test]
fn the_phone_sets_its_clock_and_an_apps_wall_clock_follows() {
    crate::kernel::reset();
    let mut client = Client::new(BUILD);
    let mut link = servers([Some(0); 3]);
    // Before any answer the wall clock is the build time plus uptime: wrong, but never before the build.
    assert!(!client.wall.synced);
    assert_eq!(client.wall.now_ns(0), BUILD * SECOND);
    // Offline: nothing is sent.
    assert_eq!(run(&mut client, &mut link, 20, false), 0);
    assert!(link.sent.is_empty());
    // Online: one round, and the clock an app reads (CLOCK_REALTIME) is true UTC.
    assert_eq!(run(&mut client, &mut link, 20, true), 1);
    assert!(client.wall.synced);
    assert_eq!((client.good, client.failed, client.rejected), (1, 0, 0));
    assert!(error_ns(&client) < 50_000_000, "{} ns off", error_ns(&client));
    // It keeps following the monotonic clock, and asks again only at the refresh.
    let sent = link.sent.len();
    run(&mut client, &mut link, 600, true);
    assert_eq!(link.sent.len(), sent);
    assert!(error_ns(&client) < 50_000_000);
    // No request carries the phone's own time: only a random nonce.
    assert!(link.sent.iter().all(|f| f[5..4 + 40].iter().all(|b| *b == 0)));
}

#[test]
fn one_lying_server_does_not_move_the_clock() {
    crate::kernel::reset();
    let mut client = Client::new(BUILD);
    let mut link = servers([Some(0), Some(86_400 * 365), Some(0)]);
    assert_eq!(run(&mut client, &mut link, 20, true), 1);
    assert!(client.wall.synced && error_ns(&client) < 50_000_000, "{} ns off", error_ns(&client));
}

#[test]
fn without_two_servers_agreeing_the_clock_is_not_set() {
    crate::kernel::reset();
    let mut client = Client::new(BUILD);
    // One silent, the other two an hour apart.
    let mut link = servers([None, Some(0), Some(3600)]);
    run(&mut client, &mut link, 30, true);
    assert!(!client.wall.synced);
    assert_eq!(client.good, 0);
    // A time before the image was built is refused whoever says it.
    let mut client = Client::new(BUILD);
    let mut link = servers([Some(-86_400 * 400); 3]);
    run(&mut client, &mut link, 30, true);
    assert!(!client.wall.synced && client.rejected > 0);
    assert_eq!(client.last_reject, Some(sntp::Reject::Bounds));
}

#[test]
fn an_unplugged_link_leaves_the_clock_alone_and_it_recovers() {
    crate::kernel::reset();
    let mut client = Client::new(BUILD);
    let mut link = servers([Some(0); 3]);
    link.down = true;
    run(&mut client, &mut link, 30, true);
    assert!(!client.wall.synced && client.failed >= 1);
    link.down = false;
    run(&mut client, &mut link, 600, true);
    assert!(client.wall.synced && error_ns(&client) < 50_000_000);
}
