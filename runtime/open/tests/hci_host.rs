//! Host tests for the Bluetooth multiplexer (runtime/open/hcimux.rs) against a
//! scripted controller, with bitchat's own BLE host (apps/open/bitchat/src/
//! ble.rs) as one of the clients. `make test-unit` runs them.
extern crate alloc;

#[path = "../hcimux.rs"]
#[allow(dead_code)]
mod hcimux;
#[path = "../../../apps/open/bitchat/src/ble.rs"]
#[allow(dead_code)]
mod ble;

use hcimux::{Mux, Refusal, COMMAND_MS};
use std::collections::VecDeque;

fn cmd(op: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![1, op as u8, (op >> 8) as u8, params.len() as u8];
    p.extend_from_slice(params);
    p
}
fn ev(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![4, code, params.len() as u8];
    p.extend_from_slice(params);
    p
}
fn acl(handle: u16, data: &[u8]) -> Vec<u8> {
    let mut p = vec![2];
    p.extend_from_slice(&handle.to_le_bytes());
    p.extend_from_slice(&(data.len() as u16).to_le_bytes());
    p.extend_from_slice(data);
    p
}
/// A client that, like any real host, unmasks the events it wants (LE Meta is
/// off by the standard's default).
fn open(b: &mut Bench, pid: u32, system: bool) -> u8 {
    let c = b.mux.open(pid, format!("app{pid}").as_bytes(), system).unwrap();
    b.write(c, &cmd(0x0c01, &[0xff; 8])).unwrap();
    b.write(c, &cmd(0x2001, &[0xff, 0xff, 0xff, 0, 0, 0, 0, 0])).unwrap();
    b.read(c);
    c
}
fn op_of(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[1], p[2]])
}
/// The opcode and status of a Command Complete or Status, if `p` is one.
fn answer(p: &[u8]) -> Option<(u16, u8)> {
    match p.get(1) {
        Some(0x0e) => Some((u16::from_le_bytes([p[4], p[5]]), p[6])),
        Some(0x0f) => Some((u16::from_le_bytes([p[5], p[6]]), p[3])),
        _ => None,
    }
}
fn conn_complete(handle: u16, role: u8) -> Vec<u8> {
    let mut e = vec![0x01, 0];
    e.extend_from_slice(&handle.to_le_bytes());
    e.extend_from_slice(&[role, 1, 1, 2, 3, 4, 5, 6, 0x18, 0, 0, 0, 0x48, 0, 0]);
    ev(0x3e, &e)
}
fn adv_report(n: u8) -> Vec<u8> {
    ev(0x3e, &[0x02, 1, 0, 1, n, 2, 3, 4, 5, 6, 3, 2, 1, 6, 0xc0])
}

/// A controller that answers every command at once and keeps the state the
/// tests look at. Like a real one it sends only the connection event its LE
/// event mask asks for, and takes either legacy or extended advertising,
/// scanning and initiating commands until it is reset.
struct Controller {
    le_mask: u64,
    /// 1 legacy, 2 extended, since the last reset.
    family: u8,
    /// Opcodes it answers with Command Disallowed.
    refuse: Vec<u16>,
    /// Every command it got, in order.
    commands: Vec<Vec<u8>>,
    acl: Vec<Vec<u8>>,
    buffers: u16,
    in_buffers: u16,
    scan: Option<bool>,
    adv: bool,
    /// Answer nothing (a hung controller).
    silent: bool,
    rand: u8,
    pending: VecDeque<Vec<u8>>,
    resets: u32,
}
impl Controller {
    fn new(buffers: u16) -> Controller {
        Controller { le_mask: 0x1f, family: 0, refuse: vec![], commands: vec![], acl: vec![], buffers, in_buffers: 0, scan: None, adv: false, silent: false, rand: 0, pending: VecDeque::new(), resets: 0 }
    }
    fn take(&mut self, p: Vec<u8>) {
        if p[0] == 2 {
            self.in_buffers += 1;
            assert!(self.in_buffers <= self.buffers, "the controller's ACL buffers overflowed");
            self.acl.push(p);
            return;
        }
        assert_eq!(p[0], 1);
        let op = op_of(&p);
        let params = p[4..].to_vec();
        self.commands.push(p);
        if self.silent {
            return;
        }
        let [lo, hi] = op.to_le_bytes();
        let complete = |ret: &[u8]| {
            let mut b = vec![1, lo, hi, 0];
            b.extend_from_slice(ret);
            ev(0x0e, &b)
        };
        let family = match op {
            0x2006..=0x200d => 1,
            0x2035..=0x204a => 2,
            _ => 0,
        };
        let status_style = matches!(op, 0x0406 | 0x200d | 0x2013 | 0x2043);
        if self.refuse.contains(&op) || (family != 0 && self.family != 0 && family != self.family) {
            let e = if status_style { ev(0x0f, &[0x0c, 1, lo, hi]) } else { ev(0x0e, &[1, lo, hi, 0x0c]) };
            self.pending.push_front(e);
            return;
        }
        if family != 0 {
            self.family = family;
        }
        let e = match op {
            0x2001 => {
                self.le_mask = u64::from_le_bytes(params[..8].try_into().unwrap());
                complete(&[])
            }
            0x2002 => complete(&[251, 0, self.buffers as u8]),
            0x1005 => complete(&[0xfd, 3, 0, 4, 0, 0, 0]),
            0x2018 => {
                self.rand += 1;
                complete(&[self.rand; 8])
            }
            0x200a => {
                self.adv = params[0] == 1;
                complete(&[])
            }
            0x200c => {
                self.scan = (params[0] == 1).then_some(params[1] == 1);
                complete(&[])
            }
            0x0c03 => {
                self.resets += 1;
                self.le_mask = 0x1f;
                self.family = 0;
                self.scan = None;
                self.adv = false;
                complete(&[])
            }
            0x0406 => {
                self.pending.push_back(ev(0x05, &[0, params[0], params[1], 0x16]));
                ev(0x0f, &[0, 1, lo, hi])
            }
            0x200d | 0x2013 | 0x2043 => ev(0x0f, &[0, 1, lo, hi]),
            0x2022 => complete(&params[..2]),
            _ => complete(&[]),
        };
        self.pending.push_front(e);
    }
    /// A connection: LE Enhanced Connection Complete when its mask asks for
    /// that (then never the legacy event), else LE Connection Complete.
    fn connect(&self, handle: u16, role: u8, status: u8) -> Option<Vec<u8>> {
        let [lo, hi] = handle.to_le_bytes();
        let peer = [1, 1, 2, 3, 4, 5, 6];
        let tail = [0x18, 0, 0, 0, 0x48, 0, 0];
        let mut e = if self.le_mask >> 9 & 1 != 0 {
            let mut e = vec![0x0a, status, lo, hi, role];
            e.extend_from_slice(&peer);
            e.extend_from_slice(&[0xaa; 6]);
            e.extend_from_slice(&[0xbb; 6]);
            e
        } else if self.le_mask & 1 != 0 {
            let mut e = vec![0x01, status, lo, hi, role];
            e.extend_from_slice(&peer);
            e
        } else {
            return None;
        };
        e.extend_from_slice(&tail);
        Some(ev(0x3e, &e))
    }
    /// The controller finished sending `n` packets of `handle`.
    fn completed(&mut self, handle: u16, n: u16) -> Vec<u8> {
        self.in_buffers -= n;
        let mut b = vec![1];
        b.extend_from_slice(&handle.to_le_bytes());
        b.extend_from_slice(&n.to_le_bytes());
        ev(0x13, &b)
    }
}

