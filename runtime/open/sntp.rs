//! Network time: an SNTP client (RFC 4330, checks from RFC 5905) and the
//! rules for moving the wall clock. Pure code with no devices, so
//! `make test-unit` runs it on the host, the driver (`Client::step`)
//! included; timesync.rs only supplies the socket and the random numbers.
//!
//! SNTP is not authenticated. What this defends against is a wrong or
//! off-path-spoofed answer, and one server (or someone on the path to one
//! server) answering for the others:
//!
//! - every request carries its own 64 random bits, drawn separately from the
//!   random generator, which the answer must echo. Seeing one request tells
//!   nothing about another, and without the generator no round starts;
//! - each server is asked twice. The first request goes to its name and
//!   finds its address; the second goes to that address, and only an answer
//!   from that address and port 123 is timed. No two servers of a round may
//!   answer from one address, so one address is one vote;
//! - one server alone never sets the clock (two of three must agree);
//! - the time must lie between this image's build and twenty years after it;
//! - once the clock is set, a jump of more than ten minutes must be seen in
//!   two rounds running.
//!
//! The second request also keeps name lookups out of the timing: it leaves
//! as soon as Radio next runs, where the first may wait seconds for DNS.
//! A network that controls every answer (or the paths to two servers) can
//! still choose the time inside those limits.

use alloc::vec::Vec;

pub const PORT: u16 = 123;
pub const PACKET: usize = 48;
/// Three independent operators.
pub const SERVERS: [&str; 3] = ["time.cloudflare.com", "time.google.com", "pool.ntp.org"];
const SECOND: u64 = 1_000_000_000;
/// Seconds from 1900 (NTP) to 1970 (Unix).
const NTP_UNIX: u64 = 2_208_988_800;
/// A timed reply later than this after its request is not used: half of the
/// round trip is the most the clock can be wrong by.
pub const MAX_ROUND_TRIP_NS: u64 = SECOND;
/// Servers' answers must agree this closely.
pub const AGREE_NS: u64 = 2 * SECOND;
/// The furthest a server's own clock may be from its reference (root
/// distance: half the root delay plus the dispersion), in 1/65536 s.
const MAX_ROOT_DISTANCE: u64 = 3 * 65536 / 2;
/// A round ends after this long, answered or not. It covers three name
/// lookups and two exchanges with each server.
pub const ROUND_NS: u64 = 8 * SECOND;
/// After joining a network, wait this long before asking.
pub const SETTLE_NS: u64 = 5 * SECOND;
/// Never ask more often than this.
pub const MIN_INTERVAL_NS: u64 = 60 * SECOND;
/// After a good round, ask again this much later.
pub const REFRESH_NS: u64 = 6 * 3600 * SECOND;
/// A change larger than this to a clock already set needs a second round.
pub const SUSPICIOUS_NS: u64 = 600 * SECOND;
/// The clock is never set later than this long after the image was built.
pub const MAX_AGE_S: u64 = 20 * 366 * 86_400;

/// A request: version 4, client mode, and `nonce` as the transmit timestamp
/// (the server echoes it as the origin; our real time is not sent).
pub fn request(nonce: u64) -> [u8; PACKET] {
    let mut p = [0u8; PACKET];
    p[0] = 4 << 3 | 3;
    p[40..48].copy_from_slice(&nonce.to_be_bytes());
    p
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reject {
    Short,
    /// Not a server's reply.
    Mode,
    Version,
    /// The origin is not the number we sent: stale, or not from our server.
    Origin,
    /// The server says its own clock is not set (leap indicator 3).
    Unsynchronised,
    /// Stratum 0: the server refuses ("kiss of death").
    KissOfDeath,
    Stratum,
    RootDistance,
    /// A zero timestamp, or one before 1970.
    Timestamp,
    /// Transmitted before received, or processing longer than the round trip.
    Order,
    /// The reply took too long.
    RoundTrip,
    /// Earlier than this image or implausibly far in the future.
    Bounds,
    /// Not from port 123, not from the address the request went to, or from
    /// an address another server of the round already answers from.
    Source,
}

/// One server's answer: when our monotonic clock read `mono_ns`, UTC was
/// `unix_ns` (nanoseconds since 1970), give or take half of `delay_ns`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sample {
    pub unix_ns: u64,
    pub mono_ns: u64,
    pub delay_ns: u64,
}

impl Sample {
    /// UTC when the monotonic clock read zero.
    fn base(&self) -> Option<u64> {
        self.unix_ns.checked_sub(self.mono_ns)
    }
}

/// A 64-bit NTP timestamp as Unix nanoseconds. Seconds with the top bit
/// clear are in the era that starts in 2036.
fn timestamp(b: &[u8]) -> Option<u64> {
    let secs = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
    let frac = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as u64;
    if secs == 0 && frac == 0 {
        return None;
    }
    let unix = if secs & 0x8000_0000 != 0 { secs.checked_sub(NTP_UNIX)? } else { secs + (1 << 32) - NTP_UNIX };
    Some(unix * SECOND + ((frac * SECOND) >> 32))
}

fn short(b: &[u8]) -> u64 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64
}

/// Check that `p` is a usable server's reply to the request that carried
/// `nonce`; returns the server's receive and transmit times.
fn check(p: &[u8], nonce: u64) -> Result<(u64, u64), Reject> {
    if p.len() < PACKET {
        return Err(Reject::Short);
    }
    if p[0] & 7 != 4 {
        return Err(Reject::Mode);
    }
    if !matches!((p[0] >> 3) & 7, 3 | 4) {
        return Err(Reject::Version);
    }
    if p[24..32] != nonce.to_be_bytes() {
        return Err(Reject::Origin);
    }
    if p[0] >> 6 == 3 {
        return Err(Reject::Unsynchronised);
    }
    match p[1] {
        0 => return Err(Reject::KissOfDeath),
        1..=15 => {}
        _ => return Err(Reject::Stratum),
    }
    if short(&p[4..8]) / 2 + short(&p[8..12]) > MAX_ROOT_DISTANCE {
        return Err(Reject::RootDistance);
    }
    let received = timestamp(&p[32..40]).ok_or(Reject::Timestamp)?;
    let transmitted = timestamp(&p[40..48]).ok_or(Reject::Timestamp)?;
    Ok((received, transmitted))
}

