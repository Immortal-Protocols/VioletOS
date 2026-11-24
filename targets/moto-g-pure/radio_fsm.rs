//! RFC skeleton: Explicit handset Radio state machine, clean relink, keepalives,
//! and 64-bit monotonic timers (`targets/moto-g-pure`).
//!
//! Fixes:
//! 1. `relink()` failing 100% of the time because `axi.pairwise_ready` and old
//!    WTBL keys were never cleared before EAPOL Message 2.
//! 2. `dispatch()` tearing down the entire chip on `GET PROFILE` whenever
//!    `LINK_ERROR` flipped true before the reconnect loop could run.
//! 3. Hair-trigger `LINK_ERROR` on TX ring full or spoofed broadcast deauths.
//! 4. 32-bit 13 MHz tick wrap every ~5.5 minutes in `radio_io::now()`.
//! 5. Cold-booting WMT/WLAN firmware on every scan and network switch.

#![allow(dead_code)]

use crate::scan_compat::{CandidateAp, ScanTable, SecurityKind};

/// 64-bit monotonic milliseconds derived from 64-bit ARM `CNTVCT` (never wraps in ~5.5 min).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonotonicMs(pub u64);

impl MonotonicMs {
    pub fn elapsed_since(self, earlier: MonotonicMs) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// Typed failure reasons so `/apps/wifi` and `AutoConnector` NEVER wipe a saved
/// password on transient RF/handshake timeouts (unlike overloaded error code 52).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinFailure {
    None = 0,
    ApNotFound = 49,
    AuthTimeout = 47,
    AssocTimeout = 48,
    MissingKey = 50,
    /// Definite credential rejection (e.g., EAPOL M2 MIC mismatch / AP deauth reason 15).
    BadPassword = 52,
    /// Transient RF/timeout during 4-way handshake—keep the saved PMK and retry!
    HandshakeTimeout = 61,
    DhcpTimeout = 53,
    UnsupportedSecurity = 62,
    ChipBusy = 63,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTxError {
    /// TX ring temporarily full: backpressure only, NOT a fatal link failure.
    WouldBlock,
    HardwareFault(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    UnicastDeauth { reason: u16 },
    /// Unauthenticated broadcast deauth: verify link with gateway ARP probe before dropping.
    BroadcastDeauth { reason: u16 },
    BeaconLoss,
    GroupRekeyOk,
    GroupRekeyFailed,
}

/// Explicit Radio lifecycle: read-only status IPCs (`GET PROFILE`, `GET RADIO`)
/// inspect this state without ever tearing down the chip during `Reconnecting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioState {
    Off,
    /// WMT + WLAN firmware loaded and ready; scanning/joining needs zero firmware re-upload.
    WarmIdle,
    Scanning { return_to_connected: bool },
    Joining { profile_id: u32, stage: u8 },
    Connected {
        profile_id: u32,
        bssid: [u8; 6],
        channel: u8,
        rssi: i8,
        last_rx_ms: MonotonicMs,
        last_keepalive_ms: MonotonicMs,
        deauth_probe_since: Option<MonotonicMs>,
    },
    Reconnecting {
        profile_id: u32,
        attempts: u8,
        next_try_ms: MonotonicMs,
        rescan_needed: bool,
    },
    HostingRelay,
}

pub const KEEPALIVE_INTERVAL_MS: u64 = 30_000;
pub const DEAUTH_VERIFY_MS: u64 = 1_500;
pub const LINK_IDLE_PROBE_MS: u64 = 45_000;

/// Hardware operations implemented by `wlan_axi::Axi` + `wpa` + `ip/async_sockets`.
pub trait RadioDriver {
    fn ensure_warm(&mut self) -> Result<(), u32>;
    fn reset_station_keys_and_peer(&mut self); // MUST set `pairwise_ready = false` + clear WTBL!
    fn scan_into(&mut self, table: &mut ScanTable) -> Result<(), u32>;
    fn associate(&mut self, ap: &CandidateAp) -> Result<u16, JoinFailure>;
    fn handshake_wpa2(&mut self, ap: &CandidateAp, pmk: &[u8; 32]) -> Result<(), JoinFailure>;
    fn start_ip(&mut self, cached_ip: Option<[u8; 4]>) -> Result<(), JoinFailure>;
    fn send_null_keepalive(&mut self) -> Result<(), LinkTxError>;
    fn send_gateway_arp_probe(&mut self) -> Result<(), LinkTxError>;
    fn poll_wpa_and_net(&mut self) -> Option<LinkEvent>;
}

pub struct RadioFsm<D: RadioDriver> {
    pub driver: D,
    pub state: RadioState,
    pub scan: ScanTable,
    pub last_failure: JoinFailure,
    pub target_ssid: [u8; 32],
    pub target_ssid_len: usize,
    pub target_pmk: Option<[u8; 32]>,
    pub cached_ip: Option<[u8; 4]>,
    pub gtk_rekeys: u32,
    pub last_deauth_reason: u16,
}

impl<D: RadioDriver> RadioFsm<D> {
    pub fn new(driver: D) -> Self {
        Self {
            driver,
            state: RadioState::Off,
            scan: ScanTable::new(),
            last_failure: JoinFailure::None,
            target_ssid: [0; 32],
            target_ssid_len: 0,
            target_pmk: None,
            cached_ip: None,
            gtk_rekeys: 0,
            last_deauth_reason: 0,
        }
    }

