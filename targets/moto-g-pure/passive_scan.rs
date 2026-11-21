//! Connac command ABI used by the XT2163DL vendor driver.
pub const SCAN_BYTES: usize = 1188;
const FIVE_GHZ: &[u8] = &[36, 40, 44, 48, 149, 153, 157, 161, 165];
pub const CHANNEL_COUNT: u8 = 11 + FIVE_GHZ.len() as u8;
pub fn request(id: u8) -> [u8; SCAN_BYTES] {
    let mut p = [0; SCAN_BYTES];
    p[0] = id;
    p[3] = 1;
    p[7] = 1;
    // Passive, no probe requests, US 2.4 GHz and non-DFS 5 GHz primary channels.
    p[154..156].copy_from_slice(&120u16.to_le_bytes());
    p[158] = 4;
    p[159] = CHANNEL_COUNT;
    for (i, c) in p[160..182].chunks_exact_mut(2).enumerate() {
        c.copy_from_slice(&[1, 1 + i as u8]);
    }
    for (pair, channel) in p[182..].chunks_exact_mut(2).zip(FIVE_GHZ) {
        pair.copy_from_slice(&[2, *channel]);
    }
    p[828..830].copy_from_slice(&100u16.to_le_bytes());
    p[1112..1118].fill(0xff);
    p
}
#[derive(Clone, Copy)]
pub struct AccessPoint {
    pub bssid: [u8; 6],
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub channel: u8,
    pub rates: [u8; 16],
    pub rate_count: usize,
    pub wpa2_psk_ccmp: bool,
    pub rsne: [u8; 257],
    pub rsne_len: usize,
}

impl Default for AccessPoint {
    fn default() -> Self {
        Self {
            bssid: [0; 6],
            ssid: [0; 32],
            ssid_len: 0,
            channel: 0,
            rates: [0; 16],
            rate_count: 0,
            wpa2_psk_ccmp: false,
            rsne: [0; 257],
            rsne_len: 0,
        }
    }
}

impl AccessPoint {
    pub fn profile_id(&self) -> u32 {
        self.bssid
            .iter()
            .fold(2166136261u32, |v, b| (v ^ *b as u32).wrapping_mul(16777619))
            .max(2)
    }
}

fn suites<'a>(fields: &mut &'a [u8]) -> Option<&'a [u8]> {
    let count = u16::from_le_bytes(fields.get(..2)?.try_into().ok()?) as usize;
    let end = 2 + 4 * count;
    let list = fields.get(2..end)?;
    *fields = &fields[end..];
    Some(list)
}

fn psk_ccmp(rsn: &[u8]) -> Option<bool> {
    if rsn.get(..6)? != [1, 0, 0, 15, 172, 4] {
        return Some(false);
    }
    let mut rest = &rsn[6..];
    let ccmp = suites(&mut rest)?
        .chunks_exact(4)
        .any(|s| s == [0, 15, 172, 4]);
    let psk = suites(&mut rest)?
        .chunks_exact(4)
        .any(|s| s == [0, 15, 172, 2]);
    let caps = if rest.is_empty() {
        0
    } else {
        u16::from_le_bytes(rest.get(..2)?.try_into().ok()?)
    };
    Some(ccmp && psk && caps & (1 << 6) == 0)
}

