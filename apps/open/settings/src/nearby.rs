//! Settings › Bluetooth › Details › Scan nearby: a passive scan of its own
//! over /dev/violet/hci. A passive scan only receives advertisements; it
//! sends no scan requests and Settings never advertises, so devices nearby
//! can't tell this phone is listening. Only one app can hold the HCI, so no
//! other app can use Bluetooth while the scan runs. Closing the device stops
//! it: the Open Runtime resets the controller (runtime/open/hci.rs).
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

const O_NONBLOCK: i32 = 0o4000;

const RESET: u16 = 0x0c03;
const SET_EVENT_MASK: u16 = 0x0c01;
const LE_SET_EVENT_MASK: u16 = 0x2001;
const LE_SET_SCAN_PARAMS: u16 = 0x200b;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;

/// A device heard advertising.
#[derive(Clone, PartialEq, Debug)]
pub struct Device {
    pub address: [u8; 6],
    /// Public (false) or random (true) device address.
    pub random: bool,
    /// The Complete or Shortened Local Name it advertised, if any.
    pub name: String,
    /// The strongest signal heard, in dBm, and the latest.
    pub rssi: i8,
    pub last_rssi: i8,
    pub reports: u32,
    /// Since the scan started.
    pub first: Duration,
    pub last: Duration,
    /// The last advertising event type: 0 ADV_IND, 1 ADV_DIRECT_IND,
    /// 2 ADV_SCAN_IND, 3 ADV_NONCONN_IND, 4 SCAN_RSP.
    pub kind: u8,
    /// What its advertising data said, kept from the latest report that had it.
    pub company: Option<u16>,
    pub services: Vec<u16>,
    /// 32- and 128-bit service UUIDs, counted only.
    pub long_services: usize,
    pub tx_power: Option<i8>,
}

/// Bluetooth SIG company identifiers this page names; others show as hex.
fn company_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0002 => "Intel",
        0x0006 => "Microsoft",
        0x000f => "Broadcom",
        0x001d => "Qualcomm",
        0x0046 => "MediaTek",
        0x004c => "Apple",
        0x0059 => "Nordic Semiconductor",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x00e0 => "Google",
        _ => return None,
    })
}

/// Assigned 16-bit service UUIDs this page names.
fn service_name(uuid: u16) -> Option<&'static str> {
    Some(match uuid {
        0x180a => "Device Information",
        0x180d => "Heart Rate",
        0x180f => "Battery",
        0x1812 => "HID",
        0x181a => "Environmental Sensing",
        0xfd6f => "Exposure Notification",
        0xfeaa => "Eddystone",
        _ => return None,
    })
}

impl Device {
    /// The Core Specification's address types; a random address's top two bits say which kind.
    pub fn address_type(&self) -> &'static str {
        if !self.random {
            return "Public";
        }
        match self.address[5] >> 6 {
            3 => "Random static",
            1 => "Resolvable private",
            0 => "Non-resolvable private",
            _ => "Random",
        }
    }

    /// How it advertises, from the advertising event type.
    pub fn advertising(&self) -> &'static str {
        match self.kind {
            0 => "Connectable",
            1 => "Connectable, directed",
            2 => "Scannable, not connectable",
            3 => "Not connectable",
            _ => "Scan response",
        }
    }

    pub fn manufacturer(&self) -> String {
        match self.company {
            None => "Not advertised".into(),
            Some(id) => match company_name(id) {
                Some(name) => format!("{name} ({id:#06x})"),
                None => format!("{id:#06x}"),
            },
        }
    }

    pub fn service_list(&self) -> String {
        let mut names: Vec<String> = self.services.iter().map(|&u| service_name(u).map_or(format!("{u:#06x}"), String::from)).collect();
        if self.long_services > 0 {
            names.push(format!("{} custom", self.long_services));
        }
        if names.is_empty() { "None advertised".into() } else { names.join(", ") }
    }

    /// The name, or the address as Bluetooth writes it (most significant byte first).
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        self.address.iter().rev().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
    }
}

