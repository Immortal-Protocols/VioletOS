//! Audio service: volume, output routing, playback and the volume buttons.
//! Only Runtime calls it. Requests are served between hardware polls, so a
//! sound keeps playing while apps read or change state.
use crate::{audio_backend as board, protocol::*};
#[path = "synth.rs"]
mod synth;

/// Raw PCM from the Open Runtime (/dev/violet/audio): a header page, then a
/// ring of 48 kHz interleaved stereo s16 frames. Words: 0 magic, 1 ring frames,
/// 2 frames written (Open Runtime), 3 frames played (Audio), 4 underruns.
mod stream {
    pub const HEADER: *mut u32 = 0x0930_0000 as *mut u32;
    pub const RING: *const [i16; 2] = 0x0930_1000 as *const [i16; 2];
    pub const MAGIC: u32 = 0x314d_4350; // "PCM1"
    pub const FRAMES: u32 = 16 * 4096 / 4;
    pub fn reg(i: usize) -> u32 {
        unsafe { HEADER.add(i).read_volatile() }
    }
    pub fn set(i: usize, v: u32) {
        unsafe { HEADER.add(i).write_volatile(v) }
    }
}

extern "C" {
    fn ipc_try_receive(slot: u32, m: &mut Message) -> u32;
}

// Volume buttons: one step per press, then repeats while held.
const DEBOUNCE_MS: u32 = 20;
const REPEAT_DELAY_MS: u32 = 500;
const REPEAT_MS: u32 = 150;
const JACK_DEBOUNCE_MS: u32 = 100;
const CHUNK: usize = 96;

#[derive(Default)]
struct Button {
    raw: bool,
    since: u32,
    pressed: bool,
    next_repeat: u32,
}
impl Button {
    /// Returns true when the press should change the volume.
    fn update(&mut self, raw: bool, now: u32, ms: u32) -> bool {
        if raw != self.raw {
            self.raw = raw;
            self.since = now;
        }
        let stable = now.wrapping_sub(self.since) >= DEBOUNCE_MS * ms;
        if !stable {
            return false;
        }
        if self.raw && !self.pressed {
            self.pressed = true;
            self.next_repeat = now.wrapping_add(REPEAT_DELAY_MS * ms);
            return true;
        }
        if !self.raw {
            self.pressed = false;
            return false;
        }
        if (now.wrapping_sub(self.next_repeat) as i32) >= 0 {
            self.next_repeat = now.wrapping_add(REPEAT_MS * ms);
            return true;
        }
        false
    }
}

