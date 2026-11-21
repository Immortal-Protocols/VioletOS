use crate::protocol::*;
extern "C" {
    fn touch_status() -> *const u8;
    fn touch_event(x: &mut u32, y: &mut u32, down: &mut bool) -> bool;
    fn app_checkpoint(region: u32, save: bool, value: &mut u32) -> bool;
}
const PACKAGE_MANAGER: u32 = 8;
const DISPLAY_SLOT: u32 = 4;
const RADIO_SLOT: u32 = 5;
const POWER_SLOT: u32 = 6;
const LIFECYCLE: u32 = 7;
#[derive(Default, Clone, Copy)]
struct Stream {
    id: u32,
    last: Option<(u32, u32)>,
}
#[derive(Default, Clone, Copy)]
struct Client {
    serial: u32,
    ticket: u32,
    result: Message,
    streams: [Stream; 3],
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
    clients: [Client; 2],
}
impl Runtime {
    fn contact(&mut self, who: u32) -> Message {
        if who != self.focus {
            return Message::error(EMPTY);
        }
        let (mut x, mut y, mut down) = (0, 0, false);
        if !unsafe { touch_event(&mut x, &mut y, &mut down) } {
            return Message::error(EMPTY);
        }
        let pressed = down && !self.held;
        self.held = down;
        if !pressed {
            return Message::error(EMPTY);
        }
        if self.asleep {
            Message::new(EXECUTE, POWER, 0).call(POWER_SLOT);
            self.asleep = false;
            return Message::error(EMPTY);
        }
        let mut r = Message::result(x);
        r.arg = y;
        r
    }
    fn dispatch(&mut self, who: u32, m: Message) -> Message {
        let component = match m.key {
            RADIO => 7,
            POWER => 8,
            SAVE => 6,
            _ => 0,
        };
        if component != 0 && self.inventory & (1 << component) == 0 {
            return Message::error(OFFLINE);
        }
        match (m.op, m.key) {
            (GET, STATUS) => self.ready,
            (GET, FOCUS) => Message::result(if self.asleep { 0 } else { self.focus }),
            (GET, SURFACE) => Message::result(0x06000000),
            (GET, CATALOG) if who == LAUNCHER => m.call(PACKAGE_MANAGER),
            (EXECUTE, OPEN)
                if (who == LAUNCHER && m.value == WATCH)
                    || (who == WATCH && m.value == LAUNCHER) =>
            {
                let r = m.call(LIFECYCLE);
                if r.op == OK {
                    self.focus = m.value;
                }
                r
            }
            (EXECUTE, QUIT) if who == WATCH => {
                let r = m.call(LIFECYCLE);
                if r.op == OK {
                    self.focus = LAUNCHER;
                    self.clients[1] = Client::default();
                }
                r
            }
            (GET | EXECUTE, SAVE) => {
                let mut value = m.value;
                if unsafe { app_checkpoint((who == LAUNCHER) as u32, m.op == EXECUTE, &mut value) }
                {
                    Message::result(value)
                } else {
                    Message::error(OFFLINE)
                }
            }
            (EXECUTE, PRESENT) if who == self.focus && !self.asleep => {
                let mut req = Message::new(EXECUTE, PRESENT, who);
                req.arg = m.value;
                req.call(DISPLAY_SLOT)
            }
            (GET, RADIO) => m.call(RADIO_SLOT),
            (EXECUTE, RADIO) if who == LAUNCHER => m.call(RADIO_SLOT),
            (GET, POWER) => m.call(POWER_SLOT),
            (EXECUTE, POWER) if who == LAUNCHER && m.value <= 1 => {
                let r = m.call(POWER_SLOT);
                if r.op == OK && m.value == 1 {
                    Message::new(EXECUTE, POWER, 1).call(DISPLAY_SLOT);
                    self.asleep = true;
                }
                r
            }
            _ => Message::error(DENIED),
        }
    }
}
pub fn run() -> ! {
    let mut status = Message::result(0);
    let ptr = unsafe { touch_status() };
    if ptr.is_null() {
        status.put_text(b"TOUCH READY");
    } else {
        let mut bytes = [0; 40];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = unsafe { *ptr.add(i) };
            if *b == 0 {
                break;
            }
        }
        status.put_text(&bytes);
    }
    let inventory = Message::new(GET, INVENTORY, 0).call(PACKAGE_MANAGER).value;
    status.arg = (inventory & (1 << 6) != 0 && unsafe { app_checkpoint(0, false, &mut 0) }) as u32;
    let mut rt = Runtime {
        inventory,
        focus: LAUNCHER,
        held: false,
        asleep: false,
        ready: status,
        clients: [Client::default(); 2],
    };
    loop {
        let (who, m) = Message::receive(SERVER);
        if !(LAUNCHER..=WATCH).contains(&who) {
            Message::error(DENIED).reply();
            continue;
        }
        let i = (who - 1) as usize;
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
                let state = rt.dispatch(who, Message::new(GET, key, 0));
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
            POLL | FORGET if m.value != 0 && m.value == c.ticket => {
                c.ticket = 0;
                if m.op == POLL {
                    c.result.reply()
                } else {
                    Message::result(0).reply()
                }
            }
            GET | EXECUTE if c.ticket == 0 => {
                let id = ticket(c);
                c.ticket = id;
                // Acknowledge acceptance before issuing the downstream operation.
                Message::result(id).reply();
                let result = rt.dispatch(who, m);
                rt.clients[i].result = result;
            }
            GET | EXECUTE => Message::error(BUSY).reply(),
            _ => Message::error(INVALID).reply(),
        }
    }
}
