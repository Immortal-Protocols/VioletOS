use crate::protocol::*;
const LIFECYCLE: u32 = 7;
#[derive(Default, Clone, Copy)]
struct Stream {
    id: u32,
    last: Option<(u32, u32)>,
}
#[derive(Default, Clone, Copy)]
struct Client {
    serial: u32,
    jobs: [Job; 7],
    streams: [Stream; 3],
}
#[derive(Default, Clone, Copy)]
struct Job {
    ticket: u32,
    lane: u32,
    result: Message,
}
fn service(key: u32) -> u32 {
    match key {
        SAVE | FILE_LIST..=FILE_DELETE => 6,
        PRESENT => 5,
        SCAN | NETWORK | PROFILE | RADIO | RADIO_TEST => 7,
        POWER => 8,
        CATALOG | INBOX | INSTALL | REMOVE => 9,
        _ => 0,
    }
}
fn job_slot(key: u32) -> usize {
    service(key).saturating_sub(3) as usize
}
const STREAM_KEYS: [u32; 3] = [CONTACT, RADIO, POWER];
fn ticket(c: &mut Client) -> u32 {
    c.serial = c.serial.wrapping_add(1).max(1);
    c.serial
}
struct Runtime {
    inventory: u32,
    focus: u32,
    held: bool,
    asleep: bool,
    ready: Message,
    clients: [Client; 15],
}
impl Runtime {
    fn contact(&mut self, who: u32) -> Message {
        if who != self.focus {
            return Message::error(EMPTY);
        }
        let event = crate::lanes::sample(4, 16, who, Message::new(GET, CONTACT, 0));
        if event.op != OK {
            return Message::error(EMPTY);
        }
        let down = event.key != 0;
        let pressed = down && !self.held;
        self.held = down;
        if !pressed {
            return Message::error(EMPTY);
        }
        if self.asleep {
            crate::lanes::take(8, 0);
            crate::lanes::queue(8, 0, who, Message::new(EXECUTE, POWER, 0));
            self.asleep = false;
            return Message::error(EMPTY);
        }
        event
    }
    fn forward(&mut self, who: u32, lane: u32, m: Message) -> Message {
        if !crate::lanes::queue(lane, who, who, m) {
            return Message::error(OFFLINE);
        }
        self.clients[who as usize].jobs[(lane - 3) as usize].lane = lane;
        Message::error(BUSY)
    }
    fn dispatch(&mut self, who: u32, m: Message) -> Message {
        let component = match m.key {
            RADIO | NETWORK | PROFILE | RADIO_TEST | SCAN => 7,
            POWER => 8,
            SAVE | FILE_LIST..=FILE_DELETE => 6,
            _ => 0,
        };
        if component != 0 && self.inventory & (1 << component) == 0 {
            return Message::error(OFFLINE);
        }
        match (m.op, m.key) {
            (GET, WHOAMI) => Message::result(who),
            (GET, STATUS) => self.ready,
            (GET, FOCUS) => Message::result(if self.asleep { 0 } else { self.focus }),
            (GET, SURFACE) => Message::result(0x06000000),
            (GET, CATALOG | INBOX) | (EXECUTE, INSTALL | REMOVE) if who == LAUNCHER => {
                self.forward(who, 9, m)
            }
            (EXECUTE, OPEN)
                if (who == LAUNCHER && is_app(m.value)) || (is_app(who) && m.value == LAUNCHER) =>
            {
                let r = m.call(LIFECYCLE);
                if r.op == OK {
                    self.focus = m.value;
                }
                r
            }
            (EXECUTE, QUIT) if who != LAUNCHER => {
                let r = Message::new(EXECUTE, QUIT, who).call(LIFECYCLE);
                if r.op == OK {
                    self.focus = LAUNCHER;
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
            | (GET | EXECUTE, SAVE) => self.forward(who, 6, m),
            (EXECUTE, PRESENT) if who == self.focus && !self.asleep => {
                let mut req = Message::new(EXECUTE, PRESENT, who);
                req.arg = m.value;
                self.forward(who, 5, req)
            }
            (GET | EXECUTE, NETWORK | PROFILE) | (EXECUTE, RADIO_TEST) if who == WIFI => {
                self.forward(who, 7, m)
            }
            (GET, SCAN) if who == WIFI => self.forward(who, 7, m),
            (GET, RADIO) => self.forward(who, 7, m),
            (EXECUTE, RADIO) if who == LAUNCHER => self.forward(who, 7, m),
            (GET, POWER) => self.forward(who, 8, m),
            (EXECUTE, POWER) if who == LAUNCHER && m.value <= 2 => {
                self.asleep = m.value == 1;
                self.forward(who, 8, m)
            }
            _ => Message::error(DENIED),
        }
    }
}
pub fn run() -> ! {
    let mut status = crate::lanes::initialize(4, LAUNCHER, Message::new(GET, STATUS, 0));
    let inventory = crate::lanes::initialize(9, LAUNCHER, Message::new(GET, INVENTORY, 0)).value;
    status.arg = (inventory & (1 << 6) != 0
        && crate::lanes::initialize(6, WATCH, Message::new(GET, SAVE, 0)).op == OK)
        as u32;
    let mut rt = Runtime {
        inventory,
        focus: LAUNCHER,
        held: false,
        asleep: false,
        ready: status,
        clients: [Client::default(); 15],
    };
    loop {
        let (who, m) = Message::receive(SERVER);
        if !is_app(who) {
            Message::error(DENIED).reply();
            continue;
        }
        unsafe {
            let health = 0x07400ff4 as *mut u32;
            let now = ticks();
            health.write_volatile(now);
            if health.add(1).read_volatile() != rt.focus || who == rt.focus {
                health.add(1).write_volatile(rt.focus);
                health.add(2).write_volatile(now);
            }
        }
        let i = who as usize;
        let c = &mut rt.clients[i];
        match m.op {
            SUBSCRIBE if STREAM_KEYS.contains(&m.key) => {
                let index = STREAM_KEYS.iter().position(|&key| key == m.key).unwrap();
                if m.key != CONTACT && rt.inventory & (1 << m.key) == 0 {
                    Message::error(OFFLINE).reply();
                } else {
                    if c.streams[index].id == 0 {
                        let id = ticket(c);
                        c.streams[index] = Stream { id, last: None };
                    }
                    Message::result(c.streams[index].id).reply();
                }
            }
            POLL | FORGET if m.value != 0 && c.streams.iter().any(|s| s.id == m.value) => {
                let index = c.streams.iter().position(|s| s.id == m.value).unwrap();
                if m.op == FORGET {
                    c.streams[index] = Stream::default();
                    Message::result(0).reply();
                    continue;
                }
                let key = STREAM_KEYS[index];
                if key == CONTACT {
                    rt.contact(who).reply();
                    continue;
                }
                let state = crate::lanes::sample(
                    if key == RADIO { 7 } else { 8 },
                    16 + who,
                    who,
                    Message::new(GET, key, 0),
                );
                let stream = &mut rt.clients[i].streams[index];
                if state.op != OK {
                    state.reply();
                } else if stream.last == Some((state.value, state.arg)) {
                    Message::error(EMPTY).reply();
                } else {
                    stream.last = Some((state.value, state.arg));
                    state.reply();
                }
            }
            POLL | FORGET if m.value != 0 => {
                let Some(job) = c.jobs.iter_mut().find(|j| j.ticket == m.value) else {
                    Message::error(INVALID).reply();
                    continue;
                };
                if job.lane != 0 {
                    if let Some(r) = crate::lanes::take(job.lane, who) {
                        job.result = r;
                        job.lane = 0;
                    } else if m.op == POLL {
                        Message::error(BUSY).reply();
                        continue;
                    }
                }
                job.ticket = 0;
                if m.op == POLL {
                    job.result.reply();
                } else {
                    Message::result(0).reply();
                }
            }
            GET | EXECUTE => {
                let slot = job_slot(m.key);
                let job = &mut c.jobs[slot];
                if job.ticket != 0 || (job.lane != 0 && crate::lanes::take(job.lane, who).is_none())
                {
                    Message::error(BUSY).reply();
                    continue;
                }
                job.lane = 0;
                let id = ticket(c);
                c.jobs[slot].ticket = id;
                Message::result(id).reply();
                let r = rt.dispatch(who, m);
                rt.clients[i].jobs[slot].result = r;
            }
            _ => Message::error(INVALID).reply(),
        }
    }
}