struct Audio {
    caps: u32,
    level: u32,
    headphones: bool,
    jack_raw: bool,
    jack_since: u32,
    up: Button,
    down: Button,
    voice: Option<synth::Voice>,
    sound: u32,
    serial: u32,
    outcome: u32,
    output_on: bool,
    /// The Open Runtime's PCM ring is mapped.
    stream: bool,
    /// Developer override over USB: 0 follows the jack, 1 speaker, 2 headphones.
    force: u32,
}
impl Audio {
    /// Where output goes: the jack decides unless a developer request forced it.
    fn to_headphones(&self) -> bool {
        match self.force {
            1 => false,
            2 => true,
            _ => self.headphones,
        }
    }
    fn flags(&self) -> u32 {
        self.caps | if self.headphones { AUDIO_HEADPHONES } else { 0 }
    }
    fn volume(&self) -> Message {
        let mut r = Message::result(self.level);
        r.arg = self.flags();
        let mut text = *b"SPEAKER VOLUME 00   ";
        if self.headphones {
            text = *b"HEADPHONE VOLUME 00 ";
        }
        let at = if self.headphones { 17 } else { 15 };
        text[at] = b'0' + (self.level / 10) as u8;
        text[at + 1] = b'0' + (self.level % 10) as u8;
        r.put_text(&text);
        r
    }
    /// The sound now playing: a built-in one, SOUND_STREAM for open apps' PCM, or 0.
    fn playing(&self) -> u32 {
        if self.voice.is_some() || (self.output_on && self.sound == SOUND_STREAM) { self.sound } else { 0 }
    }
    fn playback(&self) -> Message {
        let mut r = Message::result(self.playing());
        r.arg = (self.serial & 0xffff) | self.outcome;
        r.put_text(match (self.playing() != 0, self.outcome) {
            (true, _) => b"PLAYING" as &[u8],
            (false, PLAYBACK_NO_OUTPUT) => b"NO AUDIO OUTPUT",
            _ => b"IDLE",
        });
        r
    }
    fn wanted_output(&self) -> u32 {
        if self.to_headphones() { AUDIO_HEADPHONE_OUT } else { AUDIO_SPEAKER_OUT }
    }
    fn set_level(&mut self, level: u32) {
        self.level = level.min(VOLUME_MAX);
        board::set_volume(self.level, self.to_headphones());
    }
    fn reroute(&mut self) {
        if self.output_on {
            // Follow the plug. A path that is missing ends the sound.
            if self.caps & self.wanted_output() == 0 || !board::route(self.to_headphones()) {
                self.finish(PLAYBACK_NO_OUTPUT);
            }
        }
        board::set_volume(self.level, self.to_headphones());
    }
    /// Requests from the USB developer link: the command is in the high byte.
    fn developer(&mut self, request: u32) {
        let (command, param) = (request >> 24, request & 0xff_ffff);
        match command {
            1 => { let _ = self.play(param & 0xff, 0); },
            2 => { let _ = self.play(SOUND_TONE, (param & 0x3fff) | ((param >> 14) * 10) << 16); },
            3 => self.set_level(param),
            4 if param <= 2 => {
                self.force = param;
                self.reroute();
            }
            5 => { let _ = self.play(SOUND_STOP, 0); },
            _ => board::debug(command, param),
        }
    }
    fn finish(&mut self, outcome: u32) {
        if self.voice.take().is_some() || self.output_on {
            self.outcome = outcome;
        }
        if self.output_on {
            board::stop();
            self.output_on = false;
        }
    }
    fn play(&mut self, sound: u32, arg: u32) -> Message {
        if sound == SOUND_STOP {
            self.finish(PLAYBACK_STOPPED);
            return self.playback();
        }
        let Some(voice) = synth::Voice::new(sound, arg) else {
            return Message::error(INVALID);
        };
        self.finish(PLAYBACK_STOPPED);
        self.serial = (self.serial + 1) & 0xffff;
        self.sound = sound;
        self.outcome = 0;
        if self.caps & self.wanted_output() == 0 || !board::start(self.to_headphones()) {
            self.outcome = PLAYBACK_NO_OUTPUT;
            return Message::error(OFFLINE);
        }
        self.output_on = true;
        board::set_volume(self.level, self.to_headphones());
        self.voice = Some(voice);
        self.playback()
    }
    fn handle(&mut self, m: Message) -> Message {
        match (m.op, m.key) {
            (GET, VOLUME) => self.volume(),
            (EXECUTE, VOLUME) => {
                match m.value {
                    VOLUME_UP => self.set_level(self.level + 1),
                    VOLUME_DOWN => self.set_level(self.level.saturating_sub(1)),
                    v if v <= VOLUME_MAX => self.set_level(v),
                    _ => return Message::error(INVALID),
                }
                self.volume()
            }
            (GET, PLAYBACK) => self.playback(),
            (EXECUTE, PLAYBACK) => self.play(m.value, m.arg),
            _ => Message::error(DENIED),
        }
    }
    fn poll_inputs(&mut self) {
        let now = unsafe { ticks() };
        let ms = unsafe { ticks_per_ms };
        let input = board::inputs();
        // Pressing both buttons together changes nothing.
        let up = self.up.update(input.up && !input.down, now, ms);
        let down = self.down.update(input.down && !input.up, now, ms);
        if up {
            self.set_level(self.level + 1);
        }
        if down {
            self.set_level(self.level.saturating_sub(1));
        }
        if input.headphones != self.jack_raw {
            self.jack_raw = input.headphones;
            self.jack_since = now;
        }
        if self.jack_raw != self.headphones
            && now.wrapping_sub(self.jack_since) >= JACK_DEBOUNCE_MS * ms
        {
            self.headphones = self.jack_raw;
            self.reroute();
        }
    }
    /// Play queued PCM while no built-in sound is playing. Volume and routing
    /// apply as for every sound; the stream holds the output until it runs dry.
    fn pump_stream(&mut self) {
        if !self.stream || self.voice.is_some() {
            return;
        }
        let (written, read) = (stream::reg(2), stream::reg(3));
        let queued = written.wrapping_sub(read).min(stream::FRAMES);
        if queued == 0 {
            if self.output_on && self.sound == SOUND_STREAM && board::drained() {
                board::stop();
                self.output_on = false;
                self.sound = 0;
            }
            return;
        }
        if !self.output_on {
            if self.caps & self.wanted_output() == 0 || !board::start(self.to_headphones()) {
                // Nowhere to play: drop it rather than stall the writer.
                stream::set(3, written);
                return;
            }
            self.output_on = true;
            self.sound = SOUND_STREAM;
            board::set_volume(self.level, self.to_headphones());
        }
        let gain = synth::GAIN[self.level as usize];
        let mut due = board::writable().min(queued as usize);
        let mut at = read;
        let mut block = [[0i16; 2]; CHUNK];
        while due > 0 {
            let n = due.min(CHUNK);
            for frame in block[..n].iter_mut() {
                let f = unsafe { stream::RING.add((at % stream::FRAMES) as usize).read_volatile() };
                *frame = [(i32::from(f[0]) * gain >> 15) as i16, (i32::from(f[1]) * gain >> 15) as i16];
                at = at.wrapping_add(1);
            }
            board::write(&block[..n]);
            due -= n;
        }
        stream::set(3, at);
    }
    fn pump(&mut self) {
        self.pump_stream();
        let Some(voice) = self.voice.as_mut() else {
            return;
        };
        let gain = synth::GAIN[self.level as usize];
        let mut due = board::writable();
        let mut block = [[0i16; 2]; CHUNK];
        while due > 0 && !voice.done() {
            let n = due.min(CHUNK);
            for frame in block[..n].iter_mut() {
                let s = (voice.next() * gain >> 15) as i16;
                *frame = [s, s];
            }
            board::write(&block[..n]);
            due -= n;
        }
        if voice.done() && board::drained() {
            self.voice = None;
            self.finish(PLAYBACK_PLAYED);
        }
    }
}

