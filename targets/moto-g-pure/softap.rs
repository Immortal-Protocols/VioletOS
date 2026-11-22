//! Access-point role for the Gen4m firmware: one BSS that serves one phone.
//! Field layouts follow Motorola's gen4m driver (nicUpdateBss, bssUpdateBeaconContent,
//! cnmStaSendUpdateCmd and the AAA handlers in p2p_func.c).

/// Firmware BSS slot, own-MAC slot and WTBL entries used for the access point.
pub const BSS: u8 = 1;
pub const OWN_MAC: u8 = 1;
pub const CLIENT_WTBL: u8 = 1;
pub const GROUP_WTBL: u8 = 2;
pub const BEACON_INTERVAL: u16 = 100;
pub const DTIM: u8 = 1;

// 1, 2, 5.5 and 11 Mbit/s are basic; the OFDM rates are optional.
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const EXTENDED_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];
const OPERATIONAL_SET: u16 = 0x3fcf; // gen4m rate bit order: 1,2,5.5,11,(22,33),6..54
const BASIC_SET: u16 = 0x000f;

/// WPA2-Personal, CCMP pairwise and group cipher.
pub const RSN: &[u8] = &[
    48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2, 0, 0,
];

pub struct Config {
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub bssid: [u8; 6],
    pub channel: u8,
    pub wpa2: bool,
}

impl Config {
    pub const fn empty() -> Self {
        Self { ssid: [0; 32], ssid_len: 0, bssid: [0; 6], channel: 0, wpa2: false }
    }

    pub fn capability(&self) -> u16 {
        // ESS, short preamble, short slot time and privacy when WPA2 is enabled.
        0x0001 | 0x0020 | 0x0400 | if self.wpa2 { 0x0010 } else { 0 }
    }

    fn ies(&self, out: &mut [u8], beacon: bool) -> usize {
        let mut pos = 0;
        let mut ie = |tag: u8, data: &[u8]| {
            out[pos] = tag;
            out[pos + 1] = data.len() as u8;
            out[pos + 2..pos + 2 + data.len()].copy_from_slice(data);
            pos += 2 + data.len();
        };
        ie(0, &self.ssid[..self.ssid_len]);
        ie(1, &RATES);
        ie(3, &[self.channel]);
        if beacon {
            // The firmware rewrites the DTIM count and bitmap. gen4m reserves
            // (CFG_STA_REC_NUM + 7) / 8 = 4 bytes of partial virtual bitmap.
            ie(5, &[0, DTIM, 0, 0, 0, 0, 0]);
        }
        ie(42, &[0]);
        ie(50, &EXTENDED_RATES);
        if self.wpa2 {
            out[pos..pos + RSN.len()].copy_from_slice(RSN);
            pos += RSN.len();
        }
        pos
    }

    /// CMD_BEACON_TEMPLATE_UPDATE with IE_UPD_METHOD_UPDATE_ALL.
    pub fn beacon_template(&self, out: &mut [u8; 256]) -> usize {
        out.fill(0);
        out[0] = 1;
        out[1] = BSS;
        out[4..6].copy_from_slice(&self.capability().to_le_bytes());
        let n = self.ies(&mut out[8..], true);
        out[6..8].copy_from_slice(&(n as u16).to_le_bytes());
        8 + n
    }

