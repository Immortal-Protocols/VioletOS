//! Runtime routing policy.
//!
//! Everything Runtime decides about which app may reach which service, and
//! which reply goes back to which app, lives here. The code has no direct
//! effects: it talks to the outside world only through `Io`. The Runtime
//! process drives it with the real request cells and IPC (process.rs); the
//! proofs in verify/ drive it with a model of the cells and check the policy
//! listed in verify/README.md.
use crate::protocol::*;

pub const LIFECYCLE: u32 = 7;
pub const TOUCH_LANE: u32 = 4;
pub const DISPLAY_LANE: u32 = 5;
pub const STORAGE_LANE: u32 = 6;
pub const RADIO_LANE: u32 = 7;
pub const POWER_LANE: u32 = 8;
pub const PACKAGE_LANE: u32 = 9;
/// Request cell layout within each lane: 1..=14 app jobs, 16 the touch
/// sample, 17..=30 per-app state streams, 31 startup diagnostics.
pub const CONTACT_CELL: u32 = 16;
pub const BOOT_SLOT: u32 = 31;
/// Owner of a request that carries no app's data.
pub const SYSTEM: u32 = 0;
pub fn stream_cell(app: u32) -> u32 {
    16 + app
}

/// Where a reply's contents came from. Proof and test builds track it so the
/// model can check every reply; the Runtime image erases it.
#[cfg(any(kani, test))]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Computed by Runtime from its own state and the requester's input.
    #[default]
    Runtime,
    /// A service reply that holds no app's data (touch or disk diagnostics).
    System,
    /// A service reply to a request made on behalf of this app.
    App(u32),
    /// A touch sample read while this app was in the foreground.
    Touch(u32),
}
#[cfg(not(any(kani, test)))]
#[derive(Clone, Copy, Default)]
pub struct Origin;

#[derive(Clone, Copy, Default)]
pub struct Out {
    pub m: Message,
    pub origin: Origin,
}
impl Out {
    pub fn local(m: Message) -> Self {
        Self { m, origin: Origin::default() }
    }
    /// A worker result in the Runtime image, where origins are erased.
    #[cfg(not(any(kani, test)))]
    pub fn unlabelled(m: Message) -> Self {
        Self::local(m)
    }
    /// Contents of a system-only reply. Proofs check that it is one.
    fn system(self) -> Message {
        #[cfg(any(kani, test))]
        assert!(self.origin == Origin::System, "app data used as system status");
        self.m
    }
    /// The one deliberate cross-app flow: every app's STATUS shows whether
    /// the stopwatch checkpoint could be read at startup (DISK READY).
    fn stopwatch_checkpoint_loaded(self) -> bool {
        #[cfg(any(kani, test))]
        assert!(self.origin == Origin::App(WATCH), "unexpected disk probe source");
        self.m.op == OK
    }
}

pub trait Io {
    /// Publish a request in cell `index` of `lane`. False if the cell is busy.
    fn queue(&mut self, lane: u32, index: u32, owner: u32, m: Message) -> bool;
    /// Collect a completed cell, freeing it.
    fn take(&mut self, lane: u32, index: u32) -> Option<Out>;
    /// Ask the supervisor to open or quit an app.
    fn lifecycle(&mut self, m: Message) -> Message;
    /// Read the supervisor's update record (launcher only).
    fn supervisor_status(&mut self) -> Message;
    fn heartbeat(&mut self, who: u32, focus: u32);
    fn publish_status(&mut self, status: &Message);
    /// Tell the display worker which app may currently present (0: none).
    fn publish_presenter(&mut self, app: u32);
    fn reply(&mut self, out: Out);
}

#[derive(Default, Clone, Copy)]
pub struct Stream {
    pub id: u32,
    pub last: Option<(u32, u32)>,
}
#[derive(Default, Clone, Copy)]
pub struct Client {
    pub serial: u32,
    pub jobs: [Job; 7],
    pub streams: [Stream; 3],
}
#[derive(Default, Clone, Copy)]
pub struct Job {
    pub ticket: u32,
    pub lane: u32,
    pub result: Out,
}
fn service(key: u32) -> u32 {
    match key {
        SAVE | FILE_LIST..=FILE_DELETE => STORAGE_LANE,
        PRESENT => DISPLAY_LANE,
        SCAN | NETWORK | PROFILE | RADIO | RADIO_TEST => RADIO_LANE,
        POWER => POWER_LANE,
        CATALOG | INBOX | INSTALL | REMOVE => PACKAGE_LANE,
        _ => 0,
    }
}
fn job_slot(key: u32) -> usize {
    service(key).saturating_sub(3) as usize
}
pub const STREAM_KEYS: [u32; 3] = [CONTACT, RADIO, POWER];
fn ticket(c: &mut Client) -> u32 {
    c.serial = c.serial.wrapping_add(1).max(1);
    c.serial
}