/// The scan's HCI host: the commands to send and what the events said.
pub struct Scan {
    started: Instant,
    out: VecDeque<Vec<u8>>,
    commands: VecDeque<(u16, Vec<u8>)>,
    pending: Option<u16>,
    /// The controller took LE Set Scan Enable.
    pub listening: bool,
    pub failure: Option<String>,
    pub devices: Vec<Device>,
}

fn command(op: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![1];
    p.extend_from_slice(&op.to_le_bytes());
    p.push(params.len() as u8);
    p.extend_from_slice(params);
    p
}

/// What one advertisement's data says (Core Specification Supplement, Part A).
#[derive(Default)]
struct Advert {
    name: String,
    company: Option<u16>,
    services: Vec<u16>,
    long_services: usize,
    tx_power: Option<i8>,
}

fn advert(mut data: &[u8]) -> Advert {
    let mut a = Advert::default();
    let mut short = String::new();
    while data.len() >= 2 {
        let n = data[0] as usize;
        let Some(field) = data.get(1..1 + n) else { break };
        if let Some((&kind, v)) = field.split_first() {
            match kind {
                // Incomplete and complete lists of 16-bit service UUIDs, and service data.
                0x02 | 0x03 => a.services.extend(v.chunks_exact(2).map(|u| u16::from_le_bytes([u[0], u[1]]))),
                0x16 if v.len() >= 2 => a.services.push(u16::from_le_bytes([v[0], v[1]])),
                0x04 | 0x05 => a.long_services += v.len() / 4,
                0x06 | 0x07 => a.long_services += v.len() / 16,
                0x08 => short = String::from_utf8_lossy(v).into_owned(),
                0x09 => a.name = String::from_utf8_lossy(v).into_owned(),
                0x0a if v.len() == 1 => a.tx_power = Some(v[0] as i8),
                0xff if v.len() >= 2 => a.company = Some(u16::from_le_bytes([v[0], v[1]])),
                _ => {}
            }
        }
        data = &data[1 + n..];
    }
    if a.name.is_empty() {
        a.name = short;
    }
    a.services.dedup();
    a
}

impl Scan {
    pub fn new() -> Scan {
        let mut s = Scan {
            started: Instant::now(),
            out: VecDeque::new(),
            commands: VecDeque::new(),
            pending: None,
            listening: false,
            failure: None,
            devices: Vec::new(),
        };
        s.queue(RESET, &[]);
        // The default events (bits 0-44) and LE Meta (bit 61).
        s.queue(SET_EVENT_MASK, &[0xff, 0xff, 0xff, 0xff, 0xff, 0x1f, 0, 0x20]);
        // Advertising reports only.
        s.queue(LE_SET_EVENT_MASK, &[0x02, 0, 0, 0, 0, 0, 0, 0]);
        // Passive, 60 ms window every 100 ms, own address public (a passive scan sends nothing).
        s.queue(LE_SET_SCAN_PARAMS, &[0, 0xa0, 0, 0x60, 0, 0, 0]);
        // Every report, so signal strength stays current.
        s.queue(LE_SET_SCAN_ENABLE, &[1, 0]);
        s
    }

