use crate::{drawing::*, protocol::*, sdk::*};
pub fn run() -> ! {
    acquire_surface();
    let status = request(GET, STATUS, 0);
    let catalog = request(GET, CATALOG, 0);
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let radio_events = submit(SUBSCRIBE, RADIO, 0).ok();
    let power_events = submit(SUBSCRIBE, POWER, 0).ok();
    // A zero checkpoint is the factory default. One remembers airplane mode.
    let preference = request(GET, SAVE, 0);
    if preference.op == OK && radio_events.is_some() {
        request(EXECUTE, RADIO, (preference.value == 0) as u32);
    }
    let mut visible = false;
    let mut redraw = true;
    loop {
        let active = request(GET, FOCUS, 0).value == LAUNCHER;
        if active {
            for stream in [&radio_events, &power_events].into_iter().flatten() {
                if stream.poll().op == OK {
                    redraw = true;
                }
            }
        }
        if active && (!visible || redraw) {
            header(&status.text(), status.arg != 0);
            label(14, 76, b"VIOLET", 5, INK);
            let radio = request(GET, RADIO, 0);
            label(
                14,
                134,
                if radio.op != OK {
                    b"NO RADIO"
                } else if radio.value != 0 {
                    b"VIRTUAL RADIO ONLINE"
                } else {
                    b"VIRTUAL RADIO OFF"
                },
                1,
                INK,
            );
            let battery = request(GET, POWER, 0).value.min(100);
            let mut percent = *b"BATTERY 000";
            percent[8] += (battery / 100) as u8;
            percent[9] += ((battery / 10) % 10) as u8;
            percent[10] += (battery % 10) as u8;
            label(
                14,
                148,
                if power_events.is_some() {
                    &percent
                } else {
                    b"NO POWER SERVICE"
                },
                1,
                INK,
            );
            if catalog.value != 0 {
                button(206, &catalog.text());
            }
            if radio_events.is_some() {
                button(266, b"RADIO");
            }
            if power_events.is_some() {
                button(335, b"SLEEP");
            }
            present(0, 400);
            redraw = false;
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK && (42..678).contains(&e.value) {
            match e.arg {
                824..1024 if catalog.value != 0 => {
                    request(EXECUTE, OPEN, catalog.value);
                }
                1064..1264 if radio_events.is_some() => {
                    let r = request(GET, RADIO, 0);
                    if request(EXECUTE, RADIO, 1 - r.value).op == OK {
                        request(EXECUTE, SAVE, r.value);
                    }
                    redraw = true;
                }
                1340..1540 if power_events.is_some() => {
                    request(EXECUTE, POWER, 1);
                    visible = false;
                }
                _ => {}
            }
        }
        unsafe {
            delay_ms(8);
        }
    }
}
