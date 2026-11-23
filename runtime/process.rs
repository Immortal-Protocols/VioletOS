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
    streams: [Stream; STREAMS],
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
        SCAN | NETWORK | PROFILE | RADIO | RADIO_TEST | WEATHER | RELAY => 7,
        POWER => 8,
        CATALOG | INBOX | INSTALL | REMOVE => 9,
        _ => 0,
    }
}
fn job_slot(key: u32) -> usize {
    service(key).saturating_sub(3) as usize
}
const STREAMS: usize = 5;
const STREAM_KEYS: [u32; STREAMS] = [CONTACT, RADIO, POWER, VOLUME, PLAYBACK];
/// The component whose inventory bit gates a key.
fn component(key: u32) -> u32 {
    match key {
        RADIO | NETWORK | PROFILE | RADIO_TEST | SCAN | WEATHER | RELAY => 7,
        POWER => 8,
        SAVE | FILE_LIST..=FILE_DELETE => 6,
        VOLUME | PLAYBACK | MUSIC => AUDIO_COMPONENT,
        _ => 0,
    }
}
/// Audio answers between polls of its hardware, so Runtime calls it directly.
fn audio(m: Message) -> Message {
    Message::new(m.op, m.key, m.value).with_arg(m.arg).call(AUDIO_SLOT)
}
fn ticket(c: &mut Client) -> u32 {
    c.serial = c.serial.wrapping_add(1).max(1);
    c.serial
}
struct Runtime {
    inventory: u32,
    focus: u32,
    held: bool,
    asleep: bool,
    usb_open: u32,
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
    /// A bench request over USB (moto-package open=ID) to wake the phone and
    /// open an app. The supervisor bumps a serial in the high half of the cell.
    fn usb_open(&mut self) {
        let cell = unsafe { (0x07400000 as *const u32).add(983).read_volatile() };
        if cell == self.usb_open {
            return;
        }
        let first = self.usb_open == u32::MAX;
        self.usb_open = cell;
        let id = cell & 0xffff;
        if first || !is_app(id) {
            return;
        }
        if self.asleep {
            crate::lanes::take(8, 0);
            crate::lanes::queue(8, 0, LAUNCHER, Message::new(EXECUTE, POWER, 0));
            self.asleep = false;
        }
        if Message::new(EXECUTE, OPEN, id).call(LIFECYCLE).op == OK {
            self.focus = id;
        }
    }
    fn dispatch(&mut self, who: u32, m: Message) -> Message {
        self.usb_open();
        let component = component(m.key);
        if component != 0 && self.inventory & (1 << component) == 0 {
            return Message::error(OFFLINE);
        }
        match (m.op, m.key) {
            (GET, WHOAMI) => Message::result(who),
            (GET, STATUS) => self.ready,
            (GET, 0x4057) if who == LAUNCHER => unsafe {
                let cell=(0x07400000 as *const u32).add(980);
                let mut r=Message::result(cell.read_volatile());
                r.arg=cell.add(1).read_volatile();
                r.key=cell.add(2).read_volatile(); r
            },
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
                    client.streams = [Stream::default(); STREAMS];
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
            (EXECUTE, WEATHER) if who == WEATHER_APP && (m.value as usize) < CITIES.len() => {
                self.forward(who, 7, m)
            }
            (GET, RADIO) => self.forward(who, 7, m),
            (EXECUTE, RADIO) if who == LAUNCHER => self.forward(who, 7, m),
            (GET | EXECUTE, RELAY) if who == LAUNCHER => self.forward(who, 7, m),
            (GET, POWER) => self.forward(who, 8, m),
            (GET, VOLUME | PLAYBACK | MUSIC) => audio(m),
            // Volume is shared. Only the app on screen may change it or make sound.
            (EXECUTE, VOLUME | PLAYBACK | MUSIC) if who == self.focus && !self.asleep => audio(m),
            (EXECUTE, POWER) if who == LAUNCHER && m.value <= 2 => {
                self.asleep = m.value == 1;
                self.forward(who, 8, m)
            }
            _ => Message::error(DENIED),
        }
    }
}
const BOOT_SLOT: u32 = 31; // App stream slots end at 30.
struct Startup {
    touch_pending: bool,
    disk_pending: bool,
    disk_reason_pending: bool,
    disk_reason: Option<Message>,
}
impl Startup {
    fn begin(inventory: u32) -> Self {
        let touch_pending = crate::lanes::queue(4, BOOT_SLOT, LAUNCHER, Message::new(GET, STATUS, 0));
        let disk_pending = inventory & (1 << 6) != 0
            && crate::lanes::queue(6, BOOT_SLOT, WATCH, Message::new(GET, SAVE, 0));
        Self { touch_pending, disk_pending, disk_reason_pending: false, disk_reason: None }
    }
    fn update(&mut self, status: &mut Message) {
        if status.key == 0 { return; }
        if self.touch_pending {
            if let Some(r) = crate::lanes::take(4, BOOT_SLOT) {
                status.put_text(&r.text());
                self.touch_pending = false;
            }
        }
        if self.disk_pending {
            if let Some(r) = crate::lanes::take(6, BOOT_SLOT) {
                status.arg = (r.op == OK) as u32;
                self.disk_pending = false;
                if r.op != OK {
                    self.disk_reason_pending = crate::lanes::queue(6, BOOT_SLOT, WATCH, Message::new(GET, STATUS, 0));
                }
            }
        }
        if self.disk_reason_pending {
            if let Some(r) = crate::lanes::take(6, BOOT_SLOT) {
                self.disk_reason = Some(r);
                self.disk_reason_pending = false;
            }
        }
        if self.touch_pending || self.disk_pending || self.disk_reason_pending { return; }
        if let Some(r) = self.disk_reason {
            if status.text().starts_with(b"TOUCH READY") && r.op == OK && r.text()[0] != 0 {
                status.put_text(&r.text());
            }
        }
        status.key = 0;
        publish_status(status);
    }
}
fn publish_status(status: &Message) {
    // Publish boot diagnostics in the unused tail of the supervisor health page.
    // Only the supervisor copies this narrow record to the USB process.
    unsafe {
        let boot_status = 0x07400f80 as *mut u32;
        for (i, chunk) in status.text().chunks_exact(4).enumerate() {
            boot_status.add(i + 2).write_volatile(u32::from_le_bytes(chunk.try_into().unwrap()));
        }
        boot_status.add(1).write_volatile(status.arg);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        boot_status.write_volatile(1);
    }
}
pub fn run() -> ! {
    let inventory = crate::lanes::initialize(9, LAUNCHER, Message::new(GET, INVENTORY, 0)).value;
    let mut startup = Startup::begin(inventory);
    let mut status = Message::result(0);
    status.key = 1;
    status.put_text(b"STARTING SERVICES");
    let mut rt = Runtime {
        inventory,
        focus: LAUNCHER,
        held: false,
        asleep: false,
        usb_open: u32::MAX,
        ready: status,
        clients: [Client::default(); 15],
    };
    loop {
        let (who, m) = Message::receive(SERVER);
        startup.update(&mut rt.ready);
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
                if m.key != CONTACT && rt.inventory & (1 << component(m.key)) == 0 {
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
                let state = if matches!(key, VOLUME | PLAYBACK) {
                    audio(Message::new(GET, key, 0))
                } else {
                    crate::lanes::sample(
                        if key == RADIO { 7 } else { 8 },
                        16 + who,
                        who,
                        Message::new(GET, key, 0),
                    )
                };
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
