//! Sharing one Bluetooth controller between several open apps (hci.rs is the
//! transport to it). Each app that opens /dev/violet/hci is a client here;
//! this module decides what of its traffic reaches the controller and which
//! of the controller's packets it sees. Pure: no hardware, no clock of its
//! own, so the host tests drive it with a scripted controller
//! (runtime/open/tests/hci_host.rs).
//!
//! - One command is in flight at a time; its Command Complete or Status goes
//!   to the client that sent it. A command the controller does not answer in
//!   time is answered here with 0x1f, and no command with that opcode goes out
//!   again until the late answer has come (and been dropped) or a while has
//!   passed, so a late answer is never taken for another client's.
//! - Connection handles belong to the client that made the connection: the
//!   initiator whose create the controller accepted, for a central link; the
//!   legacy advertiser while it advertises, or the owner of the extended set
//!   that LE Advertising Set Terminated names, for a peripheral one. A link no
//!   client can be shown to own is disconnected. ACL data and handle events go
//!   only to the owner; ACL to a handle a client does not own is refused, and
//!   so is a command naming one (status 0x02).
//! - The controller's ACL buffers are counted here: clients' packets wait until
//!   the controller has room, and Number Of Completed Packets is split by owner.
//! - Scanning is shared: the controller scans while any client wants it, every
//!   scanning client sees every advertising report, and duplicate filtering is
//!   only on while a single client scans. Legacy advertising belongs to one
//!   client from its first advertising command until it turns advertising off
//!   (a command the controller refuses gives back a claim nothing else
//!   confirmed); the random address may be set only while no other client
//!   holds advertising. Each extended advertising set, and creating a
//!   connection, likewise belong to one client. Others get Command Disallowed
//!   (0x0c). The controller takes either legacy or extended advertising,
//!   scanning and initiating commands until it is reset (Core Vol 4 Part E
//!   3.1.1): a command of the other kind is refused while anything of the
//!   first kind is in use, and otherwise the controller is reset first.
//! - Reset from an ordinary app resets only its own state here (its links are
//!   disconnected, its advertising and scanning stop) and is answered as if
//!   the controller had done it. Event masks are kept per client, an ordinary
//!   app may unmask only the events routed to apps here, and the controller
//!   gets their union. Every command not listed here (vendor commands,
//!   controller configuration, BR/EDR, filter lists, test modes) is refused
//!   with Command Disallowed unless the client is a system app (from the boot
//!   image's Home, Settings or Installer), whose commands pass.
//! - Closing (or the app dying) disconnects its links and stops what it ran.
//!   A controller restart (Radio, or a system app's Reset) ends every link and
//!   tells each client that was scanning, advertising or connecting with
//!   Hardware Error.
use alloc::vec::Vec;

pub const CLIENTS: usize = 4;
/// The multiplexer's own commands and links being torn down.
const NOBODY: u8 = 0xff;
/// A peripheral link from an extended set, until Advertising Set Terminated names the set.
const PENDING: u8 = 0xfe;
/// Packets waiting for one client to read; past this the newest is dropped.
const RX_LIMIT: usize = 64;
/// Commands one client may have waiting.
const CMD_LIMIT: usize = 8;
/// ACL packets one client may have waiting for controller buffers.
const ACL_LIMIT: usize = 16;
/// A command the controller has not answered by then is answered with 0x1f.
pub const COMMAND_MS: u64 = 3000;

const UNKNOWN_CONNECTION: u8 = 0x02;
const DISALLOWED: u8 = 0x0c;
const UNSPECIFIED: u8 = 0x1f;

const RESET: u16 = 0x0c03;
const SET_EVENT_MASK: u16 = 0x0c01;
const LE_SET_EVENT_MASK: u16 = 0x2001;
const LE_READ_BUFFER_SIZE: u16 = 0x2002;
const READ_BUFFER_SIZE: u16 = 0x1005;
const DISCONNECT: u16 = 0x0406;
const RANDOM_ADDRESS: u16 = 0x2005;
const ADV_PARAMS: u16 = 0x2006;
const ADV_ENABLE: u16 = 0x200a;
const SCAN_PARAMS: u16 = 0x200b;
const SCAN_ENABLE: u16 = 0x200c;
const CREATE_CONNECTION: u16 = 0x200d;
const CREATE_CANCEL: u16 = 0x200e;
const EXT_ADV_PARAMS: u16 = 0x2036;
const EXT_CREATE_CONNECTION: u16 = 0x2043;
const EXT_ADV_ENABLE: u16 = 0x2039;
const REMOVE_ADV_SET: u16 = 0x203c;