struct Bench {
    mux: Mux,
    ctl: Controller,
    now: u64,
}
impl Bench {
    fn new(buffers: u16) -> Bench {
        let mut b = Bench { mux: Mux::new(0), ctl: Controller::new(buffers), now: 0 };
        b.run();
        b
    }
    /// Moves packets both ways until nothing moves.
    fn run(&mut self) {
        for _ in 0..1000 {
            let mut moved = false;
            while !self.mux.out.is_empty() {
                let p = self.mux.out.remove(0);
                self.ctl.take(p);
                moved = true;
            }
            while let Some(e) = self.ctl.pending.pop_front() {
                self.mux.receive(&e, self.now);
                moved = true;
            }
            self.mux.poll(self.now);
            if !moved && self.mux.out.is_empty() {
                return;
            }
        }
        panic!("the bench did not settle");
    }
    /// The controller makes a connection and says so as its mask asks.
    fn connect(&mut self, handle: u16, role: u8, status: u8) -> Vec<u8> {
        let p = self.ctl.connect(handle, role, status).expect("LE Connection Complete is masked");
        self.from_controller(&p);
        p
    }
    fn from_controller(&mut self, p: &[u8]) {
        self.mux.receive(p, self.now);
        self.run();
    }
    fn write(&mut self, c: u8, p: &[u8]) -> Result<(), Refusal> {
        let r = self.mux.write(c, p, self.now);
        self.run();
        r
    }
    fn read(&mut self, c: u8) -> Vec<Vec<u8>> {
        let mut v = vec![];
        while let Some(p) = self.mux.take(c) {
            v.push(p);
        }
        v
    }
    fn sent(&self, op: u16) -> usize {
        self.ctl.commands.iter().filter(|p| op_of(p) == op).count()
    }
}

#[test]
fn vendor_and_controller_commands_are_refused_for_apps() {
    let mut b = Bench::new(4);
    let a = open(&mut b, 10, false);
    let before = b.ctl.commands.len();
    for op in [0xfc79u16, 0xfc93, 0x0c1a, 0x2010, 0x2011, 0x201d, 0x0c13, 0x0401, 0x203d] {
        b.write(a, &cmd(op, &[0; 4])).unwrap();
        let got = b.read(a);
        assert_eq!(got.len(), 1, "{op:04x}");
        assert_eq!(answer(&got[0]), Some((op, 0x0c)), "{op:04x}");
    }
    assert_eq!(b.ctl.commands.len(), before, "nothing reached the controller");
    // A system app's vendor command does reach it.
    let s = open(&mut b, 11, true);
    b.write(s, &cmd(0xfc79, &[7, 0, 0, 0])).unwrap();
    assert_eq!(b.sent(0xfc79), 1);
    assert_eq!(answer(&b.read(s)[0]), Some((0xfc79, 0)));
}

#[test]
fn malformed_packets_are_refused() {
    let mut b = Bench::new(4);
    let a = open(&mut b, 10, false);
    assert_eq!(b.write(a, &[1, 3, 0x0c, 1]), Err(Refusal::Invalid));
    assert_eq!(b.write(a, &[3, 0, 0, 0]), Err(Refusal::Invalid));
    assert_eq!(b.write(a, &[2, 0x40, 0, 9, 0]), Err(Refusal::Invalid));
    assert_eq!(b.mux.write(7, &cmd(0x1009, &[]), 0), Err(Refusal::Invalid), "no such client");
}

