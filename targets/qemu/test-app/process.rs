use crate::{drawing::*, protocol::*, sdk::*};
pub fn run() -> ! {
    acquire_surface();
    let me = request(GET, WHOAMI, 0).value;
    let initial = file(FILE_READ, b"note.txt", 0, &[]);
    let loaded = initial.op == OK && initial.text().starts_with(b"remember this");
    if !loaded {
        assert!(file(FILE_WRITE, b"note.txt", 0, b"remember this").op == OK);
    }
    // Exercise a cross-sector read, truncation, listing and deletion as an SDK client.
    let name = b"scratch";
    for i in 0..22 {
        assert!(file(FILE_WRITE, name, i * 24, &[i as u8; 24]).op == OK);
    }
    let edge = file(FILE_READ, name, 500, &[]);
    assert!(edge.op == OK && edge.value == 28 && edge.text()[0] == 20 && edge.text()[15] == 21);
    assert!(file(FILE_TRUNCATE, name, 501, &[]).value == 501);
    assert!(file(FILE_LIST, b"", 1, &[]).text().starts_with(name));
    assert!(file(FILE_DELETE, name, 0, &[]).op == OK);
    assert!(file(FILE_READ, name, 0, &[]).op == EMPTY);
    let stream = submit(SUBSCRIBE, CONTACT, 0).unwrap();
    let mut visible = false;
    loop {
        let active = request(GET, FOCUS, 0).value == me;
        if active && !visible {
            rect(0, 0, 240, 400, BG);
            label(14, 24, b"NOTEBOOK", 2, INK);
            label(
                14,
                70,
                if loaded {
                    b"LOADED FILE"
                } else {
                    b"SAVED FILE"
                },
                1,
                INK,
            );
            button(300, b"HOME");
            present(0, 400);
        }
        visible = active;
        let event = stream.poll();
        if active && event.op == OK && (1200..1400).contains(&event.arg) {
            request(EXECUTE, OPEN, LAUNCHER);
        }
        unsafe {
            delay_ms(8);
        }
    }
}
