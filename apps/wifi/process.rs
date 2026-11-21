//! Setup for the QEMU lab access point. Runtime owns the radio endpoint.
use crate::{drawing::*, protocol::*, sdk::*};
pub fn run() -> ! {
    acquire_surface();
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let saved = request(GET, SAVE, 0);
    let mut remember = saved.op == OK && saved.value == 1;
    let mut visible = false;
    let mut redraw = true;
    let mut result: &[u8] = b"";
    loop {
        let active = request(GET, FOCUS, 0).value == WIFI;
        if active && (!visible || redraw) {
            if !visible && remember {
                request(EXECUTE, NETWORK, 1);
            }
            let state = request(GET, NETWORK, 0);
            rect(0, 0, 240, 400, BG);
            label(14, 18, b"QEMU NETWORK", 1, ACCENT);
            label(14, 40, b"WI-FI SETUP", 3, INK);
            label(14, 84, b"OPEN LAB NETWORK", 1, ACCENT);
            label(14, 101, b"VIOLET LAB", 2, INK);
            label(
                14,
                130,
                if state.op != OK {
                    b"RADIO UNAVAILABLE"
                } else if state.arg == 0 {
                    b"RADIO OFF"
                } else if state.value == 1 {
                    b"CONNECTED"
                } else {
                    b"DISCONNECTED"
                },
                1,
                INK,
            );
            label(14, 154, result, 1, ACCENT);
            button(
                182,
                if state.value == 1 {
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
                728..928 => {
                    let s = request(GET, NETWORK, 0);
                    let r = request(EXECUTE, NETWORK, (s.value == 0) as u32);
                    result = if r.op != OK {
                        b"CHECK RADIO ON HOME"
                    } else {
                        remember = r.value == 1;
                        if request(EXECUTE, SAVE, r.value).op != OK {
                            b"SESSION ONLY"
                        } else {
                            b""
                        }
                    };
                    redraw = true;
                }
                980..1180 => {
                    let r = request(EXECUTE, RADIO_TEST, 0);
                    result = match (r.op, r.value) {
                        (OK, 1) => b"GATEWAY REPLIED",
                        (OK, 0) => b"NO GATEWAY REPLY",
                        _ => b"CONNECT FIRST",
                    };
                    redraw = true;
                }
                1256..1456 => {
                    request(EXECUTE, OPEN, LAUNCHER);
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