/// Commands that change nothing another client relies on.
const PASS: &[u16] = &[
    0x1001, 0x1002, 0x1003, 0x1005, 0x1009, 0x0c14, 0x2002, 0x2003, 0x2007, 0x200f, 0x2017, 0x2018, 0x201c,
    0x2023, 0x202f, 0x203a, 0x203b,
];
/// Commands whose first parameter is a connection handle.
const ON_HANDLE: &[u16] = &[
    DISCONNECT, 0x041d, 0x1405, 0x0c2d, 0x2013, 0x2015, 0x2016, 0x2019, 0x201a, 0x201b, 0x2020, 0x2021, 0x2022,
    0x2030, 0x2032,
];
/// Commands the controller answers with Command Status, not Complete.
const STATUS_STYLE: &[u16] = &[DISCONNECT, 0x041d, 0x2013, 0x2016, 0x2019, 0x2032, CREATE_CONNECTION, EXT_CREATE_CONNECTION];
/// Legacy advertising: one client's.
const ADVERTISER: &[u16] = &[ADV_PARAMS, 0x2008, 0x2009, ADV_ENABLE];
/// Extended advertising commands naming one set (first parameter).
const ON_SET: &[u16] = &[0x2035, EXT_ADV_PARAMS, 0x2037, 0x2038, REMOVE_ADV_SET];

const LEGACY: u8 = 1;
const EXTENDED: u8 = 2;
/// Which of the two command sets of Core Vol 4 Part E 3.1.1 `op` is in.
fn family(op: u16) -> u8 {
    match op {
        0x2006..=0x200d => LEGACY,
        0x2035..=0x204a => EXTENDED,
        _ => 0,
    }
}

/// Why a write was refused.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Refusal {
    /// Not one whole command or ACL packet.
    Invalid,
    /// ACL for a connection this client does not own.
    Denied,
    /// Too much already waiting; try again.
    Full,
}

/// Checks that `p` is one whole H4 command or ACL packet.
pub fn valid(p: &[u8]) -> bool {
    match p.first() {
        Some(1) => p.len() >= 4 && p.len() == 4 + p[3] as usize,
        Some(2) => p.len() >= 5 && p.len() == 5 + u16::from_le_bytes([p[3], p[4]]) as usize && p.len() <= 1028,
        _ => false,
    }
}

/// The oldest of a short queue.
fn shift<T>(q: &mut Vec<T>) -> Option<T> {
    (!q.is_empty()).then(|| q.remove(0))
}
fn le16(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map_or(0xffff, |s| u16::from_le_bytes([s[0], s[1]]))
}
#[inline(never)]
fn command(op: u16, params: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + params.len());
    p.extend_from_slice(&[1, op as u8, (op >> 8) as u8, params.len() as u8]);
    p.extend_from_slice(params);
    p
}
#[inline(never)]
fn event(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(3 + params.len());
    p.extend_from_slice(&[4, code, params.len() as u8]);
    p.extend_from_slice(params);
    p
}

struct Client {
    pid: u32,
    /// The app (Shield owner name) the process belongs to.
    app: Vec<u8>,
    system: bool,
    rx: Vec<Vec<u8>>,
    /// Event masks as the client set them (the controller's defaults until then).
    mask: u64,
    le_mask: u64,
    /// Scanning, or asked to and not yet refused.
    scanning: bool,
    /// The duplicate filtering this client asked for.
    filter: bool,
}

struct Link {
    handle: u16,
    owner: u8,
    /// ACL packets of this link in the controller's buffers.
    sent: u16,
}

pub struct Mux {
    clients: [Option<Client>; CLIENTS],
    commands: Vec<(u8, Vec<u8>)>,
    /// The command at the controller: who sent it, the packet, its deadline.
    flight: Option<(u8, Vec<u8>, u64)>,
    /// Opcodes of commands given up on, until their late answer comes or the time.
    stale: Vec<(u16, u64)>,
    /// ACL waiting for controller buffers: owner, packet.
    acl: Vec<(u8, Vec<u8>)>,
    /// Packets for the controller, oldest first (hci.rs sends them).
    pub out: Vec<Vec<u8>>,
    links: Vec<Link>,
    /// PENDING links: handle, deadline, their Connection Complete.
    waiting: Vec<(u16, u64, Vec<u8>)>,
    /// Free controller ACL buffers, once known.
    free: Option<u16>,
    /// The legacy advertiser; `adv_held` once the controller took one of its commands.
    adv_owner: Option<u8>,
    adv_held: bool,
    adv_on: bool,
    /// Extended advertising sets: handle, owner, enabled, known to the controller.
    sets: Vec<(u8, u8, bool, bool)>,
    scan_on: bool,
    scan_filter: bool,
    scan_params: Vec<u8>,
    initiator: Option<u8>,
    /// The initiator's create is (or may be) at the controller.
    init_live: bool,
    /// LEGACY or EXTENDED once the controller has had (or will have) such a command.
    family: u8,
    /// A reset for `switch` is on its way.
    switching: bool,
    /// Packets dropped because a client was not reading.
    pub dropped: u32,
}

const DEFAULT_MASK: u64 = 0x0000_1fff_ffff_ffff;
const DEFAULT_LE_MASK: u64 = 0x1f;
/// Events the multiplexer itself must see: Disconnection Complete, LE Meta;
/// LE Connection Complete, Advertising Set Terminated. (Not Enhanced
/// Connection Complete: with it unmasked the controller sends nothing else.)
const NEED_MASK: u64 = 1 << 4 | 1 << 61;
const NEED_LE_MASK: u64 = 1 | 1 << 17;
/// What an ordinary app may unmask: the events routed to apps here.
/// Disconnection, Encryption Change, Remote Version, Hardware Error, Buffer
/// Overflow, Key Refresh, LE Meta; LE subevents 0x01-0x07, 0x0a-0x0d, 0x12, 0x14.
const APP_MASK: u64 = 1 << 4 | 1 << 7 | 1 << 11 | 1 << 15 | 1 << 25 | 1 << 47 | 1 << 61;
const APP_LE_MASK: u64 = 0x7f | 0xf << 9 | 1 << 17 | 1 << 19;
/// Enhanced Connection Complete v2 (0x29) would replace the events links are
/// tracked by: nobody unmasks it.
const NEVER_LE_MASK: u64 = 1 << 40;