    fn queue(&mut self, op: u16, params: &[u8]) {
        self.commands.push_back((op, params.to_vec()));
        self.next();
    }
    fn next(&mut self) {
        if self.pending.is_none() {
            if let Some((op, p)) = self.commands.pop_front() {
                self.pending = Some(op);
                self.out.push_back(command(op, &p));
            }
        }
    }
    pub fn take_out(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    fn completed(&mut self, op: u16, status: u8) {
        if self.pending != Some(op) {
            return;
        }
        self.pending = None;
        if status != 0 {
            self.failure = Some(format!("The controller refused {op:#06x} ({status:#04x})"));
            self.commands.clear();
            return;
        }
        if op == LE_SET_SCAN_ENABLE {
            self.listening = true;
        }
        self.next();
    }

    /// One H4 packet from the controller.
    pub fn receive(&mut self, packet: &[u8]) {
        if packet.len() < 3 || packet[0] != 4 {
            return;
        }
        let p = &packet[3..];
        match packet[1] {
            // Command Complete: packets, opcode, status.
            0x0e if p.len() >= 4 => self.completed(u16::from_le_bytes([p[1], p[2]]), p[3]),
            // Command Status: status, packets, opcode.
            0x0f if p.len() >= 4 => self.completed(u16::from_le_bytes([p[2], p[3]]), p[0]),
            // LE Advertising Report: subevent, count, then each report's
            // event type, address type, address, data length, data, RSSI.
            0x3e if p.len() >= 2 && p[0] == 0x02 => {
                let mut at = 2;
                for _ in 0..p[1] {
                    let Some(r) = p.get(at..at + 9) else { break };
                    let n = r[8] as usize;
                    let Some(data) = p.get(at + 9..at + 9 + n) else { break };
                    let Some(&rssi) = p.get(at + 9 + n) else { break };
                    self.heard(r[0], r[2..8].try_into().unwrap(), r[1] & 1 == 1, data, rssi as i8);
                    at += 10 + n;
                }
            }
            _ => {}
        }
    }

    fn heard(&mut self, kind: u8, address: [u8; 6], random: bool, data: &[u8], rssi: i8) {
        // 127 means the controller couldn't measure it.
        let rssi = if rssi == 127 { i8::MIN } else { rssi };
        let now = self.started.elapsed();
        let a = advert(data);
        let d = match self.devices.iter().position(|d| d.address == address && d.random == random) {
            Some(i) => &mut self.devices[i],
            None => {
                self.devices.push(Device {
                    address, random, name: String::new(), rssi, last_rssi: rssi, reports: 0, first: now, last: now,
                    kind, company: None, services: Vec::new(), long_services: 0, tx_power: None,
                });
                self.devices.last_mut().unwrap()
            }
        };
        d.reports += 1;
        d.rssi = d.rssi.max(rssi);
        d.last_rssi = rssi;
        d.last = now;
        // A scan response says nothing about how the device advertises.
        if kind != 4 {
            d.kind = kind;
        }
        if !a.name.is_empty() {
            d.name = a.name;
        }
        if a.company.is_some() {
            d.company = a.company;
        }
        if !a.services.is_empty() || a.long_services > 0 {
            d.services = a.services;
            d.long_services = a.long_services;
        }
        if a.tx_power.is_some() {
            d.tx_power = a.tx_power;
        }
    }

    /// How long the scan has run.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The devices, strongest signal first.
    pub fn strongest(&self) -> Vec<&Device> {
        let mut v: Vec<&Device> = self.devices.iter().collect();
        v.sort_by(|a, b| b.rssi.cmp(&a.rssi).then(a.label().cmp(&b.label())));
        v
    }
}

/// The scan over the HCI. Dropping it closes the device, which stops the scan.
pub struct Listener {
    hci: File,
    pub scan: Scan,
}

impl Listener {
    pub fn start() -> Result<Listener, String> {
        let hci = OpenOptions::new().read(true).write(true).custom_flags(O_NONBLOCK).open("/dev/violet/hci").map_err(|e| match e.raw_os_error() {
            Some(16) => "Another app is using Bluetooth".to_string(),
            Some(19) | Some(2) => "This phone has no Bluetooth controller".to_string(),
            _ => format!("Couldn't open Bluetooth ({e})"),
        })?;
        Ok(Listener { hci, scan: Scan::new() })
    }