#[derive(Default, Clone, Copy)]
pub struct Startup {
    pub touch_pending: bool,
    pub disk_pending: bool,
    pub disk_reason_pending: bool,
    pub disk_reason: Option<Message>,
}
impl Startup {
    fn begin(inventory: u32, io: &mut impl Io) -> Self {
        let touch_pending = io.queue(TOUCH_LANE, BOOT_SLOT, SYSTEM, Message::new(GET, STATUS, 0));
        let disk_pending = inventory & (1 << STORAGE_LANE) != 0
            && io.queue(STORAGE_LANE, BOOT_SLOT, WATCH, Message::new(GET, SAVE, 0));
        Self { touch_pending, disk_pending, disk_reason_pending: false, disk_reason: None }
    }
    fn update(&mut self, status: &mut Message, io: &mut impl Io) {
        if status.key == 0 {
            return;
        }
        if self.touch_pending {
            if let Some(r) = io.take(TOUCH_LANE, BOOT_SLOT) {
                status.put_text(&r.system().text());
                self.touch_pending = false;
            }
        }
        if self.disk_pending {
            if let Some(r) = io.take(STORAGE_LANE, BOOT_SLOT) {
                let loaded = r.stopwatch_checkpoint_loaded();
                status.arg = loaded as u32;
                self.disk_pending = false;
                if !loaded {
                    self.disk_reason_pending = io.queue(
                        STORAGE_LANE, BOOT_SLOT, SYSTEM, Message::new(GET, STATUS, 0));
                }
            }
        }
        if self.disk_reason_pending {
            if let Some(r) = io.take(STORAGE_LANE, BOOT_SLOT) {
                self.disk_reason = Some(r.system());
                self.disk_reason_pending = false;
            }
        }
        if self.touch_pending || self.disk_pending || self.disk_reason_pending {
            return;
        }
        if let Some(r) = self.disk_reason {
            if status.text().starts_with(b"TOUCH READY") && r.op == OK && r.text()[0] != 0 {
                status.put_text(&r.text());
            }
        }
        status.key = 0;
        io.publish_status(status);
    }
}