pub fn run() -> ! {
    let caps = board::init();
    let input = board::inputs();
    let mut audio = Audio {
        caps,
        level: 8,
        headphones: input.headphones,
        jack_raw: input.headphones,
        jack_since: 0,
        up: Button { raw: input.up, pressed: input.up, ..Button::default() },
        down: Button { raw: input.down, pressed: input.down, ..Button::default() },
        voice: None,
        sound: 0,
        serial: 0,
        outcome: 0,
        output_on: false,
        stream: false,
        force: 0,
    };
    // The supervisor always maps the PCM ring; it stays empty without an Open Runtime.
    audio.stream = true;
    stream::set(1, stream::FRAMES);
    stream::set(3, stream::reg(2));
    stream::set(0, stream::MAGIC);
    board::set_volume(audio.level, audio.headphones);
    board::publish(audio.level, audio.flags(), 0, 0);
    loop {
        let mut m = Message::default();
        if unsafe { ipc_try_receive(SERVER, &mut m) } != 0 {
            audio.handle(m).reply();
        }
        if let Some(request) = board::request() {
            audio.developer(request);
        }
        audio.poll_inputs();
        audio.pump();
        board::publish(
            audio.level,
            audio.flags(),
            audio.serial,
            audio.playing(),
        );
        unsafe { delay_ms(1) };
    }
}
