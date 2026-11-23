//! Sounds: shows the shared volume and output, plays the built-in sounds, and
//! records a few seconds from the microphone to play back. Volume, playback and
//! microphone state arrive through Runtime subscriptions, so the hardware
//! buttons and the headphone jack update this screen as they change.
use crate::{drawing::*, protocol::*, sdk::*};

const MINUS: (usize, usize) = (14, 118);
const PLUS: (usize, usize) = (124, 118);
const PADS: [(usize, usize, &[u8], u32, u32); 4] = [
    (14, 190, b"CHIME", SOUND_CHIME, 0),
    (124, 190, b"ALERT", SOUND_ALERT, 0),
    (14, 240, b"TONE", SOUND_TONE, 440 | 1500 << 16),
    (124, 240, b"STOP", SOUND_STOP, 0),
];
const RECORD: (usize, usize) = (14, 290);
const PLAY: (usize, usize) = (124, 290);
const PAD_W: usize = 102;
const PAD_H: usize = 42;
const HOME_Y: usize = 344;

fn pad(x: usize, y: usize, text: &[u8]) {
    rect(x, y, PAD_W, PAD_H, PANEL);
    label(x + 12, y + 15, text, 2, INK);
}
fn inside(x: usize, y: usize, at: (usize, usize)) -> bool {
    (at.0..at.0 + PAD_W).contains(&x) && (at.1..at.1 + PAD_H).contains(&y)
}
fn sound_name(sound: u32) -> &'static [u8] {
    match sound {
        SOUND_CLICK => b"CLICK",
        SOUND_CHIME => b"CHIME",
        SOUND_ALERT => b"ALERT",
        SOUND_TONE => b"TONE 440 HZ",
        SOUND_RECORDING => b"RECORDING",
        _ => b"",
    }
}

fn draw(volume: &Message, playback: &Message, mic: &Message) {
    rect(0, 0, 240, 400, BG);
    label(14, 14, b"SOUNDS", 1, ACCENT);
    if volume.op != OK {
        label(14, 34, b"NO AUDIO SERVICE", 2, INK);
        button(HOME_Y, b"HOME");
        present(0, 400);
        return;
    }
    let level = volume.value.min(VOLUME_MAX);
    let mut big = *b"VOLUME 00";
    big[7] += (level / 10) as u8;
    big[8] += (level % 10) as u8;
    label(14, 32, &big, 3, INK);
    let flags = volume.arg;
    let output: &[u8] = match (flags & AUDIO_HEADPHONES != 0, flags) {
        (true, f) if f & AUDIO_HEADPHONE_OUT != 0 => b"HEADPHONES",
        (true, _) => b"HEADPHONES - NO OUTPUT",
        (false, f) if f & AUDIO_SPEAKER_OUT != 0 => b"SPEAKER",
        (false, _) => b"SPEAKER - NO OUTPUT",
    };
    label(14, 62, output, 2, INK);
    for i in 0..VOLUME_MAX as usize {
        let colour = if (i as u32) < level { ACCENT } else { PANEL };
        rect(14 + i * 14, 88, 10, 18, colour);
    }
    pad(MINUS.0, MINUS.1, b"-");
    pad(PLUS.0, PLUS.1, b"+");
    let playing = playback.op == OK && playback.value != 0;
    let status: &[u8] = if playing {
        b"PLAYING"
    } else if playback.op != OK {
        b""
    } else {
        match playback.arg & PLAYBACK_OUTCOME {
            PLAYBACK_PLAYED => b"PLAYED",
            PLAYBACK_STOPPED => b"STOPPED",
            PLAYBACK_NO_OUTPUT => b"NO AUDIO OUTPUT",
            _ => b"READY",
        }
    };
    label(14, 168, status, 1, ACCENT);
    if playing {
        label(62, 168, sound_name(playback.value), 1, INK);
    }
    for (x, y, name, _, _) in PADS {
        pad(x, y, name);
    }
    if flags & AUDIO_MIC != 0 {
        draw_mic(mic);
    }
    button(HOME_Y, b"HOME");
    present(0, 400);
}

/// The microphone line and its two pads: RECORD (FINISH while recording) and PLAY.
fn draw_mic(mic: &Message) {
    let (mode, tens) = if mic.op == OK { (mic.arg & MIC_MODE, mic.arg >> 16) } else { (MIC_OFF, 0) };
    let recording = mode == MIC_RECORD;
    let mut text = *b"MIC 0.0 S";
    text[4] = b'0' + (tens / 100 % 10) as u8;
    text[6] = b'0' + (tens / 10 % 10) as u8;
    if tens > 0 || recording {
        label(14, 178, &text, 1, if recording { ACCENT } else { INK });
    }
    // Ten segments, 10 level points each, while the microphone is on.
    if mode != MIC_OFF {
        for i in 0..10 {
            let lit = (i as u32) * 10 < mic.value;
            rect(76 + i * 15, 177, 11, 8, if lit { ACCENT } else { PANEL });
        }
    }
    pad(RECORD.0, RECORD.1, if recording { b"FINISH" } else { b"RECORD" });
    pad(PLAY.0, PLAY.1, b"PLAY");
}

pub fn run() -> ! {
    acquire_surface();
    let me = request(GET, WHOAMI, 0).value;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let volume_events = submit(SUBSCRIBE, VOLUME, 0).ok();
    let playback_events = submit(SUBSCRIBE, PLAYBACK, 0).ok();
    let mic_events = submit(SUBSCRIBE, MICROPHONE, 0).ok();
    let mut volume = Message::error(OFFLINE);
    let mut playback = Message::error(OFFLINE);
    let mut mic = Message::error(OFFLINE);
    let mut visible = false;
    let mut redraw = true;
    loop {
        for (stream, state) in [
            (&volume_events, &mut volume),
            (&playback_events, &mut playback),
            (&mic_events, &mut mic),
        ] {
            if let Some(s) = stream {
                let r = s.poll();
                if r.op == OK {
                    *state = r;
                    redraw = true;
                }
            }
        }
        let active = request(GET, FOCUS, 0).value == me;
        if active && (!visible || redraw) {
            draw(&volume, &playback, &mic);
            redraw = false;
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK && (42..678).contains(&e.value) {
            let (x, y) = (e.value as usize / 3, e.arg as usize / 4);
            if (HOME_Y..HOME_Y + 50).contains(&y) {
                request(EXECUTE, OPEN, LAUNCHER);
            } else if inside(x, y, MINUS) || inside(x, y, PLUS) {
                let step = if inside(x, y, PLUS) { VOLUME_UP } else { VOLUME_DOWN };
                let r = request(EXECUTE, VOLUME, step);
                if r.op == OK {
                    volume = r;
                    // Let the new level be heard.
                    request(EXECUTE, PLAYBACK, SOUND_CLICK);
                    redraw = true;
                }
            } else if volume.arg & AUDIO_MIC != 0 && inside(x, y, RECORD) {
                let recording = mic.op == OK && mic.arg & MIC_MODE == MIC_RECORD;
                let r = request(EXECUTE, MICROPHONE, if recording { MIC_OFF } else { MIC_RECORD });
                if r.op == OK {
                    mic = r;
                    redraw = true;
                }
            } else if volume.arg & AUDIO_MIC != 0 && inside(x, y, PLAY) {
                request(EXECUTE, PLAYBACK, SOUND_RECORDING);
                redraw = true;
            } else if let Some(&(_, _, _, sound, arg)) =
                PADS.iter().find(|p| inside(x, y, (p.0, p.1)))
            {
                request_with(EXECUTE, PLAYBACK, sound, arg);
                redraw = true;
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