#[test]
fn one_client_per_app_and_a_place_kept_for_core_apps() {
    let mut b = Bench::new(4);
    assert!(b.mux.open(1, b"chat", false).is_some());
    assert!(b.mux.open(1, b"other", false).is_none(), "one per process");
    // Child processes of one app (each its own pid) get no more places.
    assert!(b.mux.open(2, b"chat", false).is_none(), "one per app");
    assert!(b.mux.open(3, b"b", false).is_some() && b.mux.open(4, b"c", false).is_some());
    assert!(b.mux.open(5, b"d", false).is_none(), "the last place is a core app's");
    assert!(b.mux.open(6, b"settings", true).is_some());
    assert!(b.mux.open(7, b"home", true).is_none(), "all four taken");
}

#[test]
fn an_app_reset_resets_only_that_app() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200b, &[0, 0xa0, 0, 0x60, 0, 1, 0])).unwrap();
    b.write(a, &cmd(0x200c, &[1, 0])).unwrap();
    b.read(a);
    b.write(c, &cmd(0x0c03, &[])).unwrap();
    assert_eq!(b.ctl.resets, 0, "the controller was not reset");
    assert_eq!(answer(&b.read(c)[0]), Some((0x0c03, 0)));
    assert_eq!(b.ctl.scan, Some(false), "A still scans");
    b.from_controller(&adv_report(1));
    assert_eq!(b.read(a).len(), 1);
    assert!(b.read(c).is_empty(), "C does not scan");
}

#[test]
fn two_scanners_share_the_scan() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    let params = [0, 0xa0, 0, 0x60, 0, 1, 0];
    b.write(a, &cmd(0x200b, &params)).unwrap();
    b.write(a, &cmd(0x200c, &[1, 1])).unwrap();
    assert_eq!(b.ctl.scan, Some(true), "one scanner: duplicates filtered as asked");
    // Different parameters while A scans are refused; the same ones are fine.
    b.write(c, &cmd(0x200b, &[1, 0x10, 0, 0x10, 0, 0, 0])).unwrap();
    b.write(c, &cmd(0x200b, &params)).unwrap();
    let got: Vec<_> = b.read(c).iter().filter_map(|p| answer(p)).collect();
    assert_eq!(got, vec![(0x200b, 0x0c), (0x200b, 0)]);
    b.write(c, &cmd(0x200c, &[1, 1])).unwrap();
    assert_eq!(b.ctl.scan, Some(false), "two scanners: no duplicate filtering");
    b.read(a);
    b.read(c);
    b.from_controller(&adv_report(7));
    assert_eq!(b.read(a), vec![adv_report(7)]);
    assert_eq!(b.read(c), vec![adv_report(7)]);
    let enables = b.sent(0x200c);
    b.write(a, &cmd(0x200c, &[0, 0])).unwrap();
    assert_eq!(b.sent(0x200c), enables, "C still scans");
    assert_eq!(answer(&b.read(a)[0]), Some((0x200c, 0)));
    b.from_controller(&adv_report(8));
    assert!(b.read(a).is_empty());
    assert_eq!(b.read(c).len(), 1);
    b.write(c, &cmd(0x200c, &[0, 0])).unwrap();
    assert_eq!(b.ctl.scan, None, "the last scanner stopped the scan");
}

#[test]
fn one_advertiser_at_a_time() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2005, &[1, 2, 3, 4, 5, 0xc6])).unwrap();
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    assert!(b.ctl.adv);
    for op in [0x2005u16, 0x2006, 0x2008, 0x200a] {
        b.write(c, &cmd(op, &[0])).unwrap();
    }
    let got: Vec<_> = b.read(c).iter().filter_map(|p| answer(p)).map(|(_, s)| s).collect();
    assert_eq!(got, vec![0x0c; 4]);
    assert_eq!(b.sent(0x2005), 1);
    // Once A is gone, C may advertise.
    b.mux.close(a, 0);
    b.run();
    assert!(!b.ctl.adv, "closing stopped A's advertising");
    b.write(c, &cmd(0x200a, &[1])).unwrap();
    assert!(b.ctl.adv);
}

#[test]
fn extended_advertising_sets_belong_to_their_app() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2036, &[1; 25])).unwrap();
    b.write(c, &cmd(0x2036, &[2; 25])).unwrap();
    b.write(c, &cmd(0x2037, &[1, 3, 1, 0])).unwrap(); // A's set
    b.write(c, &cmd(0x2039, &[1, 1, 1, 0, 0, 0])).unwrap(); // A's set
    b.write(c, &cmd(0x2039, &[0, 0])).unwrap(); // every set
    b.write(c, &cmd(0x2039, &[1, 1, 2, 0, 0, 0])).unwrap(); // its own
    let got: Vec<_> = b.read(c).iter().filter_map(|p| answer(p)).collect();
    assert_eq!(got, vec![(0x2036, 0), (0x2037, 0x0c), (0x2039, 0x0c), (0x2039, 0x0c), (0x2039, 0)]);
    b.mux.close(c, 0);
    b.run();
    let last: Vec<_> = b.ctl.commands.iter().rev().take(2).cloned().collect();
    assert_eq!(last, vec![cmd(0x203c, &[2]), cmd(0x2039, &[0, 1, 2, 0, 0, 0])], "closing disabled and removed C's set");
}

