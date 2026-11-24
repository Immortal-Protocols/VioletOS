//! RFC skeleton: Expanded 64-AP active/passive scanner, RSSI-sorted mesh selection,
//! Open + WPA2-mixed (TKIP group) + optional PMF security parsing, and 802.11n HT/WMM IEs.
//!
//! Replaces the 8-AP passive-only limit and strict CCMP-only group cipher filter in
//! `targets/moto-g-pure/passive_scan.rs` and legacy-only assoc request in `station.rs`.

#![allow(dead_code)]

pub const MAX_APS: usize = 64;
pub const MAX_ROAM_CANDIDATES: usize = 8;

/// 2.4 GHz (active), non-DFS 5 GHz (active), and DFS 5 GHz (passive-only per FCC/ETSI).
pub const CHANNELS_2G_ACTIVE: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
pub const CHANNELS_5G_ACTIVE: &[u8] = &[36, 40, 44, 48, 149, 153, 157, 161, 165];
pub const CHANNELS_5G_DFS_PASSIVE: &[u8] = &[52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144];

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SecurityKind {
    #[default]
    Unsupported = 0,
    /// Open / unencrypted (capability Privacy bit == 0, no RSN IE).
    Open = 1,
    /// Standard WPA2-PSK with CCMP pairwise + CCMP group.
    Wpa2Ccmp = 2,
    /// Common WPA/WPA2-mixed home router: CCMP pairwise + TKIP group cipher (`00-0f-ac-2`).
    Wpa2MixedTkipGroup = 3,
    /// WPA2/WPA3-transition AP with MFPC=1, MFPR=0 (joinable via WPA2-PSK).
    Wpa2PmfOptional = 4,
}

#[derive(Clone, Copy, Debug)]
pub struct CandidateAp {
    pub bssid: [u8; 6],
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub channel: u8,
    pub rssi: i8, // dBm (e.g., -45 strong, -85 weak); read from Gen4m RXD group 3
    pub security: SecurityKind,
    pub ht_capable: bool, // AP advertised 802.11n HT Capabilities (IE 45)
    pub wmm_capable: bool,
    pub rates: [u8; 16],
    pub rate_count: usize,
    pub rsne: [u8; 257],
    pub rsne_len: usize,
}

impl Default for CandidateAp {
    fn default() -> Self {
        Self {
            bssid: [0; 6], ssid: [0; 32], ssid_len: 0, channel: 0, rssi: -100,
            security: SecurityKind::Unsupported, ht_capable: false, wmm_capable: false,
            rates: [0; 16], rate_count: 0, rsne: [0; 257], rsne_len: 0,
        }
    }
}

impl CandidateAp {
    pub fn profile_id(&self) -> u32 {
        self.bssid
            .iter()
            .fold(2166136261u32, |v, b| (v ^ *b as u32).wrapping_mul(16777619))
            .max(2)
    }

    pub fn is_joinable(&self) -> bool {
        !matches!(self.security, SecurityKind::Unsupported)
    }
}

/// Classify AP security from 802.11 capability info (`privacy` bit) and optional RSN IE body.
pub fn classify_security(privacy: bool, rsn_body: Option<&[u8]>) -> SecurityKind {
    let Some(rsn) = rsn_body else {
        return if privacy { SecurityKind::Unsupported } else { SecurityKind::Open };
    };
    if rsn.len() < 8 || rsn[..2] != [1, 0] {
        return SecurityKind::Unsupported;
    }
    let group_cipher = &rsn[2..6];
    let group_ccmp = group_cipher == [0, 15, 172, 4];
    let group_tkip = group_cipher == [0, 15, 172, 2];
    if !group_ccmp && !group_tkip {
        return SecurityKind::Unsupported;
    }
    let Some((pairwise, rest)) = take_suites(&rsn[6..]) else { return SecurityKind::Unsupported };
    let Some((akms, rest)) = take_suites(rest) else { return SecurityKind::Unsupported };
    let has_ccmp = pairwise.chunks_exact(4).any(|s| s == [0, 15, 172, 4]);
    let has_psk = akms.chunks_exact(4).any(|s| s == [0, 15, 172, 2] || s == [0, 15, 172, 6]);
    if !has_ccmp || !has_psk {
        return SecurityKind::Unsupported;
    }
    let caps = if rest.len() >= 2 { u16::from_le_bytes([rest[0], rest[1]]) } else { 0 };
    let mfpr = caps & (1 << 6) != 0;
    let mfpc = caps & (1 << 7) != 0;
    if mfpr {
        // Phase 4: add BIP-CMAC-128 for MFPR=1 / WPA3-SAE.
        return SecurityKind::Unsupported;
    }
    if group_tkip {
        SecurityKind::Wpa2MixedTkipGroup
    } else if mfpc {
        SecurityKind::Wpa2PmfOptional
    } else {
        SecurityKind::Wpa2Ccmp
    }
}

