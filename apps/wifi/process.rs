//! Wi-Fi setup for networks reported by the board Radio service. Runtime owns the radio endpoint.
use crate::{drawing::*, protocol::*, sdk::*};
pub fn run() -> ! {
    acquire_surface();
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let saved = request(GET, SAVE, 0);
    let mut remember = if saved.op == OK { saved.value } else { 0 };
    let mut index = 0;
    let mut state = Message::error(OFFLINE);
    let mut selected = Message::default();
    let mut pending: Option<Ticket> = None;
    let mut visible = false;
    let mut redraw = true;
    let mut uuid = [0u8; 40];
    let mut result: &[u8] = b"";
    loop {
        if let Some(t) = &pending {
            let r = t.poll();
            if r.op != BUSY {
                pending = None;
                uuid = r.text();
                result = match (r.op, r.value) {
                    (OK, 1) => b"UUID FROM HTTPS",
                    (OK, 2) => b"HTTPS TEST FAILED",
                    (OK, 0) => b"NO GATEWAY REPLY",
                    _ => b"CONNECT FIRST",
                };
                redraw = true;
            }
        }
        let active = request(GET, FOCUS, 0).value == WIFI;
        if active && (!visible || redraw) {
            if !visible && remember != 0 && pending.is_none() {
                let profile = if remember == 1 {
                    request(GET, SCAN, 0).value
                } else {
                    remember
                };
                let restored = request(EXECUTE, PROFILE, profile);
                if restored.op == OK {
                    for i in 0..8 {
                        if request(GET, SCAN, i).value == restored.value {
                            index = i;
                            break;
                        }
                    }
                }
            }
            if pending.is_none() {
                state = request(GET, PROFILE, 0);
                selected = request(GET, SCAN, index);
            }
            rect(0, 0, 240, 400, BG);
            label(14, 18, b"NETWORK", 1, ACCENT);
            label(14, 40, b"WI-FI SETUP", 3, INK);
            label(14, 84, b"NETWORK NAME", 1, ACCENT);
            let title = selected.text();
            label(
                14,
                101,
                if selected.value != 0 {
                    &title
                } else {
                    b"NO NETWORKS"
                },
                2,
                INK,
            );
            label(157, 84, b"NEXT NETWORK", 1, ACCENT);
            label(
                14,
                130,
                if state.op != OK {
                    b"RADIO UNAVAILABLE"
                } else if state.arg == 0 {
                    b"RADIO OFF"
                } else if state.value != 0 && state.value == selected.value {
                    b"CONNECTED"
                } else {
                    b"DISCONNECTED"
                },
                1,
                INK,
            );
            label(
                14,
                141,
                if pending.is_some() {
                    b"TESTING HTTPS"
                } else {
                    result
                },
                1,
                ACCENT,
            );
            label(14, 152, &uuid[..18], 2, INK);
            label(14, 168, &uuid[18..36], 2, INK);
            button(
                186,
                if state.value != 0 && state.value == selected.value {
                    b"DISCONNECT"
                } else {
                    b"CONNECT"
                },
            );
            button(245, b"TEST RADIO");
            button(314, b"HOME");
            present(0, 400);
            redraw = false;
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK && (42..678).contains(&e.value) {
            match e.arg {
                284..400 if pending.is_none() && e.value >= 420 => {
                    index = if request(GET, SCAN, index + 1).value != 0 {
                        index + 1
                    } else {
                        0
                    };
                    uuid.fill(0);
                    result = b"";
                    redraw = true;
                }
                744..944 if pending.is_none() && selected.value != 0 => {
                    uuid.fill(0);
                    let s = request(GET, PROFILE, 0);
                    let r = request(
                        EXECUTE,
                        PROFILE,
                        if s.value != 0 && s.value == selected.value {
                            0
                        } else {
                            selected.value
                        },
                    );
                    result = if r.op != OK {
                        b"CHECK RADIO ON HOME"
                    } else {
                        remember = r.value;
                        if request(EXECUTE, SAVE, r.value).op != OK {
                            b"SESSION ONLY"
                        } else {
                            b""
                        }
                    };
                    redraw = true;
                }
                980..1180 => {
                    rect(14, 140, 223, 44, BG);
                    label(14, 141, b"TESTING HTTPS", 1, ACCENT);
                    present(140, 184);
                    if pending.is_none() {
                        pending = submit(EXECUTE, RADIO_TEST, 0).ok();
                    }
                }
                1256..1456 => {
                    request(EXECUTE, OPEN, LAUNCHER);
                    uuid.fill(0);
                    result = b"";
                }
                _ => {}
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
