//! Wi-Fi through the "wifi" commands in /dev/violet/sys (runtime/open/wifi.rs).
//! Anyone may scan and read the state; only Settings may join or leave, or
//! read the joined link's addresses.
use std::net::Ipv4Addr;
use std::num::NonZeroU32;

use crate::device::{fields, sys};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Security {
    /// WPA2-Personal: needs a password.
    Wpa2,
    Open,
    /// Something Radio cannot join yet (WPA3, Enterprise, WEP).
    Other,
}

#[derive(Clone, Debug)]
pub struct Network {
    /// Radio's profile ID for this access point.
    pub id: u32,
    pub channel: u32,
    pub security: Security,
    pub ssid: Vec<u8>,
    /// The channel of every access point seen with this name, `channel` first.
    pub channels: Vec<u32>,
}

/// The middle of a channel in MHz (IEEE 802.11: 2.4 GHz channels 1 to 14 and
/// the 5 GHz channels), None for a number that is not a channel.
pub fn frequency(channel: u32) -> Option<u32> {
    match channel {
        1..=13 => Some(2407 + 5 * channel),
        14 => Some(2484),
        32..=177 => Some(5000 + 5 * channel),
        _ => None,
    }
}

impl Network {
    /// The name to show (an SSID is bytes, almost always UTF-8).
    pub fn name(&self) -> String {
        let s = String::from_utf8_lossy(&self.ssid).into_owned();
        if s.trim().is_empty() { "(hidden network)".to_string() } else { s }
    }
    pub fn band(&self) -> &'static str {
        band(self.channel)
    }
}

pub fn band(channel: u32) -> &'static str {
    if channel == 0 {
        ""
    } else if channel <= 14 {
        "2.4 GHz"
    } else {
        "5 GHz"
    }
}

/// Where the phone's Wi-Fi address came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AddressFrom {
    /// The chip's own.
    Chip,
    /// The chip has none: a random one, new each time Wi-Fi starts.
    Random,
    /// The one set over USB (moto-wifi-profile with VIOLET_FIXED_MAC).
    Fixed,
}

/// The joined network's link as Radio reports it ("wifi info"). Radio leaves
/// out what it does not know, so every part is optional.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    /// This phone's Wi-Fi (MAC) address, and where it came from.
    pub mac: Option<[u8; 6]>,
    pub from: Option<AddressFrom>,
    /// The joined access point's address (its BSSID) and channel (0: unknown).
    pub bssid: Option<[u8; 6]>,
    pub channel: u32,
    pub ip: Option<Ipv4Addr>,
    pub mask: Option<Ipv4Addr>,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub dhcp: Option<Ipv4Addr>,
    /// The DHCP lease and what is left of it, in seconds.
    pub lease: Option<(u32, u32)>,
    /// The name sent with DHCP requests; None when none is sent.
    pub hostname: Option<String>,
    pub radio: Option<String>,
    /// The Wi-Fi firmware's version and its build date as the firmware spells it.
    pub firmware: Option<String>,
    pub built: Option<String>,
}