fn take_suites(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let count = u16::from_le_bytes(buf.get(..2)?.try_into().ok()?) as usize;
    let end = 2 + count * 4;
    Some((buf.get(2..end)?, buf.get(end..)?))
}

/// 64-AP scan table deduplicated by BSSID and evicted by weakest RSSI when full.
pub struct ScanTable {
    pub aps: [CandidateAp; MAX_APS],
    pub count: usize,
}

impl ScanTable {
    pub const fn new() -> Self {
        Self {
            aps: [CandidateAp {
                bssid: [0; 6], ssid: [0; 32], ssid_len: 0, channel: 0, rssi: -100,
                security: SecurityKind::Unsupported, ht_capable: false, wmm_capable: false,
                rates: [0; 16], rate_count: 0, rsne: [0; 257], rsne_len: 0,
            }; MAX_APS],
            count: 0,
        }
    }

    pub fn upsert(&mut self, ap: CandidateAp) {
        if let Some(existing) = self.aps[..self.count].iter_mut().find(|a| a.bssid == ap.bssid) {
            *existing = ap;
            return;
        }
        if self.count < MAX_APS {
            self.aps[self.count] = ap;
            self.count += 1;
            return;
        }
        // Evict weakest-RSSI AP so nearby routers are never crowded out in busy buildings.
        if let Some((weak_i, weak)) = self.aps.iter().enumerate().min_by_key(|(_, a)| a.rssi) {
            if ap.rssi > weak.rssi {
                self.aps[weak_i] = ap;
            }
        }
    }

    /// Return up to `MAX_ROAM_CANDIDATES` joinable BSSIDs for `ssid`, sorted strongest RSSI first.
    pub fn candidates_for_ssid(&self, ssid: &[u8]) -> [Option<CandidateAp>; MAX_ROAM_CANDIDATES] {
        let mut out = [None; MAX_ROAM_CANDIDATES];
        let mut n = 0usize;
        for ap in &self.aps[..self.count] {
            if ap.is_joinable() && ap.ssid_len == ssid.len() && &ap.ssid[..ap.ssid_len] == ssid {
                if n < MAX_ROAM_CANDIDATES {
                    out[n] = Some(*ap);
                    n += 1;
                }
            }
        }
        // Insertion sort by descending RSSI (prefers stronger band/mesh node automatically).
        for i in 1..n {
            let mut j = i;
            while j > 0 && out[j].unwrap().rssi > out[j - 1].unwrap().rssi {
                out.swap(j, j - 1);
                j -= 1;
            }
        }
        out
    }
}

/// Append basic 802.11n HT Capabilities (IE 45, 1x1 20 MHz MCS 0-7) and WMM Info (IE 221)
/// to an Association Request so modern mesh/enterprise APs do not throttle or kick legacy-only stations.
pub fn append_ht_wmm_ies(ap: &CandidateAp, out: &mut [u8], pos: &mut usize) {
    if ap.ht_capable && *pos + 28 <= out.len() {
        let mut ht = [0u8; 28];
        ht[0] = 45; // HT Capabilities IE
        ht[1] = 26;
        ht[2..4].copy_from_slice(&0x002cu16.to_le_bytes()); // Short GI 20MHz, Rx STBC 0
        ht[4] = 0x1b; // A-MPDU params
        ht[5] = 0xff; // Supported MCS 0..7 (1 spatial stream)
        out[*pos..*pos + 28].copy_from_slice(&ht);
        *pos += 28;
    }
    if ap.wmm_capable && *pos + 9 <= out.len() {
        const WMM_INFO: [u8; 9] = [221, 7, 0x00, 0x50, 0xf2, 2, 0, 1, 0];
        out[*pos..*pos + 9].copy_from_slice(&WMM_INFO);
        *pos += 9;
    }
}
