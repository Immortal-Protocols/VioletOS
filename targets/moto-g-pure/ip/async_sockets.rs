//! RFC skeleton: Non-blocking per-slot socket & DHCP `INIT-REBOOT` state machine
//! replacing the synchronous blocking loops in `targets/moto-g-pure/ip/sockets.c`
//! and `targets/moto-g-pure/ip/client.c`.
//!
//! Problem today:
//! `sockets.c::tcp_open()`, `put()`, and `get()` spin synchronously for 5-8 s inside
//! `open_sockets_poll()`, and `client.c::station_net_join()` blocks up to 30 s in DHCP.
//! While any socket blocks, `station_wpa_poll()` is never called -> WPA2 GTK group rekeys
//! time out and the router kicks the phone off Wi-Fi.

#![allow(dead_code)]

pub const SOCKET_SLOTS: usize = 8;
pub const CONNECT_TIMEOUT_MS: u64 = 8_000;
pub const DNS_TIMEOUT_MS: u64 = 4_000;
pub const DHCP_FAST_TIMEOUT_MS: u64 = 3_000;
pub const DHCP_FULL_TIMEOUT_MS: u64 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpPhase {
    Idle,
    /// Fast path: send DHCPREQUEST for `SavedNetwork::last_ip` (~200-500 ms on known networks).
    InitReboot { requested_ip: [u8; 4], started_ms: u64 },
    /// Standard DISCOVER/OFFER/REQUEST fallback if router NAKs or ignores INIT-REBOOT.
    Discovering { started_ms: u64 },
    Bound { ip: [u8; 4], lease_expires_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Idle,
    ResolvingDns { started_ms: u64 },
    TcpConnecting { addr: [u8; 4], port: u16, started_ms: u64 },
    /// Incremental SOCKS5 auth + connect when routing via iPhone Phone Internet relay.
    RelayHandshake { step: u8, started_ms: u64 },
    Established,
    Closing,
    Failed { errno: u16 },
}

pub struct AsyncSlot {
    pub state: SlotState,
    pub peer_port: u16,
    pub tx_pending: u32,
    pub rx_ready: u32,
}

impl AsyncSlot {
    pub const fn new() -> Self {
        Self { state: SlotState::Idle, peer_port: 0, tx_pending: 0, rx_ready: 0 }
    }
}

/// Driven once per 1 ms Radio event-loop tick: NEVER blocks, guaranteeing
/// `station_wpa_poll()` and all 8 app sockets make progress concurrently.
pub struct AsyncNetEngine {
    pub dhcp: DhcpPhase,
    pub slots: [AsyncSlot; SOCKET_SLOTS],
}

impl AsyncNetEngine {
    pub const fn new() -> Self {
        Self {
            dhcp: DhcpPhase::Idle,
            slots: [
                AsyncSlot::new(), AsyncSlot::new(), AsyncSlot::new(), AsyncSlot::new(),
                AsyncSlot::new(), AsyncSlot::new(), AsyncSlot::new(), AsyncSlot::new(),
            ],
        }
    }

    pub fn begin_dhcp(&mut self, cached_ip: Option<[u8; 4]>, now_ms: u64) {
        self.dhcp = match cached_ip {
            Some(ip) if ip != [0; 4] => DhcpPhase::InitReboot { requested_ip: ip, started_ms: now_ms },
            _ => DhcpPhase::Discovering { started_ms: now_ms },
        };
    }

    /// Bounded non-blocking poll step (< 100 us): drains RX ring, advances lwIP timers,
    /// steps DHCP, and progresses every active socket slot without spinning.
    pub fn step(&mut self, now_ms: u64, wpa_poll: impl FnOnce(), lwip_poll: impl FnOnce()) {
        wpa_poll(); // Always serviced first so WPA2 GTK group rekeys never time out.
        lwip_poll();
        if let DhcpPhase::InitReboot { started_ms, .. } = self.dhcp {
            if now_ms.saturating_sub(started_ms) >= DHCP_FAST_TIMEOUT_MS {
                // Router ignored cached-IP request: fall back seamlessly to full DHCPDISCOVER.
                self.dhcp = DhcpPhase::Discovering { started_ms: now_ms };
            }
        }
        for slot in &mut self.slots {
            match slot.state {
                SlotState::ResolvingDns { started_ms }
                    if now_ms.saturating_sub(started_ms) >= DNS_TIMEOUT_MS =>
                {
                    slot.state = SlotState::Failed { errno: 2 }; // E_NONAME
                }
                SlotState::TcpConnecting { started_ms, .. }
                | SlotState::RelayHandshake { started_ms, .. }
                    if now_ms.saturating_sub(started_ms) >= CONNECT_TIMEOUT_MS =>
                {
                    slot.state = SlotState::Failed { errno: 110 }; // E_TIMEOUT
                }
                _ => {}
            }
        }
    }
}