impl Mux {
    pub fn new(now: u64) -> Mux {
        let mut m = Mux {
            clients: [None, None, None, None],
            commands: Vec::new(),
            flight: None,
            stale: Vec::new(),
            acl: Vec::new(),
            out: Vec::new(),
            links: Vec::new(),
            waiting: Vec::new(),
            free: None,
            adv_owner: None,
            adv_held: false,
            adv_on: false,
            sets: Vec::new(),
            scan_on: false,
            scan_filter: false,
            scan_params: Vec::new(),
            initiator: None,
            init_live: false,
            family: 0,
            switching: false,
            dropped: 0,
        };
        m.internal(LE_READ_BUFFER_SIZE, &[], now);
        m
    }

    /// A new client for process `pid` of app `app` (`system`: a core app), or
    /// None when the process already has one, another process of the same
    /// ordinary app has one, or no place is left. One place is kept for core
    /// apps: ordinary apps get at most CLIENTS - 1.
    pub fn open(&mut self, pid: u32, app: &[u8], system: bool) -> Option<u8> {
        let mut apps = 0;
        for c in self.clients.iter().flatten() {
            if c.pid == pid || (!system && !c.system && c.app == app) {
                return None;
            }
            apps += !c.system as usize;
        }
        if !system && apps >= CLIENTS - 1 {
            return None;
        }
        let i = self.clients.iter().position(|c| c.is_none())?;
        self.clients[i] = Some(Client {
            pid,
            app: app.to_vec(),
            system,
            rx: Vec::new(),
            mask: DEFAULT_MASK,
            le_mask: DEFAULT_LE_MASK,
            scanning: false,
            filter: false,
        });
        Some(i as u8)
    }
    /// Nothing waiting for or at the controller.
    pub fn idle(&self) -> bool {
        self.flight.is_none() && self.commands.is_empty() && self.out.is_empty()
    }
    pub fn clients(&self) -> usize {
        self.clients.iter().flatten().count()
    }

    pub fn close(&mut self, c: u8, now: u64) {
        self.release(c, now);
        if let Some(slot) = self.clients.get_mut(c as usize) {
            *slot = None;
        }
        // The controller remembers legacy or extended until a reset: the next
        // apps start from neither.
        if self.clients() == 0 && self.family != 0 {
            self.internal(RESET, &[], now);
        }
    }

    fn client(&mut self, c: u8) -> Option<&mut Client> {
        self.clients.get_mut(c as usize).and_then(|c| c.as_mut())
    }
    #[inline(never)]
    fn deliver(&mut self, c: u8, p: Vec<u8>) {
        let mut dropped = false;
        if let Some(cl) = self.client(c) {
            if cl.rx.len() >= RX_LIMIT {
                dropped = true;
            } else {
                cl.rx.push(p);
            }
        }
        self.dropped += dropped as u32;
    }
    pub fn readable(&self, c: u8) -> bool {
        self.clients.get(c as usize).and_then(|c| c.as_ref()).map_or(false, |c| !c.rx.is_empty())
    }
    pub fn take(&mut self, c: u8) -> Option<Vec<u8>> {
        shift(&mut self.client(c)?.rx)
    }
    /// Whether a write from `c` would be taken now.
    pub fn writable(&self, c: u8) -> bool {
        self.commands.iter().filter(|q| q.0 == c).count() < CMD_LIMIT && self.acl.iter().filter(|q| q.0 == c).count() < ACL_LIMIT
    }

    fn owner(&self, handle: u16) -> Option<u8> {
        self.links.iter().find(|l| l.handle == handle & 0xfff).map(|l| l.owner)
    }

    /// Nothing of the current command set is in use or on its way, so the
    /// controller may be reset to take the other.
    fn family_idle(&self) -> bool {
        let old = self.family;
        !self.adv_on
            && self.adv_owner.is_none()
            && self.sets.is_empty()
            && !self.scan_on
            && self.initiator.is_none()
            && self.links.is_empty()
            && !self.clients.iter().flatten().any(|c| c.scanning)
            && !self.commands.iter().map(|q| &q.1).chain(self.flight.iter().map(|f| &f.1)).any(|p| family(le16(p, 1)) == old)
    }

