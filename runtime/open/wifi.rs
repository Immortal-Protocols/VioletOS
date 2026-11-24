//! Wi-Fi for open apps: the "wifi" commands in /dev/violet/sys.
//!
//!   wifi                    state=idle|scanning|joining|leaving connected=ID
//!                           ssid=HEX hosting=0|1 last=scan|join|leave|-
//!                           result=ok|failed|- reason=N
//!                           (hosting: Radio hosts Phone Internet, not a network)
//!   wifi scan               start a scan (answers at once; poll "wifi")
//!   wifi list               one line per network from the last scan:
//!                           id=ID channel=N security=wpa2|open|other ssid=HEX
//!   wifi join ID [PMK]      join a network; PMK is 64 hex digits for WPA2
//!   wifi leave              leave the joined network
//!
//! Radio's requests can take seconds on the handset (it boots the Wi-Fi chip,
//! scans and runs the WPA2 handshake), so they run as Runtime tickets polled
//! from here, and other apps keep running meanwhile. Only /apps/wifi may join
//! or leave a network: the others can look but not change it.
use super::{request, sys};
use crate::protocol::*;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The program allowed to change the network.
pub const SETTINGS_APP: &str = "/apps/wifi";

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Scan,
    Join,
    Leave,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Scan => "scan",
            Step::Join => "join",
            Step::Leave => "leave",
        }
    }
}

#[derive(Default)]
pub struct Wifi {
    /// The request Runtime is working on: what it is, and its ticket.
    job: Option<(Step, u32)>,
    /// Requests still to send once it finishes (leave, scan, then join).
    queue: VecDeque<(Step, Message)>,
    /// The outcome of the last request the app asked for: (what, ok, reason).
    last: Option<(Step, bool, u32)>,
    connected: u32,
    /// The joined network's SSID, by profile ID.
    name: (u32, Vec<u8>),
}

/// The profile ID Radio reports while it hosts Phone Internet (targets/moto-g-pure/radio.rs).
const HOSTING: u32 = 0x5245_4c59;

fn submit(m: Message) -> Result<u32, u32> {
    let r = m.call(sys::SLOT_RUNTIME);
    if r.op == OK {
        Ok(r.value)
    } else {
        Err(r.op)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(key)
}

impl Wifi {
    /// Advance the running request and start the next one.
    fn poll(&mut self) {
        loop {
            if let Some((step, ticket)) = self.job {
                let p = Message::new(POLL, 0, ticket).call(sys::SLOT_RUNTIME);
                if p.op == BUSY {
                    return;
                }
                self.job = None;
                let ok = p.op == OK;
                match step {
                    Step::Join if ok => self.connected = p.value,
                    Step::Leave if ok => self.connected = 0,
                    _ => {}
                }
                // A failed step ends the chain: a join after a failed leave would be refused.
                if !ok {
                    self.queue.clear();
                }
                if self.queue.is_empty() {
                    self.last = Some((step, ok, if ok { 0 } else { p.value }));
                }
            }
            let Some((step, m)) = self.queue.pop_front() else { return };
            match submit(m) {
                Ok(ticket) => self.job = Some((step, ticket)),
                Err(code) => {
                    self.queue.clear();
                    self.last = Some((step, false, code));
                    return;
                }
            }
        }
    }

    fn start(&mut self, steps: &[(Step, Message)]) -> String {
        self.poll();
        if self.job.is_some() {
            return String::from("error busy\n");
        }
        self.last = None;
        self.queue.extend(steps.iter().copied());
        self.poll();
        String::from("ok\n")
    }

    /// Answer one "wifi ..." command from `path`.
    pub fn command(&mut self, path: &str, words: &mut core::str::SplitWhitespace) -> String {
        let sub = words.next();
        if matches!(sub, Some("join" | "leave")) && path != SETTINGS_APP {
            return String::from("error denied: only Settings changes the network\n");
        }
        match sub {
            None | Some("state") => {
                self.poll();
                if self.job.is_none() {
                    let p = request(GET, PROFILE, 0);
                    if p.op == OK {
                        self.connected = p.value;
                    }
                }
                let state = match self.job.map(|j| j.0) {
                    None => "idle",
                    Some(Step::Scan) => "scanning",
                    Some(Step::Join) => "joining",
                    Some(Step::Leave) => "leaving",
                };
                let (last, result, reason) = match self.last {
                    None => ("-", "-", 0),
                    Some((s, ok, r)) => (s.name(), if ok { "ok" } else { "failed" }, r),
                };
                if self.job.is_none() && self.connected != 0 && self.connected != HOSTING && self.name.0 != self.connected {
                    self.name = (self.connected, Vec::new());
                    if let Some(n) = (0..16).map(|i| request(GET, SCAN, i)).take_while(|r| r.op == OK && r.value != 0).find(|r| r.value == self.connected) {
                        let text = n.text();
                        let len = text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(32);
                        self.name.1 = text[..len].to_vec();
                    }
                }
                let ssid = if self.connected != 0 && self.name.0 == self.connected { hex(&self.name.1) } else { String::new() };
                format!(
                    "state={} connected={} ssid={} hosting={} last={} result={} reason={}\n",
                    state, self.connected, ssid, (self.connected == HOSTING) as u32, last, result, reason
                )
            }
            Some("scan") => self.start(&[(Step::Scan, Message::new(EXECUTE, RADIO, 1))]),
            Some("list") => {
                self.poll();
                if self.job.is_some() {
                    return String::from("error busy\n");
                }
                let mut out = String::new();
                for i in 0..16 {
                    let r = request(GET, SCAN, i);
                    if r.op != OK || r.value == 0 {
                        break;
                    }
                    let text = r.text();
                    let n = text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(32);
                    let security = if r.arg & SCAN_WPA2 != 0 {
                        "wpa2"
                    } else if r.arg & SCAN_OPEN != 0 {
                        "open"
                    } else {
                        "other"
                    };
                    out += &format!("id={} channel={} security={} ssid={}\n", r.value, r.arg & 0xff, security, hex(&text[..n]));
                }
                if out.is_empty() {
                    out.push('\n');
                }
                out
            }
            Some("join") => {
                let Some(id) = words.next().and_then(|w| w.parse::<u32>().ok()).filter(|&id| id != 0) else {
                    return String::from("error usage: wifi join ID [PMK]\n");
                };
                let mut join = Message::new(EXECUTE, PROFILE, id);
                if let Some(k) = words.next() {
                    match unhex(k) {
                        Some(key) => join.data = key,
                        None => return String::from("error the key is 64 hex digits\n"),
                    }
                }
                self.poll();
                if self.job.is_none() {
                    let p = request(GET, PROFILE, 0);
                    if p.op == OK {
                        self.connected = p.value;
                    }
                }
                // Radio joins one network at a time: leave the current one, scan
                // again (the handset needs a fresh scan to join), then join.
                let answer = if self.connected != 0 && self.connected != id {
                    self.start(&[
                        (Step::Leave, Message::new(EXECUTE, PROFILE, 0)),
                        (Step::Scan, Message::new(EXECUTE, RADIO, 1)),
                        (Step::Join, join),
                    ])
                } else {
                    self.start(&[(Step::Join, join)])
                };
                join.data = [0; 32];
                answer
            }
            Some("leave") => self.start(&[(Step::Leave, Message::new(EXECUTE, PROFILE, 0))]),
            Some(_) => String::from("error usage: wifi [state|scan|list|join ID [PMK]|leave]\n"),
        }
    }
}