/// "04:de:ca:c0:ff:ee"
pub fn mac_text(a: &[u8; 6]) -> String {
    a.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

impl Link {
    /// The answer to "wifi info"; None when there is no joined link to describe.
    pub fn parse(answer: &str) -> Option<Link> {
        if answer.starts_with("error") {
            return None;
        }
        let f = fields(answer);
        let get = |k: &str| f.get(k).map(String::as_str);
        let mac = |k: &str| get(k).and_then(unhex).and_then(|b| <[u8; 6]>::try_from(b).ok());
        let ip = |k: &str| get(k).and_then(|v| v.parse::<Ipv4Addr>().ok());
        // Names come from outside (the chip, the USB profile): show only plain ASCII.
        let text = |k: &str| {
            let bytes = get(k).and_then(unhex)?;
            let s: String = bytes.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect();
            Some(s.trim().to_string()).filter(|s| !s.is_empty())
        };
        let number = |k: &str| get(k).and_then(|v| v.parse::<u32>().ok());
        Some(Link {
            mac: Some(mac("mac")?),
            from: match get("from") {
                Some("chip") => Some(AddressFrom::Chip),
                Some("random") => Some(AddressFrom::Random),
                Some("fixed") => Some(AddressFrom::Fixed),
                _ => None,
            },
            bssid: mac("bssid"),
            channel: number("channel").unwrap_or(0),
            ip: ip("ip"),
            mask: ip("mask"),
            router: ip("router"),
            dns: ["dns", "dns2"].iter().filter_map(|k| ip(k)).collect(),
            dhcp: ip("dhcp"),
            lease: number("lease").map(|total| (total, number("left").unwrap_or(total).min(total))),
            hostname: text("hostname"),
            radio: text("radio"),
            firmware: get("firmware").filter(|v| v.bytes().all(|b| b.is_ascii_digit() || b == b'.')).map(String::from),
            built: text("built"),
        })
    }
}

/// The joined network's link. Only Settings is answered.
pub fn link() -> Option<Link> {
    Link::parse(&sys("wifi info"))
}

/// What the runtime reports in "wifi".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// "scanning", "joining" or "leaving" while a request runs.
    pub busy: Option<String>,
    /// The joined network's profile ID, 0 for none.
    pub connected: u32,
    pub ssid: Vec<u8>,
    /// Radio is hosting Phone Internet instead of joining a network.
    pub hosting: bool,
    /// The last request and its outcome: ("join", false, reason).
    pub last: Option<(String, bool, u32)>,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn state() -> State {
    let f = fields(&sys("wifi"));
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    State {
        busy: Some(get("state")).filter(|s| !s.is_empty() && *s != "idle").map(String::from),
        connected: get("connected").parse().unwrap_or(0),
        ssid: unhex(get("ssid")).unwrap_or_default(),
        hosting: get("hosting") == "1",
        last: match get("last") {
            "" | "-" => None,
            what => Some((what.to_string(), get("result") == "ok", get("reason").parse().unwrap_or(0))),
        },
    }
}

impl State {
    /// Whether `n` is the joined network. Radio tries every access point with
    /// the chosen name, so the one it joined can differ from the one chosen.
    pub fn is_joined(&self, n: &Network) -> bool {
        self.connected != 0 && (n.id == self.connected || (!self.ssid.is_empty() && n.ssid == self.ssid))
    }
}

/// Start a scan; poll `state()` until it is no longer busy, then read `networks()`.
pub fn scan() -> Result<(), String> {
    answer(sys("wifi scan"))
}

/// The networks from the last scan, one entry per name (the first access point
/// seen for it: Radio tries each one with that name when joining).
pub fn networks() -> Vec<Network> {
    let mut out: Vec<Network> = Vec::new();
    for line in sys("wifi list").lines() {
        let f = fields(line);
        let Some(id) = f.get("id").and_then(|v| v.parse().ok()) else { continue };
        let ssid = f.get("ssid").and_then(|v| unhex(v)).unwrap_or_default();
        let channel = f.get("channel").and_then(|v| v.parse().ok()).unwrap_or(0);
        if let Some(first) = out.iter_mut().find(|n| !ssid.is_empty() && n.ssid == ssid) {
            first.channels.push(channel);
            continue;
        }
        out.push(Network {
            id,
            channel,
            channels: vec![channel],
            security: match f.get("security").map(String::as_str) {
                Some("wpa2") => Security::Wpa2,
                Some("open") => Security::Open,
                _ => Security::Other,
            },
            ssid,
        });
    }
    out
}

/// Start joining network `id`, with its WPA2 key when it has one.
pub fn join(id: u32, key: Option<&[u8; 32]>) -> Result<(), String> {
    match key {
        Some(k) => answer(sys(&format!("wifi join {id} {}", hex(k)))),
        None => answer(sys(&format!("wifi join {id}"))),
    }
}

pub fn leave() -> Result<(), String> {
    answer(sys("wifi leave"))
}

fn answer(a: String) -> Result<(), String> {
    if a == "ok" { Ok(()) } else { Err(a.trim_start_matches("error ").to_string()) }
}

/// The WPA2 pairwise master key: PBKDF2-HMAC-SHA1 of the passphrase, salted
/// with the SSID, 4096 rounds (IEEE 802.11i). The password never leaves the app.
pub fn pmk(ssid: &[u8], passphrase: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        NonZeroU32::new(4096).unwrap(),
        ssid,
        passphrase.as_bytes(),
        &mut key,
    );
    key
}

/// A WPA2 passphrase is 8 to 63 printable ASCII characters.
pub fn valid_passphrase(p: &str) -> bool {
    (8..=63).contains(&p.len()) && p.bytes().all(|b| (32..127).contains(&b))
}