    /// One H4 packet from client `c`.
    #[inline(never)]
    pub fn write(&mut self, c: u8, p: &[u8], now: u64) -> Result<(), Refusal> {
        if !valid(p) {
            return Err(Refusal::Invalid);
        }
        let Some(system) = self.client(c).map(|cl| cl.system) else { return Err(Refusal::Invalid) };
        if !self.writable(c) {
            return Err(Refusal::Full);
        }
        if p[0] == 2 {
            if self.owner(le16(p, 1)) != Some(c) {
                return Err(Refusal::Denied);
            }
            self.acl.push((c, p.to_vec()));
            self.flush();
            return Ok(());
        }
        let op = le16(p, 1);
        let params = &p[4..];
        let refuse = |m: &mut Mux, status: u8| {
            m.answer(c, op, status, if ON_HANDLE.contains(&op) { params.get(..2).unwrap_or(&[]) } else { &[] });
            Ok(())
        };
        let fam = family(op);
        let switch = fam != 0 && self.family != 0 && fam != self.family;
        if switch && !self.family_idle() {
            return refuse(self, DISALLOWED);
        }
        match op {
            RESET if !system => {
                self.release(c, now);
                if let Some(cl) = self.client(c) {
                    cl.rx.clear();
                    cl.mask = DEFAULT_MASK;
                    cl.le_mask = DEFAULT_LE_MASK;
                }
                return refuse(self, 0);
            }
            SET_EVENT_MASK | LE_SET_EVENT_MASK if params.len() == 8 => {
                let v = u64::from_le_bytes(params.try_into().unwrap());
                let cl = self.client(c).unwrap();
                if op == SET_EVENT_MASK {
                    cl.mask = if system { v } else { v & APP_MASK };
                } else {
                    cl.le_mask = v & !NEVER_LE_MASK & if system { !0 } else { APP_LE_MASK };
                }
                let m = self.union(op == LE_SET_EVENT_MASK);
                self.queue(c, command(op, &m.to_le_bytes()), now);
                return Ok(());
            }
            _ if PASS.contains(&op) => {}
            _ if ON_HANDLE.contains(&op) => {
                if self.owner(le16(params, 0)) != Some(c) {
                    return refuse(self, UNKNOWN_CONNECTION);
                }
            }
            // The random address is the advertiser's while one advertises.
            RANDOM_ADDRESS if self.adv_owner.map_or(false, |o| o != c) => return refuse(self, DISALLOWED),
            RANDOM_ADDRESS => {}
            _ if ADVERTISER.contains(&op) => {
                if self.adv_owner.map_or(false, |o| o != c) {
                    return refuse(self, DISALLOWED);
                }
                // Claimed now so no other client's advertising commands
                // interleave; given back if the controller refuses it.
                self.adv_owner = Some(c);
            }
            _ if ON_SET.contains(&op) => {
                let Some(&h) = params.first() else { return refuse(self, DISALLOWED) };
                match self.sets.iter().find(|s| s.0 == h) {
                    Some(s) if s.1 != c => return refuse(self, DISALLOWED),
                    Some(_) => {}
                    None => self.sets.push((h, c, false, false)),
                }
            }
            EXT_ADV_ENABLE => {
                // Enable, number of sets, then four bytes a set (handle first).
                let n = *params.get(1).unwrap_or(&0) as usize;
                let mine = |h: &u8| self.sets.iter().any(|s| s.0 == *h && s.1 == c);
                if params.len() != 2 + 4 * n || (n == 0 && !system) || !params[2..].iter().step_by(4).all(mine) {
                    return refuse(self, DISALLOWED);
                }
            }
            SCAN_PARAMS => {
                if self.scan_on {
                    return refuse(self, if params == &self.scan_params[..] { 0 } else { DISALLOWED });
                }
            }
            SCAN_ENABLE if params.len() == 2 => {
                let on = params[0] == 1;
                let (scan_on, scan_filter) = (self.scan_on, self.scan_filter);
                let others = self.clients.iter().enumerate().any(|(i, s)| i != c as usize && s.as_ref().map_or(false, |s| s.scanning));
                let cl = self.client(c).unwrap();
                cl.filter = params[1] == 1;
                // Counted as scanning from now on, so a second client's enable
                // sees it even before the controller has answered.
                cl.scanning = on;
                if !on {
                    if others || !scan_on {
                        return refuse(self, 0);
                    }
                } else {
                    let filter = cl.filter && !others;
                    if scan_on && filter == scan_filter {
                        return refuse(self, 0);
                    }
                    self.switch(switch, now);
                    self.queue(c, command(SCAN_ENABLE, &[1, filter as u8]), now);
                    return Ok(());
                }
            }
            CREATE_CONNECTION | EXT_CREATE_CONNECTION => {
                if self.initiator.map_or(false, |o| o != c) {
                    return refuse(self, DISALLOWED);
                }
                self.initiator = Some(c);
            }
            CREATE_CANCEL if self.initiator == Some(c) => {}
            _ if system && op != SCAN_ENABLE && op != CREATE_CANCEL => {}
            _ => return refuse(self, DISALLOWED),
        }
        self.switch(switch, now);
        self.queue(c, p.to_vec(), now);
        Ok(())
    }

    /// Resets the controller ahead of a command of the other set, when
    /// nothing of the current one is in use (`family_idle`).
    fn switch(&mut self, switch: bool, now: u64) {
        if switch {
            self.switching = true;
            self.internal(RESET, &[], now);
        }
    }

