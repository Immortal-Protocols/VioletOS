//! Audio service: volume, output routing, playback and the volume buttons.
//! Only Runtime calls it. Requests are served between hardware polls, so a
//! sound keeps playing while apps read or change state.
use crate::{audio_backend as board, protocol::*};
#[path = "synth.rs"]
mod synth;
#[path = "music.rs"]
mod music;
static mut PLAYER: music::Music = music::Music::new();
fn music() -> &'static mut music::Music {
    unsafe { &mut *core::ptr::addr_of_mut!(PLAYER) }
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
    fn playback(&self) -> Message {
        let mut r = Message::result(if self.voice.is_some() { self.sound } else { 0 });
        r.arg = (self.serial & 0xffff) | self.outcome;
        r.put_text(match (self.voice.is_some(), self.outcome) {
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
            // Music: 6 plays track (low byte) from seconds (above); 7 sends a MUSIC command.
            6 => { music().command(MUSIC_PLAY, param); },
            7 => { music().command(param & 0xff, param >> 8); },
            _ => board::debug(command, param),
        }
    }
    fn finish(&mut self, outcome: u32) {
        if self.voice.take().is_some() {
            self.outcome = outcome;
        }
        if self.output_on && !music().active() {
            board::stop();
            self.output_on = false;
        }
    }
    /// Start the output if it is off. False when there is no path to play on.
    fn ensure_output(&mut self) -> bool {
        if self.output_on {
            return true;
        }
        if self.caps & self.wanted_output() == 0 || !board::start(self.to_headphones()) {
            return false;
        }
        self.output_on = true;
        board::set_volume(self.level, self.to_headphones());
        true
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
        if !self.ensure_output() {
            self.outcome = PLAYBACK_NO_OUTPUT;
            return Message::error(OFFLINE);
        }
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
            (GET, MUSIC) => music().status(),
            (EXECUTE, MUSIC) => {
                if !music().command(m.value, m.arg) {
                    return Message::error(INVALID);
                }
                music().status()
            }
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
    fn pump(&mut self) {
        let music = music();
        music.poll();
        if music.active() && !self.ensure_output() {
            // No output path: the music cannot play.
            music.command(MUSIC_PAUSE, 0);
        }
        if !self.output_on {
            return;
        }
        let gain = synth::GAIN[self.level as usize];
        let mut due = board::writable();
        let mut block = [[0i16; 2]; CHUNK];
        let voice_left = self.voice.as_ref().is_some_and(|v| !v.done());
        let sounding = voice_left || music.active();
        while due > 0 && sounding {
            let n = due.min(CHUNK);
            for frame in block[..n].iter_mut() {
                let (mut l, mut r) = (0i32, 0i32);
                if let Some(voice) = self.voice.as_mut() {
                    if !voice.done() {
                        let s = voice.next();
                        l += s;
                        r += s;
                    }
                }
                if let Some([a, b]) = music.next() {
                    l += a as i32;
                    r += b as i32;
                }
                let clip = |x: i32| ((x * gain) >> 15).clamp(-32768, 32767) as i16;
                *frame = [clip(l), clip(r)];
            }
            board::write(&block[..n]);
            due -= n;
        }
        // A finished sound is over once its tail has played, or at once while
        // music keeps the output busy.
        if self.voice.as_ref().is_some_and(|v| v.done()) && (music.active() || board::drained()) {
            self.voice = None;
            self.finish(PLAYBACK_PLAYED);
        }
        if self.voice.is_none() && !music.active() && self.output_on && board::drained() {
            board::stop();
            self.output_on = false;
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
        force: 0,
    };
    music().init();
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
        board::music(music().debug_words());
        board::radio(music().radio_words());
        board::publish(
            audio.level,
            audio.flags(),
            audio.serial,
            if audio.voice.is_some() { audio.sound } else { 0 },
        );
        unsafe { delay_ms(1) };
    }
}
