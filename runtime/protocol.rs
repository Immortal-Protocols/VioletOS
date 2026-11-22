// Stable wire ABI: four control words followed by up to forty bytes of data.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Message {
    pub op: u32,
    pub key: u32,
    pub value: u32,
    pub arg: u32,
    pub data: [u8; 32],
    pub tail: [u8; 8],
}
impl Message {
    pub fn new(op: u32, key: u32, value: u32) -> Self {
        Self {
            op,
            key,
            value,
            ..Self::default()
        }
    }
    pub fn result(value: u32) -> Self {
        Self::new(OK, 0, value)
    }
    pub fn error(code: u32) -> Self {
        Self::new(code, 0, 0)
    }
    pub fn call(mut self, slot: u32) -> Self {
        unsafe { ipc_call(slot, &mut self) };
        self
    }
    pub fn receive(slot: u32) -> (u32, Self) {
        let mut m = Self::default();
        let badge = unsafe { ipc_receive(slot, &mut m) };
        (badge, m)
    }
    pub fn reply(mut self) {
        unsafe { ipc_reply(&mut self) }
    }
    pub fn put_text(&mut self, bytes: &[u8]) {
        for (dest, src) in self.data.iter_mut().chain(&mut self.tail).zip(bytes) {
            *dest = *src;
        }
    }
    pub fn text(&self) -> [u8; 40] {
        let mut bytes = [0; 40];
        bytes[..32].copy_from_slice(&self.data);
        bytes[32..].copy_from_slice(&self.tail);
        bytes
    }
}
extern "C" {
    fn ipc_call(slot: u32, m: &mut Message);
    fn ipc_receive(slot: u32, m: &mut Message) -> u32;
    fn ipc_reply(m: &mut Message);
    pub fn ticks() -> u32;
    pub static ticks_per_ms: u32;
    pub fn delay_ms(ms: u32);
}
pub const SERVER: u32 = 1;
pub const LAUNCHER: u32 = 1;
pub const WATCH: u32 = 2;
pub const SUBSCRIBE: u32 = 1;
pub const GET: u32 = 2;
pub const EXECUTE: u32 = 3;
pub const POLL: u32 = 4;
pub const FORGET: u32 = 5;
pub const OK: u32 = 0;
pub const DENIED: u32 = 1;
pub const INVALID: u32 = 2;
pub const OFFLINE: u32 = 3;
pub const EMPTY: u32 = 4;
pub const BUSY: u32 = 5;
pub const CONTACT: u32 = 1;
pub const STATUS: u32 = 2;
pub const FOCUS: u32 = 3;
pub const OPEN: u32 = 4;
pub const SAVE: u32 = 5;
pub const PRESENT: u32 = 6;
pub const RADIO: u32 = 7;
pub const POWER: u32 = 8;
// GET reply flags. Diagnostic radio status has no on/off control.
// Internal service-to-worker handshake. Apps cannot dispatch this selector.
pub const RADIO_RAILS: u32 = 0x4051;
// Moto Radio -> supervisor. GET inspects, EXECUTE prepares the two fixed windows.
pub const RADIO_MEMORY: u32 = 0x4058;
pub const RADIO_PREPARE_POWER: u32 = 0x4052;
pub const RADIO_RELEASE_POWER:u32=0x405f;
pub const RADIO_RETAIN_POWER:u32=0x4053;
pub const RADIO_DIAGNOSTIC: u32 = 1 << 31;
pub const POWER_MILLIVOLTS: u32 = 1 << 31;
pub const CATALOG: u32 = 9;
pub const QUIT: u32 = 10;
pub fn serve(mut dispatch: impl FnMut(Message) -> Message) -> ! {
    loop {
        let (_, m) = Message::receive(SERVER);
        dispatch(m).reply();
    }
}

pub const INVENTORY: u32 = 11;
pub const SURFACE: u32 = 12;

pub const APP_LAST: u32 = 14;
pub fn is_app(id: u32) -> bool {
    matches!(id, LAUNCHER | WATCH | WIFI..=APP_LAST)
}
pub const INBOX: u32 = 16;
pub const INSTALL: u32 = 17;
pub const REMOVE: u32 = 18;
pub const WHOAMI: u32 = 15;
pub const WIFI: u32 = 10;
pub const NETWORK: u32 = 13;
pub const RADIO_TEST: u32 = 14;

pub const FILE_LIST: u32 = 21;
pub const FILE_READ: u32 = 22;
pub const FILE_WRITE: u32 = 23;
pub const FILE_TRUNCATE: u32 = 24;
pub const FILE_DELETE: u32 = 25;
pub const STORAGE_FILE: u32 = 0x200;
pub fn region(who: u32) -> u32 {
    match who {
        WATCH => 0,
        LAUNCHER => 1,
        WIFI..=APP_LAST => who - 8,
        _ => u32::MAX,
    }
}

pub const SCAN: u32 = 26;

/// Fetch one line of current weather for `CITIES[value]` over HTTPS (wttr.in).
pub const WEATHER: u32 = 28;
/// The built-in Weather app's catalog slot; only it may issue WEATHER.
pub const WEATHER_APP: u32 = 14;
pub const CITIES: [&[u8]; 20] = [
    b"SAN FRANCISCO", b"OAKLAND", b"LOS ANGELES", b"SEATTLE", b"NEW YORK",
    b"CHICAGO", b"AUSTIN", b"MIAMI", b"DENVER", b"BOSTON",
    b"HONOLULU", b"ANCHORAGE", b"MEXICO CITY", b"LONDON", b"PARIS",
    b"BERLIN", b"TOKYO", b"SYDNEY", b"MUMBAI", b"CAIRO",
];
/// Build the wttr.in request path for a city: "/San+Francisco?format=%t;%C&u" + NUL.
pub fn weather_path(city: usize, out: &mut [u8; 64]) -> Option<usize> {
    let name = CITIES.get(city)?;
    let mut n = 0;
    let mut push = |b: u8, n: &mut usize| {
        out[*n] = b;
        *n += 1;
    };
    push(b'/', &mut n);
    let mut word_start = true;
    for &c in name.iter() {
        if c == b' ' {
            push(b'+', &mut n);
            word_start = true;
        } else {
            push(if word_start { c } else { c.to_ascii_lowercase() }, &mut n);
            word_start = false;
        }
    }
    for &c in b"?format=%t;%C&u\0".iter() {
        push(c, &mut n);
    }
    Some(n - 1)
}

pub const PROFILE: u32 = 27;

pub const STORAGE_DIAGNOSTIC: u32 = 0x300;