    fn union(&self, le: bool) -> u64 {
        let m = self.clients.iter().flatten().fold(0, |m, c| m | if le { c.le_mask } else { c.mask });
        m | if le { NEED_LE_MASK } else { NEED_MASK }
    }

    /// A Command Complete (or Status) made here for client `c`.
    #[inline(never)]
    fn answer(&mut self, c: u8, op: u16, status: u8, ret: &[u8]) {
        let [lo, hi] = op.to_le_bytes();
        let p = if STATUS_STYLE.contains(&op) {
            event(0x0f, &[status, 1, lo, hi])
        } else {
            let mut b = Vec::from([1, lo, hi, status]);
            b.extend_from_slice(ret);
            event(0x0e, &b)
        };
        self.deliver(c, p);
    }

    #[inline(never)]
    fn queue(&mut self, c: u8, p: Vec<u8>, now: u64) {
        let f = family(le16(&p, 1));
        if f != 0 {
            self.family = f;
        }
        self.commands.push((c, p));
        self.next(now);
    }
    /// Disconnects a link nobody here owns any more.
    fn let_go(&mut self, handle: u16, now: u64) {
        let [a, b] = handle.to_le_bytes();
        self.internal(DISCONNECT, &[a, b, 0x13], now);
    }
    #[inline(never)]
    fn internal(&mut self, op: u16, params: &[u8], now: u64) {
        self.queue(NOBODY, command(op, params), now);
    }
    fn next(&mut self, now: u64) {
        if self.flight.is_some() {
            return;
        }
        let Some(op) = self.commands.first().map(|q| le16(&q.1, 1)) else { return };
        if self.stale.iter().any(|s| s.0 == op) {
            return;
        }
        let (c, p) = self.commands.remove(0);
        self.out.push(p.clone());
        self.flight = Some((c, p, now + COMMAND_MS));
    }

    /// Sends waiting ACL while the controller has buffers.
    fn flush(&mut self) {
        while let (Some(free @ 1..), Some((_, p))) = (self.free, self.acl.first()) {
            let handle = le16(p, 1) & 0xfff;
            let (_, p) = self.acl.remove(0);
            if let Some(l) = self.links.iter_mut().find(|l| l.handle == handle) {
                l.sent += 1;
                self.free = Some(free - 1);
                self.out.push(p);
            }
        }
    }

    /// Ends what client `c` started: its links, advertising, scanning and
    /// connection attempt. Its queued writes are dropped and an answer to a
    /// command it has at the controller goes nowhere.
    fn release(&mut self, c: u8, now: u64) {
        let creating = self
            .flight
            .as_ref()
            .map_or(false, |f| f.0 == c && matches!(le16(&f.1, 1), CREATE_CONNECTION | EXT_CREATE_CONNECTION));
        self.commands.retain(|q| q.0 != c);
        self.acl.retain(|q| q.0 != c);
        if let Some(f) = self.flight.as_mut().filter(|f| f.0 == c) {
            f.0 = NOBODY;
        }
        for i in 0..self.links.len() {
            if self.links[i].owner == c {
                self.links[i].owner = NOBODY;
                self.let_go(self.links[i].handle, now);
            }
        }
        if self.adv_owner == Some(c) {
            self.adv_owner = None;
            self.adv_held = false;
            if self.adv_on {
                self.internal(ADV_ENABLE, &[0], now);
            }
        }
        while let Some(i) = self.sets.iter().position(|s| s.1 == c) {
            let (h, _, on, _) = self.sets.remove(i);
            if on {
                self.internal(EXT_ADV_ENABLE, &[0, 1, h, 0, 0, 0], now);
            }
            self.internal(REMOVE_ADV_SET, &[h], now);
        }
        if let Some(cl) = self.client(c) {
            cl.scanning = false;
        }
        if self.scan_on && !self.clients.iter().flatten().any(|s| s.scanning) {
            self.internal(SCAN_ENABLE, &[0, 0], now);
        }
        if self.initiator == Some(c) {
            if self.init_live || creating {
                self.initiator = Some(NOBODY);
                self.internal(CREATE_CANCEL, &[], now);
            } else {
                // Its create never reached the controller (it was dropped above).
                self.initiator = None;
            }
        }
    }