    /// Clean reconnect: ALWAYS clears stale pairwise/WTBL keys before associating
    /// so EAPOL Message 2 goes out in plaintext, and falls back to all BSSIDs of
    /// the SSID (strongest RSSI first) if the old AP/channel stops answering.
    pub fn relink_once(&mut self, now: MonotonicMs, rescan: bool) -> Result<(), JoinFailure> {
        self.driver.ensure_warm().map_err(|_| JoinFailure::ChipBusy)?;
        self.driver.reset_station_keys_and_peer();
        if rescan {
            let _ = self.driver.scan_into(&mut self.scan);
        }
        let ssid = &self.target_ssid[..self.target_ssid_len];
        let mut last_err = JoinFailure::ApNotFound;
        let candidates = self.scan.candidates_for_ssid(ssid);
        for ap in candidates.iter().flatten() {
            match self.join_candidate(ap, now) {
                Ok(()) => return Ok(()),
                Err(JoinFailure::BadPassword) => return Err(JoinFailure::BadPassword),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn join_candidate(&mut self, ap: &CandidateAp, now: MonotonicMs) -> Result<(), JoinFailure> {
        self.driver.reset_station_keys_and_peer();
        self.driver.associate(ap)?;
        match ap.security {
            SecurityKind::Open => {} // Handset now supports Open networks directly!
            SecurityKind::Wpa2Ccmp | SecurityKind::Wpa2MixedTkipGroup | SecurityKind::Wpa2PmfOptional => {
                let pmk = self.target_pmk.ok_or(JoinFailure::MissingKey)?;
                self.driver.handshake_wpa2(ap, &pmk)?;
            }
            SecurityKind::Unsupported => return Err(JoinFailure::UnsupportedSecurity),
        }
        self.driver.start_ip(self.cached_ip)?;
        self.last_failure = JoinFailure::None;
        self.state = RadioState::Connected {
            profile_id: ap.profile_id(),
            bssid: ap.bssid,
            channel: ap.channel,
            rssi: ap.rssi,
            last_rx_ms: now,
            last_keepalive_ms: now,
            deauth_probe_since: None,
        };
        Ok(())
    }

    /// Driven every 1 ms from Radio's main loop: maintains keepalives, verifies
    /// broadcast deauths, and runs exponential-backoff reconnects without blocking IPC.
    pub fn tick(&mut self, now: MonotonicMs) {
        match self.state {
            RadioState::Connected {
                profile_id, last_rx_ms, last_keepalive_ms, deauth_probe_since, ..
            } => {
                if let Some(ev) = self.driver.poll_wpa_and_net() {
                    match ev {
                        LinkEvent::GroupRekeyOk => self.gtk_rekeys = self.gtk_rekeys.wrapping_add(1),
                        LinkEvent::UnicastDeauth { reason } => {
                            self.last_deauth_reason = reason;
                            self.enter_reconnecting(profile_id, now, false);
                            return;
                        }
                        LinkEvent::BroadcastDeauth { reason } => {
                            self.last_deauth_reason = reason;
                            if deauth_probe_since.is_none() {
                                let _ = self.driver.send_gateway_arp_probe();
                                if let RadioState::Connected { deauth_probe_since: p, .. } = &mut self.state {
                                    *p = Some(now);
                                }
                            }
                        }
                        LinkEvent::BeaconLoss | LinkEvent::GroupRekeyFailed => {
                            self.enter_reconnecting(profile_id, now, true);
                            return;
                        }
                    }
                }
                if let Some(t0) = deauth_probe_since {
                    if now.elapsed_since(t0) >= DEAUTH_VERIFY_MS {
                        self.enter_reconnecting(profile_id, now, true);
                        return;
                    }
                }
                if now.elapsed_since(last_keepalive_ms) >= KEEPALIVE_INTERVAL_MS
                    && now.elapsed_since(last_rx_ms) >= KEEPALIVE_INTERVAL_MS
                {
                    let _ = self.driver.send_null_keepalive();
                    if let RadioState::Connected { last_keepalive_ms: k, .. } = &mut self.state {
                        *k = now;
                    }
                }
            }
            RadioState::Reconnecting { profile_id, attempts, next_try_ms, rescan_needed }
                if now >= next_try_ms =>
            {
                let do_rescan = rescan_needed || attempts >= 1;
                match self.relink_once(now, do_rescan) {
                    Ok(()) => {}
                    Err(JoinFailure::BadPassword) => {
                        self.last_failure = JoinFailure::BadPassword;
                        self.state = RadioState::WarmIdle;
                    }
                    Err(e) => {
                        self.last_failure = e;
                        let backoff_ms = (1_000u64 << attempts.min(5)).min(30_000);
                        self.state = RadioState::Reconnecting {
                            profile_id,
                            attempts: attempts.saturating_add(1),
                            next_try_ms: MonotonicMs(now.0 + backoff_ms),
                            rescan_needed: true,
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn enter_reconnecting(&mut self, profile_id: u32, now: MonotonicMs, rescan_needed: bool) {
        self.driver.reset_station_keys_and_peer();
        self.state = RadioState::Reconnecting {
            profile_id,
            attempts: 0,
            next_try_ms: now,
            rescan_needed,
        };
    }
}