/// Check a reply to the request that carried `nonce`, sent at `sent_ns` and
/// received at `received_ns` on the monotonic clock.
pub fn parse(p: &[u8], nonce: u64, sent_ns: u64, received_ns: u64) -> Result<Sample, Reject> {
    let (received, transmitted) = check(p, nonce)?;
    let round_trip = received_ns.checked_sub(sent_ns).ok_or(Reject::Order)?;
    if round_trip > MAX_ROUND_TRIP_NS {
        return Err(Reject::RoundTrip);
    }
    let processing = transmitted.checked_sub(received).ok_or(Reject::Order)?;
    let delay_ns = round_trip.checked_sub(processing).ok_or(Reject::Order)?;
    Ok(Sample { unix_ns: transmitted + delay_ns / 2, mono_ns: received_ns, delay_ns })
}

/// The time the servers agree on, as UTC at monotonic zero: with three
/// answers the middle one if another is within AGREE_NS of it, with two the
/// one with the shorter delay if they agree, with one nothing.
pub fn combine(samples: &[Option<Sample>; 3]) -> Option<u64> {
    let mut found: [(u64, u64); 3] = [(0, 0); 3];
    let mut n = 0;
    for s in samples.iter().flatten() {
        if let Some(base) = s.base() {
            found[n] = (base, s.delay_ns);
            n += 1;
        }
    }
    let found = &mut found[..n];
    // At most three: sorted by hand (a library sort is a kilobyte of code).
    for i in 1..n {
        let mut j = i;
        while j > 0 && found[j - 1] > found[j] {
            found.swap(j - 1, j);
            j -= 1;
        }
    }
    match *found {
        [a, b] if b.0 - a.0 <= AGREE_NS => Some(if a.1 <= b.1 { a.0 } else { b.0 }),
        [a, b, c] if b.0 - a.0 <= AGREE_NS || c.0 - b.0 <= AGREE_NS => Some(b.0),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Applied {
    /// The clock now follows the servers.
    Set,
    /// A large change to a clock already set: kept back until the next
    /// round says the same.
    Held,
}

/// The wall clock: UTC = base + monotonic time. It starts at the image's
/// build time, which is all the phone knows until a network answers.
pub struct Wall {
    base_ns: u64,
    pub synced: bool,
    /// Monotonic time of the last accepted round.
    pub synced_at_ns: u64,
    held: Option<u64>,
}

impl Wall {
    pub const fn new(build_epoch_s: u64) -> Self {
        Self { base_ns: build_epoch_s * SECOND, synced: false, synced_at_ns: 0, held: None }
    }
    pub fn now_ns(&self, mono_ns: u64) -> u64 {
        self.base_ns.saturating_add(mono_ns)
    }
    pub fn apply(&mut self, base_ns: u64, mono_ns: u64) -> Applied {
        if self.synced && base_ns.abs_diff(self.base_ns) > SUSPICIOUS_NS {
            let confirmed = self.held.is_some_and(|h| h.abs_diff(base_ns) <= AGREE_NS);
            if !confirmed {
                self.held = Some(base_ns);
                return Applied::Held;
            }
        }
        self.held = None;
        self.base_ns = base_ns;
        self.synced = true;
        self.synced_at_ns = mono_ns;
        Applied::Set
    }
}

/// What `poll` decided.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Do {
    Nothing,
    /// A round began: open a socket.
    Start,
    /// The round is over: close the socket.
    Finish,
}

/// Where a request goes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum To {
    /// The server's name: a round's first request, which finds its address.
    Name(&'static str),
    /// The address that answered: the request whose answer is timed.
    Address(u32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sent {
    Yes,
    /// No room now: try on the next step.
    Later,
    /// It cannot be sent (no such name, a refused address).
    Never,
}

/// The network and the random generator as a round uses them.
pub trait Wire {
    /// Open the round's UDP socket.
    fn open(&mut self) -> bool;
    fn close(&mut self);
    /// The socket is gone or failed.
    fn broken(&mut self) -> bool;
    /// 64 fresh bits from the random generator, independent of every earlier
    /// call. None if the generator cannot supply them.
    fn random(&mut self) -> Option<u64>;
    fn send(&mut self, to: To, packet: &[u8; PACKET]) -> Sent;
    /// The next datagram received: its source address and port, and payload.
    fn receive(&mut self) -> Option<((u32, u16), Vec<u8>)>;
}

/// One server's part in a round.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Server {
    /// Nothing sent yet.
    Unasked,
    /// The request to its name is out.
    Probing { nonce: u64 },
    /// It answered from `address`: the timed request goes there.
    Found { address: u32 },
    Timing { address: u32, nonce: u64, sent_ns: u64 },
    /// Answered, or left out of the round (address 0 if it never had one).
    Closed { address: u32 },
}

impl Server {
    fn address(&self) -> u32 {
        match *self {
            Server::Found { address } | Server::Timing { address, .. } | Server::Closed { address } => address,
            _ => 0,
        }
    }
}

struct Round {
    started_ns: u64,
    servers: [Server; 3],
    samples: [Option<Sample>; 3],
}

pub struct Client {
    pub wall: Wall,
    min_unix_ns: u64,
    max_unix_ns: u64,
    round: Option<Round>,
    next_ns: u64,
    last_round_ns: Option<u64>,
    failures: u32,
    /// Rounds that set the clock, rounds that did not, and replies refused.
    pub good: u32,
    pub failed: u32,
    pub rejected: u32,
    pub last_reject: Option<Reject>,
}

impl Client {
    pub const fn new(build_epoch_s: u64) -> Self {
        Self {
            wall: Wall::new(build_epoch_s),
            min_unix_ns: build_epoch_s * SECOND,
            max_unix_ns: (build_epoch_s + MAX_AGE_S) * SECOND,
            round: None,
            next_ns: 0,
            last_round_ns: None,
            failures: 0,
            good: 0,
            failed: 0,
            rejected: 0,
            last_reject: None,
        }
    }

    pub fn poll(&mut self, now_ns: u64, online: bool) -> Do {
        if let Some(r) = &self.round {
            let answered = r.servers.iter().all(|s| matches!(s, Server::Closed { .. }));
            if answered || now_ns.saturating_sub(r.started_ns) >= ROUND_NS {
                self.finish(now_ns);
                return Do::Finish;
            }
            return Do::Nothing;
        }
        if !online {
            // Not before the network has been back for a moment.
            self.next_ns = self.next_ns.max(now_ns + SETTLE_NS);
            return Do::Nothing;
        }
        let earliest = self.last_round_ns.map_or(0, |t| t + MIN_INTERVAL_NS);
        if now_ns < self.next_ns.max(earliest) {
            return Do::Nothing;
        }
        self.last_round_ns = Some(now_ns);
        self.round = Some(Round { started_ns: now_ns, servers: [Server::Unasked; 3], samples: [None; 3] });
        Do::Start
    }

    /// Run the clock's network side: call it on every poll of the runtime.
    /// True when a round ended in this call (worth a line in the log).
    pub fn step(&mut self, wire: &mut dyn Wire, now_ns: u64, online: bool) -> bool {
        match self.poll(now_ns, online) {
            Do::Start => {
                if !wire.open() {
                    self.abort(now_ns);
                    return true;
                }
            }
            Do::Finish => {
                wire.close();
                return true;
            }
            Do::Nothing => {}
        }
        if self.round.is_none() {
            return false;
        }
        if wire.broken() {
            return self.give_up(wire, now_ns);
        }
        for k in 0..SERVERS.len() {
            let Some(to) = self.wants(k) else { continue };
            // Each request has random bits of its own. Without the generator
            // there is no round: a guessable request proves nothing.
            let Some(nonce) = wire.random() else { return self.give_up(wire, now_ns) };
            match wire.send(to, &request(nonce)) {
                Sent::Yes => self.sent(k, nonce, now_ns),
                Sent::Later => {}
                Sent::Never => self.leave_out(k),
            }
        }
        while let Some((from, data)) = wire.receive() {
            self.reply(from, &data, now_ns);
        }
        false
    }

    fn give_up(&mut self, wire: &mut dyn Wire, now_ns: u64) -> bool {
        self.abort(now_ns);
        wire.close();
        true
    }

    /// What server `index` needs sent now, if anything.
    fn wants(&self, index: usize) -> Option<To> {
        match *self.round.as_ref()?.servers.get(index)? {
            Server::Unasked => Some(To::Name(SERVERS[index])),
            Server::Found { address } => Some(To::Address(address)),
            _ => None,
        }
    }

    /// The request `wants` asked for left at `now_ns` carrying `nonce`.
    fn sent(&mut self, index: usize, nonce: u64, now_ns: u64) {
        let Some(s) = self.round.as_mut().and_then(|r| r.servers.get_mut(index)) else { return };
        *s = match *s {
            Server::Unasked => Server::Probing { nonce },
            Server::Found { address } => Server::Timing { address, nonce, sent_ns: now_ns },
            other => other,
        };
    }

    /// Server `index` takes no (further) part in this round.
    fn leave_out(&mut self, index: usize) {
        if let Some(s) = self.round.as_mut().and_then(|r| r.servers.get_mut(index)) {
            *s = Server::Closed { address: s.address() };
        }
    }

    /// A datagram from `from` (address, port) arrived on the round's socket.
    pub fn reply(&mut self, from: (u32, u16), data: &[u8], now_ns: u64) {
        let (min, max) = (self.min_unix_ns, self.max_unix_ns);
        let Some(round) = self.round.as_mut() else { return };
        let mut result = Err(Reject::Origin);
        if from.1 != PORT || from.0 == 0 {
            result = Err(Reject::Source);
        } else {
            for k in 0..3 {
                match round.servers[k] {
                    Server::Probing { nonce } => match check(data, nonce) {
                        Err(Reject::Origin) => continue,
                        // Another server of this round answers from the same
                        // address: one address is one vote.
                        Ok(_) if round.servers.iter().any(|s| s.address() == from.0) => {
                            round.servers[k] = Server::Closed { address: 0 };
                            result = Err(Reject::Source);
                        }
                        Ok(_) => {
                            round.servers[k] = Server::Found { address: from.0 };
                            result = Ok(());
                        }
                        // It answered, and cannot be used (unsynchronised, a refusal).
                        Err(e) => {
                            round.servers[k] = Server::Closed { address: 0 };
                            result = Err(e);
                        }
                    },
                    Server::Timing { address, nonce, sent_ns } => match parse(data, nonce, sent_ns, now_ns) {
                        Err(Reject::Origin) => continue,
                        // The right number from the wrong place: the request
                        // stays open for the server it was sent to.
                        _ if from.0 != address => result = Err(Reject::Source),
                        Ok(s) if s.unix_ns >= min && s.unix_ns <= max => {
                            round.samples[k] = Some(s);
                            round.servers[k] = Server::Closed { address };
                            result = Ok(());
                        }
                        // One answer per request, usable or not.
                        other => {
                            round.servers[k] = Server::Closed { address };
                            result = Err(other.err().unwrap_or(Reject::Bounds));
                        }
                    },
                    _ => continue,
                }
                break;
            }
        }
        if let Err(e) = result {
            self.rejected = self.rejected.saturating_add(1);
            self.last_reject = Some(e);
        }
    }

    /// The socket could not be opened or failed: the round is lost.
    pub fn abort(&mut self, now_ns: u64) {
        if self.round.take().is_some() {
            self.fail(now_ns);
        }
    }

    fn fail(&mut self, now_ns: u64) {
        self.failed = self.failed.saturating_add(1);
        self.failures = (self.failures + 1).min(7);
        // 2, 4, 8 ... minutes, at most an hour.
        self.next_ns = now_ns + (MIN_INTERVAL_NS << self.failures).min(3600 * SECOND);
    }

    fn finish(&mut self, now_ns: u64) {
        let Some(round) = self.round.take() else { return };
        match combine(&round.samples) {
            Some(base) => match self.wall.apply(base, now_ns) {
                Applied::Set => {
                    self.good = self.good.saturating_add(1);
                    self.failures = 0;
                    self.next_ns = now_ns + REFRESH_NS;
                }
                // Confirm (or forget) the jump as soon as servers may be asked again.
                Applied::Held => self.next_ns = now_ns + MIN_INTERVAL_NS,
            },
            None => self.fail(now_ns),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD: u64 = 1_795_000_000; // November 2026
    const NOW: u64 = BUILD + 30 * 86_400;

    /// A server's reply to `nonce`, received and sent at Unix `unix_ns`.
    fn reply(nonce: u64, unix_ns: u64) -> [u8; PACKET] {
        reply_between(nonce, unix_ns, unix_ns)
    }
    fn ntp(unix_ns: u64) -> [u8; 8] {
        let secs = (unix_ns / SECOND + NTP_UNIX) as u32;
        let frac = (((unix_ns % SECOND) << 32) / SECOND) as u32;
        let mut b = [0; 8];
        b[..4].copy_from_slice(&secs.to_be_bytes());
        b[4..].copy_from_slice(&frac.to_be_bytes());
        b
    }
    fn reply_between(nonce: u64, received: u64, transmitted: u64) -> [u8; PACKET] {
        let mut p = [0u8; PACKET];
        p[0] = 4 << 3 | 4;
        p[1] = 2;
        p[24..32].copy_from_slice(&nonce.to_be_bytes());
        p[32..40].copy_from_slice(&ntp(received));
        p[40..48].copy_from_slice(&ntp(transmitted));
        p
    }

    #[test]
    fn request_packet() {
        let p = request(0x0102_0304_0506_0708);
        assert_eq!(p[0], 0x23); // LI 0, version 4, mode 3 (RFC 4330 section 5)
        assert!(p[1..40].iter().all(|b| *b == 0));
        assert_eq!(p[40..], [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn timestamps() {
        // RFC 5905 figure 4: Unix zero is NTP 2,208,988,800.
        assert_eq!(timestamp(&[0x83, 0xaa, 0x7e, 0x80, 0, 0, 0, 0]), Some(0));
        // Half a second.
        assert_eq!(timestamp(&[0x83, 0xaa, 0x7e, 0x81, 0x80, 0, 0, 0]), Some(1_500_000_000));
        // The last second of era 0 (7 February 2036 06:28:15 UTC) and the first of era 1.
        assert_eq!(timestamp(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]), Some(2_085_978_495 * SECOND));
        assert_eq!(timestamp(&[0, 0, 0, 0, 0, 0, 0, 1]), Some(2_085_978_496 * SECOND));
        assert_eq!(timestamp(&[0, 0, 0, 1, 0, 0, 0, 0]), Some(2_085_978_497 * SECOND));
        // Zero means "not set"; 1968 is before the Unix epoch.
        assert_eq!(timestamp(&[0; 8]), None);
        assert_eq!(timestamp(&[0x80, 0, 0, 0, 0, 0, 0, 0]), None);
        // Our own encoder round-trips to the nanosecond or one under.
        let t = NOW * SECOND + 123_456_789;
        assert!(t - timestamp(&ntp(t)).unwrap() <= 1);
    }

    #[test]
    fn good_reply() {
        // Sent at 10 s, back at 10.2 s; the server held it 50 ms.
        let unix = NOW * SECOND;
        let p = reply_between(7, unix, unix + 50_000_000);
        let s = parse(&p, 7, 10 * SECOND, 10 * SECOND + 200_000_000).unwrap();
        assert!(s.delay_ns.abs_diff(150_000_000) <= 1);
        assert_eq!(s.mono_ns, 10 * SECOND + 200_000_000);
        // UTC on arrival is the transmit time plus half the network delay.
        assert!(s.unix_ns.abs_diff(unix + 50_000_000 + 75_000_000) <= 2);
        // Version 3 servers are accepted.
        let mut v3 = p;
        v3[0] = 3 << 3 | 4;
        assert!(parse(&v3, 7, 0, SECOND).is_ok());
        // Trailing bytes (extensions, a MAC) are ignored.
        let mut long = [0u8; 68];
        long[..48].copy_from_slice(&p);
        assert!(parse(&long, 7, 0, SECOND).is_ok());
    }

    #[test]
    fn bad_replies() {
        let unix = NOW * SECOND;
        let good = reply(7, unix);
        let check = |edit: &dyn Fn(&mut [u8; PACKET]), want: Reject| {
            let mut p = good;
            edit(&mut p);
            assert_eq!(parse(&p, 7, 0, SECOND), Err(want));
        };
        assert_eq!(parse(&good[..47], 7, 0, SECOND), Err(Reject::Short));
        assert_eq!(parse(&[], 7, 0, SECOND), Err(Reject::Short));
        // Our own request reflected back, a broadcast, symmetric mode.
        check(&|p| p[0] = 4 << 3 | 3, Reject::Mode);
        check(&|p| p[0] = 4 << 3 | 5, Reject::Mode);
        check(&|p| p[0] = 4 << 3 | 1, Reject::Mode);
        check(&|p| p[0] = 2 << 3 | 4, Reject::Version);
        check(&|p| p[0] = 7 << 3 | 4, Reject::Version);
        // A spoofed or stale answer: wrong origin, by one bit or entirely.
        check(&|p| p[31] ^= 1, Reject::Origin);
        check(&|p| p[24..32].fill(0), Reject::Origin);
        assert_eq!(parse(&good, 8, 0, SECOND), Err(Reject::Origin));
        check(&|p| p[0] |= 3 << 6, Reject::Unsynchronised);
        check(&|p| p[1] = 0, Reject::KissOfDeath);
        check(&|p| p[1] = 16, Reject::Stratum);
        check(&|p| p[1] = 255, Reject::Stratum);
        // Root delay 4 s, or dispersion 2 s: the server does not know the time well.
        check(&|p| p[4..8].copy_from_slice(&(4u32 << 16).to_be_bytes()), Reject::RootDistance);
        check(&|p| p[8..12].copy_from_slice(&(2u32 << 16).to_be_bytes()), Reject::RootDistance);
        check(&|p| p[40..48].fill(0), Reject::Timestamp);
        check(&|p| p[32..40].fill(0), Reject::Timestamp);
        check(&|p| p[40..48].copy_from_slice(&ntp(unix - SECOND)), Reject::Order);
        // Held by the server longer than the whole exchange took.
        check(&|p| p[40..48].copy_from_slice(&ntp(unix + 2 * SECOND)), Reject::Order);
        assert_eq!(parse(&good, 7, 0, MAX_ROUND_TRIP_NS + 1), Err(Reject::RoundTrip));
        assert_eq!(parse(&good, 7, 5, 4), Err(Reject::Order));
        // Leap second warnings (LI 1 and 2) are fine.
        let mut leap = good;
        leap[0] |= 1 << 6;
        assert!(parse(&leap, 7, 0, SECOND).is_ok());
        // Mild root values are fine.
        let mut root = good;
        root[4..8].copy_from_slice(&0x0000_8000u32.to_be_bytes());
        root[8..12].copy_from_slice(&0x0000_4000u32.to_be_bytes());
        assert!(parse(&root, 7, 0, SECOND).is_ok());
    }

    fn sample(base_s: u64, delay_ms: u64) -> Option<Sample> {
        Some(Sample { unix_ns: base_s * SECOND + 5 * SECOND, mono_ns: 5 * SECOND, delay_ns: delay_ms * 1_000_000 })
    }

    #[test]
    fn combining() {
        assert_eq!(combine(&[None, None, None]), None);
        // One server alone never sets the clock.
        assert_eq!(combine(&[sample(NOW, 10), None, None]), None);
        // Two that agree: the one with the shorter delay.
        assert_eq!(combine(&[sample(NOW, 80), None, sample(NOW + 1, 10)]), Some((NOW + 1) * SECOND));
        assert_eq!(combine(&[sample(NOW + 3, 10), sample(NOW, 10), None]), None);
        // Three: the middle one, whatever the outlier says.
        assert_eq!(combine(&[sample(NOW + 1, 1), sample(NOW, 1), sample(NOW + 2, 1)]), Some((NOW + 1) * SECOND));
        assert_eq!(combine(&[sample(NOW, 1), sample(NOW + 1_000_000, 1), sample(NOW + 1, 1)]), Some((NOW + 1) * SECOND));
        assert_eq!(combine(&[sample(1, 1), sample(NOW + 1, 1), sample(NOW, 1)]), Some(NOW * SECOND));
        // Three that all disagree.
        assert_eq!(combine(&[sample(NOW, 1), sample(NOW + 100, 1), sample(NOW + 200, 1)]), None);
        // A sample from before our own uptime cannot be placed.
        let odd = Some(Sample { unix_ns: 3, mono_ns: 5, delay_ns: 0 });
        assert_eq!(combine(&[odd, sample(NOW, 1), None]), None);
    }

    #[test]
    fn wall_clock_steps() {
        let mut w = Wall::new(BUILD);
        assert_eq!(w.now_ns(3 * SECOND), (BUILD + 3) * SECOND);
        assert!(!w.synced);
        // The first answer is taken whatever the distance (within the client's bounds).
        assert_eq!(w.apply(NOW * SECOND, 100), Applied::Set);
        assert!(w.synced);
        assert_eq!((w.now_ns(SECOND), w.synced_at_ns), ((NOW + 1) * SECOND, 100));
        // Small corrections, either way, apply at once.
        assert_eq!(w.apply((NOW - 2) * SECOND, 200), Applied::Set);
        assert_eq!(w.apply((NOW + 500) * SECOND, 300), Applied::Set);
        assert_eq!(w.now_ns(0), (NOW + 500) * SECOND);
        // A jump of a day is held until a second round agrees.
        let day = (NOW + 500 + 86_400) * SECOND;
        assert_eq!(w.apply(day, 400), Applied::Held);
        assert_eq!(w.now_ns(0), (NOW + 500) * SECOND);
        assert_eq!(w.apply(day + SECOND, 500), Applied::Set);
        assert_eq!((w.now_ns(0), w.synced_at_ns), (day + SECOND, 500));
        // Two different jumps do not confirm each other.
        let back = (NOW - 86_400) * SECOND;
        assert_eq!(w.apply(back, 600), Applied::Held);
        assert_eq!(w.apply(back - 3600 * SECOND, 700), Applied::Held);
        assert_eq!(w.apply(back - 3600 * SECOND, 800), Applied::Set);
        // A held jump is forgotten once a normal answer arrives.
        let here = w.now_ns(0);
        assert_eq!(w.apply(here + 86_400 * SECOND, 900), Applied::Held);
        assert_eq!(w.apply(here + SECOND, 1000), Applied::Set);
        assert_eq!(w.apply(here + 86_400 * SECOND, 1100), Applied::Held);
    }

    const ADDRESS: [u32; 3] = [0x0a00_0001, 0x0a00_0002, 0x0a00_0003];
    const EVIL: u32 = 0x0a00_0666;
    const STEP: u64 = 10_000_000;

    /// A network of time servers for `Client::step`. True UTC is NOW plus the
    /// monotonic clock; each server may be silent or wrong by whole seconds.
    struct Net {
        now: u64,
        open: bool,
        broken: bool,
        /// The random generator has nothing to give.
        dry: bool,
        /// The socket has no room.
        full: bool,
        counter: u64,
        drawn: Vec<u64>,
        /// Each name's address (0: no such name) and its clock's error in
        /// seconds (None: it never answers).
        hosts: [(u32, Option<i64>); 3],
        /// How long a request to a name waits for the lookup.
        dns_ns: u64,
        /// One way.
        latency_ns: u64,
        sent: Vec<(To, u64)>,
        inbox: Vec<(u64, (u32, u16), Vec<u8>)>,
    }

    impl Net {
        fn new(clocks: [Option<i64>; 3]) -> Self {
            Net {
                now: 0, open: false, broken: false, dry: false, full: false, counter: 0, drawn: Vec::new(),
                hosts: [(ADDRESS[0], clocks[0]), (ADDRESS[1], clocks[1]), (ADDRESS[2], clocks[2])],
                dns_ns: 0, latency_ns: STEP, sent: Vec::new(), inbox: Vec::new(),
            }
        }
    }

    impl Wire for Net {
        fn open(&mut self) -> bool {
            self.open = true;
            true
        }
        fn close(&mut self) {
            self.open = false;
            self.inbox.clear();
        }
        fn broken(&mut self) -> bool {
            self.broken
        }
        fn random(&mut self) -> Option<u64> {
            if self.dry {
                return None;
            }
            // Stands in for the generator: distinct, and not a sequence the
            // tests' attackers are given.
            self.counter += 1;
            let v = (self.counter.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5851_f42d_4c95_7f2d).rotate_left(29);
            self.drawn.push(v);
            Some(v)
        }
        fn send(&mut self, to: To, packet: &[u8; PACKET]) -> Sent {
            assert!(self.open, "sent on a closed socket");
            if self.full {
                return Sent::Later;
            }
            let nonce = u64::from_be_bytes(packet[40..48].try_into().unwrap());
            let (host, leaves) = match to {
                To::Name(name) => {
                    let k = SERVERS.iter().position(|s| *s == name).unwrap();
                    if self.hosts[k].0 == 0 {
                        return Sent::Never;
                    }
                    (Some(k), self.now + self.dns_ns)
                }
                To::Address(a) => (self.hosts.iter().position(|h| h.0 == a), self.now),
            };
            self.sent.push((to, nonce));
            if let Some((address, Some(wrong_s))) = host.map(|k| self.hosts[k]) {
                let there = leaves + self.latency_ns;
                let says = ((NOW * SECOND + there) as i64 + wrong_s * SECOND as i64) as u64;
                self.inbox.push((there + self.latency_ns, (address, PORT), reply(nonce, says).to_vec()));
            }
            Sent::Yes
        }
        fn receive(&mut self) -> Option<((u32, u16), Vec<u8>)> {
            let i = self.inbox.iter().position(|d| d.0 <= self.now)?;
            let (_, from, data) = self.inbox.remove(i);
            Some((from, data))
        }
    }

    /// Step from `start`, where a round must begin, until it ends; `each`
    /// runs after every step (an attacker's turn).
    fn run_with(c: &mut Client, net: &mut Net, start: u64, each: &mut dyn FnMut(&mut Net)) -> u64 {
        let mut now = start;
        loop {
            net.now = now;
            if c.step(net, now, true) {
                return now;
            }
            assert!(c.round.is_some(), "no round at {now}");
            each(net);
            now += STEP;
        }
    }
    fn run(c: &mut Client, net: &mut Net, start: u64) -> u64 {
        run_with(c, net, start, &mut |_| {})
    }
    /// How far the clock is from true UTC.
    fn error_ns(c: &Client) -> u64 {
        c.wall.now_ns(0).abs_diff(NOW * SECOND)
    }

    #[test]
    fn client_sets_the_clock() {
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        assert!(!c.step(&mut net, 0, false));
        assert!(!c.step(&mut net, SECOND, true)); // settling
        assert!(!net.open && net.sent.is_empty());
        let end = run(&mut c, &mut net, SETTLE_NS);
        // Two exchanges of 20 ms, a step between them and one to notice the end.
        assert_eq!(end, SETTLE_NS + 6 * STEP);
        assert!(c.wall.synced && !net.open);
        assert_eq!((c.good, c.failed, c.rejected), (1, 0, 0));
        assert!(error_ns(&c) <= 4, "{}", error_ns(&c));
        // First each name, then each address that answered.
        let to: Vec<To> = net.sent.iter().map(|s| s.0).collect();
        assert_eq!(to[..3], [To::Name(SERVERS[0]), To::Name(SERVERS[1]), To::Name(SERVERS[2])]);
        assert_eq!(to[3..], [To::Address(ADDRESS[0]), To::Address(ADDRESS[1]), To::Address(ADDRESS[2])]);
        // Nothing more until the refresh.
        assert_eq!(c.poll(end + REFRESH_NS - 1, true), Do::Nothing);
        assert_eq!(c.poll(end + REFRESH_NS, true), Do::Start);
        assert_eq!(c.wants(3), None);
    }

    #[test]
    fn every_request_has_random_bits_of_its_own() {
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        // No room in the socket: nothing is recorded as sent, and it is tried again.
        net.full = true;
        net.now = SETTLE_NS;
        assert!(!c.step(&mut net, SETTLE_NS, true));
        assert!(net.sent.is_empty());
        assert!((0..3).all(|k| c.wants(k) == Some(To::Name(SERVERS[k]))));
        net.full = false;
        net.drawn.clear();
        net.now = SETTLE_NS + STEP;
        assert!(!c.step(&mut net, SETTLE_NS + STEP, true));
        let mut now = SETTLE_NS + 2 * STEP;
        while !c.step(&mut net, now, true) {
            now += STEP;
            net.now = now;
        }
        assert!(c.wall.synced);
        // Six requests, six draws from the generator, used as drawn: no
        // request's number is derived from another's.
        let nonces: Vec<u64> = net.sent.iter().map(|s| s.1).collect();
        assert_eq!(nonces.len(), 6);
        assert_eq!(nonces, net.drawn);
        for (i, a) in nonces.iter().enumerate() {
            assert!(nonces[i + 1..].iter().all(|b| a != b));
        }
    }

    #[test]
    fn no_round_without_the_random_generator() {
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        net.dry = true;
        net.now = SETTLE_NS;
        assert!(c.step(&mut net, SETTLE_NS, true));
        assert!(net.sent.is_empty() && !net.open);
        assert_eq!((c.good, c.failed, c.wall.synced), (0, 1, false));
        // It runs dry between the two exchanges: the round is dropped, and
        // the answers already in hand set nothing.
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        net.now = SETTLE_NS;
        assert!(!c.step(&mut net, SETTLE_NS, true));
        net.dry = true;
        let end = run(&mut c, &mut net, SETTLE_NS + STEP);
        // The answers arrive two steps on; the next step wants a number.
        assert_eq!(end, SETTLE_NS + 3 * STEP);
        assert_eq!((net.sent.len(), c.failed, c.wall.synced, net.open), (3, 1, false, false));
    }

    #[test]
    fn one_address_is_one_vote() {
        // The worst case: someone who answers first, from one address, and
        // knows the number in every request of the round (as one server did
        // when the numbers were derived from each other). The honest
        // servers are slower.
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        net.latency_ns = 20 * STEP;
        let lie = (NOW + 5 * 366 * 86_400) * SECOND;
        let mut answered = 0;
        run_with(&mut c, &mut net, SETTLE_NS, &mut |net| {
            for i in answered..net.sent.len() {
                let forged = reply(net.sent[i].1, lie + net.now);
                net.inbox.push((net.now, (EVIL, PORT), forged.to_vec()));
            }
            answered = net.sent.len();
        });
        // It got one server's vote at most, which sets nothing.
        assert!(!c.wall.synced);
        assert_eq!(c.wall.now_ns(0), BUILD * SECOND);
        assert_eq!((c.good, c.failed), (0, 1));
        assert!(c.rejected >= 2);

        // The same, but it can only see the requests sent to it (it runs the
        // third server, or sits on the path to it), and answers those from
        // its own address three times over. The honest two outvote it.
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0), Some(0), None]);
        let mut answered = 0;
        run_with(&mut c, &mut net, SETTLE_NS, &mut |net| {
            for i in answered..net.sent.len() {
                let (to, nonce) = net.sent[i];
                if to == To::Name(SERVERS[2]) || to == To::Address(ADDRESS[2]) {
                    for guess in [nonce, nonce.wrapping_sub(0x9e37_79b9_7f4a_7c15), nonce.wrapping_add(0x9e37_79b9_7f4a_7c15)] {
                        net.inbox.push((net.now, (ADDRESS[2], PORT), reply(guess, lie + net.now).to_vec()));
                    }
                }
            }
            answered = net.sent.len();
        });
        assert!(c.wall.synced);
        assert!(error_ns(&c) <= 4);
    }

    #[test]
    fn name_lookups_are_not_timed() {
        // Each name takes two seconds to resolve. Only the second request,
        // to the address, is timed, so the clock is still right.
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        net.dns_ns = 2 * SECOND;
        let end = run(&mut c, &mut net, SETTLE_NS);
        assert!(end > SETTLE_NS + 2 * SECOND);
        assert!(c.wall.synced);
        assert!(error_ns(&c) <= 4, "{}", error_ns(&c));
        // A path too slow to time well is refused, not averaged in.
        let mut c = Client::new(BUILD);
        let mut net = Net::new([Some(0); 3]);
        net.latency_ns = MAX_ROUND_TRIP_NS / 2 + STEP;
        run(&mut c, &mut net, SETTLE_NS);
        assert!(!c.wall.synced);
        assert_eq!((c.rejected, c.last_reject), (3, Some(Reject::RoundTrip)));
    }

    #[test]
    fn client_needs_two_servers_and_backs_off() {
        let mut c = Client::new(BUILD);
        let end = run(&mut c, &mut Net::new([Some(0), None, None]), SETTLE_NS);
        assert_eq!(end, SETTLE_NS + ROUND_NS);
        assert!(!c.wall.synced);
        assert_eq!((c.good, c.failed), (0, 1));
        // Two minutes, then four.
        assert_eq!(c.poll(end + 2 * MIN_INTERVAL_NS - 1, true), Do::Nothing);
        let end = run(&mut c, &mut Net::new([None; 3]), end + 2 * MIN_INTERVAL_NS);
        assert_eq!(c.poll(end + 4 * MIN_INTERVAL_NS - 1, true), Do::Nothing);
        // Two answering is enough; a name that does not resolve is not waited for.
        let mut net = Net::new([None, Some(0), Some(0)]);
        net.hosts[0].0 = 0;
        let start = end + 4 * MIN_INTERVAL_NS;
        assert_eq!(run(&mut c, &mut net, start), start + 6 * STEP);
        assert!(c.wall.synced);
        // The back-off is capped at an hour.
        let mut c = Client::new(BUILD);
        let mut at = SETTLE_NS;
        for _ in 0..12 {
            let end = run(&mut c, &mut Net::new([None; 3]), at);
            assert_eq!(c.poll(end + 3600 * SECOND - 1, true) == Do::Start, c.failed <= 5);
            c.abort(end + 3600 * SECOND); // if that started a round, drop it
            at = end + 2 * 3600 * SECOND + 1;
        }
    }

    #[test]
    fn client_refuses_spoofed_and_absurd_answers() {
        let mut c = Client::new(BUILD);
        assert_eq!(c.poll(SETTLE_NS, true), Do::Start);
        let now = SETTLE_NS + 1_000_000;
        let from = |k: usize| (ADDRESS[k], PORT);
        let time = NOW * SECOND;
        // A reply before anything was sent, and one with a guessed nonce.
        c.reply(from(0), &reply(1000, time), now);
        assert_eq!((c.rejected, c.last_reject), (1, Some(Reject::Origin)));
        for k in 0..3 {
            c.sent(k, 1000 + k as u64, SETTLE_NS);
        }
        c.reply(from(0), &reply(999, time), now);
        assert_eq!(c.rejected, 2);
        // The right nonce from a port that is not NTP's, or from no address.
        c.reply((ADDRESS[0], 124), &reply(1000, time), now);
        c.reply((0, PORT), &reply(1000, time), now);
        assert_eq!((c.rejected, c.last_reject), (4, Some(Reject::Source)));
        assert_eq!(c.wants(0), None); // still waiting for the real one
        // The servers answer; the third from the first one's address, which
        // is no second vote.
        c.reply(from(0), &reply(1000, time), now);
        c.reply(from(1), &reply(1001, time), now);
        assert_eq!(c.rejected, 4);
        c.reply(from(0), &reply(1002, time), now);
        assert_eq!((c.rejected, c.last_reject), (5, Some(Reject::Source)));
        assert_eq!((c.wants(0), c.wants(1), c.wants(2)), (Some(To::Address(ADDRESS[0])), Some(To::Address(ADDRESS[1])), None));
        // A first answer cannot be played again, as itself or as a time.
        c.reply(from(0), &reply(1000, time), now);
        assert_eq!((c.rejected, c.last_reject), (6, Some(Reject::Origin)));
        c.sent(0, 2000, now);
        c.sent(1, 2001, now);
        c.reply(from(0), &reply(1000, time), now);
        assert_eq!(c.rejected, 7);
        // The timed request's nonce from somewhere else: refused, and the
        // request stays open for the server it went to.
        c.reply(from(1), &reply(2000, time), now);
        c.reply((EVIL, PORT), &reply(2000, time), now);
        assert_eq!((c.rejected, c.last_reject), (9, Some(Reject::Source)));
        // Right nonce and place, impossible time: thirty years on. That
        // was its one answer.
        c.reply(from(1), &reply(2001, (BUILD + 30 * 366 * 86_400) * SECOND), now);
        assert_eq!((c.rejected, c.last_reject), (10, Some(Reject::Bounds)));
        c.reply(from(1), &reply(2001, time), now);
        assert_eq!((c.rejected, c.last_reject), (11, Some(Reject::Origin)));
        // One honest server is not enough: the round fails, the clock is untouched.
        c.reply(from(0), &reply(2000, time), now);
        assert_eq!(c.rejected, 11);
        // A second answer to a request already answered is not another vote.
        c.reply(from(0), &reply(2000, time), now);
        assert_eq!((c.rejected, c.last_reject), (12, Some(Reject::Origin)));
        // Every server is done, so the round ends without waiting.
        assert_eq!(c.poll(now, true), Do::Finish);
        assert!(!c.wall.synced);
        assert_eq!(c.wall.now_ns(0), BUILD * SECOND);
        // Replies after the round are ignored.
        c.reply(from(1), &reply(2001, time), now);
        assert_eq!((c.good, c.failed, c.rejected), (0, 1, 12));

        // A server that refuses the first request (a kiss of death, or one
        // from 2001) is left out of the round.
        let mut c = Client::new(BUILD);
        assert_eq!(c.poll(SETTLE_NS, true), Do::Start);
        c.sent(0, 1, SETTLE_NS);
        c.sent(1, 2, SETTLE_NS);
        let mut kod = reply(1, time);
        kod[1] = 0;
        c.reply(from(0), &kod, now);
        assert_eq!(c.last_reject, Some(Reject::KissOfDeath));
        assert_eq!(c.wants(0), None);
        c.reply(from(1), &reply(2, time), now);
        c.sent(1, 3, now);
        c.reply(from(1), &reply(3, 1_000_000_000 * SECOND), now);
        assert_eq!(c.last_reject, Some(Reject::Bounds));
    }

    #[test]
    fn client_outvotes_one_liar_and_confirms_big_jumps() {
        let mut c = Client::new(BUILD);
        let liar = 5 * 366 * 86_400;
        let end = run(&mut c, &mut Net::new([Some(liar), Some(0), Some(0)]), SETTLE_NS);
        assert!(error_ns(&c) <= 4);
        // Later every answer says a year on. Not believed the first time...
        let year = 366 * 86_400;
        let end = run(&mut c, &mut Net::new([Some(year); 3]), end + REFRESH_NS);
        assert!(error_ns(&c) <= 4);
        assert_eq!(c.good, 1);
        // ...asked again a minute later, and believed when it repeats.
        assert_eq!(c.poll(end + MIN_INTERVAL_NS - 1, true), Do::Nothing);
        run(&mut c, &mut Net::new([Some(year), Some(year), None]), end + MIN_INTERVAL_NS);
        assert!(c.wall.now_ns(0).abs_diff((NOW + year as u64) * SECOND) <= 4);
        assert_eq!(c.good, 2);
    }

    #[test]
    fn client_offline_and_rate_limit() {
        let mut c = Client::new(BUILD);
        // Offline for an hour: nothing is sent.
        for s in 0..3600 {
            assert_eq!(c.poll(s * SECOND, false), Do::Nothing);
        }
        // Online: one settle delay, then a round.
        assert_eq!(c.poll(3600 * SECOND, true), Do::Nothing);
        let mut net = Net::new([Some(0); 3]);
        let end = run(&mut c, &mut net, 3599 * SECOND + SETTLE_NS);
        // The network drops and returns: the clock is set, so nothing is
        // asked until the refresh is due.
        assert_eq!(c.poll(end + SECOND, false), Do::Nothing);
        assert_eq!(c.poll(end + SECOND + SETTLE_NS, true), Do::Nothing);
        let again = end + REFRESH_NS;
        assert_eq!(c.poll(again - 1, true), Do::Nothing);
        // The socket fails mid-round: the round is lost, counted and closed.
        net.now = again;
        assert!(!c.step(&mut net, again, true));
        assert!(net.open);
        net.broken = true;
        assert!(c.step(&mut net, again + 1, true));
        assert!(!net.open);
        assert_eq!(c.failed, 1);
        c.abort(again + 2);
        assert_eq!(c.failed, 1);
        assert_eq!(c.poll(again + 3, true), Do::Nothing);
        // A phone that was never set and comes online mid back-off still
        // waits out the minimum interval.
        let mut c = Client::new(BUILD);
        assert_eq!(c.poll(SETTLE_NS, true), Do::Start);
        c.abort(SETTLE_NS + 1);
        for s in 0..59 {
            assert_eq!(c.poll(SETTLE_NS + s * SECOND, s % 2 == 0), Do::Nothing);
        }
    }
}