/// Why a join failed, for people (reasons from runtime/protocol.rs).
pub fn reason(code: u32) -> &'static str {
    match code {
        52 => "Wrong password. Check it and try again.",
        53 => "Connected, but the network gave no address.",
        49 => "The network is out of range. Scan again.",
        47 | 48 => "The network didn't answer. Move closer and try again.",
        57 => "The network didn't finish connecting. Try again.",
        50 => "This network needs its password.",
        2 => "Radio refused this network.",
        3 => "Wi-Fi is unavailable right now.",
        _ => "Couldn't connect. Try again.",
    }
}

#[cfg(test)]
mod tests {
    use super::{AddressFrom, Link};

    #[test]
    fn link_parses_what_the_runtime_reports() {
        let name = super::hex(b"Violet");
        let radio = super::hex(b"MediaTek MT6765");
        let built = super::hex(b"20210608152745");
        let l = Link::parse(&format!(
            "mac=04decac0ffee from=fixed bssid=48a98a010203 channel=161 ip=192.168.88.23 mask=255.255.255.0 \
             router=192.168.88.1 dns=192.168.88.1 dns2=1.1.1.1 dhcp=192.168.88.1 lease=86400 left=85000 \
             hostname={name} radio={radio} firmware=2.1.48 built={built}"
        ))
        .unwrap();
        assert_eq!(super::mac_text(&l.mac.unwrap()), "04:de:ca:c0:ff:ee");
        assert_eq!(l.from, Some(AddressFrom::Fixed));
        assert_eq!(super::mac_text(&l.bssid.unwrap()), "48:a9:8a:01:02:03");
        assert_eq!(l.channel, 161);
        assert_eq!(l.ip.unwrap().to_string(), "192.168.88.23");
        assert_eq!(l.mask.unwrap().octets(), [255, 255, 255, 0]);
        assert_eq!(l.router, l.dhcp);
        assert_eq!(l.dns.iter().map(|d| d.to_string()).collect::<Vec<_>>(), ["192.168.88.1", "1.1.1.1"]);
        assert_eq!(l.lease, Some((86400, 85000)));
        assert_eq!(l.hostname.as_deref(), Some("Violet"));
        assert_eq!(l.radio.as_deref(), Some("MediaTek MT6765"));
        assert_eq!(l.firmware.as_deref(), Some("2.1.48"));
        assert_eq!(l.built.as_deref(), Some("20210608152745"));
    }

    #[test]
    fn link_keeps_only_what_is_reported() {
        // The emulator's radio: an address and a router, nothing else.
        let l = Link::parse("mac=525400560001 from=chip ip=10.0.2.15 router=10.0.2.2").unwrap();
        assert_eq!(l.from, Some(AddressFrom::Chip));
        assert_eq!((l.bssid, l.channel, l.mask, l.lease), (None, 0, None, None));
        assert!(l.dns.is_empty() && l.hostname.is_none() && l.firmware.is_none());
        // Not joined, another app asking, or an answer that is not a link.
        for answer in ["error offline", "error denied: only Settings reads the link's addresses", "", "mac=1234", "ip=10.0.2.15"] {
            assert_eq!(Link::parse(answer), None, "{answer}");
        }
        // Bytes that are not plain text never reach the screen as they are.
        let odd = Link::parse(&format!("mac=020000000001 hostname={} firmware=1.2;3", super::hex(b"a\x1b[2Jb\xff"))).unwrap();
        assert_eq!(odd.hostname.as_deref(), Some("a?[2Jb?"));
        assert_eq!(odd.firmware, None);
        // Less left than the lease, never more.
        assert_eq!(Link::parse("mac=020000000001 lease=600 left=900").unwrap().lease, Some((600, 600)));
    }

    #[test]
    fn channel_frequencies() {
        for (channel, mhz) in [(1, 2412), (6, 2437), (8, 2447), (11, 2462), (14, 2484), (36, 5180), (161, 5805), (165, 5825)] {
            assert_eq!(super::frequency(channel), Some(mhz));
        }
        assert_eq!(super::frequency(0), None);
        assert_eq!(super::frequency(20), None);
        assert_eq!((super::band(8), super::band(36), super::band(0)), ("2.4 GHz", "5 GHz", ""));
    }

    #[test]
    fn pmk_matches_ieee_test_vector() {
        // IEEE 802.11i-2004, H.4.1: "password" and "IEEE".
        let key = super::pmk(b"IEEE", "password");
        assert_eq!(super::hex(&key), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
    }
}
