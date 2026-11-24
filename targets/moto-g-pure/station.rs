//! Infrastructure station management frames for a WPA2-Personal CCMP network
//! or an open one.
use super::passive_scan::AccessPoint;

pub const RSN: &[u8] = &[
    48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2, 0, 0,
];

pub fn request(ap: &AccessPoint, station: [u8; 6], associate: bool, out: &mut [u8; 128]) -> usize {
    out.fill(0);
    out[0] = if associate { 0 } else { 0xb0 };
    out[4..10].copy_from_slice(&ap.bssid);
    out[10..16].copy_from_slice(&station);
    out[16..22].copy_from_slice(&ap.bssid);
    if !associate {
        out[26] = 1; // Open System transaction 1 precedes WPA2 key negotiation.
        return 30;
    }
    // Infrastructure, plus privacy for WPA2.
    out[24..26].copy_from_slice(&(if ap.open { 0x01u16 } else { 0x11 }).to_le_bytes());
    out[26..28].copy_from_slice(&10u16.to_le_bytes());
    let mut pos = 28;
    let mut ie = |tag, data: &[u8]| {
        out[pos] = tag;
        out[pos + 1] = data.len() as u8;
        out[pos + 2..pos + 2 + data.len()].copy_from_slice(data);
        pos += 2 + data.len();
    };
    ie(0, &ap.ssid[..ap.ssid_len]);
    let rates = &ap.rates[..ap.rate_count];
    let basic = rates.len().min(8);
    ie(1, &rates[..basic]);
    if rates.len() > basic {
        ie(50, &rates[basic..]);
    }
    if ap.open {
        return pos;
    }
    out[pos..pos + RSN.len()].copy_from_slice(RSN);
    pos + RSN.len()
}

// Ignore unrelated traffic. Only a successful response from this BSSID advances state.
pub fn response(
    frame: &[u8],
    ap: &AccessPoint,
    station: [u8; 6],
    associate: bool,
) -> Option<Result<u16, u32>> {
    if frame.len() < 30
        || frame[0] != if associate { 0x10 } else { 0xb0 }
        || frame[1] & 3 != 0
        || frame[4..10] != station
        || frame[10..16] != ap.bssid
        || frame[16..22] != ap.bssid
    {
        return None;
    }
    if !associate && frame[24..28] != [0, 0, 2, 0] {
        return None;
    }
    let status = if associate {
        &frame[26..28]
    } else {
        &frame[28..30]
    };
    if status != [0, 0] {
        return Some(Err(49));
    }
    if !associate {
        return Some(Ok(0));
    }
    let aid = u16::from_le_bytes([frame[28], frame[29]]) & 0x3fff;
    Some(if (1..=2007).contains(&aid) {
        Ok(aid)
    } else {
        Err(49)
    })
}

// Gen4m long-preamble CCK and legacy OFDM hardware rate codes.
pub fn basic_rate(ap: &AccessPoint) -> u8 {
    let rates = [
        (2, 0),
        (4, 1),
        (11, 2),
        (12, 0x4b),
        (18, 0x4f),
        (22, 3),
        (24, 0x4a),
        (36, 0x4e),
        (48, 0x49),
        (72, 0x4d),
        (96, 0x48),
        (108, 0x4c),
    ];
    rates
        .into_iter()
        .find(|(raw, _)| ap.rates[..ap.rate_count].contains(&(raw | 128)))
        .map(|(_, code)| code)
        .unwrap_or(if ap.channel > 14 { 0x4b } else { 0 })
}