    /// CMD_SET_BSS_INFO for an access point on 2.4 GHz, 802.11b/g.
    pub fn bss_info(&self) -> [u8; 116] {
        let mut b = [0; 116];
        b[0] = BSS;
        b[1] = 0; // MEDIA_STATE_CONNECTED
        b[2] = 2; // OP_MODE_ACCESS_POINT
        b[3] = self.ssid_len as u8;
        b[4..36].copy_from_slice(&self.ssid);
        b[36..42].copy_from_slice(&self.bssid);
        b[44..46].copy_from_slice(&OPERATIONAL_SET.to_le_bytes());
        b[46..48].copy_from_slice(&BASIC_SET.to_le_bytes());
        b[48] = 0xff; // no STA record of an AP
        b[50..52].copy_from_slice(&0u16.to_le_bytes()); // 1 Mbit/s CCK
        b[52] = 1; // non-HT basic PHY: ERP
        if self.wpa2 {
            b[53] = 7; // AUTH_MODE_WPA2_PSK
            b[54] = 6; // ENUM_ENCRYPTION3_ENABLED
        } else {
            b[53] = 0; // AUTH_MODE_OPEN
            b[54] = 1; // ENUM_ENCRYPTION_DISABLED
        }
        b[55] = 3; // HR/DSSS and ERP
        b[57] = 1; // ucIsApMode
        b[58] = GROUP_WTBL;
        // CMD_SET_BSS_RLM_PARAM at offset 68.
        b[68] = BSS;
        b[69] = 1; // BAND_2G4
        b[70] = self.channel;
        b[81] = 1; // short preamble
        b[82] = 1; // short slot time
        b[88] = 1; // TX NSS
        b[89] = 1; // RX NSS
        b[90] = 4; // DBDC band: auto
        b[93] = 1; // NSS
        b
    }

    /// CMD_INDICATE_PM_BSS_CREATED starts beacon transmission.
    pub fn pm_created(&self) -> [u8; 8] {
        let mut p = [0; 8];
        p[0] = BSS;
        p[1] = DTIM;
        p[2..4].copy_from_slice(&BEACON_INTERVAL.to_le_bytes());
        p
    }

    fn header(&self, out: &mut [u8], fc: u8, to: &[u8; 6]) {
        out[..24].fill(0);
        out[0] = fc;
        out[4..10].copy_from_slice(to);
        out[10..16].copy_from_slice(&self.bssid);
        out[16..22].copy_from_slice(&self.bssid);
    }

    pub fn probe_response(&self, to: &[u8; 6], out: &mut [u8; 256]) -> usize {
        self.header(out, 0x50, to);
        out[24..32].fill(0);
        out[32..34].copy_from_slice(&BEACON_INTERVAL.to_le_bytes());
        out[34..36].copy_from_slice(&self.capability().to_le_bytes());
        36 + self.ies(&mut out[36..], false)
    }

    pub fn auth_response(&self, to: &[u8; 6], out: &mut [u8; 256]) -> usize {
        self.header(out, 0xb0, to);
        out[24..30].copy_from_slice(&[0, 0, 2, 0, 0, 0]); // open system, seq 2, success
        30
    }

    pub fn assoc_response(&self, to: &[u8; 6], reassoc: bool, aid: u16, out: &mut [u8; 256]) -> usize {
        self.header(out, if reassoc { 0x30 } else { 0x10 }, to);
        out[24..26].copy_from_slice(&self.capability().to_le_bytes());
        out[26..28].copy_from_slice(&[0, 0]);
        out[28..30].copy_from_slice(&(aid | 0xc000).to_le_bytes());
        let mut pos = 30;
        for (tag, data) in [(1u8, &RATES[..]), (50, &EXTENDED_RATES[..])] {
            out[pos] = tag;
            out[pos + 1] = data.len() as u8;
            out[pos + 2..pos + 2 + data.len()].copy_from_slice(data);
            pos += 2 + data.len();
        }
        pos
    }
}