#[test]
fn links_belong_to_whoever_made_them() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    // C connects out as a central while A advertises.
    b.write(c, &cmd(0x200d, &[0; 25])).unwrap();
    b.write(a, &cmd(0x200d, &[0; 25])).unwrap();
    assert_eq!(answer(&b.read(a).pop().unwrap()), Some((0x200d, 0x0c)), "one initiator at a time");
    b.read(c);
    b.from_controller(&conn_complete(0x40, 1)); // a central connected to A
    b.from_controller(&conn_complete(0x41, 0)); // C's connection
    assert_eq!(b.read(a), vec![conn_complete(0x40, 1)]);
    assert_eq!(b.read(c), vec![conn_complete(0x41, 0)]);
    // Data and commands only for one's own links.
    assert_eq!(b.write(c, &acl(0x40, &[1, 2, 3])), Err(Refusal::Denied));
    assert_eq!(b.write(c, &acl(0x2040, &[1])), Err(Refusal::Denied), "flags do not hide the handle");
    b.write(c, &cmd(0x2022, &[0x40, 0, 251, 0, 0x48, 8])).unwrap();
    b.write(c, &cmd(0x0406, &[0x40, 0, 0x13])).unwrap();
    let got: Vec<_> = b.read(c).iter().filter_map(|p| answer(p)).collect();
    assert_eq!(got, vec![(0x2022, 0x02), (0x0406, 0x02)]);
    assert_eq!(b.sent(0x0406), 0);
    // Controller-wide settings stay refused even when the first octets name one's own link.
    b.write(c, &cmd(0x2014, &[0x41, 0, 0xff, 0xff, 0x1f])).unwrap();
    b.write(c, &cmd(0x2031, &[0x41, 0, 1])).unwrap();
    let got: Vec<_> = b.read(c).iter().filter_map(|p| answer(p)).collect();
    assert_eq!(got, vec![(0x2014, 0x0c), (0x2031, 0x0c)]);
    b.from_controller(&acl(0x2040, &[9, 9]));
    b.from_controller(&ev(0x08, &[0, 0x40, 0, 1]));
    assert_eq!(b.read(a), vec![acl(0x2040, &[9, 9]), ev(0x08, &[0, 0x40, 0, 1])]);
    assert!(b.read(c).is_empty());
    b.from_controller(&ev(0x05, &[0, 0x40, 0, 0x13]));
    assert_eq!(b.read(a), vec![ev(0x05, &[0, 0x40, 0, 0x13])]);
    assert!(b.read(c).is_empty());
    assert_eq!(b.write(a, &acl(0x40, &[1])), Err(Refusal::Denied), "the link is gone");
}

#[test]
fn closing_ends_what_the_app_ran() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.write(a, &cmd(0x200c, &[1, 0])).unwrap();
    b.from_controller(&conn_complete(0x40, 1));
    b.write(c, &cmd(0x200d, &[0; 25])).unwrap();
    b.mux.close(a, 0);
    b.run();
    assert!(b.ctl.commands.contains(&cmd(0x0406, &[0x40, 0, 0x13])));
    assert_eq!(b.ctl.scan, None);
    b.read(c);
    // C's connection attempt is untouched and C is still served.
    b.from_controller(&conn_complete(0x41, 0));
    assert_eq!(b.read(c), vec![conn_complete(0x41, 0)]);
    // A central connecting while nobody advertises is let go at once.
    b.mux.close(c, 0);
    b.run();
    b.from_controller(&conn_complete(0x42, 1));
    assert!(b.ctl.commands.contains(&cmd(0x0406, &[0x42, 0, 0x13])));
    assert!(b.mux.idle());
}

#[test]
fn closing_cancels_a_connection_attempt() {
    let mut b = Bench::new(4);
    let a = open(&mut b, 10, false);
    b.write(a, &cmd(0x200d, &[0; 25])).unwrap();
    b.mux.close(a, 0);
    b.run();
    assert_eq!(b.sent(0x200e), 1);
    // The controller connects anyway (a race): the link is dropped.
    b.from_controller(&conn_complete(0x43, 0));
    assert!(b.ctl.commands.contains(&cmd(0x0406, &[0x43, 0, 0x13])));
}

#[test]
fn commands_go_one_at_a_time_and_answers_go_to_the_sender() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.ctl.silent = true;
    let before = b.ctl.commands.len();
    b.write(a, &cmd(0x2018, &[])).unwrap();
    b.write(c, &cmd(0x2018, &[])).unwrap();
    b.write(a, &cmd(0x1009, &[])).unwrap();
    assert_eq!(b.ctl.commands.len(), before + 1, "only one command is at the controller");
    // The controller answers the first LE Rand: A's.
    b.from_controller(&ev(0x0e, &[1, 0x18, 0x20, 0, 5, 5, 5, 5, 5, 5, 5, 5]));
    assert_eq!(b.ctl.commands.len(), before + 2);
    b.from_controller(&ev(0x0e, &[1, 0x18, 0x20, 0, 6, 6, 6, 6, 6, 6, 6, 6]));
    let ra = b.read(a);
    let rc = b.read(c);
    assert_eq!(ra[0][7], 5);
    assert_eq!(rc[0][7], 6);
    // A stray answer for nothing in flight is dropped.
    b.from_controller(&ev(0x0e, &[1, 0x18, 0x20, 0, 7, 7, 7, 7, 7, 7, 7, 7]));
    assert!(b.read(a).is_empty() && b.read(c).is_empty());
}

#[test]
fn an_unanswered_command_times_out() {
    let mut b = Bench::new(4);
    let a = open(&mut b, 10, false);
    b.ctl.silent = true;
    b.write(a, &cmd(0x1009, &[])).unwrap();
    b.write(a, &cmd(0x2018, &[])).unwrap();
    b.now = COMMAND_MS - 1;
    b.run();
    assert!(b.read(a).is_empty());
    b.now = COMMAND_MS + 1;
    b.run();
    assert_eq!(answer(&b.read(a)[0]), Some((0x1009, 0x1f)));
    assert_eq!(op_of(b.ctl.commands.last().unwrap()), 0x2018, "the next command went out");
}

