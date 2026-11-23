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
    pub fn with_arg(mut self, arg: u32) -> Self {
        self.arg = arg;
        self
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

/// Phone internet: Radio hosts a Wi-Fi network for a phone running Violet Relay.
/// GET: value is RELAY_OFF..=RELAY_ONLINE, arg the last failure code.
/// EXECUTE 1 turns it on with the pairing code in the text (12 characters: the
/// network tag, then the relay password); EXECUTE 0 turns it off; EXECUTE 2 returns
/// a new random pairing code. Only Home (the launcher) may send it.
pub const RELAY: u32 = 29;
pub const RELAY_OFF: u32 = 0;
pub const RELAY_STARTING: u32 = 1;
pub const RELAY_WAITING: u32 = 2;
pub const RELAY_JOINED: u32 = 3;
pub const RELAY_ONLINE: u32 = 4;
pub const RELAY_CODE_LEN: usize = 12;
/// Network name for a pairing code: "VioletRelayOpen-" + its first four characters.
pub fn relay_ssid(code: &[u8], out: &mut [u8; 32]) -> usize {
    let prefix = b"VioletRelayOpen-";
    out[..prefix.len()].copy_from_slice(prefix);
    out[prefix.len()..prefix.len() + 4].copy_from_slice(&code[..4]);
    prefix.len() + 4
}
pub fn valid_relay_code(code: &[u8]) -> bool {
    code.len() >= RELAY_CODE_LEN
        && code[..RELAY_CODE_LEN].iter().all(|c| matches!(c, b'0'..=b'9' | b'A'..=b'F'))
}

pub const STORAGE_DIAGNOSTIC: u32 = 0x300;

// Audio. VOLUME and PLAYBACK can be subscribed to, read with GET and changed
// with EXECUTE. Runtime forwards them to the Audio process, which also owns the
// hardware volume buttons and the headphone jack.
pub const VOLUME: u32 = 30;
pub const PLAYBACK: u32 = 31;
/// Audio is component 22, beside USB Diagnostics, so app and service IDs keep their values.
pub const AUDIO_COMPONENT: u32 = 22;
/// Runtime's capability slot for the Audio endpoint.
pub const AUDIO_SLOT: u32 = 2;
pub const VOLUME_MAX: u32 = 15;
/// EXECUTE VOLUME values beside an absolute level 0..=VOLUME_MAX.
pub const VOLUME_UP: u32 = 0x100;
pub const VOLUME_DOWN: u32 = 0x101;
/// VOLUME replies: value is the level, arg carries these flags.
pub const AUDIO_HEADPHONES: u32 = 1 << 0; // A plug is in the jack; output follows it.
pub const AUDIO_SPEAKER_OUT: u32 = 1 << 1; // The speaker path is available.
pub const AUDIO_HEADPHONE_OUT: u32 = 1 << 2; // The headphone path is available.
pub const AUDIO_BUTTONS: u32 = 1 << 3; // Hardware volume buttons are being read.
pub const AUDIO_JACK: u32 = 1 << 4; // Jack detection is being read.
/// PLAYBACK: EXECUTE value is a sound (SOUND_STOP stops). For SOUND_TONE, arg is
/// frequency in hertz in the low 16 bits and duration in milliseconds in the high 16.
/// Replies carry the sound now playing in value (0 when idle) and, in arg, the
/// serial of the most recent request in the low 16 bits and its outcome above.
pub const SOUND_STOP: u32 = 0;
pub const SOUND_CLICK: u32 = 1;
pub const SOUND_CHIME: u32 = 2;
pub const SOUND_ALERT: u32 = 3;
pub const SOUND_TONE: u32 = 4;
pub const SOUND_LAST: u32 = SOUND_TONE;
/// PLAYBACK's value while an open app's PCM stream (/dev/violet/audio) plays.
pub const SOUND_STREAM: u32 = 5;
pub const PLAYBACK_PLAYED: u32 = 1 << 16; // The last request finished playing.
pub const PLAYBACK_STOPPED: u32 = 2 << 16; // It was stopped or replaced.
pub const PLAYBACK_NO_OUTPUT: u32 = 3 << 16; // No output path could play it.
pub const PLAYBACK_OUTCOME: u32 = 3 << 16;
/// Internal: Audio -> Power, a PMIC register read or masked write on the handset.
pub const PMIC_ACCESS: u32 = 0x4070;

// Open Runtime. It hosts Linux-ABI apps (see runtime/open) and is Runtime's
// client 9, an identity no catalog app uses. The launcher brings it to the
// front with EXECUTE OPEN OPEN_APPS; it draws the screen itself while in front.
pub const OPEN_APPS: u32 = 9;
pub const OPEN_COMPONENT: u32 = 24;