    /// Sends what is queued and reads what arrived; true when anything changed.
    pub fn pump(&mut self) -> bool {
        let before = (self.scan.listening, self.scan.devices.len(), self.scan.failure.is_some());
        let mut reports = 0;
        while let Some(p) = self.scan.take_out() {
            if let Err(e) = self.hci.write_all(&p) {
                self.scan.failure = Some(if e.raw_os_error() == Some(100) { "Bluetooth is off".into() } else { format!("Couldn't send to Bluetooth ({e})") });
                break;
            }
        }
        let mut buf = [0u8; 1100];
        loop {
            match self.hci.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let had: u32 = self.scan.devices.iter().map(|d| d.reports).sum();
                    self.scan.receive(&buf[..n]);
                    reports += self.scan.devices.iter().map(|d| d.reports).sum::<u32>() - had;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(100) => {
                    self.scan.failure = Some("Bluetooth is off".into());
                    break;
                }
                _ => break,
            }
        }
        reports > 0 || before != (self.scan.listening, self.scan.devices.len(), self.scan.failure.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(s: &mut Scan) {
        while let Some(p) = s.take_out() {
            let op = [p[1], p[2]];
            s.receive(&[4, 0x0e, 4, 1, op[0], op[1], 0]);
        }
    }

    #[test]
    fn passive_scan_sends_nothing_over_the_air() {
        let mut s = Scan::new();
        let mut sent = Vec::new();
        while let Some(p) = s.take_out() {
            sent.push(u16::from_le_bytes([p[1], p[2]]));
            if u16::from_le_bytes([p[1], p[2]]) == LE_SET_SCAN_PARAMS {
                assert_eq!(p[4], 0, "passive");
            }
            s.receive(&[4, 0x0e, 4, 1, p[1], p[2], 0]);
        }
        assert_eq!(sent, [RESET, SET_EVENT_MASK, LE_SET_EVENT_MASK, LE_SET_SCAN_PARAMS, LE_SET_SCAN_ENABLE]);
        assert!(s.listening);
        // Nothing that advertises or connects.
        assert!(!sent.iter().any(|op| matches!(op, 0x2006 | 0x2008 | 0x200a | 0x200d | 0x2043)));
    }

    #[test]
    fn reports_count_each_device_once() {
        let mut s = Scan::new();
        complete(&mut s);
        // Two reports in one event: a named random device at -60 dBm, a public one at -80.
        let mut ev = vec![0x02, 2];
        ev.extend_from_slice(&[0, 1, 1, 2, 3, 4, 5, 0xc6, 7, 6, 0x09, b'P', b'h', b'o', b'n', b'e', (-60i8) as u8]);
        ev.extend_from_slice(&[3, 0, 9, 9, 9, 9, 9, 9, 0, (-80i8) as u8]);
        let mut p = vec![4, 0x3e, ev.len() as u8];
        p.extend_from_slice(&ev);
        s.receive(&p);
        s.receive(&p);
        assert_eq!(s.devices.len(), 2);
        let d = s.strongest();
        assert_eq!((d[0].label().as_str(), d[0].random, d[0].reports), ("Phone", true, 2));
        assert_eq!((d[1].label().as_str(), d[1].random), ("09:09:09:09:09:09", false));
    }

    #[test]
    fn advertising_data() {
        // Flags, a 16-bit service list (Battery, Heart Rate), TX power 4 dBm, Apple manufacturer data.
        let data = [2, 0x01, 0x06, 5, 0x03, 0x0f, 0x18, 0x0d, 0x18, 2, 0x0a, 4, 5, 0xff, 0x4c, 0x00, 0x10, 0x05];
        let a = advert(&data);
        assert_eq!((a.services, a.tx_power, a.company), (vec![0x180f, 0x180d], Some(4), Some(0x004c)));
        // A truncated field stops the parse without panicking.
        assert!(advert(&[9, 0x09, b'x']).name.is_empty());
    }

    #[test]
    fn refusal_stops_the_sequence() {
        let mut s = Scan::new();
        let p = s.take_out().unwrap();
        s.receive(&[4, 0x0e, 4, 1, p[1], p[2], 0x0c]);
        assert!(s.failure.is_some() && s.take_out().is_none() && !s.listening);
    }
}
