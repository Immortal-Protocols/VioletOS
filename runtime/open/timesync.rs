//! The wall clock and its network time: gives sntp.rs's client a UDP socket
//! of the runtime's own and the random generator, and applies what it
//! decides. Apps read the result as CLOCK_REALTIME (syscall.rs) and its state
//! as "time" in /dev/violet/sys.
//!
//! Until a network answers, the clock is the image's build time plus the
//! time since boot, as before. Phone Internet has no UDP, so the clock is
//! not set over it. A request is timed from when it is queued for Radio and
//! its reply when the runtime next polls (at most 50 ms later); the timed
//! request goes to an address, so no name lookup is in between.
use super::net::{self, Net};
use super::slots;
use super::sntp::{self, Client, Sent, To, Wire};
use alloc::string::String;
use alloc::vec::Vec;

struct State {
    client: Client,
    slot: Option<usize>,
}

/// Seconds since 1970 when this image was built.
fn build_epoch() -> u64 {
    option_env!("VIOLET_BUILD_EPOCH").and_then(|s| s.parse().ok()).unwrap_or(1_795_000_000)
}

// The Open Runtime is single-threaded.
static mut STATE: Option<State> = None;
fn state() -> &'static mut State {
    let state = unsafe { &mut *core::ptr::addr_of_mut!(STATE) };
    state.get_or_insert_with(|| State { client: Client::new(build_epoch()), slot: None })
}

/// CLOCK_REALTIME in nanoseconds when the monotonic clock reads `mono_ns`.
pub fn wall_ns(mono_ns: u64) -> u64 {
    state().client.wall.now_ns(mono_ns)
}

/// A round's socket and the random generator, as sntp.rs asks for them.
struct Link<'a> {
    net: &'a mut Net,
    slot: &'a mut Option<usize>,
    random: &'a mut dyn FnMut() -> Option<u64>,
}

impl Wire for Link<'_> {
    fn open(&mut self) -> bool {
        self.close();
        *self.slot = self.net.udp_open_for(slots::SYSTEM, 0).ok();
        self.slot.is_some()
    }
    fn close(&mut self) {
        if let Some(slot) = self.slot.take() {
            net::close(slot);
        }
    }
    fn broken(&mut self) -> bool {
        self.slot.map_or(true, |slot| matches!(self.net.state(slot).0, net::FAILED | net::IDLE | net::PEER_CLOSED))
    }
    fn random(&mut self) -> Option<u64> {
        (self.random)()
    }
    fn send(&mut self, to: To, packet: &[u8; sntp::PACKET]) -> Sent {
        let Some(slot) = *self.slot else { return Sent::Never };
        let ip = match to {
            To::Name(name) => self.net.address_for(name),
            // A reply's source is a real address. One that claims to be a
            // resolver stand-in would send the timed request to a name.
            To::Address(ip) if self.net.is_stand_in(ip) => 0,
            To::Address(ip) => ip,
        };
        if ip == 0 {
            return Sent::Never;
        }
        match self.net.send_datagram(slot, slots::SYSTEM, ip, sntp::PORT, packet) {
            Ok(true) => Sent::Yes,
            Ok(false) => Sent::Later,
            Err(_) => Sent::Never,
        }
    }
    fn receive(&mut self) -> Option<((u32, u16), Vec<u8>)> {
        self.net.recv_datagram((*self.slot)?)
    }
}

/// Called on every poll of Runtime. `random` draws 64 fresh bits from the
/// random generator (None if it has none): one call for each request sent.
pub fn tick(net: &mut Net, now_ns: u64, random: &mut dyn FnMut() -> Option<u64>) {
    let s = state();
    // 1: a joined network, 3: the emulator's host. 2 is Phone Internet (no UDP).
    let online = matches!(net.online(), 1 | 3);
    let mut link = Link { net, slot: &mut s.slot, random };
    if s.client.step(&mut link, now_ns, online) {
        let c = &s.client;
        crate::log!("time: synced={} rounds {}/{} refused {}", c.wall.synced as u32, c.good, c.good + c.failed, c.rejected);
    }
}

/// "time": the clock's state for apps and diagnostics.
pub fn command(now_ns: u64) -> String {
    let c = &state().client;
    let age = if c.wall.synced { (now_ns.saturating_sub(c.wall.synced_at_ns) / 1_000_000_000) as i64 } else { -1 };
    alloc::format!(
        "unix={} synced={} age={} rounds={} failed={} refused={}\n",
        c.wall.now_ns(now_ns) / 1_000_000_000, c.wall.synced as u32, age, c.good, c.failed, c.rejected
    )
}