#[test]
fn acl_buffers_are_shared_and_completions_split() {
    let mut b = Bench::new(2);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.from_controller(&conn_complete(0x40, 1));
    b.write(c, &cmd(0x200d, &[0; 25])).unwrap();
    b.from_controller(&conn_complete(0x41, 0));
    b.read(a);
    b.read(c);
    for k in 0..3 {
        b.write(a, &acl(0x40, &[k])).unwrap();
        b.write(c, &acl(0x41, &[k])).unwrap();
    }
    assert_eq!(b.ctl.acl.len(), 2, "two buffers, two packets");
    let e = b.ctl.completed(0x40, 1);
    b.from_controller(&e);
    assert_eq!(b.ctl.acl.len(), 3);
    assert_eq!(b.read(a), vec![e]);
    assert!(b.read(c).is_empty(), "C hears only of its own packets");
    // Both handles in one event: each owner hears of its own.
    b.ctl.in_buffers -= 2;
    let both = ev(0x13, &[2, 0x40, 0, 1, 0, 0x41, 0, 1, 0]);
    b.from_controller(&both);
    assert_eq!(b.read(a), vec![ev(0x13, &[1, 0x40, 0, 1, 0])]);
    assert_eq!(b.read(c), vec![ev(0x13, &[1, 0x41, 0, 1, 0])]);
    // A disconnection frees that link's buffers.
    b.from_controller(&ev(0x05, &[0, 0x41, 0, 0x13]));
    b.ctl.in_buffers = b.ctl.in_buffers.saturating_sub(1);
    assert_eq!(b.ctl.acl.len(), 5, "the closed link's last packet was dropped, not sent");
}

#[test]
fn a_slow_reader_loses_only_its_own_packets() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200c, &[1, 0])).unwrap();
    b.write(c, &cmd(0x200c, &[1, 0])).unwrap();
    b.read(a);
    b.read(c);
    let mut got = 0;
    for n in 0..200u8 {
        b.from_controller(&adv_report(n));
        got += b.read(a).len();
    }
    assert_eq!(got, 200);
    assert_eq!(b.read(c).len(), 64);
    assert!(b.mux.dropped >= 136);
}

#[test]
fn event_masks_are_per_app() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.from_controller(&conn_complete(0x40, 1));
    // A wants no Encryption Change (bit 7); C wants only Hardware Error (bit 15).
    let mask_a: u64 = 0x2000_0000_0000_0010;
    b.write(a, &cmd(0x0c01, &mask_a.to_le_bytes())).unwrap();
    b.write(c, &cmd(0x0c01, &(1u64 << 15).to_le_bytes())).unwrap();
    let sent = b.ctl.commands.iter().rev().find(|p| op_of(p) == 0x0c01).unwrap().clone();
    let union = u64::from_le_bytes(sent[4..12].try_into().unwrap());
    assert_eq!(union, mask_a | 1 << 15 | 1 << 4 | 1 << 61);
    b.read(a);
    b.read(c);
    b.from_controller(&ev(0x08, &[0, 0x40, 0, 1]));
    assert!(b.read(a).is_empty());
    b.from_controller(&ev(0x10, &[1]));
    assert!(b.read(a).is_empty());
    assert_eq!(b.read(c), vec![ev(0x10, &[1])]);
}

#[test]
fn vendor_events_reach_only_system_apps() {
    let mut b = Bench::new(4);
    let (a, s) = (open(&mut b, 10, false), open(&mut b, 11, true));
    b.from_controller(&ev(0xff, &[0x21, 1, 2]));
    assert!(b.read(a).is_empty());
    assert_eq!(b.read(s).len(), 1);
}

#[test]
fn a_restart_or_system_reset_ends_every_link() {
    let mut b = Bench::new(4);
    let (a, s) = (open(&mut b, 10, false), open(&mut b, 11, true));
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.from_controller(&conn_complete(0x40, 1));
    b.read(a);
    b.write(s, &cmd(0x0c03, &[])).unwrap();
    assert_eq!(b.ctl.resets, 1);
    // Its link ended, and so did its advertising.
    assert_eq!(b.read(a), vec![ev(0x05, &[0, 0x40, 0, 0x16]), ev(0x10, &[0])]);
    // Event masks are given back to the fresh controller.
    let tail: Vec<u16> = b.ctl.commands.iter().rev().take(3).map(|p| op_of(p)).collect();
    assert_eq!(tail, vec![0x2002, 0x0c01, 0x2001]);
    // Radio restarting the controller does the same.
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.from_controller(&conn_complete(0x44, 1));
    b.read(a);
    b.mux.restarted(b.now);
    b.run();
    assert_eq!(b.read(a), vec![ev(0x05, &[0, 0x44, 0, 0x16]), ev(0x10, &[0])]);
}

/// Runs bitchat's BLE host as client `c` until neither side has anything to send.
fn serve(b: &mut Bench, c: u8, h: &mut ble::Host) {
    for _ in 0..100 {
        let mut moved = false;
        while let Some(p) = h.out.pop_front() {
            b.write(c, &p).unwrap();
            moved = true;
        }
        while let Some(p) = b.mux.take(c) {
            h.receive(&p);
            moved = true;
        }
        if !moved {
            return;
        }
    }
    panic!("bitchat did not settle");
}

