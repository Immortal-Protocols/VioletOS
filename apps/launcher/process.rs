//! The boot process (component 1). It has no screen of its own: once the
//! services are up it hands the screen to the Open Runtime, where Home and
//! Settings run. It stays running to restore the saved radio choice in QEMU
//! and to pick up Wi-Fi jobs the Mac sends over USB on the handset (Radio
//! takes those only through here). Phone Internet is restored by the Open
//! Runtime from Settings' saved choice.
use crate::{drawing::*, protocol::*, sdk::*};

pub fn run() -> ! {
    acquire_surface();
    let inventory = request(GET, INVENTORY, 0);
    let open_apps = inventory.op == OK && inventory.value & (1 << OPEN_COMPONENT) != 0;
    if !open_apps {
        // A build without the Open Runtime has nothing to show.
        header(b"NO OPEN RUNTIME", false);
        present(0, 400);
    }
    let radio_events = submit(SUBSCRIBE, RADIO, 0).ok();
    let mut radio_state = Message::error(OFFLINE);
    let mut status = request(GET, STATUS, 0);
    let mut restore_radio = true;
    let mut handed_over = false;
    let mut scan_ticket: Option<Ticket> = None;
    let mut usb_request = 0;
    let mut usb_poll = unsafe { ticks() };
    loop {
        if open_apps && !handed_over {
            handed_over = request(EXECUTE, OPEN, OPEN_APPS).op == OK;
        }
        if status.key != 0 {
            status = request(GET, STATUS, 0);
        }
        if let Some(stream) = &radio_events {
            let r = stream.poll();
            if r.op == OK {
                radio_state = r;
            }
        }
        if let Some(t) = &scan_ticket {
            if t.poll().op != BUSY {
                scan_ticket = None;
            }
        }
        // QEMU's virtual radio remembers whether it was switched off.
        if restore_radio && status.key == 0 && radio_state.op == OK {
            restore_radio = false;
            if status.arg != 0 && radio_state.arg & RADIO_DIAGNOSTIC == 0 {
                let preference = request(GET, SAVE, 0);
                if preference.op == OK {
                    request(EXECUTE, RADIO, (preference.value == 0) as u32);
                }
            }
        }
        // USB requests (join-wifi from the Mac) start a scan on the handset.
        if unsafe { ticks().wrapping_sub(usb_poll) >= ticks_per_ms * 500 } {
            usb_poll = unsafe { ticks() };
            let usb = request(GET, 0x4057, 0);
            if usb.op == OK && usb.key != 0 && usb.key != usb_request && scan_ticket.is_none()
                && radio_state.op == OK && radio_state.arg & RADIO_DIAGNOSTIC != 0
            {
                usb_request = usb.key;
                scan_ticket = submit(EXECUTE, RADIO, 1).ok();
            }
        }
        unsafe {
            delay_ms(20);
        }
    }
}