    /// The controller started again (Radio restarted it, or a system app
    /// reset it): nothing of its state remains. Owners hear their links end;
    /// clients that were scanning, advertising or connecting get Hardware Error.
    pub fn restarted(&mut self, now: u64) {
        let links = core::mem::take(&mut self.links);
        for l in links {
            let [a, b] = l.handle.to_le_bytes();
            self.deliver(l.owner, event(0x05, &[0, a, b, 0x16]));
        }
        if let Some((c, p, _)) = self.flight.take() {
            let op = le16(&p, 1);
            if op != RESET && c != NOBODY {
                self.answer(c, op, UNSPECIFIED, &[]);
            }
        }
        let mut hit = [false; CLIENTS];
        for o in self.adv_owner.iter().chain(self.initiator.iter()).chain(self.sets.iter().map(|s| &s.1)) {
            if let Some(h) = hit.get_mut(*o as usize) {
                *h = true;
            }
        }
        for (i, cl) in self.clients.iter_mut().enumerate() {
            if let Some(cl) = cl {
                hit[i] |= cl.scanning;
                cl.scanning = false;
            }
        }
        self.acl.clear();
        self.out.clear();
        self.stale.clear();
        self.waiting.clear();
        self.free = None;
        self.adv_owner = None;
        self.adv_held = false;
        self.adv_on = false;
        self.sets.clear();
        self.scan_on = false;
        self.initiator = None;
        self.init_live = false;
        self.switching = false;
        self.commands.retain(|q| q.0 != NOBODY);
        // A scan asked for and not yet sent still counts.
        for q in &self.commands {
            if le16(&q.1, 1) == SCAN_ENABLE && q.1.get(4) == Some(&1) {
                if let Some(Some(cl)) = self.clients.get_mut(q.0 as usize) {
                    cl.scanning = true;
                }
            }
        }
        self.family = self.commands.iter().map(|q| family(le16(&q.1, 1))).find(|&f| f != 0).unwrap_or(0);
        self.commands.insert(0, (NOBODY, command(LE_READ_BUFFER_SIZE, &[])));
        for le in [false, true] {
            let m = self.union(le);
            let op = if le { LE_SET_EVENT_MASK } else { SET_EVENT_MASK };
            self.commands.insert(0, (NOBODY, command(op, &m.to_le_bytes())));
        }
        for i in 0..CLIENTS {
            if hit[i] && self.wants(i, 0x10, 0) {
                self.deliver(i as u8, event(0x10, &[0]));
            }
        }
        self.next(now);
    }

    /// Answers a command the controller has not answered in time, and lets
    /// go of peripheral links no set was named for.
    pub fn poll(&mut self, now: u64) {
        while let Some(i) = self.waiting.iter().position(|w| w.1 <= now) {
            let h = self.waiting.remove(i).0;
            if let Some(l) = self.links.iter_mut().find(|l| l.handle == h) {
                l.owner = NOBODY;
            }
            self.let_go(h, now);
        }
        let stale = self.stale.len();
        self.stale.retain(|s| s.1 > now);
        if self.stale.len() != stale {
            self.next(now);
        }
        if let Some((c, p, deadline)) = self.flight.take() {
            if now < deadline {
                self.flight = Some((c, p, deadline));
                return;
            }
            let op = le16(&p, 1);
            if c != NOBODY {
                self.answer(c, op, UNSPECIFIED, &[]);
            }
            // The controller may still answer it.
            self.stale.push((op, now + 3 * COMMAND_MS));
            if matches!(op, CREATE_CONNECTION | EXT_CREATE_CONNECTION) && self.initiator == Some(c) {
                // It may still be trying: only a cancel or a Connection Complete ends it.
                self.init_live = true;
            }
            if matches!(op, LE_READ_BUFFER_SIZE | READ_BUFFER_SIZE) && self.free.is_none() {
                // Without the controller's count, one buffer at a time is always safe.
                self.free = Some(1);
            }
            self.next(now);
        }
    }

    fn wants(&self, c: usize, code: u8, sub: u8) -> bool {
        let Some(cl) = &self.clients[c] else { return false };
        match code {
            0x0e | 0x0f | 0x13 => true,
            0x3e => cl.mask >> 61 & 1 != 0 && sub >= 1 && sub <= 64 && cl.le_mask >> (sub - 1) & 1 != 0,
            1..=64 => cl.mask >> (code - 1) & 1 != 0,
            _ => true,
        }
    }
    /// An event for the owner of a handle, if it asked for it.
    #[inline(never)]
    fn to_owner(&mut self, handle: u16, p: &[u8]) {
        if let Some(o) = self.owner(handle) {
            if (o as usize) < CLIENTS && self.wants(o as usize, p[1], p.get(3).copied().unwrap_or(0)) {
                self.deliver(o, p.to_vec());
            }
        }
    }
    #[inline(never)]
    fn to_all(&mut self, p: &[u8], only: fn(&Client) -> bool) {
        for i in 0..CLIENTS {
            if self.clients[i].as_ref().map_or(false, only) && self.wants(i, p[1], p.get(3).copied().unwrap_or(0)) {
                self.deliver(i as u8, p.to_vec());
            }
        }
    }
    /// A (Enhanced) Connection Complete for client `o`, in the form it unmasked:
    /// the controller sends only one of the two, whichever the union asks for.
    #[inline(never)]
    fn connected_to(&mut self, o: u8, p: &[u8]) {
        if o as usize >= CLIENTS {
            return;
        }
        let e = &p[3..];
        if self.wants(o as usize, 0x3e, e[0]) {
            return self.deliver(o, p.to_vec());
        }
        // 0x01 <-> 0x0a: the enhanced one has the two resolvable addresses
        // after the peer's, and peer address types 2 and 3 for resolved identities.
        let other = e[0] ^ 0x0b;
        let mut b = Vec::from([other]);
        if e[0] == 0x0a && e.len() >= 31 {
            b.extend_from_slice(&e[1..5]);
            b.push(e[5] & 1);
            b.extend_from_slice(&e[6..12]);
            b.extend_from_slice(&e[24..31]);
        } else if e[0] == 0x01 && e.len() >= 19 {
            b.extend_from_slice(&e[1..12]);
            b.extend_from_slice(&[0; 12]);
            b.extend_from_slice(&e[12..19]);
        } else {
            return;
        }
        if self.wants(o as usize, 0x3e, other) {
            self.deliver(o, event(0x3e, &b));
        }
    }

