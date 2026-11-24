use crate::{drawing::*, protocol::*, sdk::*};
#[path = "library.rs"]
mod library;
#[path = "phone.rs"]
mod phone;
pub fn run() -> ! {
    acquire_surface();
    let mut status = request(GET, STATUS, 0);
    let wifi = request(GET, CATALOG, WIFI);
    let catalog = request(GET, CATALOG, 0);
    let inventory = request(GET, INVENTORY, 0);
    // Linux-ABI apps from /apps run under the Open Runtime.
    let open_apps = inventory.op == OK && inventory.value & (1 << OPEN_COMPONENT) != 0;
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap_or_else(|_| panic!());
    let radio_events = submit(SUBSCRIBE, RADIO, 0).ok();
    let power_events = submit(SUBSCRIBE, POWER, 0).ok();
    let volume_events = submit(SUBSCRIBE, VOLUME, 0).ok();
    let mut volume_state = Message::error(OFFLINE);
    let mut radio_state = Message::error(OFFLINE);
    let mut power_state = Message::error(OFFLINE);
    let mut previous = Message::error(EMPTY);
    let mut restore_radio = true;
    let mut manual_scan = false;
    let mut scan_ticket: Option<Ticket> = None;
    let mut scan_attempted = false;
    // A Phone Internet start still in progress (from boot or from the Phone page).
    let mut relay_start: Option<Ticket> = None;
    let mut usb_stage = (0,0);
    let mut usb_request=0;
    let mut usb_poll = unsafe { ticks() };
    let mut visible = false;
    let mut redraw = true;
    loop {
        let active = request(GET, FOCUS, 0).value == LAUNCHER;
        if status.key != 0 {
            let next = request(GET, STATUS, 0);
            if next.key != status.key || next.arg != status.arg || next.text() != status.text() {
                status = next;
                redraw = true;
            }
        }
        if let Some(ticket) = &scan_ticket {
            if ticket.poll().op != BUSY {
                scan_ticket = None;
                radio_state = request(GET, RADIO, 0);
                if radio_state.op == OK {
                    file(FILE_WRITE, b"radio-probe", 0, &radio_state.text()[..24]);
                    previous = file(FILE_READ, b"radio-probe", 0, &[]);
                }
                redraw = true;
            }
        }
        if let Some(t) = &relay_start {
            if t.poll().op != BUSY {
                relay_start = None;
                radio_state = request(GET, RADIO, 0);
                redraw = true;
            }
        }
        if active {
            if let Some(stream) = &radio_events {
                let r = stream.poll();
                if r.op == OK {
                    radio_state = r;
                    redraw = true;
                }
            }
            if let Some(stream) = &volume_events {
                let r = stream.poll();
                if r.op == OK {
                    volume_state = r;
                    redraw = true;
                }
            }
            if let Some(stream) = &power_events {
                let r = stream.poll();
                if r.op == OK {
                    power_state = r;
                    redraw = true;
                }
            }
        }
        if active && unsafe { ticks().wrapping_sub(usb_poll) >= ticks_per_ms*500 } {
            usb_poll=unsafe {ticks()};
            let usb=request(GET,0x4057,0);
            if usb.op==OK && usb.key!=0 && usb.key!=usb_request && scan_ticket.is_none() && relay_start.is_none() {
                usb_request=usb.key;scan_attempted=false;manual_scan=true;
            }
            if (usb.value,usb.arg)!=usb_stage {usb_stage=(usb.value,usb.arg);redraw=true;}
        }
        if active && (!visible || redraw) {
            header(&status.text(), status.arg != 0);
            label(14, 62, b"VIOLET", 4, INK);
            if volume_state.op == OK {
                // The side buttons work on every screen; Home shows where sound goes.
                let mut text = *b"VOL 00 SPK";
                text[4] += (volume_state.value.min(99) / 10) as u8;
                text[5] += (volume_state.value % 10) as u8;
                if volume_state.arg & AUDIO_HEADPHONES != 0 {
                    text[7..].copy_from_slice(b"HP ");
                }
                label(166, 20, &text, 1, ACCENT);
            }
            rect(157, 53, 70, 32, PANEL);
            label(163, 65, b"APPS", 2, INK);
            if open_apps {
                rect(157, 89, 70, 26, PANEL);
                label(163, 99, b"/APPS", 1, ACCENT);
            }
            let radio = radio_state;
            let radio_text = radio.text();
            label(
                14,
                104,
                if radio.op != OK {
                    b"NO RADIO"
                } else if radio.arg & RADIO_DIAGNOSTIC != 0 {
                    &radio_text
                } else if radio.value != 0 {
                    b"VIRTUAL RADIO ONLINE"
                } else {
                    b"VIRTUAL RADIO OFF"
                },
                1,
                INK,
            );

            if radio.arg & RADIO_DIAGNOSTIC != 0 && previous.op == OK {
                label(14, 129, b"LAST:", 1, INK);
                label(46, 129, &previous.text(), 1, INK);
            }
            let battery = power_state.value.min(100);
            let power_text = power_state.text();
            let mut percent = *b"BATTERY 000";
            percent[8] += (battery / 100) as u8;
            percent[9] += ((battery / 10) % 10) as u8;
            percent[10] += (battery % 10) as u8;
            label(
                14,
                116,
                if power_state.arg & POWER_MILLIVOLTS != 0 {
                    &power_text
                } else if power_state.op == OK {
                    &percent
                } else if power_events.is_some() {
                    b"READING BATTERY"
                } else {
                    b"NO POWER SERVICE"
                },
                1,
                INK,
            );
            if wifi.value == WIFI {
                button(146, &wifi.text());
            }
            if catalog.value != 0 {
                button(206, &catalog.text());
            }
            // The handset radio offers Phone Internet; the virtual radio has an on/off switch.
            if radio_state.op == OK && radio_state.arg & RADIO_DIAGNOSTIC != 0 {
                button(266, b"PHONE INTERNET");
            } else if radio_state.op == OK && !scan_attempted {
                button(266, b"RADIO");
            }
            if power_events.is_some() {
                button(
                    335,
                    if power_state.arg & POWER_MILLIVOLTS != 0 {
                        b"SCREEN OFF"
                    } else {
                        b"SLEEP"
                    },
                );
            }
            if usb_stage.0!=0 {
                let mut line=*b"USB 0 LINK 00";
                line[4]=b'0'+usb_stage.0.min(9) as u8;
                let hex=b"0123456789ABCDEF";
                line[11]=hex[((usb_stage.1>>4)&15) as usize];
                line[12]=hex[(usb_stage.1&15) as usize];
                label(14,390,&line,1,INK);
            }
            present(0, 400);
            redraw = false;
        }
        if active && manual_scan && radio_state.op == OK
            && radio_state.arg & RADIO_DIAGNOSTIC != 0 && !scan_attempted
        {
            if let Ok(ticket) = submit(EXECUTE, RADIO, 1) {
                scan_ticket = Some(ticket);
                scan_attempted = true;
                previous = Message::result(0);
                previous.put_text(b"WIFI SCAN STARTED");
                redraw = true;
            }
            manual_scan = false;
        }
        // Show Home before reading optional preferences or the previous scan.
        if restore_radio && active && status.key == 0 && radio_state.op == OK {
            restore_radio = false;
            if status.arg != 0 {
                if radio_state.arg & RADIO_DIAGNOSTIC != 0 {
                    previous = file(FILE_READ, b"radio-probe", 0, &[]);
                    // Phone Internet was on before the restart: turn it back on.
                    if let Some(p) = phone::load().filter(|p| p.on) {
                        relay_start = phone::start(&p.code);
                    }
                } else {
                    let preference = request(GET, SAVE, 0);
                    if preference.op == OK { request(EXECUTE, RADIO, (preference.value == 0) as u32); }
                }
                redraw = true;
            }
        }
        visible = active;
        let e = touch.poll();
        if active && e.op == OK && (42..678).contains(&e.value) {
            match e.arg {
                212..340 if e.value >= 471 => {
                    library::open();
                    redraw = true;
                }
                356..460 if e.value >= 471 && open_apps => {
                    request(EXECUTE, OPEN, OPEN_APPS);
                }
                584..784 if wifi.value == WIFI => {
                    request(EXECUTE, OPEN, WIFI);
                }
                824..1024 if catalog.value != 0 => {
                    request(EXECUTE, OPEN, catalog.value);
                }
                1064..1264 if radio_state.op == OK && radio_state.arg & RADIO_DIAGNOSTIC != 0 => {
                    if scan_ticket.is_none() {
                        phone::open(&mut relay_start);
                    }
                    redraw = true;
                }
                1064..1264 if radio_state.op == OK => {
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