#[derive(Default)]
pub struct Results {
    pub preferred_ssid: [u8; 32],
    pub preferred_len: usize,
    pub aps: [AccessPoint; 8],
    pub ap_count: usize,
    pub frames: u32,
    pub name: [u8; 32],
    pub name_len: usize,
    pub channels: u8,
}
impl Results {
    pub fn frame(&mut self, frame: &[u8]) {
        if frame.len() < 36 || !matches!(frame[0], 0x80 | 0x50) || frame[1] & 3 != 0 {
            return;
        }
        let mut ap = AccessPoint::default();
        ap.bssid.copy_from_slice(&frame[16..22]);
        let mut tags = &frame[36..];
        while !tags.is_empty() {
            let Some(h) = tags.get(..2) else { return };
            let len = h[1] as usize;
            let Some(v) = tags.get(2..2 + len) else {
                return;
            };
            match h[0] {
                0 => {
                    if len > 32 {
                        return;
                    }
                    ap.ssid[..len].copy_from_slice(v);
                    ap.ssid_len = len;
                }
                1 | 50 => {
                    for &rate in v {
                        if ap.rate_count == ap.rates.len() {
                            break;
                        }
                        ap.rates[ap.rate_count] = rate;
                        ap.rate_count += 1;
                    }
                }
                3 if len == 1 => ap.channel = v[0],
                61 if len >= 1 => ap.channel = v[0],
                48 => {
                    if ap.rsne_len != 0 {
                        return;
                    }
                    ap.rsne_len = len + 2;
                    ap.rsne[..ap.rsne_len].copy_from_slice(&tags[..ap.rsne_len]);
                    let Some(supported) = psk_ccmp(v) else { return };
                    ap.wpa2_psk_ccmp = supported && frame[34] & 0x10 != 0;
                }
                _ => {}
            }
            tags = &tags[2 + len..];
        }
        self.frames = self.frames.saturating_add(1);
        if ap.ssid[..ap.ssid_len].iter().all(|&b| b == 0) {
            return;
        }
        if self.name_len == 0 {
            self.name_len = ap.ssid_len;
            for (out, &b) in self.name.iter_mut().zip(&ap.ssid[..ap.ssid_len]) {
                *out = if b.is_ascii_graphic() || b == 32 {
                    b
                } else {
                    b'?'
                };
            }
        }
        if ap.bssid == [0; 6] || ap.bssid[0] & 1 != 0 {
            return;
        }
        if let Some(old) = self.aps[..self.ap_count]
            .iter_mut()
            .find(|p| p.bssid == ap.bssid)
        {
            *old = ap;
        } else if self.ap_count < self.aps.len() {
            self.aps[self.ap_count] = ap;
            self.ap_count += 1;
        } else if ap.wpa2_psk_ccmp
            && ap.ssid_len == self.preferred_len
            && ap.ssid[..ap.ssid_len] == self.preferred_ssid[..self.preferred_len]
        {
            if let Some(other) = self.aps.iter_mut().rev().find(|p| {
                !p.wpa2_psk_ccmp
                    || p.ssid_len != ap.ssid_len
                    || p.ssid[..p.ssid_len] != ap.ssid[..ap.ssid_len]
            }) {
                *other = ap;
            }
        }
    }
    pub fn receive(&mut self, packet: &[u8], id: u8) -> bool {
        if packet.len() < 16 {
            return false;
        }
        let len = u16::from_le_bytes([packet[0], packet[1]]) as usize;
        let Some(p) = packet.get(..len) else {
            return false;
        };
        if p.len() < 16 {
            return false;
        }
        let ty = u16::from_le_bytes([p[2], p[3]]);
        match ty & 0xe00f {
            0xe000 => {
                let Some(event) = p.get(16..) else {
                    return false;
                };
                if event.len() < 20
                    || event[2..4] != [0, 0xe0]
                    || event[4] != 0x0d
                    || event[12] != id
                {
                    return false;
                }
                self.channels = event[16];
                self.channels == CHANNEL_COUNT && event[17] == 7
            }
            0xe001 => {
                if p[10] & 0x12 != 0 {
                    return false;
                } // bad frame checksum or integrity check
                if let Some(frame) = management_frame(p) {
                    self.frame(frame);
                }
                false
            }
            _ => false,
        }
    }
}

/// Decode the RX descriptor before interpreting a received management frame.
pub fn management_frame(p: &[u8]) -> Option<&[u8]> {
    if p.len() < 16 {
        return None;
    }
    let n = u16::from_le_bytes([p[0], p[1]]) as usize;
    let p = p.get(..n)?;
    if p.len() < 16 {
        return None;
    }
    let ty = u16::from_le_bytes([p[2], p[3]]);
    if ty & 0xe00f != 0xe001 || p[10] & 0x12 != 0 {
        return None;
    }
    rx_payload(p)
}

pub fn rx_payload(p: &[u8]) -> Option<&[u8]> {
    let n = u16::from_le_bytes(p.get(..2)?.try_into().ok()?) as usize;
    let p = p.get(..n)?;
    let groups = (u16::from_le_bytes(p.get(2..4)?.try_into().ok()?) >> 9) & 15;
    let base = 16
        + [16, 8, 24, 16]
            .iter()
            .enumerate()
            .filter(|(i, _)| groups & (1 << i) != 0)
            .map(|(_, n)| n)
            .sum::<usize>();
    p.get(base + if p.get(6)? & 0x40 != 0 { 2 } else { 0 }..)
}