#[test]
fn bitchat_works_through_the_multiplexer_beside_a_scanner() {
    let mut b = Bench::new(4);
    let chat = open(&mut b, 10, false);
    let other = open(&mut b, 11, false);
    let mut h = ble::Host::new("VioletOS");
    h.start();
    serve(&mut b, chat, &mut h);
    assert_eq!(b.ctl.resets, 0, "bitchat's reset stayed virtual");
    let rand: Vec<_> = h.results.iter().filter(|r| r.0 == 0x2018 && r.1 == 0).collect();
    assert_eq!(rand.len(), 2, "LE Rand answered twice");
    h.advertise([1, 2, 3, 4, 5, 6]);
    serve(&mut b, chat, &mut h);
    assert!(h.ready && h.advertising, "advertising: {:?}", h.failure);
    assert!(h.failure.is_none(), "{:?}", h.failure);
    assert!(b.ctl.adv && b.ctl.scan == Some(true));
    // Another app scans too; both hear an advertisement.
    b.write(other, &cmd(0x200c, &[1, 0])).unwrap();
    b.read(other);
    assert_eq!(b.ctl.scan, Some(false));
    let reports = h.reports;
    b.from_controller(&adv_report(3));
    serve(&mut b, chat, &mut h);
    assert_eq!(h.reports, reports + 1);
    assert_eq!(b.read(other).len(), 1);
    // A phone connects to bitchat and exchanges the ATT MTU. The other app
    // unmasked Enhanced Connection Complete, so that is what the controller
    // sends; bitchat asked for the legacy event and gets that.
    let cc = b.connect(0x40, 1, 0);
    assert_eq!(cc[3], 0x0a);
    let mut got = vec![];
    while let Some(p) = b.mux.take(chat) {
        got.push(p.clone());
        h.receive(&p);
    }
    assert_eq!(got[0][3], 0x01, "translated for bitchat");
    serve(&mut b, chat, &mut h);
    assert!(h.events.iter().any(|e| matches!(e, ble::Event::Connected(0x40))));
    assert!(b.read(other).is_empty(), "the other app sees nothing of bitchat's link");
    b.from_controller(&acl(0x2040, &[3, 0, 4, 0, 0x02, 0x0f, 0x02]));
    serve(&mut b, chat, &mut h);
    let reply = b.ctl.acl.last().unwrap();
    assert_eq!(&reply[..3], &[2, 0x40, 0]);
    assert_eq!(&reply[5..], &[3, 0, 4, 0, 0x03, 0x05, 0x02]);
    // The other app may not touch it.
    assert_eq!(b.write(other, &acl(0x40, &[0, 0, 4, 0])), Err(Refusal::Denied));
    // A second bitchat cannot advertise while the first does, and says so.
    let second = b.mux.open(12, b"bitchat2", false).unwrap();
    let mut h2 = ble::Host::new("Second");
    h2.start();
    serve(&mut b, second, &mut h2);
    h2.advertise([9, 9, 9, 9, 9, 9]);
    serve(&mut b, second, &mut h2);
    assert_eq!(h2.failure.as_deref(), Some("command 2005 failed with 0c"));
    assert!(!h2.ready);
}

fn answers(b: &mut Bench, c: u8) -> Vec<(u16, u8)> {
    b.read(c).iter().filter_map(|p| answer(p)).collect()
}

#[test]
fn the_random_address_claims_nothing_and_set_links_go_to_the_set_owner() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2036, &[0; 25])).unwrap();
    b.write(a, &cmd(0x2039, &[1, 1, 0, 0, 0, 0])).unwrap();
    // X only sets the random address: that makes it no advertiser.
    b.write(x, &cmd(0x2005, &[1, 2, 3, 4, 5, 0xc6])).unwrap();
    b.read(a);
    b.read(x);
    let cc = b.connect(0x40, 1, 0);
    assert!(b.read(a).is_empty() && b.read(x).is_empty(), "nobody's until the set is named");
    let term = ev(0x3e, &[0x12, 0, 0, 0x40, 0, 1]);
    b.from_controller(&term);
    assert_eq!(b.read(a), vec![cc, term]);
    assert!(b.read(x).is_empty());
    assert_eq!(b.write(x, &acl(0x40, &[1])), Err(Refusal::Denied));
    b.write(a, &acl(0x40, &[1])).unwrap();
    assert_eq!(b.sent(0x0406), 0, "the link was kept");
}

#[test]
fn a_refused_advertising_command_gives_the_claim_back() {
    let mut b = Bench::new(4);
    let (a, c) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.ctl.refuse.push(0x2006);
    b.write(a, &cmd(0x2006, &[0; 15])).unwrap();
    assert_eq!(answers(&mut b, a), vec![(0x2006, 0x0c)]);
    b.ctl.refuse.clear();
    b.write(c, &cmd(0x2006, &[0; 15])).unwrap();
    b.write(c, &cmd(0x200a, &[1])).unwrap();
    assert_eq!(answers(&mut b, c), vec![(0x2006, 0), (0x200a, 0)]);
    assert!(b.ctl.adv);
    // The random address is now the advertiser's alone.
    b.write(a, &cmd(0x2005, &[1, 2, 3, 4, 5, 0xc6])).unwrap();
    assert_eq!(answers(&mut b, a), vec![(0x2005, 0x0c)]);
    // Turning advertising off gives it up.
    b.write(c, &cmd(0x200a, &[0])).unwrap();
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    assert_eq!(answers(&mut b, a), vec![(0x200a, 0)]);
    // A link made then is the new advertiser's.
    let cc = b.connect(0x40, 1, 0);
    assert_eq!(b.read(a), vec![cc]);
}