    /// One H4 packet from the controller.
    #[inline(never)]
    pub fn receive(&mut self, p: &[u8], now: u64) {
        if p.first() == Some(&2) && p.len() >= 5 {
            let h = le16(p, 1);
            if let Some(o) = self.owner(h).filter(|&o| (o as usize) < CLIENTS) {
                self.deliver(o, p.to_vec());
            }
            return;
        }
        if p.first() != Some(&4) || p.len() < 3 || p.len() != 3 + p[2] as usize {
            return;
        }
        let e = &p[3..];
        match p[1] {
            0x0e | 0x0f if e.len() >= 4 => {
                let (op, status, ret) = if p[1] == 0x0e { (le16(e, 1), e[3], &e[4..]) } else { (le16(e, 2), e[0], &[][..]) };
                if let Some(i) = self.stale.iter().position(|s| s.0 == op) {
                    // The late answer to a command already answered with 0x1f.
                    self.stale.remove(i);
                    self.next(now);
                } else if op != 0 && self.flight.as_ref().map_or(false, |f| le16(&f.1, 1) == op) {
                    let (c, cmd, _) = self.flight.take().unwrap();
                    if c != NOBODY {
                        self.deliver(c, p.to_vec());
                    }
                    self.completed(c, &cmd, status, ret, now);
                    self.next(now);
                }
            }
            0x05 if e.len() >= 4 => {
                let h = le16(e, 1) & 0xfff;
                self.to_owner(h, p);
                if e[0] == 0 {
                    if let Some(i) = self.links.iter().position(|l| l.handle == h) {
                        let l = self.links.remove(i);
                        self.free = self.free.map(|f| f + l.sent);
                        self.acl.retain(|q| le16(&q.1, 1) & 0xfff != h);
                    }
                    self.waiting.retain(|w| w.0 != h);
                }
            }
            // Number Of Completed Packets: each owner hears of its own links, one event each.
            0x13 if !e.is_empty() && e.len() >= 1 + 4 * e[0] as usize => {
                for k in 0..e[0] as usize {
                    let entry = &e[1 + 4 * k..5 + 4 * k];
                    let (h, n) = (le16(entry, 0) & 0xfff, le16(entry, 2));
                    if let Some(l) = self.links.iter_mut().find(|l| l.handle == h) {
                        let n = n.min(l.sent);
                        l.sent -= n;
                        self.free = self.free.map(|f| f + n);
                        let o = l.owner;
                        let mut b = Vec::from([1]);
                        b.extend_from_slice(entry);
                        self.deliver(o, event(0x13, &b));
                    }
                }
                self.flush();
            }
            // Encryption Change, Read Remote Version Complete, Key Refresh: status, handle.
            0x08 | 0x0c | 0x30 if e.len() >= 3 => self.to_owner(le16(e, 1), p),
            // Authenticated Payload Timeout Expired: handle.
            0x57 if e.len() >= 2 => self.to_owner(le16(e, 0), p),
            // Hardware Error, Data Buffer Overflow: everybody's problem.
            0x10 | 0x1a => self.to_all(p, |_| true),
            0x3e if e.len() >= 2 => self.meta(p, now),
            _ => self.to_all(p, |c| c.system),
        }
    }

    fn meta(&mut self, p: &[u8], now: u64) {
        let e = &p[3..];
        match e[0] {
            // (Enhanced) Connection Complete: status, handle, role.
            0x01 | 0x0a if e.len() >= 5 => {
                let (status, h, role) = (e[1], le16(e, 2) & 0xfff, e[4]);
                // A cancelled create (0x02) is the initiator's whatever its role byte says.
                let owner = if role == 1 && status != UNKNOWN_CONNECTION {
                    // The peripheral role: legacy advertising (which a
                    // connection or a directed advertising timeout ends), else
                    // an extended set, named by Advertising Set Terminated next.
                    let legacy = self.adv_owner.filter(|_| self.adv_on);
                    self.adv_on = false;
                    match legacy {
                        Some(o) => o,
                        None if status == 0 && self.sets.iter().any(|s| s.2) => PENDING,
                        None => NOBODY,
                    }
                } else if self.init_live {
                    self.init_live = false;
                    self.initiator.take().unwrap_or(NOBODY)
                } else {
                    NOBODY
                };
                if status == 0 {
                    self.links.push(Link { handle: h, owner, sent: 0 });
                    if owner == PENDING {
                        self.waiting.push((h, now + COMMAND_MS, p.to_vec()));
                    } else if owner as usize >= CLIENTS {
                        self.let_go(h, now);
                    }
                }
                self.connected_to(owner, p);
            }
            // Advertising reports (legacy, directed, extended): every scanner.
            0x02 | 0x0b | 0x0d => self.to_all(p, |c| c.scanning),
            // Status then handle.
            0x03 | 0x04 | 0x0c if e.len() >= 4 => self.to_owner(le16(e, 2), p),
            // Handle first.
            0x05 | 0x06 | 0x07 | 0x14 => self.to_owner(le16(e, 1), p),
            // Advertising Set Terminated: status, set, handle. The set stopped;
            // with status 0 a connection to it was made, and is its owner's.
            0x12 if e.len() >= 5 => {
                let (status, h) = (e[1], le16(e, 3) & 0xfff);
                let owner = self.sets.iter_mut().find(|s| s.0 == e[2]).map(|s| {
                    s.2 = false;
                    s.1
                });
                if let Some(i) = self.waiting.iter().position(|w| w.0 == h).filter(|_| status == 0) {
                    let (_, _, cc) = self.waiting.remove(i);
                    let o = owner.unwrap_or(NOBODY);
                    if let Some(l) = self.links.iter_mut().find(|l| l.handle == h) {
                        l.owner = o;
                    }
                    if o == NOBODY {
                        self.let_go(h, now);
                    }
                    self.connected_to(o, &cc);
                }
                if let Some(o) = owner.filter(|&o| self.wants(o as usize, 0x3e, 0x12)) {
                    self.deliver(o, p.to_vec());
                }
            }
            _ => self.to_all(p, |c| c.system),
        }
    }