/// The factory Gen4m ABI uses one station (WTBL 1) and a separate group key (WTBL 2).
pub fn firmware_peer(ap: &AccessPoint, aid: u16) -> ([u8; 116], [u8; 168]) {
    let mut bss = [0; 116];
    let mut sta = [0; 168];
    let mut basic = 0u16;
    let mut allowed = 0u16;
    let rates = [2, 4, 11, 22, 44, 66, 12, 18, 24, 36, 48, 72, 96, 108];
    for &rate in &ap.rates[..ap.rate_count] {
        if let Some(i) = rates.iter().position(|&v| v == rate & 127) {
            allowed |= 1 << i;
            if rate & 128 != 0 {
                basic |= 1 << i;
            }
        }
    }
    let phy = if ap.channel > 14 { 3 } else { 1 };
    bss[1] = 0; // Gen4m MEDIA_STATE_CONNECTED.
    bss[3] = ap.ssid_len as u8;
    bss[4..36].copy_from_slice(&ap.ssid);
    bss[36..42].copy_from_slice(&ap.bssid);
    bss[44..46].copy_from_slice(&allowed.to_le_bytes());
    bss[46..48].copy_from_slice(&basic.to_le_bytes());
    bss[50] = basic_rate(ap);
    // Basic PHY type, then gen4m's auth mode and encryption status:
    // WPA2-PSK (7) with AES (6), or open (0) with encryption disabled (1).
    let (auth, enc) = if ap.open { (0, 1) } else { (7, 6) };
    bss[52..56].copy_from_slice(&[phy, auth, enc, 1 << phy]);
    bss[58] = 2;
    bss[69..71].copy_from_slice(&[if ap.channel > 14 { 2 } else { 1 }, ap.channel]);
    bss[82] = 1;
    bss[88..90].fill(1);
    bss[90] = 4;
    bss[93] = 1;
    sta[1] = 0x41;
    sta[2..8].copy_from_slice(&ap.bssid);
    sta[8..10].copy_from_slice(&aid.to_le_bytes());
    sta[10] = 10;
    sta[13] = 1 << phy;
    sta[14..16].copy_from_slice(&allowed.to_le_bytes());
    sta[16..18].copy_from_slice(&basic.to_le_bytes());
    sta[20] = 2; // Associated.
    sta[51] = 1; // Request the firmware's activation event.
    sta[54..56].copy_from_slice(&[1, 2]);
    (bss, sta)
}

/// Extract EAPOL from an RX data descriptor in Ethernet or native 802.11 format.
pub fn eapol<'a>(packet: &'a [u8], mac: &[u8; 6], ap: &[u8; 6]) -> Option<&'a [u8]> {
    if packet.len() < 16 || packet[10] & 0xfa != 0 {
        return None;
    }
    let ty = u16::from_le_bytes([packet[2], packet[3]]);
    let offloaded = ty & 0xe00f == 0xe001
        && ((packet[13] >> 4) & 3 == 1 || packet[6] & 0x80 != 0);
    if packet[3] >> 5 != 2 && !offloaded {
        return None;
    }
    let frame = super::passive_scan::rx_payload(packet)?;
    let body = if packet[6] & 0x80 != 0 {
        if frame.get(..6)? != mac || frame.get(6..12)? != ap || frame.get(12..14)? != [0x88, 0x8e] {
            return None;
        }
        frame.get(14..)?
    } else {
        if frame.first()? & 0x0f != 8
            || frame.get(1)? & 3 != 2
            || frame.get(4..10)? != mac
            || frame.get(10..16)? != ap
            || frame.get(16..22)? != ap
        {
            return None;
        }
        let hdr = if frame[0] & 0x80 != 0 { 26 } else { 24 };
        if frame.get(hdr..hdr + 8)? != [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e] {
            return None;
        }
        frame.get(hdr + 8..)?
    };
    let len = u16::from_be_bytes(body.get(2..4)?.try_into().ok()?) as usize;
    body.get(..4 + len)
}

/// `open`: the link is an open network, whose frames carry no cipher.
pub fn ethernet(packet: &[u8], mac: &[u8; 6], ap: &[u8; 6], out: &mut [u8], open: bool) -> Option<usize> {
    if packet.len() < 16
        || packet[3] >> 5 != 2
        || packet[10] & 0xfe != 0
        || packet[11] & 1 != 0
        || (!open && packet[9] >> 4 != 4)
        || !matches!(packet[8], 1 | 2)
        || packet[7] & 3 != 0
    {
        return None;
    }
    let f = super::passive_scan::rx_payload(packet)?;
    let target = |dst: &[u8]| dst == mac || dst.first().is_some_and(|b| b & 1 != 0);
    if packet[6] & 0x80 != 0 {
        if f.len() < 14 || f.len() > out.len() || !target(&f[..6]) {
            return None;
        }
        out[..f.len()].copy_from_slice(f);
        return Some(f.len());
    }
    if f.len() < 32
        || f[0] & 0x0f != 8
        || f[1] & 0x43 != if open { 0x02 } else { 0x42 }
        || f[10..16] != *ap
        || !target(&f[4..10])
    {
        return None;
    }
    let hdr = if f[0] & 128 != 0 { 26 } else { 24 };
    if f.get(hdr..hdr + 6)? != [0xaa, 0xaa, 3, 0, 0, 0] {
        return None;
    }
    let payload = f.get(hdr + 6..)?;
    let n = 12 + payload.len();
    if n > out.len() {
        return None;
    }
    out[..6].copy_from_slice(&f[4..10]);
    out[6..12].copy_from_slice(&f[16..22]);
    out[12..n].copy_from_slice(payload);
    Some(n)
}