#[test]
fn a_directed_advertising_timeout_is_the_advertisers_not_the_initiators() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x200d, &[0; 25])).unwrap();
    b.read(a);
    b.write(x, &cmd(0x2006, &[0; 15])).unwrap();
    b.write(x, &cmd(0x200a, &[1])).unwrap();
    b.read(x);
    let timeout = b.connect(0, 1, 0x3c);
    assert_eq!(b.read(x), vec![timeout]);
    assert!(b.read(a).is_empty(), "A's attempt goes on");
    b.write(x, &cmd(0x200d, &[0; 25])).unwrap();
    assert_eq!(answers(&mut b, x), vec![(0x200d, 0x0c)], "A still creates the connection");
    let cc = b.connect(0x41, 0, 0);
    assert_eq!(b.read(a), vec![cc]);
    b.write(a, &acl(0x41, &[1])).unwrap();
    assert!(b.read(x).is_empty());
}

#[test]
fn a_create_that_never_reached_the_controller_leaves_no_initiator() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.ctl.silent = true;
    b.write(a, &cmd(0x1009, &[])).unwrap(); // at the controller
    b.write(a, &cmd(0x200d, &[0; 25])).unwrap(); // waits behind it
    b.write(a, &cmd(0x0c03, &[])).unwrap(); // a virtual reset drops it
    assert_eq!(answers(&mut b, a), vec![(0x0c03, 0)]);
    b.ctl.silent = false;
    b.from_controller(&ev(0x0e, &[1, 0x09, 0x10, 0, 1, 2, 3, 4, 5, 6]));
    assert_eq!((b.sent(0x200d), b.sent(0x200e)), (0, 0), "nothing to cancel");
    b.write(x, &cmd(0x200d, &[0; 25])).unwrap();
    assert_eq!(answers(&mut b, x), vec![(0x200d, 0)]);
    // A cancel the controller refuses (no attempt pending there) frees it too.
    b.ctl.refuse.push(0x200e);
    b.mux.close(x, 0);
    b.run();
    assert_eq!(b.sent(0x200e), 1);
    b.write(a, &cmd(0x200d, &[0; 25])).unwrap();
    assert_eq!(answers(&mut b, a), vec![(0x200d, 0)]);
}

#[test]
fn apps_unmask_only_what_is_routed_to_them() {
    let mut b = Bench::new(4);
    let a = open(&mut b, 10, false);
    b.write(a, &cmd(0x2001, &[0xff; 8])).unwrap();
    b.write(a, &cmd(0x0c01, &[0xff; 8])).unwrap();
    let last = |b: &Bench, op| {
        let p = b.ctl.commands.iter().rev().find(|p| op_of(p) == op).unwrap();
        u64::from_le_bytes(p[4..12].try_into().unwrap())
    };
    assert_eq!(last(&b, 0x2001), 0x7f | 0xf << 9 | 1 << 17 | 1 << 19);
    assert_eq!(last(&b, 0x0c01), 1 << 4 | 1 << 7 | 1 << 11 | 1 << 15 | 1 << 25 | 1 << 47 | 1 << 61);
    // A core app may unmask more, but never Enhanced Connection Complete v2,
    // which would hide connections from the multiplexer.
    let s = open(&mut b, 11, true);
    b.write(s, &cmd(0x2001, &[0xff; 8])).unwrap();
    assert_eq!(last(&b, 0x2001), !(1u64 << 40));
}

#[test]
fn each_app_hears_of_its_connection_in_the_form_it_unmasked() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2001, &[0x1f, 0, 0, 0, 0, 0, 0, 0])).unwrap(); // legacy only
    b.write(x, &cmd(0x2001, &[0x1e, 2, 0, 0, 0, 0, 0, 0])).unwrap(); // enhanced only
    b.write(a, &cmd(0x200a, &[1])).unwrap();
    b.write(x, &cmd(0x200d, &[0; 25])).unwrap();
    b.read(a);
    b.read(x);
    let cc = b.connect(0x40, 1, 0);
    assert_eq!(cc[3], 0x0a, "the controller sends only the enhanced event");
    assert_eq!(b.read(a), vec![conn_complete(0x40, 1)]);
    let cc = b.connect(0x41, 0, 0);
    assert_eq!(b.read(x), vec![cc]);
    // A controller without the enhanced event: X still hears of its link.
    b.write(x, &cmd(0x200d, &[0; 25])).unwrap();
    b.read(x);
    b.from_controller(&conn_complete(0x42, 0));
    let mut want = vec![0x0a, 0, 0x42, 0, 0, 1, 1, 2, 3, 4, 5, 6];
    want.extend_from_slice(&[0; 12]);
    want.extend_from_slice(&[0x18, 0, 0, 0, 0x48, 0, 0]);
    assert_eq!(b.read(x), vec![ev(0x3e, &want)]);
}

#[test]
fn with_two_set_owners_the_terminated_set_names_the_owner() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2036, &[1; 25])).unwrap();
    b.write(x, &cmd(0x2036, &[2; 25])).unwrap();
    b.write(a, &cmd(0x2039, &[1, 1, 1, 0, 0, 0])).unwrap();
    b.write(x, &cmd(0x2039, &[1, 1, 2, 0, 0, 0])).unwrap();
    b.read(a);
    b.read(x);
    let cc = b.connect(0x40, 1, 0);
    let term = ev(0x3e, &[0x12, 0, 2, 0x40, 0, 1]);
    b.from_controller(&term);
    assert_eq!(b.read(x), vec![cc, term]);
    assert!(b.read(a).is_empty());
    assert_eq!(b.sent(0x0406), 0);
    // A link no set is named for in time is let go.
    b.connect(0x41, 1, 0);
    b.now += COMMAND_MS;
    b.run();
    assert!(b.ctl.commands.contains(&cmd(0x0406, &[0x41, 0, 0x13])));
    assert!(b.read(a).is_empty() && b.read(x).is_empty());
}