/// CMD_UPDATE_STA_RECORD for the associated phone.
pub fn client_record(mac: &[u8; 6], aid: u16, listen: u16, state: u8, respond: bool) -> [u8; 168] {
    let mut s = [0; 168];
    s[0] = 0; // STA record index
    s[1] = 0x22; // STA_TYPE_P2P_GC: a client of a P2P-network BSS
    s[2..8].copy_from_slice(mac);
    s[8..10].copy_from_slice(&aid.to_le_bytes());
    s[10..12].copy_from_slice(&listen.to_le_bytes());
    s[12] = BSS;
    s[13] = 3; // HR/DSSS and ERP
    s[14..16].copy_from_slice(&OPERATIONAL_SET.to_le_bytes());
    s[16..18].copy_from_slice(&BASIC_SET.to_le_bytes());
    s[20] = state; // 0: STA_STATE_1, 2: STA_STATE_3
    s[51] = respond as u8;
    s[54] = CLIENT_WTBL;
    s[55] = GROUP_WTBL;
    s
}

/// Received management frame addressed to this access point.
pub enum Request {
    Probe([u8; 6]),
    Auth([u8; 6]),
    Assoc { from: [u8; 6], reassoc: bool, listen: u16, rsn: bool },
    Leave([u8; 6]),
}

pub fn classify(f: &[u8], cfg: &Config) -> Option<Request> {
    if f.len() < 24 || f[1] & 3 != 0 {
        return None;
    }
    let to: [u8; 6] = f[4..10].try_into().ok()?;
    let from: [u8; 6] = f[10..16].try_into().ok()?;
    let ours = to == cfg.bssid && f[16..22] == cfg.bssid;
    match f[0] {
        0x40 => {
            // Answer wildcard and exact-SSID probes, broadcast or directed.
            if to != cfg.bssid && to != [0xff; 6] {
                return None;
            }
            let (tag, len) = (*f.get(24)?, *f.get(25)? as usize);
            let ssid = f.get(26..26 + len)?;
            (tag == 0 && (len == 0 || ssid == &cfg.ssid[..cfg.ssid_len])).then_some(Request::Probe(from))
        }
        0xb0 if ours && f.get(24..28)? == [0, 0, 1, 0] => Some(Request::Auth(from)),
        0x00 | 0x20 if ours => {
            let reassoc = f[0] == 0x20;
            let listen = u16::from_le_bytes([*f.get(26)?, *f.get(27)?]);
            let mut pos = if reassoc { 34 } else { 28 };
            let mut rsn = false;
            while pos + 2 <= f.len() {
                let (tag, len) = (f[pos], f[pos + 1] as usize);
                if tag == 48 {
                    rsn = true;
                }
                pos += 2 + len;
            }
            Some(Request::Assoc { from, reassoc, listen, rsn })
        }
        0xa0 | 0xc0 if ours => Some(Request::Leave(from)),
        _ => None,
    }
}

/// Convert a data frame received from the phone into Ethernet II.
/// RX descriptors carry either a translated Ethernet frame or native 802.11.
pub fn ethernet(packet: &[u8], cfg: &Config, client: &[u8; 6], out: &mut [u8]) -> Option<usize> {
    if packet.len() < 16 || packet[3] >> 5 != 2 {
        return None;
    }
    let f = super::passive_scan::rx_payload(packet)?;
    if packet[6] & 0x80 != 0 {
        if f.len() < 14 || f.len() > out.len() || f[6..12] != *client {
            return None;
        }
        out[..f.len()].copy_from_slice(f);
        return Some(f.len());
    }
    // ToDS data from the client: A1 = BSSID, A2 = source, A3 = destination.
    if f.len() < 32 || f[0] & 0x0f != 8 || f[1] & 3 != 1 || f[4..10] != cfg.bssid || f[10..16] != *client {
        return None;
    }
    let hdr = if f[0] & 0x80 != 0 { 26 } else { 24 };
    if f.get(hdr..hdr + 6)? != [0xaa, 0xaa, 3, 0, 0, 0] {
        return None;
    }
    let payload = f.get(hdr + 6..)?;
    let n = 12 + payload.len();
    if n > out.len() {
        return None;
    }
    out[..6].copy_from_slice(&f[16..22]);
    out[6..12].copy_from_slice(&f[10..16]);
    out[12..n].copy_from_slice(payload);
    Some(n)
}
