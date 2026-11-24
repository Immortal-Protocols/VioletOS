//! RFC skeleton: Dedicated persistent Wi-Fi profile store (`VWIF1`), stable device MAC,
//! and boot-time `AutoConnector` (`runtime/open`).
//!
//! Fixes:
//! 1. `/data/networks` silently dropped when `services/storage/files.rs` hits `SLOTS = 4`.
//! 2. Zero auto-connect on boot (previously only `/apps/wifi` ever read `/data/networks`).
//! 3. Saved PMK deleted on transient handshake timeouts (now only flagged after 2x definite
//!    `BadPassword` rejections, never wiped automatically).
//! 4. Random MAC invented on every cold boot (now persisted once in `DeviceIdentity`).

#![allow(dead_code)]

use alloc::vec::Vec;

pub const STORE_MAGIC: [u8; 5] = *b"VWIF1";
pub const MAX_SAVED_NETWORKS: usize = 32;
pub const MAX_CONSEC_BAD_PASSWORD: u8 = 2;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedSecurity {
    Open = 1,
    Wpa2 = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct SavedNetwork {
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    pub security: SavedSecurity,
    pub pmk: [u8; 32],
    /// Fast-resume hints: try last known BSSID/channel + DHCP INIT-REBOOT (< 1 s reconnect).
    pub last_bssid: [u8; 6],
    pub last_channel: u8,
    pub last_ip: [u8; 4],
    pub priority: u16,
    pub auto_connect: bool,
    /// Incremented ONLY on definite `BadPassword` (never on RF/handshake timeout).
    pub consec_auth_failures: u8,
}

impl SavedNetwork {
    pub fn ssid_bytes(&self) -> &[u8] {
        &self.ssid[..self.ssid_len as usize]
    }
    pub fn needs_reauth(&self) -> bool {
        self.consec_auth_failures >= MAX_CONSEC_BAD_PASSWORD
    }
}

/// Persisted once on first boot so routers keep DHCP leases and reservations across reboots.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceIdentity {
    pub mac_valid: bool,
    pub stable_mac: [u8; 6],
}

/// Dedicated journaled 8 KiB bank in `services/storage` (or expanded 32-slot `/data`),
/// returning explicit write errors instead of silently failing lazy background flushes.
pub trait ProfileStore {
    fn load(&mut self) -> Result<(DeviceIdentity, Vec<SavedNetwork>), i32>;
    fn save_atomic(&mut self, id: &DeviceIdentity, nets: &[SavedNetwork]) -> Result<(), i32>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoState {
    /// Waiting for `data::load()` and initial scan after boot.
    BootScan,
    /// Joining a saved network selected from the scan results.
    AutoJoining { saved_index: usize },
    /// Connected (or user manually took control / hosting Phone Internet).
    Idle,
    /// Out of range of all saved networks: retry with exponential backoff.
    Backoff { next_scan_ms: u64, tries: u8 },
}

#[derive(Clone, Copy, Debug)]
pub struct VisibleNetwork {
    pub id: u32,
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    pub rssi: i8,
    pub open: bool,
}

/// Boot-time and out-of-range auto-connector owned by Open Runtime.
pub struct AutoConnector {
    pub identity: DeviceIdentity,
    pub saved: Vec<SavedNetwork>,
    pub state: AutoState,
    pub user_override: bool, // true while user explicitly disconnected or started Phone Internet
}

impl AutoConnector {
    pub fn new(identity: DeviceIdentity, saved: Vec<SavedNetwork>) -> Self {
        let state = if saved.iter().any(|n| n.auto_connect && !n.needs_reauth()) {
            AutoState::BootScan
        } else {
            AutoState::Idle
        };
        Self { identity, saved, state, user_override: false }
    }

    /// Choose the highest-priority (tie-broken by strongest RSSI) visible saved network.
    pub fn pick_best(&self, visible: &[VisibleNetwork]) -> Option<(usize, VisibleNetwork)> {
        let mut best: Option<(usize, VisibleNetwork, u16, i8)> = None;
        for (idx, s) in self.saved.iter().enumerate() {
            if !s.auto_connect || s.needs_reauth() {
                continue;
            }
            for v in visible {
                if v.ssid_len == s.ssid_len && v.ssid[..v.ssid_len as usize] == s.ssid[..s.ssid_len as usize] {
                    let better = match best {
                        None => true,
                        Some((_, _, bp, br)) => (s.priority, v.rssi) > (bp, br),
                    };
                    if better {
                        best = Some((idx, *v, s.priority, v.rssi));
                    }
                }
            }
        }
        best.map(|(i, v, _, _)| (i, v))
    }

    /// Record join result without ever wiping a saved PMK on transient RF timeouts.
    pub fn record_join_outcome<S: ProfileStore>(
        &mut self,
        store: &mut S,
        saved_index: usize,
        ok: bool,
        definite_bad_password: bool,
        bssid: [u8; 6],
        channel: u8,
        ip: [u8; 4],
        now_ms: u64,
    ) {
        if let Some(net) = self.saved.get_mut(saved_index) {
            if ok {
                net.consec_auth_failures = 0;
                net.last_bssid = bssid;
                net.last_channel = channel;
                net.last_ip = ip;
                let _ = store.save_atomic(&self.identity, &self.saved);
                self.state = AutoState::Idle;
                return;
            }
            if definite_bad_password {
                net.consec_auth_failures = net.consec_auth_failures.saturating_add(1);
                // Persist failure counter so UI shows "Check password", but NEVER delete PMK.
                let _ = store.save_atomic(&self.identity, &self.saved);
            }
        }
        let next_tries = match self.state {
            AutoState::Backoff { tries, .. } => tries.saturating_add(1),
            _ => 1,
        };
        let wait_ms = (3_000u64 << next_tries.min(4)).min(60_000);
        self.state = AutoState::Backoff { next_scan_ms: now_ms + wait_ms, tries: next_tries };
    }
}