#[test]
fn legacy_and_extended_commands_do_not_mix() {
    let mut b = Bench::new(4);
    let (a, chat) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.write(a, &cmd(0x2036, &[0; 25])).unwrap();
    for (op, p) in [(0x2006u16, &[0u8; 15][..]), (0x200b, &[0, 0xa0, 0, 0x60, 0, 1, 0]), (0x200c, &[1, 0]), (0x200d, &[0; 25])] {
        b.write(chat, &cmd(op, p)).unwrap();
    }
    assert_eq!(answers(&mut b, chat), vec![(0x2006, 0x0c), (0x200b, 0x0c), (0x200c, 0x0c), (0x200d, 0x0c)]);
    assert_eq!(b.sent(0x2006) + b.sent(0x200b) + b.sent(0x200c) + b.sent(0x200d), 0, "none reached the controller");
    // Once nothing extended is in use, the controller is reset for legacy.
    b.mux.close(a, 0);
    b.run();
    let resets = b.ctl.resets;
    b.write(chat, &cmd(0x2006, &[0; 15])).unwrap();
    b.write(chat, &cmd(0x200a, &[1])).unwrap();
    assert_eq!(answers(&mut b, chat), vec![(0x2006, 0), (0x200a, 0)]);
    assert_eq!(b.ctl.resets, resets + 1);
    assert!(b.ctl.adv);
    assert_eq!(b.ctl.le_mask & 1, 1, "the event masks were given back");
    // Now extended commands wait for legacy to end.
    let x = open(&mut b, 12, false);
    b.write(x, &cmd(0x2036, &[0; 25])).unwrap();
    assert_eq!(answers(&mut b, x), vec![(0x2036, 0x0c)]);
    // The last app to close leaves the controller reset for the next.
    b.mux.close(x, 0);
    b.mux.close(chat, 0);
    b.run();
    assert_eq!(b.ctl.resets, resets + 2);
}

#[test]
fn two_enables_at_once_still_turn_duplicate_filtering_off() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.mux.write(a, &cmd(0x200c, &[1, 1]), 0).unwrap();
    b.mux.write(x, &cmd(0x200c, &[1, 1]), 0).unwrap();
    b.run();
    assert_eq!(b.ctl.scan, Some(false));
    // An enable whose app left before it was answered does not leave the
    // controller scanning for nobody.
    let mut b = Bench::new(4);
    let y = open(&mut b, 12, false);
    b.mux.write(y, &cmd(0x200c, &[1, 0]), 0).unwrap();
    b.mux.close(y, 0);
    b.run();
    assert_eq!(b.ctl.scan, None);
}

#[test]
fn a_restart_tells_the_apps_whose_scan_or_advertising_stopped() {
    let mut b = Bench::new(4);
    let (chat, s) = (open(&mut b, 10, false), open(&mut b, 11, true));
    b.write(chat, &cmd(0x2006, &[0; 15])).unwrap();
    b.write(chat, &cmd(0x200a, &[1])).unwrap();
    b.write(chat, &cmd(0x200c, &[1, 0])).unwrap();
    b.read(chat);
    b.write(s, &cmd(0x0c03, &[])).unwrap();
    assert_eq!(b.read(chat), vec![ev(0x10, &[0])]);
    assert_eq!(answers(&mut b, s), vec![(0x0c03, 0)]);
    // Its advertising went with the reset, so another app may advertise.
    let x = open(&mut b, 12, false);
    b.write(x, &cmd(0x200a, &[1])).unwrap();
    assert_eq!(answers(&mut b, x), vec![(0x200a, 0)]);
}

#[test]
fn a_late_answer_never_reaches_another_app() {
    let mut b = Bench::new(4);
    let (a, x) = (open(&mut b, 10, false), open(&mut b, 11, false));
    b.ctl.silent = true;
    b.write(a, &cmd(0x2017, &[0; 32])).unwrap();
    b.now = COMMAND_MS + 1;
    b.run();
    assert_eq!(answers(&mut b, a), vec![(0x2017, 0x1f)]);
    let sent = b.ctl.commands.len();
    b.write(x, &cmd(0x2017, &[1; 32])).unwrap();
    assert_eq!(b.ctl.commands.len(), sent, "held while A's answer may still come");
    let mut late = vec![1, 0x17, 0x20, 0];
    late.extend_from_slice(&[0xaa; 16]);
    b.from_controller(&ev(0x0e, &late));
    assert!(b.read(x).is_empty() && b.read(a).is_empty(), "A's late answer is dropped");
    assert_eq!(b.ctl.commands.len(), sent + 1, "then X's command goes");
    let mut mine = vec![1, 0x17, 0x20, 0];
    mine.extend_from_slice(&[0xbb; 16]);
    b.from_controller(&ev(0x0e, &mine));
    assert_eq!(b.read(x), vec![ev(0x0e, &mine)]);
    // With no late answer at all, the hold ends after a while.
    b.write(a, &cmd(0x2018, &[])).unwrap();
    b.now += COMMAND_MS + 1;
    b.run();
    b.read(a);
    let sent = b.ctl.commands.len();
    b.write(x, &cmd(0x2018, &[])).unwrap();
    assert_eq!(b.ctl.commands.len(), sent);
    b.now += 3 * COMMAND_MS + 1;
    b.run();
    assert_eq!(b.ctl.commands.len(), sent + 1);
}
