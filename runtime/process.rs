use crate::protocol::*;
const LIFECYCLE: u32 = 7;
// What is refused while the phone is locked (host-tested with the compositor).
#[path = "../system/compositor/gate.rs"]
mod gate;
const _: () = assert!(
    gate::EXECUTE == EXECUTE && gate::OPEN == OPEN && gate::QUIT == QUIT
        && gate::INSTALL == INSTALL && gate::REMOVE == REMOVE && gate::RELAY == RELAY
        && gate::PRESENT == PRESENT && gate::VOLUME == VOLUME && gate::PLAYBACK == PLAYBACK
        && gate::MICROPHONE == MICROPHONE
);
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
        SAVE | FILE_LIST..=FILE_DELETE | APPSTORE => 6,
        PRESENT => 5,
        SCAN | NETWORK | PROFILE | RADIO | RADIO_TEST | RELAY => 7,
        POWER => 8,
        CATALOG | INBOX | INSTALL | REMOVE => 9,
        _ => 0,
    }
}
fn job_slot(key: u32) -> usize {
    service(key).saturating_sub(3) as usize
}
const STREAMS: usize = 6;
const STREAM_KEYS: [u32; STREAMS] = [CONTACT, RADIO, POWER, VOLUME, PLAYBACK, MICROPHONE];
/// The component whose inventory bit gates a key.
fn component(key: u32) -> u32 {
    match key {
        RADIO | NETWORK | PROFILE | RADIO_TEST | SCAN | RELAY => 7,
        POWER => 8,
        SAVE | FILE_LIST..=FILE_DELETE | APPSTORE => 6,
        VOLUME | PLAYBACK | MICROPHONE => AUDIO_COMPONENT,
        _ => 0,
    }
}
/// Audio answers commands between polls of its hardware, so Runtime calls it
/// directly. Its state (GET VOLUME, PLAYBACK, MICROPHONE) Runtime reads from
/// the page Audio publishes it in, so a read never waits for Audio's loop.
fn audio(m: Message) -> Message {
    if m.op == GET {
        if let Some(r) = audio_snapshot(m.key) {
            return r;
        }
    }
    Message::new(m.op, m.key, m.value).with_arg(m.arg).call(AUDIO_SLOT)
}
/// Audio's published reply for key (services/audio/process.rs, Audio::snapshot),
/// unless the page is missing or being rewritten.
fn audio_snapshot(key: u32) -> Option<Message> {
    let index = match key {
        VOLUME => 0,
        PLAYBACK => 1,
        MICROPHONE => 2,
        _ => return None,
    };
    let page = audio_page();
    for _ in 0..4 {
        unsafe {
            let seq = page.read_volatile();
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            if seq & 1 != 0 || page.add(1).read_volatile() != 0x3154_5341 {
                continue;
            }
            let r = (page.add(4) as *const Message).add(index).read_volatile();
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            if page.read_volatile() == seq {
                return Some(r);
            }
        }
    }
    None
}
/// Where Audio's state page is mapped in this process.
#[cfg(not(test))]
fn audio_page() -> *const u32 {
    0x0750_0000 as *const u32
}
/// On the host (tests below) there is no such page: an empty one, which
/// `audio_snapshot` reads as "not published" and so asks Audio.
#[cfg(test)]
fn audio_page() -> *const u32 {
    static EMPTY: [u32; 64] = [0; 64];
    EMPTY.as_ptr()
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
    /// The Open Runtime's lock screen is up (it says so with each GET FOCUS),
    /// or the phone slept and the Open Runtime has not answered since.
    locked: bool,
    /// The phone slept since the Open Runtime last asked for the focus: its
    /// next GET FOCUS is answered 0 (screen off, so it locks) even if awake.
    slept: bool,
    ready: Message,
    clients: [Client; 15],
}
impl Runtime {
    /// The screen goes off. A passcode locks then, so the phone counts as
    /// locked at once, not when the Open Runtime next polls.
    fn sleep(&mut self) {
        self.asleep = true;
        self.mic_off();
        if self.inventory & (1 << OPEN_COMPONENT) != 0 {
            self.locked = true;
            self.slept = true;
        }
    }
    /// The screen comes on. Locked, it shows the open apps (the lock screen)
    /// from this moment: the built-in screen in front before sleep gets no
    /// touch, present or sound.
    fn wake(&mut self) {
        self.asleep = false;
        if self.locked && self.focus != OPEN_APPS {
            self.focus = OPEN_APPS;
            self.mic_off();
        }
    }
    /// A built-in client's present is still with Display (lane 5). Finished
    /// ones are collected here for the client's POLL, as `run` would.
    fn presenting(&mut self) -> bool {
        let mut busy = false;
        for (who, c) in self.clients.iter_mut().enumerate() {
            let job = &mut c.jobs[job_slot(PRESENT)];
            if who as u32 == OPEN_APPS || job.lane != 5 {
                continue;
            }
            match crate::lanes::take(5, who as u32) {
                Some(r) => {
                    job.result = r;
                    job.lane = 0;
                }
                None => busy = true,
            }
        }
        busy
    }
    /// The microphone belongs to the app on screen: it stops when focus moves or the phone sleeps.
    fn mic_off(&self) {
        if self.inventory & (1 << AUDIO_COMPONENT) != 0 {
            audio(Message::new(EXECUTE, MICROPHONE, MIC_OFF));
        }
    }
    fn contact(&mut self, who: u32) -> Message {
        if who != self.focus {
            return Message::error(EMPTY);
        }
        let event = crate::lanes::sample(4, 16, who, Message::new(GET, CONTACT, 0));
        if event.op != OK {
            return Message::error(EMPTY);
        }
        let down = event.key != 0;
        // The Open Runtime passes every touch change on to its apps.
        if who == OPEN_APPS {
            self.held = down;
            return event;
        }
        let pressed = down && !self.held;
        self.held = down;
        if !pressed {
            return Message::error(EMPTY);
        }
        if self.asleep {
            crate::lanes::take(8, 0);
            crate::lanes::queue(8, 0, who, Message::new(EXECUTE, POWER, 0));
            self.wake();
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
        let component = component(m.key);
        if component != 0 && self.inventory & (1 << component) == 0 {
            return Message::error(OFFLINE);
        }
        // Locked: nothing else comes to the front (system/compositor/gate.rs).
        if self.locked && gate::locked_refuses(m.op, m.key, who == OPEN_APPS) {
            return Message::error(DENIED);
        }
        match (m.op, m.key) {
            (GET, WHOAMI) => Message::result(who),
            // Which optional components are installed, one bit per component ID.
            (GET, INVENTORY) => Message::result(self.inventory),
            (GET, STATUS) => self.ready,
            (GET, 0x4057) if who == LAUNCHER => unsafe {
                let cell=(0x07400000 as *const u32).add(980);
                let mut r=Message::result(cell.read_volatile());
                r.arg=cell.add(1).read_volatile();
                r.key=cell.add(2).read_volatile(); r
            },
            // The lock screen is the Open Runtime's: while it says it is up
            // (and the screen is on), the open apps are in front, so no
            // built-in screen is shown or touched behind the lock.
            (GET, FOCUS) if who == OPEN_APPS && self.inventory & (1 << OPEN_COMPONENT) != 0 => {
                // A sleep it has not seen: it hears 0 (and locks, if there is
                // a passcode) now, even if the phone is awake again. Its word
                // was chosen before that answer, so the lock stays until the
                // next poll says what it decided.
                if self.slept {
                    self.slept = false;
                    return Message::result(0);
                }
                self.locked = m.value == gate::LOCKED;
                if !self.asleep {
                    self.wake();
                }
                // It draws the lock screen once it is in front: not before a
                // built-in app's present still with Display has finished.
                let hold = self.asleep || (self.locked && self.presenting());
                Message::result(if hold { 0 } else { self.focus })
            }
            (GET, FOCUS) => Message::result(if self.asleep { 0 } else { self.focus }),
            (GET, SURFACE) => Message::result(0x06000000),
            (GET, CATALOG | INBOX) | (EXECUTE, INSTALL | REMOVE) if who == LAUNCHER => {
                self.forward(who, 9, m)
            }
            // A full-system update's check or switch ("OTA1"/"OTA2"), for the
            // Open Runtime's `update` commands; the supervisor decides.
            (EXECUTE, INSTALL) if who == OPEN_APPS && (m.value == 0x3141_544f || m.value == 0x3241_544f) => {
                self.forward(who, 9, m)
            }
            // The Open Runtime is always running, so focus moves without a launch.
            (EXECUTE, OPEN)
                if who == LAUNCHER && m.value == OPEN_APPS
                    && self.inventory & (1 << OPEN_COMPONENT) != 0 =>
            {
                self.focus = OPEN_APPS;
                Message::result(0)
            }
            (EXECUTE, OPEN)
                if (who == LAUNCHER && is_app(m.value))
                    || ((is_app(who) || who == OPEN_APPS) && m.value == LAUNCHER) =>
            {
                let r = m.call(LIFECYCLE);
                if r.op == OK {
                    self.focus = m.value;
                    self.mic_off();
                }
                r
            }
            (EXECUTE, QUIT) if who != LAUNCHER => {
                let r = Message::new(EXECUTE, QUIT, who).call(LIFECYCLE);
                if r.op == OK {
                    self.focus = LAUNCHER;
                    self.mic_off();
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
            (GET | EXECUTE, APPSTORE) if who == OPEN_APPS => self.forward(who, 6, m),
            (EXECUTE, PRESENT) if who == self.focus && !self.asleep => {
                let mut req = Message::new(EXECUTE, PRESENT, who);
                req.arg = m.value;
                self.forward(who, 5, req)
            }
            // The Open Runtime's Wi-Fi settings (the "wifi" commands in
            // /dev/violet/sys, which only /apps/wifi may use to join or leave).
            (GET, SCAN | PROFILE) | (EXECUTE, PROFILE) if who == OPEN_APPS => self.forward(who, 7, m),
            (EXECUTE, RADIO) if who == OPEN_APPS && m.value == 1 => self.forward(who, 7, m),
            (GET, RADIO) => self.forward(who, 7, m),
            (EXECUTE, RADIO) if who == LAUNCHER => self.forward(who, 7, m),
            // Phone Internet: the Open Runtime asks for Settings (runtime/open/wifi.rs).
            (GET | EXECUTE, RELAY) if who == LAUNCHER || who == OPEN_APPS => self.forward(who, 7, m),
            (GET, POWER) => self.forward(who, 8, m),
            (GET, VOLUME | PLAYBACK | MICROPHONE) => audio(m),
            // Volume is shared. Only the app on screen may change it, make sound
            // or listen.
            (EXECUTE, VOLUME | PLAYBACK | MICROPHONE) if who == self.focus && !self.asleep => audio(m),
            (EXECUTE, POWER) if who == LAUNCHER && m.value <= 2 => {
                if m.value == 1 {
                    self.sleep();
                } else {
                    self.wake();
                }
                self.forward(who, 8, m)
            }
            // Wake locks for open apps (the Open Runtime names the app in arg).
            (EXECUTE, POWER) if who == OPEN_APPS && m.value & !0xff == POWER_LOCK => self.forward(who, 8, m),
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
        locked: false,
        slept: false,
        ready: status,
        clients: [Client::default(); 15],
    };
    loop {
        let (who, m) = Message::receive(SERVER);
        startup.update(&mut rt.ready);
        if !is_app(who) && who != OPEN_APPS {
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
                let state = if matches!(key, VOLUME | PLAYBACK | MICROPHONE) {
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

/// Who may ask Runtime for what: the broker's rules, pinned on the host
/// (`make test-unit`, runtime/open/tests/broker_host.rs supplies stand-ins for
/// the lanes and the IPC calls and records what `dispatch` sent where).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{sent, Sent};

    const APPS: [u32; 7] = [LAUNCHER, WATCH, 10, 11, 12, 13, APP_LAST];
    const EVERYONE: [u32; 8] = [LAUNCHER, WATCH, 10, 11, 12, 13, APP_LAST, OPEN_APPS];
    const ALL: u32 = u32::MAX;

    fn runtime(inventory: u32, focus: u32) -> Runtime {
        Runtime { inventory, focus, held: false, asleep: false, locked: false, slept: false, ready: Message::result(0), clients: [Client::default(); 15] }
    }
    /// What one request did: the reply's code, and where it went.
    fn ask(rt: &mut Runtime, who: u32, op: u32, key: u32, value: u32) -> (u32, Vec<Sent>) {
        sent(); // forget earlier requests
        let r = rt.dispatch(who, Message::new(op, key, value));
        (r.op, sent())
    }
    fn denied(rt: &mut Runtime, who: u32, op: u32, key: u32, value: u32) -> bool {
        let (code, went) = ask(rt, who, op, key, value);
        assert!(code != DENIED || went.is_empty(), "client {who}: op {op} key {key} was denied but still sent {went:?}");
        code == DENIED
    }
    fn lane_of(rt: &mut Runtime, who: u32, op: u32, key: u32, value: u32) -> Option<u32> {
        match ask(rt, who, op, key, value) {
            (BUSY, went) => {
                // (Putting the phone to sleep also tells Audio to stop listening.)
                let lanes: Vec<&Sent> = went.iter().filter(|s| matches!(s, Sent::Lane { .. })).collect();
                match lanes[..] {
                    [&Sent::Lane { lane, owner, .. }] => {
                        assert_eq!(owner, who, "a forwarded request is queued as its sender");
                        Some(lane)
                    }
                    _ => panic!("client {who}: op {op} key {key} sent {went:?}"),
                }
            }
            _ => None,
        }
    }

    #[test]
    fn only_the_launcher_manages_packages() {
        for who in EVERYONE {
            let mut rt = runtime(ALL, who);
            for (op, key) in [(GET, CATALOG), (GET, INBOX), (EXECUTE, INSTALL), (EXECUTE, REMOVE)] {
                if who == LAUNCHER {
                    assert_eq!(lane_of(&mut rt, who, op, key, 0), Some(9));
                } else {
                    assert!(denied(&mut rt, who, op, key, 0), "client {who} op {op} key {key}");
                    assert!(denied(&mut rt, who, op, key, 12), "client {who} op {op} key {key}");
                }
            }
        }
        // The Open Runtime may ask for exactly two things: check a full-system
        // update and switch to it. The supervisor decides both.
        let mut rt = runtime(ALL, OPEN_APPS);
        assert_eq!(lane_of(&mut rt, OPEN_APPS, EXECUTE, INSTALL, 0x3141_544f), Some(9));
        assert_eq!(lane_of(&mut rt, OPEN_APPS, EXECUTE, INSTALL, 0x3241_544f), Some(9));
        for value in [0, 1, 0x3041_544f, 0x3341_544f, u32::MAX] {
            assert!(denied(&mut rt, OPEN_APPS, EXECUTE, INSTALL, value));
        }
        for app in [WATCH, 11, APP_LAST] {
            assert!(denied(&mut rt, app, EXECUTE, INSTALL, 0x3241_544f), "an app cannot switch the system");
        }
    }

    #[test]
    fn apps_cannot_put_other_apps_in_front() {
        for who in APPS {
            for target in EVERYONE {
                let mut rt = runtime(ALL, who);
                let (code, went) = ask(&mut rt, who, EXECUTE, OPEN, target);
                // The launcher opens anything; an app only hands the screen back to it.
                let allowed = who == LAUNCHER || target == LAUNCHER;
                if !allowed {
                    assert_eq!(code, DENIED, "client {who} opening {target}");
                    assert!(went.is_empty());
                    assert_eq!(rt.focus, who);
                } else {
                    assert_eq!(code, OK, "client {who} opening {target}");
                    assert_eq!(rt.focus, target);
                }
            }
        }
        // The Open Runtime may only hand the screen back to the launcher.
        for target in [11, OPEN_APPS, WATCH] {
            let mut rt = runtime(ALL, OPEN_APPS);
            assert!(denied(&mut rt, OPEN_APPS, EXECUTE, OPEN, target));
            assert_eq!(rt.focus, OPEN_APPS);
        }
        let mut rt = runtime(ALL, OPEN_APPS);
        assert_eq!(ask(&mut rt, OPEN_APPS, EXECUTE, OPEN, LAUNCHER).0, OK);
        assert_eq!(rt.focus, LAUNCHER);
        // Without the Open Runtime installed there is nothing to put in front.
        let mut rt = runtime(ALL & !(1 << OPEN_COMPONENT), LAUNCHER);
        assert!(denied(&mut rt, LAUNCHER, EXECUTE, OPEN, OPEN_APPS));
    }

    #[test]
    fn only_the_app_in_front_draws_makes_sound_or_listens() {
        for focus in [11, OPEN_APPS] {
            for who in EVERYONE {
                for asleep in [false, true] {
                    let mut rt = runtime(ALL, focus);
                    rt.asleep = asleep;
                    let allowed = who == focus && !asleep;
                    assert_eq!(lane_of(&mut rt, who, EXECUTE, PRESENT, 0) == Some(5), allowed, "PRESENT by {who}, focus {focus}, asleep {asleep}");
                    for key in [VOLUME, PLAYBACK, MICROPHONE] {
                        let (code, went) = ask(&mut rt, who, EXECUTE, key, 1);
                        if allowed {
                            assert!(matches!(went[..], [Sent::Call { slot: AUDIO_SLOT, key: k, .. }] if k == key));
                        } else {
                            assert_eq!(code, DENIED, "EXECUTE {key} by {who}, focus {focus}, asleep {asleep}");
                            assert!(went.is_empty());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_microphone_stops_when_focus_moves_or_the_phone_sleeps() {
        let mic_off = |went: &[Sent]| went.iter().any(|s| matches!(s, Sent::Call { slot: AUDIO_SLOT, key: MICROPHONE, value: MIC_OFF, op: EXECUTE }));
        let mut rt = runtime(ALL, LAUNCHER);
        assert!(mic_off(&ask(&mut rt, LAUNCHER, EXECUTE, OPEN, 11).1), "an app comes to the front");
        assert!(mic_off(&ask(&mut rt, 11, EXECUTE, OPEN, LAUNCHER).1), "it leaves");
        rt.focus = 11;
        assert!(mic_off(&ask(&mut rt, 11, EXECUTE, QUIT, 0).1), "it quits");
        assert_eq!(rt.focus, LAUNCHER);
        assert!(mic_off(&ask(&mut rt, LAUNCHER, EXECUTE, POWER, 1).1), "the phone sleeps");
        assert!(rt.asleep);
        // Asleep, nobody is in front.
        assert_eq!(rt.dispatch(11, Message::new(GET, FOCUS, 0)).value, 0);
    }

    #[test]
    fn power_radio_and_store_controls_are_not_for_apps() {
        for who in EVERYONE {
            let mut rt = runtime(ALL, who);
            // Sleep, wake and shut down: the launcher. (Values past 2 are nobody's.)
            for value in 0..=2 {
                assert_eq!(lane_of(&mut rt, who, EXECUTE, POWER, value) == Some(8), who == LAUNCHER, "POWER {value} by {who}");
                // (A sleep locks: start the next check unlocked and awake.)
                (rt.asleep, rt.locked, rt.slept, rt.focus) = (false, false, false, who);
            }
            assert!(denied(&mut rt, who, EXECUTE, POWER, 3));
            // Wake locks: the Open Runtime, on behalf of the app it names.
            for value in [POWER_LOCK, POWER_LOCK | POWER_LOCK_FULL, POWER_LOCK | 0xff] {
                assert_eq!(lane_of(&mut rt, who, EXECUTE, POWER, value) == Some(8), who == OPEN_APPS, "wake lock by {who}");
            }
            assert!(denied(&mut rt, who, EXECUTE, POWER, POWER_LOCK + 0x100));
            // Wi-Fi: scanning and profiles through the Open Runtime only;
            // the radio switch for the launcher, and "on" for the Open Runtime.
            for (op, key) in [(GET, SCAN), (GET, PROFILE), (EXECUTE, PROFILE)] {
                assert_eq!(lane_of(&mut rt, who, op, key, 0) == Some(7), who == OPEN_APPS, "op {op} key {key} by {who}");
            }
            assert_eq!(lane_of(&mut rt, who, EXECUTE, RADIO, 1) == Some(7), who == LAUNCHER || who == OPEN_APPS);
            assert_eq!(lane_of(&mut rt, who, EXECUTE, RADIO, 0) == Some(7), who == LAUNCHER);
            for op in [GET, EXECUTE] {
                assert_eq!(lane_of(&mut rt, who, op, RELAY, 0) == Some(7), who == LAUNCHER || who == OPEN_APPS);
                // The installed-app store on disk belongs to the Open Runtime.
                assert_eq!(lane_of(&mut rt, who, op, APPSTORE, 0) == Some(6), who == OPEN_APPS);
            }
            assert!(denied(&mut rt, who, EXECUTE, SCAN, 0));
            assert!(denied(&mut rt, who, GET, NETWORK, 0) && denied(&mut rt, who, EXECUTE, NETWORK, 0));
            assert!(denied(&mut rt, who, GET, RADIO_TEST, 0) && denied(&mut rt, who, EXECUTE, RADIO_TEST, 0));
        }
    }

    /// Everything an ordinary app that is not in front can do, as a complete
    /// list: a new arm in `dispatch` that reaches apps shows up here.
    #[test]
    fn an_app_in_the_background_can_do_exactly_this() {
        let gets = [STATUS, FOCUS, SAVE, RADIO, POWER, INVENTORY, SURFACE, WHOAMI, FILE_LIST, FILE_READ, VOLUME, PLAYBACK, MICROPHONE];
        let executes = [SAVE, QUIT, FILE_WRITE, FILE_TRUNCATE, FILE_DELETE];
        for who in [WATCH, 10, 12, APP_LAST] {
            for key in 0..256 {
                for value in [0, 1, 2, who, 11, OPEN_APPS, 0x3241_544f, POWER_LOCK, u32::MAX] {
                    let mut rt = runtime(ALL, 11);
                    let open_launcher = key == OPEN && value == LAUNCHER;
                    assert_eq!(!denied(&mut rt, who, GET, key, value), gets.contains(&key), "GET {key} ({value}) by {who}");
                    let mut rt = runtime(ALL, 11);
                    assert_eq!(!denied(&mut rt, who, EXECUTE, key, value), executes.contains(&key) || open_launcher, "EXECUTE {key} ({value}) by {who}");
                }
            }
            // Other operations never reach `dispatch` (see `run`); if one did, it is denied.
            for op in [0, SUBSCRIBE, POLL, FORGET, 6, u32::MAX] {
                for key in 0..64 {
                    assert!(denied(&mut runtime(ALL, 11), who, op, key, LAUNCHER), "op {op} key {key}");
                }
            }
        }
    }

    #[test]
    fn a_missing_service_is_offline_for_everyone() {
        for who in EVERYONE {
            let mut rt = runtime(0, who);
            for (op, key) in [(GET, RADIO), (EXECUTE, RADIO), (GET, POWER), (EXECUTE, POWER), (GET, SAVE), (EXECUTE, FILE_WRITE), (GET, APPSTORE), (GET, VOLUME), (EXECUTE, MICROPHONE), (GET, SCAN), (EXECUTE, RELAY)] {
                let (code, went) = ask(&mut rt, who, op, key, 1);
                assert_eq!(code, OFFLINE, "op {op} key {key} by {who}");
                assert!(went.is_empty(), "nothing is sent to a service that is not there");
            }
        }
    }

    #[test]
    fn quitting_returns_the_screen_and_forgets_the_client() {
        let mut rt = runtime(ALL, 12);
        rt.clients[12].streams[0] = Stream { id: 7, last: None };
        rt.clients[12].jobs[2].ticket = 9;
        let (code, went) = ask(&mut rt, 12, EXECUTE, QUIT, 0);
        assert_eq!(code, OK);
        assert!(matches!(went[0], Sent::Call { slot: LIFECYCLE, key: QUIT, value: 12, .. }), "the supervisor is told who quit, not whom the app named");
        assert_eq!(rt.focus, LAUNCHER);
        assert!(rt.clients[12].streams.iter().all(|s| s.id == 0) && rt.clients[12].jobs.iter().all(|j| j.ticket == 0));
        // The launcher does not quit.
        assert!(denied(&mut rt, LAUNCHER, EXECUTE, QUIT, 0));
        // An app cannot quit another: the value it sends is ignored.
        let mut rt = runtime(ALL, 12);
        let (_, went) = ask(&mut rt, 13, EXECUTE, QUIT, 12);
        assert!(matches!(went[0], Sent::Call { slot: LIFECYCLE, value: 13, .. }));
    }

    /// The lock screen is the Open Runtime's (system/compositor/gate.rs).
    #[test]
    fn locked_the_open_apps_stay_in_front_and_nothing_else_opens() {
        // Only the Open Runtime's own word locks: an app's GET FOCUS value is ignored.
        for who in APPS {
            let mut rt = runtime(ALL, LAUNCHER);
            assert_eq!(rt.dispatch(who, Message::new(GET, FOCUS, gate::LOCKED)).value, LAUNCHER);
            assert!(!rt.locked);
        }
        // A built-in screen is in front when the Open Runtime reports its lock
        // screen up: the open apps come to the front (and the microphone stops).
        let mic_off = |went: &[Sent]| went.iter().any(|s| matches!(s, Sent::Call { slot: AUDIO_SLOT, key: MICROPHONE, value: MIC_OFF, op: EXECUTE }));
        let mut rt = runtime(ALL, 11);
        let (code, went) = ask(&mut rt, OPEN_APPS, GET, FOCUS, gate::LOCKED);
        assert_eq!((code, rt.focus, rt.locked), (OK, OPEN_APPS, true));
        assert!(mic_off(&went));
        assert_eq!(rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, gate::LOCKED)).value, OPEN_APPS);
        // Behind the lock no built-in screen presents, is opened, or quits to the launcher.
        for who in APPS {
            assert_eq!(lane_of(&mut rt, who, EXECUTE, PRESENT, 0), None, "PRESENT by {who} while locked");
            for target in EVERYONE {
                assert!(denied(&mut rt, who, EXECUTE, OPEN, target), "{who} opening {target} while locked");
            }
            assert!(denied(&mut rt, who, EXECUTE, QUIT, 0));
            assert_eq!(rt.focus, OPEN_APPS);
        }
        // Not even the Open Runtime hands the screen away while locked.
        assert!(denied(&mut rt, OPEN_APPS, EXECUTE, OPEN, LAUNCHER));
        assert!(denied(&mut rt, OPEN_APPS, EXECUTE, QUIT, 0));
        // The launcher, off screen, installs, removes and relays nothing.
        for (key, value) in [(INSTALL, 0), (REMOVE, 12), (RELAY, 1)] {
            assert!(denied(&mut rt, LAUNCHER, EXECUTE, key, value), "EXECUTE {key} by the launcher while locked");
        }
        // Its radio switch goes on: the boot restore of the saved choice and
        // the Mac's USB Wi-Fi job are sent once and never retried
        // (apps/launcher/process.rs), and show nothing.
        for value in [0, 1] {
            rt.clients[LAUNCHER as usize].jobs[job_slot(RADIO)] = Job::default();
            assert_eq!(lane_of(&mut rt, LAUNCHER, EXECUTE, RADIO, value), Some(7), "RADIO {value} by the launcher while locked");
        }
        // The Open Runtime's own work goes on: presents (the lock screen is
        // its), a system update's check, wake locks.
        assert_eq!(lane_of(&mut rt, OPEN_APPS, EXECUTE, PRESENT, 0), Some(5));
        assert_eq!(lane_of(&mut rt, OPEN_APPS, EXECUTE, INSTALL, 0x3141_544f), Some(9));
        assert_eq!(lane_of(&mut rt, OPEN_APPS, EXECUTE, POWER, POWER_LOCK), Some(8));
        // Unlocked: the open apps are still in front, and the screen can be handed back.
        assert_eq!(rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, 0)).value, OPEN_APPS);
        assert!(!rt.locked);
        assert_eq!(ask(&mut rt, OPEN_APPS, EXECUTE, OPEN, LAUNCHER).0, OK);
        assert_eq!(rt.focus, LAUNCHER);
        // Asleep, the focus is left alone (a touch on the screen it was on
        // wakes the phone); the lock is still remembered and still refuses.
        let mut rt = runtime(ALL, LAUNCHER);
        rt.asleep = true;
        assert_eq!(rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, gate::LOCKED)).value, 0);
        assert_eq!((rt.focus, rt.locked), (LAUNCHER, true));
        assert!(denied(&mut rt, LAUNCHER, EXECUTE, OPEN, 11));
        rt.asleep = false;
        assert_eq!(rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, gate::LOCKED)).value, OPEN_APPS);
        // Without the Open Runtime installed nobody can claim its identity's word.
        let mut rt = runtime(ALL & !(1 << OPEN_COMPONENT), LAUNCHER);
        rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, gate::LOCKED));
        assert_eq!((rt.focus, rt.locked), (LAUNCHER, false));
    }

    // ---- the lock at sleep and wake (system/compositor/gate.rs) ----

    fn poll(rt: &mut Runtime, locked: bool) -> u32 {
        rt.dispatch(OPEN_APPS, Message::new(GET, FOCUS, locked as u32)).value
    }
    fn press(rt: &mut Runtime, who: u32) -> Message {
        let mut down = Message::result(0);
        down.key = 1;
        crate::lanes::TOUCH.with(|t| t.set(Some(down)));
        rt.held = false;
        let r = rt.contact(who);
        crate::lanes::TOUCH.with(|t| t.set(None));
        r
    }
    /// What a built-in client in front could still do: draw, sound, a touch.
    fn reachable(rt: &mut Runtime, who: u32) -> bool {
        rt.clients[who as usize].jobs = [Job::default(); 7];
        let present = lane_of(rt, who, EXECUTE, PRESENT, 0) == Some(5);
        rt.clients[who as usize].jobs = [Job::default(); 7];
        let mic = ask(rt, who, EXECUTE, MICROPHONE, 1).1.iter().any(|s| matches!(s, Sent::Call { key: MICROPHONE, value: 1, .. }));
        present || mic || press(rt, who).op == OK
    }

    /// Both wakes (the launcher's POWER 0 and a touch on the built-in screen
    /// in front) put the lock screen in front at once, with no poll between.
    #[test]
    fn a_wake_while_locked_shows_the_lock_screen_at_once() {
        for front in [11, LAUNCHER] {
            for by_touch in [false, true] {
                let mut rt = runtime(ALL, front);
                assert!(reachable(&mut rt, front), "unlocked, {front} in front is usable");
                assert_eq!(poll(&mut rt, true), OPEN_APPS);
                // Unlocking hands nothing back by itself; put `front` in front again.
                assert_eq!(poll(&mut rt, false), OPEN_APPS);
                rt.focus = front;
                // The Open Runtime polled while the phone slept: it is locked.
                assert_eq!(lane_of(&mut rt, LAUNCHER, EXECUTE, POWER, 1), Some(8));
                assert_eq!(poll(&mut rt, false), 0, "the sleep it had not seen");
                assert_eq!(poll(&mut rt, true), 0);
                assert_eq!(rt.focus, front, "asleep, the focus is left where it was");
                sent();
                if by_touch {
                    press(&mut rt, front);
                } else {
                    rt.clients[LAUNCHER as usize].jobs = [Job::default(); 7];
                    assert_eq!(lane_of(&mut rt, LAUNCHER, EXECUTE, POWER, 0), Some(8));
                }
                assert!(!rt.asleep);
                assert_eq!(rt.focus, OPEN_APPS, "woken by touch {by_touch}: the lock screen is in front before any poll");
                if front != LAUNCHER {
                    assert!(!reachable(&mut rt, front), "{front} draws, sounds or is touched behind the lock");
                }
                assert!(denied(&mut rt, LAUNCHER, EXECUTE, OPEN, front));
                assert_eq!(poll(&mut rt, true), OPEN_APPS);
            }
        }
    }

    /// A sleep and wake inside one Open Runtime poll interval: Runtime counts
    /// the phone locked from the sleep, and the Open Runtime is told it slept.
    #[test]
    fn a_sleep_locks_even_if_the_wake_beats_the_next_poll() {
        for by_touch in [false, true] {
            let mut rt = runtime(ALL, 11);
            assert_eq!(poll(&mut rt, false), 11);
            assert_eq!(lane_of(&mut rt, LAUNCHER, EXECUTE, POWER, 1), Some(8));
            assert!(rt.locked, "the sleep itself locks");
            // Asleep and not yet polled: the launcher changes nothing.
            assert!(denied(&mut rt, LAUNCHER, EXECUTE, OPEN, 11));
            assert!(denied(&mut rt, LAUNCHER, EXECUTE, INSTALL, 0));
            // Woken before the Open Runtime polled even once.
            if by_touch {
                press(&mut rt, 11);
            } else {
                rt.clients[LAUNCHER as usize].jobs = [Job::default(); 7];
                lane_of(&mut rt, LAUNCHER, EXECUTE, POWER, 0);
            }
            assert_eq!((rt.asleep, rt.focus, rt.locked), (false, OPEN_APPS, true));
            assert!(!reachable(&mut rt, 11));
            for (key, value) in [(OPEN, 11), (OPEN, LAUNCHER), (INSTALL, 0), (REMOVE, 12), (RELAY, 1)] {
                assert!(denied(&mut rt, LAUNCHER, EXECUTE, key, value), "EXECUTE {key} while the lock is pending");
            }
            // Its first poll still says "unlocked" (chosen before it knew):
            // it is answered 0, so it locks, and Runtime stays locked.
            assert_eq!(poll(&mut rt, false), 0);
            assert!(rt.locked);
            assert!(denied(&mut rt, LAUNCHER, EXECUTE, OPEN, 11));
            // Then it says what it decided. Locked: it is in front.
            assert_eq!(poll(&mut rt, true), OPEN_APPS);
            assert!(rt.locked);
            // No passcode: unlocked, and the open apps stay in front.
            assert_eq!(poll(&mut rt, false), OPEN_APPS);
            assert!(!rt.locked);
        }
        // Without the Open Runtime there is no lock to take.
        let mut rt = runtime(ALL & !(1 << OPEN_COMPONENT), LAUNCHER);
        lane_of(&mut rt, LAUNCHER, EXECUTE, POWER, 1);
        assert!(!rt.locked && !rt.slept);
    }

    /// The lock screen is not drawn while a built-in app's earlier present is
    /// still with Display, so that present cannot land over it.
    #[test]
    fn the_lock_screen_waits_for_a_present_already_with_display() {
        let mut rt = runtime(ALL, 11);
        assert_eq!(lane_of(&mut rt, 11, EXECUTE, PRESENT, 0), Some(5));
        crate::lanes::DONE.with(|d| d.set(false));
        assert_eq!(poll(&mut rt, true), 0, "not in front while 11's present is in Display");
        assert_eq!(rt.focus, OPEN_APPS, "but 11 is already out of front");
        assert!(!reachable(&mut rt, 11));
        rt.clients[11].jobs[job_slot(PRESENT)].lane = 5; // (reachable reset it)
        assert_eq!(poll(&mut rt, true), 0);
        crate::lanes::DONE.with(|d| d.set(true));
        assert_eq!(poll(&mut rt, true), OPEN_APPS, "Display finished it");
        crate::lanes::DONE.with(|d| d.set(false));
        // The result was kept for 11's POLL.
        assert_eq!((rt.clients[11].jobs[job_slot(PRESENT)].lane, rt.clients[11].jobs[job_slot(PRESENT)].result.op), (0, OK));
        // Unlocked, nothing waits for anyone's present.
        let mut rt = runtime(ALL, OPEN_APPS);
        rt.clients[11].jobs[job_slot(PRESENT)].lane = 5;
        assert_eq!(poll(&mut rt, false), OPEN_APPS);
    }

    /// Every order of sleeps, wakes, polls, opens and touches: whenever the
    /// phone is locked and awake, only the open apps are in front, and no
    /// built-in client draws, makes sound or gets a touch.
    #[test]
    fn locked_and_awake_only_the_open_apps_are_in_front() {
        #[derive(Clone, Copy, Debug)]
        enum Step { Sleep, Wake, Poll(bool), Open(u32), Touch, Quit }
        let steps = [Step::Sleep, Step::Wake, Step::Poll(true), Step::Poll(false), Step::Open(11), Step::Open(OPEN_APPS), Step::Touch, Step::Quit];
        let n = steps.len();
        for code in 0..n.pow(5) {
            let mut rt = runtime(ALL, LAUNCHER);
            let mut c = code;
            let mut done = Vec::new();
            for _ in 0..5 {
                let step = steps[c % n];
                c /= n;
                done.push(step);
                rt.clients = [Client::default(); 15];
                match step {
                    Step::Sleep => { rt.dispatch(LAUNCHER, Message::new(EXECUTE, POWER, 1)); }
                    Step::Wake => { rt.dispatch(LAUNCHER, Message::new(EXECUTE, POWER, 0)); }
                    Step::Poll(l) => { poll(&mut rt, l); }
                    Step::Open(t) => { rt.dispatch(LAUNCHER, Message::new(EXECUTE, OPEN, t)); }
                    Step::Touch => { let f = rt.focus; press(&mut rt, f); }
                    Step::Quit => { let f = rt.focus; rt.dispatch(f, Message::new(EXECUTE, QUIT, 0)); }
                }
                if rt.locked && !rt.asleep {
                    assert_eq!(rt.focus, OPEN_APPS, "after {done:?}");
                    for who in APPS {
                        assert!(!reachable(&mut rt, who), "{who} reachable after {done:?}");
                    }
                    assert_eq!(rt.focus, OPEN_APPS);
                }
            }
        }
    }
}
