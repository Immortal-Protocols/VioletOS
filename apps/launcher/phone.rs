//! Phone Internet: the handset hosts a Wi-Fi network, a phone running Violet Relay
//! joins it, and VioletOS reaches the internet over the phone's cellular data.
//! The pairing code and the on/off choice are saved, so a restart turns it back on.
use crate::{drawing::*, protocol::*, sdk::*};

const FILE: &[u8] = b"phone-relay";

#[derive(Clone, Copy)]
pub struct Pairing {
    pub on: bool,
    pub code: [u8; RELAY_CODE_LEN],
}

/// The saved pairing, or None when there is none yet.
pub fn load() -> Option<Pairing> {
    let r = file(FILE_READ, FILE, 0, &[]);
    let t = r.text();
    if r.op != OK || !valid_relay_code(&t[1..]) {
        return None;
    }
    Some(Pairing { on: t[0] == 1, code: t[1..1 + RELAY_CODE_LEN].try_into().unwrap() })
}

fn store(p: &Pairing) -> bool {
    let mut d = [0u8; 1 + RELAY_CODE_LEN];
    d[0] = p.on as u8;
    d[1..].copy_from_slice(&p.code);
    file(FILE_WRITE, FILE, 0, &d).op == OK
}

/// Ask Radio to host the network for `code`. Hosting takes several seconds.
pub fn start(code: &[u8; RELAY_CODE_LEN]) -> Option<Ticket> {
    submit_text(EXECUTE, RELAY, 1, code).ok()
}

fn new_code() -> Option<[u8; RELAY_CODE_LEN]> {
    let r = request(EXECUTE, RELAY, 2);
    let t = r.text();
    (r.op == OK && valid_relay_code(&t)).then(|| t[..RELAY_CODE_LEN].try_into().unwrap())
}

fn spaced(code: &[u8; RELAY_CODE_LEN]) -> [u8; RELAY_CODE_LEN + 2] {
    let mut out = [b' '; RELAY_CODE_LEN + 2];
    out[..4].copy_from_slice(&code[..4]);
    out[5..9].copy_from_slice(&code[4..8]);
    out[10..].copy_from_slice(&code[8..]);
    out
}

fn status_text(pending: bool, on: bool, state: &Message) -> ([u8; 20], usize) {
    let mut out = [0u8; 20];
    let text: &[u8] = if pending {
        b"STARTING"
    } else if state.op != OK {
        b"CHECKING"
    } else {
        match state.value {
            RELAY_WAITING => b"WAITING FOR PHONE",
            RELAY_JOINED => b"PHONE JOINED",
            RELAY_ONLINE => b"ONLINE VIA PHONE",
            RELAY_STARTING => b"STARTING",
            _ if on && state.arg != 0 => {
                let mut msg = *b"FAILED - CODE 00";
                msg[14] = b'0' + (state.arg / 10 % 10) as u8;
                msg[15] = b'0' + (state.arg % 10) as u8;
                out[..16].copy_from_slice(&msg);
                return (out, 16);
            }
            _ => b"OFF",
        }
    };
    out[..text.len()].copy_from_slice(text);
    (out, text.len())
}

/// The Phone Internet page. Returns when the person taps HOME.
/// `starting` carries a start request Home already made (for example after boot).
pub fn open(starting: &mut Option<Ticket>) {
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap();
    let mut pairing = match load() {
        Some(p) => p,
        None => match new_code() {
            Some(code) => {
                let p = Pairing { on: false, code };
                store(&p);
                p
            }
            None => Pairing { on: false, code: [b'0'; RELAY_CODE_LEN] },
        },
    };
    let mut state = Message::error(BUSY);
    let mut status_ticket: Option<Ticket> = None;
    let mut last_poll = 0u32;
    let mut redraw = true;
    let mut note: &[u8] = b"";
    loop {
        if let Some(t) = starting {
            let r = t.poll();
            if r.op != BUSY {
                *starting = None;
                state = r;
                redraw = true;
            }
        } else if let Some(t) = &status_ticket {
            let r = t.poll();
            if r.op != BUSY {
                status_ticket = None;
                if r.op != state.op || r.value != state.value || r.arg != state.arg {
                    redraw = true;
                }
                state = r;
            }
        } else if unsafe { ticks().wrapping_sub(last_poll) >= ticks_per_ms * 700 } {
            last_poll = unsafe { ticks() };
            status_ticket = submit(GET, RELAY, 0).ok();
        }
        if redraw {
            rect(0, 0, 240, 400, BG);
            label(14, 18, b"NETWORK", 1, ACCENT);
            label(14, 34, b"PHONE INTERNET", 2, INK);
            label(14, 66, b"STATUS", 1, ACCENT);
            let (text, n) = status_text(starting.is_some(), pairing.on, &state);
            label(14, 80, &text[..n], 2, INK);
            label(14, 108, b"PAIRING CODE", 1, ACCENT);
            label(14, 122, &spaced(&pairing.code), 2, INK);
            label(14, 146, b"ENTER IT IN VIOLET RELAY", 1, INK);
            label(14, 157, b"ON YOUR PHONE THEN TAP JOIN", 1, INK);
            label(14, 172, note, 1, ACCENT);
            let on = pairing.on || starting.is_some();
            button(186, if on { b"TURN OFF" } else { b"TURN ON" });
            if !on {
                button(245, b"NEW CODE");
            }
            button(314, b"HOME");
            present(0, 400);
            redraw = false;
        }
        let e = touch.poll();
        if e.op == OK && (42..678).contains(&e.value) {
            match e.arg {
                744..944 if status_ticket.is_none() => {
                    note = b"";
                    if pairing.on || starting.is_some() {
                        pairing.on = false;
                        if let Some(t) = starting.take() {
                            t.wait();
                        }
                        request(EXECUTE, RELAY, 0);
                        state = Message::result(RELAY_OFF);
                    } else {
                        pairing.on = true;
                        *starting = start(&pairing.code);
                        if starting.is_none() {
                            note = b"RADIO IS BUSY - TRY AGAIN";
                            pairing.on = false;
                        }
                    }
                    if !store(&pairing) {
                        note = b"NOT SAVED - DISK UNAVAILABLE";
                    }
                    redraw = true;
                }
                980..1180 if !pairing.on && starting.is_none() && status_ticket.is_none() => {
                    if let Some(code) = new_code() {
                        pairing.code = code;
                        note = if store(&pairing) { b"NEW CODE SAVED" } else { b"NOT SAVED - DISK UNAVAILABLE" };
                    }
                    redraw = true;
                }
                1256..1456 => {
                    if let Some(t) = status_ticket.take() {
                        t.forget();
                    }
                    return;
                }
                _ => {}
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
