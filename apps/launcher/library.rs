//! Installed app pages. Read titles and identities from the active catalog.
use crate::{drawing::*, protocol::*, sdk::*};

pub fn open() {
    let touch = submit(SUBSCRIBE, CONTACT, 0).unwrap();
    let ids = [WATCH, WIFI, 11, 12, 13, 14, 1, 3, 4, 5, 6, 7, 8];
    let mut selection = 0;
    let mut confirm = false;
    let mut status: &[u8] = b"";
    loop {
        let item = request(GET, CATALOG, ids[selection]);
        let inbox = request(GET, INBOX, 0);
        rect(0, 0, 240, 400, BG);
        label(14, 15, b"APPLICATIONS", 2, INK);
        label(14, 48, b"INSTALLED", 1, ACCENT);
        let title = item.text();
        label(
            14,
            62,
            if item.value == 0 {
                b"EMPTY SLOT"
            } else {
                &title
            },
            2,
            INK,
        );
        button(86, b"NEXT");
        if is_app(item.value) {
            button(139, b"OPEN");
        }
        if matches!(item.value,2|6..=8|10..=14) {
            button(192, b"UNINSTALL");
        }
        if inbox.value != 0 {
            label(14, 248, &inbox.text(), 1, ACCENT);
            button(264, b"INSTALL PACKAGE");
        }
        label(14, 320, status, 1, INK);
        button(339, b"HOME");
        rect(120, 339, 106, 50, PANEL);
        label(125, 357, if confirm { b"CONFIRM" } else { b"OFF" }, 2, INK);
        present(0, 400);
        loop {
            let event = touch.poll();
            if event.op == OK && (42..678).contains(&event.value) {
                match event.arg / 4 {
                    86..136 => {
                        // Skip slots with nothing installed so every page shows a component.
                        for _ in 0..ids.len() {
                            selection = (selection + 1) % ids.len();
                            if request(GET, CATALOG, ids[selection]).value != 0 {
                                break;
                            }
                        }
                        break;
                    }
                    139..189 if is_app(item.value) => {
                        request(EXECUTE, OPEN, item.value);
                        return;
                    }
                    192..242 if matches!(item.value,2|6..=8|10..=14) => {
                        request(EXECUTE, REMOVE, item.value);
                        status = b"REMOVE FAILED";
                        break;
                    }
                    264..314 if inbox.value != 0 => {
                        request(EXECUTE, INSTALL, inbox.value);
                        status = b"INSTALL FAILED";
                        break;
                    }
                    339..389 if event.value >= 360 => {
                        if confirm {
                            request(EXECUTE, POWER, 2);
                        }
                        confirm = true;
                        status = b"CONFIRM POWER OFF";
                        break;
                    }
                    339..389 => return,
                    _ => {}
                }
            }
            unsafe {
                delay_ms(8);
            }
        }
    }
}