    /// What a command's answer changes here.
    fn completed(&mut self, c: u8, cmd: &[u8], status: u8, ret: &[u8], now: u64) {
        let op = le16(cmd, 1);
        let params = &cmd[4..];
        let ok = status == 0;
        match op {
            CREATE_CONNECTION | EXT_CREATE_CONNECTION if self.initiator == Some(c) => {
                if ok {
                    self.init_live = true;
                } else if !self.init_live {
                    self.initiator = None;
                }
            }
            // No attempt is pending at the controller.
            CREATE_CANCEL if !ok => {
                self.initiator = None;
                self.init_live = false;
            }
            _ if ADVERTISER.contains(&op) => {
                if ok && op == ADV_ENABLE {
                    self.adv_on = params.first() == Some(&1);
                }
                if !ok {
                    // A claim nothing confirmed, with nothing more of it on the way.
                    let more = self.commands.iter().any(|q| q.0 == c && ADVERTISER.contains(&le16(&q.1, 1)));
                    if self.adv_owner == Some(c) && !self.adv_held && !self.adv_on && !more {
                        self.adv_owner = None;
                    }
                } else if op == ADV_ENABLE && !self.adv_on {
                    if self.adv_owner == Some(c) {
                        self.adv_owner = None;
                        self.adv_held = false;
                    }
                } else if c != NOBODY && self.adv_owner.map_or(true, |o| o == c) {
                    self.adv_owner = Some(c);
                    self.adv_held = true;
                }
            }
            _ if ON_SET.contains(&op) => {
                let h = params.first().copied();
                if !ok && op == EXT_ADV_PARAMS {
                    // A set the controller never took.
                    self.sets.retain(|s| Some(s.0) != h || s.1 != c || s.2 || s.3);
                } else if ok && op == REMOVE_ADV_SET {
                    self.sets.retain(|s| Some(s.0) != h);
                } else if ok {
                    for s in self.sets.iter_mut().filter(|s| Some(s.0) == h) {
                        s.3 = true;
                    }
                }
            }
            EXT_ADV_ENABLE if ok && params.len() >= 2 => {
                let on = params[0] == 1;
                for h in params[2..].iter().step_by(4) {
                    if let Some(s) = self.sets.iter_mut().find(|s| s.0 == *h) {
                        s.2 = on;
                    }
                }
            }
            SCAN_ENABLE if params.len() == 2 => {
                if ok {
                    self.scan_on = params[0] == 1;
                    self.scan_filter = params[1] == 1;
                } else if let Some(cl) = self.client(c).filter(|_| params[0] == 1) {
                    cl.scanning = false;
                }
                // Everyone who asked for it stopped while it was on its way.
                if self.scan_on && !self.clients.iter().flatten().any(|s| s.scanning) {
                    self.internal(SCAN_ENABLE, &[0, 0], now);
                }
            }
            // The reset `switch` asked for: nothing was in use, so only the
            // event masks need giving back.
            RESET if c == NOBODY && self.switching => {
                self.switching = false;
                if ok {
                    for le in [true, false] {
                        let m = self.union(le);
                        let op = if le { LE_SET_EVENT_MASK } else { SET_EVENT_MASK };
                        self.commands.insert(0, (NOBODY, command(op, &m.to_le_bytes())));
                    }
                }
            }
            _ if !ok => {}
            RESET => {
                // A system app (or the multiplexer) reset the controller.
                self.flight = None;
                self.restarted(now);
            }
            LE_READ_BUFFER_SIZE if ret.len() >= 3 => {
                if ret[2] == 0 {
                    self.internal(READ_BUFFER_SIZE, &[], now);
                } else if self.free.is_none() {
                    self.free = Some(ret[2] as u16);
                }
            }
            READ_BUFFER_SIZE if ret.len() >= 7 && self.free.is_none() => {
                self.free = Some(le16(ret, 3).max(1));
            }
            SCAN_PARAMS => self.scan_params = params.to_vec(),
            _ => {}
        }
        self.flush();
    }
}
