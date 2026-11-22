//! Weather: tap one of twenty cities to fetch its current conditions over HTTPS.
//! Radio fetches wttr.in, checks the certificate and returns "TEMP;CONDITION".
use crate::{drawing::*, protocol::*, sdk::*};

const GRID_TOP: usize = 118;
const ROW: usize = 22;
const ROWS: usize = 10;
const HOME_Y: usize = 344;

fn split(text: &[u8]) -> (&[u8], &[u8]) {
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    let text = &text[..end];
    match text.iter().position(|&b| b == b';') {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => (text, b""),
    }
}

fn draw(city: Option<usize>, pending: bool, status: &[u8], reply: &[u8; 40]) {
    rect(0, 0, 240, 400, BG);
    label(14, 14, b"WEATHER", 1, ACCENT);
    if let Some(c) = city {
        label(14, 26, CITIES[c], 2, INK);
        let (temp, condition) = split(reply);
        if !pending && !temp.is_empty() {
            label(14, 48, temp, 4, INK);
            // Long conditions use the small font so the line stays on screen.
            if condition.len() <= 18 {
                label(14, 82, condition, 2, INK);
            } else {
                label(14, 86, condition, 1, INK);
            }
        }
    } else {
        label(14, 30, b"CHOOSE A CITY", 2, INK);
    }
    label(14, 104, if pending { b"FETCHING" } else { status }, 1, ACCENT);
    for (i, name) in CITIES.iter().enumerate() {
        let (row, col) = (i % ROWS, i / ROWS);
        let (x, y) = (14 + col * 108, GRID_TOP + row * ROW);
        if city == Some(i) {
            rect(x - 4, y - 5, 106, ROW - 2, PANEL);
        }
        label(x, y + 1, name, 1, if city == Some(i) { ACCENT } else { INK });
    }
    button(HOME_Y, b"HOME");
    present(0, 400);
}

pub fn run() -> ! {
    acquire_surface();
    let me = request(GET, WHOAMI, 0).value;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let mut city: Option<usize> = None;
    let mut pending: Option<Ticket> = None;
    let mut reply = [0u8; 40];
    let mut status: &[u8] = b"TAP A CITY FOR LIVE WEATHER";
    let mut visible = false;
    let mut redraw = true;
    loop {
        if let Some(t) = &pending {
            let r = t.poll();
            if r.op != BUSY {
                pending = None;
                reply = [0; 40];
                status = match (r.op, r.value) {
                    (OK, 1) => {
                        reply = r.text();
                        b"LIVE FROM WTTR.IN"
                    }
                    (OK, _) => b"FETCH FAILED - TAP TO RETRY",
                    _ => b"NO NETWORK - CONNECT WI-FI",
                };
                redraw = true;
            }
        }
        let active = request(GET, FOCUS, 0).value == me;
        if active && (!visible || redraw) {
            draw(city, pending.is_some(), status, &reply);
            redraw = false;
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK && (42..678).contains(&e.value) {
            let (x, y) = (e.value as usize / 3, e.arg as usize / 4);
            if (HOME_Y..HOME_Y + 50).contains(&y) {
                request(EXECUTE, OPEN, LAUNCHER);
            } else if pending.is_none() && (GRID_TOP - 5..GRID_TOP - 5 + ROWS * ROW).contains(&y) {
                let index = (y + 5 - GRID_TOP) / ROW + if x >= 118 { ROWS } else { 0 };
                if index < CITIES.len() {
                    city = Some(index);
                    reply = [0; 40];
                    pending = submit(EXECUTE, WEATHER, index as u32).ok();
                    if pending.is_none() {
                        status = b"NO NETWORK - CONNECT WI-FI";
                    }
                    redraw = true;
                }
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
