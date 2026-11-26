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
    /// The strongest signal heard, in dBm.
    pub rssi: i8,
    pub reports: u32,
}

impl Device {
    /// The name, or the address as Bluetooth writes it (most significant byte first).
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        self.address.iter().rev().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
    }
}

/// The scan's HCI host: the commands to send and what the events said.
#[derive(Default)]
pub struct Scan {
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

/// The Local Name in advertising data (AD types 0x09, then 0x08).
fn local_name(mut data: &[u8]) -> String {
    let mut short = String::new();
    while data.len() >= 2 {
        let n = data[0] as usize;
        let Some(field) = data.get(1..1 + n) else { break };
        match field.first() {
            Some(0x09) => return String::from_utf8_lossy(&field[1..]).into_owned(),
            Some(0x08) => short = String::from_utf8_lossy(&field[1..]).into_owned(),
            _ => {}
        }
        data = &data[1 + n..];
    }
    short
}

impl Scan {
    pub fn new() -> Scan {
        let mut s = Scan::default();
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
                    self.heard(r[2..8].try_into().unwrap(), r[1] & 1 == 1, data, rssi as i8);
                    at += 10 + n;
                }
            }
            _ => {}
        }
    }

    fn heard(&mut self, address: [u8; 6], random: bool, data: &[u8], rssi: i8) {
        // 127 means the controller couldn't measure it.
        let rssi = if rssi == 127 { i8::MIN } else { rssi };
        let name = local_name(data);
        match self.devices.iter_mut().find(|d| d.address == address && d.random == random) {
            Some(d) => {
                d.reports += 1;
                d.rssi = d.rssi.max(rssi);
                if d.name.is_empty() {
                    d.name = name;
                }
            }
            None => self.devices.push(Device { address, random, name, rssi, reports: 1 }),
        }
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
    fn refusal_stops_the_sequence() {
        let mut s = Scan::new();
        let p = s.take_out().unwrap();
        s.receive(&[4, 0x0e, 4, 1, p[1], p[2], 0x0c]);
        assert!(s.failure.is_some() && s.take_out().is_none() && !s.listening);
    }
}