pub struct Runtime {
    pub inventory: u32,
    pub focus: u32,
    pub held: bool,
    pub asleep: bool,
    /// The outstanding touch sample, if any, was requested since the last
    /// focus change. A sample that is not fresh is discarded, never delivered.
    pub contact_fresh: bool,
    pub ready: Message,
    pub startup: Startup,
    pub clients: [Client; 15],
}
impl Runtime {
    pub fn start(inventory: u32, io: &mut impl Io) -> Self {
        let startup = Startup::begin(inventory, io);
        let mut ready = Message::result(0);
        ready.key = 1;
        ready.put_text(b"STARTING SERVICES");
        let rt = Self {
            inventory,
            focus: LAUNCHER,
            held: false,
            asleep: false,
            contact_fresh: false,
            ready,
            startup,
            clients: [Client::default(); 15],
        };
        io.publish_presenter(rt.presenter());
        rt
    }
    pub fn presenter(&self) -> u32 {
        if self.asleep { 0 } else { self.focus }
    }
    fn set_focus(&mut self, app: u32, io: &mut impl Io) {
        self.focus = app;
        self.contact_fresh = false;
        io.publish_presenter(self.presenter());
    }
    fn set_asleep(&mut self, asleep: bool, io: &mut impl Io) {
        self.asleep = asleep;
        io.publish_presenter(self.presenter());
    }
    fn contact(&mut self, who: u32, io: &mut impl Io) -> Out {
        if who != self.focus {
            return Out::local(Message::error(EMPTY));
        }
        let event = match io.take(TOUCH_LANE, CONTACT_CELL) {
            Some(event) if self.contact_fresh => event,
            // Nothing ready, or a sample read while another app was in front.
            _ => {
                if io.queue(TOUCH_LANE, CONTACT_CELL, who, Message::new(GET, CONTACT, 0)) {
                    self.contact_fresh = true;
                }
                return Out::local(Message::error(EMPTY));
            }
        };
        if event.m.op != OK {
            return Out::local(Message::error(EMPTY));
        }
        let down = event.m.key != 0;
        let pressed = down && !self.held;
        self.held = down;
        if !pressed {
            return Out::local(Message::error(EMPTY));
        }
        if self.asleep {
            io.take(POWER_LANE, 0);
            io.queue(POWER_LANE, 0, who, Message::new(EXECUTE, POWER, 0));
            self.set_asleep(false, io);
            return Out::local(Message::error(EMPTY));
        }
        event
    }
    fn sample(&mut self, lane: u32, who: u32, key: u32, io: &mut impl Io) -> Out {
        if let Some(r) = io.take(lane, stream_cell(who)) {
            return r;
        }
        io.queue(lane, stream_cell(who), who, Message::new(GET, key, 0));
        Out::local(Message::error(EMPTY))
    }
    fn forward(&mut self, who: u32, lane: u32, m: Message, io: &mut impl Io) -> Message {
        if !io.queue(lane, who, who, m) {
            return Message::error(OFFLINE);
        }
        self.clients[who as usize].jobs[(lane - 3) as usize].lane = lane;
        Message::error(BUSY)
    }
    fn dispatch(&mut self, who: u32, m: Message, io: &mut impl Io) -> Message {
        let component = match m.key {
            RADIO | NETWORK | PROFILE | RADIO_TEST | SCAN => RADIO_LANE,
            POWER => POWER_LANE,
            SAVE | FILE_LIST..=FILE_DELETE => STORAGE_LANE,
            _ => 0,
        };
        if component != 0 && self.inventory & (1 << component) == 0 {
            return Message::error(OFFLINE);
        }
        match (m.op, m.key) {
            (GET, WHOAMI) => Message::result(who),
            (GET, STATUS) => self.ready,
            (GET, 0x4057) if who == LAUNCHER => io.supervisor_status(),
            (GET, FOCUS) => Message::result(if self.asleep { 0 } else { self.focus }),
            (GET, SURFACE) => Message::result(0x06000000),
            (GET, CATALOG | INBOX) | (EXECUTE, INSTALL | REMOVE) if who == LAUNCHER => {
                self.forward(who, PACKAGE_LANE, m, io)
            }
            (EXECUTE, OPEN)
                if (who == LAUNCHER && is_app(m.value)) || (is_app(who) && m.value == LAUNCHER) =>
            {
                let r = io.lifecycle(m);
                if r.op == OK {
                    self.set_focus(m.value, io);
                }
                r
            }
            (EXECUTE, QUIT) if who != LAUNCHER => {
                let r = io.lifecycle(Message::new(EXECUTE, QUIT, who));
                if r.op == OK {
                    self.set_focus(LAUNCHER, io);
                    let client = &mut self.clients[who as usize];
                    client.streams = [Stream::default(); 3];
                    for job in &mut client.jobs {
                        job.ticket = 0;
                    }
                }
                r
            }
            (GET, FILE_LIST | FILE_READ)
            | (EXECUTE, FILE_WRITE | FILE_TRUNCATE | FILE_DELETE)
            | (GET | EXECUTE, SAVE) => self.forward(who, STORAGE_LANE, m, io),
            (EXECUTE, PRESENT) if who == self.focus && !self.asleep => {
                let mut req = Message::new(EXECUTE, PRESENT, who);
                req.arg = m.value;
                self.forward(who, DISPLAY_LANE, req, io)
            }
            (GET | EXECUTE, NETWORK | PROFILE) | (EXECUTE, RADIO_TEST) if who == WIFI => {
                self.forward(who, RADIO_LANE, m, io)
            }
            (GET, SCAN) if who == WIFI => self.forward(who, RADIO_LANE, m, io),
            (GET, RADIO) => self.forward(who, RADIO_LANE, m, io),
            (EXECUTE, RADIO) if who == LAUNCHER => self.forward(who, RADIO_LANE, m, io),
            (GET, POWER) => self.forward(who, POWER_LANE, m, io),
            (EXECUTE, POWER) if who == LAUNCHER && m.value <= 2 => {
                self.set_asleep(m.value == 1, io);
                self.forward(who, POWER_LANE, m, io)
            }
            _ => Message::error(DENIED),
        }
    }

    /// Handle one request from the app identified by seL4 badge `who`.
    /// Replies exactly once.
    pub fn handle(&mut self, who: u32, m: Message, io: &mut impl Io) {
        self.startup_step(io);
        self.serve(who, m, io);
    }
    /// Collect startup diagnostics. Touches only the startup cells.
    pub fn startup_step(&mut self, io: &mut impl Io) {
        self.startup.update(&mut self.ready, io);
    }
    pub fn serve(&mut self, who: u32, m: Message, io: &mut impl Io) {
        if !is_app(who) {
            io.reply(Out::local(Message::error(DENIED)));
            return;
        }
        self.serve_app(who, m, io);
    }
    /// Requests from an app. `who` has already been checked with `is_app`.
    pub fn serve_app(&mut self, who: u32, m: Message, io: &mut impl Io) {
        io.heartbeat(who, self.focus);
        let i = who as usize;
        match m.op {
            SUBSCRIBE if STREAM_KEYS.contains(&m.key) => {
                let index = STREAM_KEYS.iter().position(|&key| key == m.key).unwrap();
                if m.key != CONTACT && self.inventory & (1 << m.key) == 0 {
                    io.reply(Out::local(Message::error(OFFLINE)));
                } else {
                    let c = &mut self.clients[i];
                    if c.streams[index].id == 0 {
                        let id = ticket(c);
                        c.streams[index] = Stream { id, last: None };
                    }
                    io.reply(Out::local(Message::result(c.streams[index].id)));
                }
            }
            POLL | FORGET if m.value != 0 && self.clients[i].streams.iter().any(|s| s.id == m.value) => {
                let index = self.clients[i].streams.iter().position(|s| s.id == m.value).unwrap();
                if m.op == FORGET {
                    self.clients[i].streams[index] = Stream::default();
                    io.reply(Out::local(Message::result(0)));
                    return;
                }
                let key = STREAM_KEYS[index];
                if key == CONTACT {
                    let event = self.contact(who, io);
                    io.reply(event);
                    return;
                }
                let lane = if key == RADIO { RADIO_LANE } else { POWER_LANE };
                let state = self.sample(lane, who, key, io);
                let stream = &mut self.clients[i].streams[index];
                if state.m.op != OK {
                    io.reply(state);
                } else if stream.last == Some((state.m.value, state.m.arg)) {
                    io.reply(Out::local(Message::error(EMPTY)));
                } else {
                    stream.last = Some((state.m.value, state.m.arg));
                    io.reply(state);
                }
            }
            POLL | FORGET if m.value != 0 => {
                let Some(job) = self.clients[i].jobs.iter_mut().find(|j| j.ticket == m.value) else {
                    io.reply(Out::local(Message::error(INVALID)));
                    return;
                };
                if job.lane != 0 {
                    if let Some(r) = io.take(job.lane, who) {
                        job.result = r;
                        job.lane = 0;
                    } else if m.op == POLL {
                        io.reply(Out::local(Message::error(BUSY)));
                        return;
                    }
                }
                job.ticket = 0;
                if m.op == POLL {
                    io.reply(job.result);
                } else {
                    io.reply(Out::local(Message::result(0)));
                }
            }
            GET | EXECUTE => {
                let slot = job_slot(m.key);
                let job = &mut self.clients[i].jobs[slot];
                if job.ticket != 0 || (job.lane != 0 && io.take(job.lane, who).is_none()) {
                    io.reply(Out::local(Message::error(BUSY)));
                    return;
                }
                job.lane = 0;
                let id = ticket(&mut self.clients[i]);
                self.clients[i].jobs[slot].ticket = id;
                io.reply(Out::local(Message::result(id)));
                let r = self.dispatch(who, m, io);
                self.clients[i].jobs[slot].result = Out::local(r);
            }
            _ => io.reply(Out::local(Message::error(INVALID))),
        }
    }
}

/// Display worker: present only for the app Runtime most recently published
/// as foreground, and only that app's own surface.
pub fn present_allowed(owner: u32, m: &Message, presenter: u32) -> bool {
    is_app(owner) && owner == presenter && m.op == EXECUTE && m.key == PRESENT && m.value == owner
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StorageRoute {
    Diagnostic,
    Checkpoint(u32),
    File(u32),
    Reject,
}
/// Storage worker: the disk region always comes from the request's owner,
/// which Runtime sets from the caller's badge, never from message contents.
pub fn storage_route(owner: u32, m: &Message) -> StorageRoute {
    if m.key == STATUS {
        StorageRoute::Diagnostic
    } else if !is_app(owner) {
        StorageRoute::Reject
    } else if m.key == SAVE {
        StorageRoute::Checkpoint(region(owner))
    } else {
        StorageRoute::File(STORAGE_FILE + region(owner))
    }
}
