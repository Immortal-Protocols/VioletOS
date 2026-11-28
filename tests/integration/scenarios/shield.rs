//! Violet Shield enforcing on its own: what targets/qemu/test_shield_enforce.py
//! checked with the demo apps (notes, photosync, themes) against a
//! strict-mode image, against the shipped shield.rs joined to the policy
//! and the file system. Every refusal here is the phone's own (shield ids
//! below 16): no companion, no signed command. The recorded traces are
//! replayed by targets/qemu/test_shield_host.sh (make test-unit), and the
//! signed-command path stays in `make test-shield`.
use crate::kernel::clock;
use crate::open::front::Gate;
use crate::open::policy::Action;
use crate::open::shield::{self, Mode, ShieldPolicy};
use crate::world::*;

fn phone(mode: Mode) -> World {
    let mut w = World::boot(Image::new().app("notes", "").app("photosync", "caps: net\n").app("themes", ""));
    w.rt.shield = ShieldPolicy::with_mode(mode);
    w
}
fn home_program(w: &World) -> Vec<u8> {
    w.rt.fs.contents("/apps/home").unwrap().to_vec()
}

#[test]
fn an_ordinary_app_is_not_touched() {
    let mut w = phone(Mode::Strict);
    let notes = w.open_app("notes");
    assert_eq!(w.app(notes).write_file("/data/notes", b"shopping list"), Ok(()));
    assert_eq!(w.app(notes).read_file("/data/notes"), Ok(b"shopping list".to_vec()));
    assert_eq!(w.app(notes).write_file("/tmp/notes.tmp", b"x"), Ok(()));
    assert_eq!(w.app(notes).syscall(248, [0; 6]), 0);
    assert_eq!((w.rt.shield.denied, w.rt.shield.quarantines), (0, 0));
    assert_eq!(w.logged("shield"), 0, "{}", w.log());
}

#[test]
fn an_app_cannot_change_another_apps_program_by_any_door() {
    // themes: /apps/home is refused (shield 1) for a write, for a write and
    // a truncate through a read-only descriptor, and for a read-only open
    // with O_TRUNC.
    let mut w = phone(Mode::Strict);
    let before = home_program(&w);
    let themes = w.open_app("themes");
    let pid = w.app(themes).pid();
    let mut t = w.app(themes);
    assert_eq!(t.open("/apps/home", O_WRONLY), Err(EACCES));
    assert_eq!(t.open("/apps/home", O_RDWR), Err(EACCES));
    assert_eq!(t.open("/apps/home", O_RDONLY | O_TRUNC), Err(EACCES), "a truncating read-only open");
    assert_eq!(t.open("/apps/home", O_WRONLY | O_CREAT | O_TRUNC), Err(EACCES));
    let fd = t.open("/apps/home", O_RDONLY).expect("reading a program is allowed");
    assert_eq!(t.write(fd, b"defaced"), Err(EBADF), "a write through a read-only descriptor");
    assert_eq!(t.ftruncate(fd, 0, false), Err(EBADF), "ftruncate through a read-only descriptor");
    assert_eq!(t.ftruncate(fd, 0, true), Err(EBADF), "ftruncate64 through a read-only descriptor");
    assert_eq!(t.truncate("/apps/home", 0), Err(EACCES), "truncate by path");
    assert_eq!(t.unlink("/apps/home"), Err(EACCES));
    assert_eq!(t.rename("/apps/themes", "/apps/home"), Err(EACCES));
    assert_eq!(t.rename("/apps/home", "/tmp/home"), Err(EACCES));
    assert_eq!(t.mkdir("/apps/home2"), Err(EACCES));
    assert_eq!(t.write_file("/bin/sh", b"x"), Err(EACCES), "shield 2: system files");
    assert_eq!(t.write_file("/data/.shield-mode", b"monitor"), Err(EACCES), "shield 3: its own state");
    assert_eq!(home_program(&w), before, "themes overwrote /apps/home");
    assert!(w.logged(&format!("shield 1: denied (pid {pid})")) >= 6, "{}", w.log());
    assert!(w.rt.shield.denied >= 9);
    // Home still answers, and was never a candidate for containment.
    let home = w.rt.home.unwrap();
    assert!(!w.rt.shield.eligible(&w.rt.subject(home)));
    w.front(None);
    assert_eq!(w.app(home).sys("get city"), "");
}

#[test]
fn an_upload_after_reading_another_apps_data_is_quarantined() {
    // photosync reads notes' data, goes to the background and starts its
    // 4 MiB upload: the phone quarantines it and most of it never leaves.
    let mut w = phone(Mode::Strict);
    let notes = w.open_app("notes");
    w.app(notes).write_file("/data/notes", b"private").unwrap();
    let sync = w.open_app("photosync");
    let pid = w.app(sync).pid();
    // The read is refused by the namespace, and the Shield saw it tried.
    assert_eq!(w.app(sync).read_file("/data/notes"), Err(ENOENT));
    assert!(w.app(sync).socket(2, 1).is_ok());
    assert_eq!(w.rt.gate(sync, &Action::Connect { host: "photos.example.test", port: 443 }), Gate::Allow);
    // It hides behind Home, long enough that no touch vouches for it.
    w.front(None);
    w.advance_secs(30);
    let (mut left, chunk) = (0u32, 32 * 1024);
    for _ in 0..(4 << 20) / chunk {
        let who = w.rt.subject(sync);
        if w.rt.shield.is_quarantined(clock::now_ns(), who.key()) {
            break;
        }
        w.rt.shield.note_sent(clock::now_ns(), &who, chunk);
        left += chunk;
        w.advance_ms(20);
    }
    let who = w.rt.subject(sync);
    assert!(w.rt.shield.is_quarantined(clock::now_ns(), who.key()), "never quarantined after {left} bytes");
    assert!(left <= shield::EXFIL_BURST + 2 * chunk, "{left} bytes left before the quarantine (4 MiB asked)");
    assert_eq!(w.rt.shield.quarantines, 1);
    assert!(w.rt.shield.quarantined_names(clock::now_ns()).contains("photosync"));
    // Cut off: no new connection, no other app's data; its own files still work.
    assert_eq!(w.rt.gate(sync, &Action::Connect { host: "photos.example.test", port: 443 }), Gate::Deny);
    assert_eq!(w.app(sync).read_file("/data/notes"), Err(EACCES), "refused by the Shield now, before the namespace");
    assert_eq!(w.app(sync).write_file("/data/photosync", b"state"), Ok(()));
    assert_eq!(w.logged(&format!("shield {}: denied (pid {pid})", shield::ID_QUARANTINE)), 2, "{}", w.log());
    // The first quarantine is 30 s; then it may connect again.
    w.advance_secs(u64::from(shield::QUARANTINE_SECS[0]) + 2);
    w.rt.shield.expire_quarantines(clock::now_ns());
    assert_eq!(w.rt.gate(sync, &Action::Connect { host: "photos.example.test", port: 443 }), Gate::Allow);
    // Notes and Home were never contained.
    let home = w.rt.home.unwrap();
    for i in [notes, home] {
        let who = w.rt.subject(i);
        assert!(!w.rt.shield.is_quarantined(clock::now_ns(), who.key()));
    }
}

#[test]
fn image_apps_are_contained_only_in_strict_mode_and_nothing_in_monitor() {
    // Why the QEMU test needed a strict image: the demo apps are in the image.
    for (mode, contained) in [(Mode::Strict, true), (Mode::Enforce, false), (Mode::Monitor, false)] {
        let mut w = phone(mode);
        let sync = w.open_app("photosync");
        let _ = w.app(sync).read_file("/data/notes");
        w.front(None);
        w.advance_secs(30);
        for _ in 0..64 {
            let who = w.rt.subject(sync);
            w.rt.shield.note_sent(clock::now_ns(), &who, 64 * 1024);
        }
        let who = w.rt.subject(sync);
        assert_eq!(w.rt.shield.is_quarantined(clock::now_ns(), who.key()), contained, "{}", mode.name());
        // The built-in rules hold for every app in every enforcing mode;
        // monitor only records what it would have done.
        let themes = w.open_app("themes");
        let refused = w.app(themes).open("/apps/home", O_WRONLY);
        if mode == Mode::Monitor {
            assert_eq!(refused, Err(EROFS), "the policy still refuses what the Shield only reports");
            assert!(w.rt.shield.shadow_hits >= 1);
        } else {
            assert_eq!(refused, Err(EACCES));
        }
    }
    // An installed app (not part of the image) is contained in plain enforce mode.
    let mut w = phone(Mode::Enforce);
    w.install("uploader", "caps: net\n");
    let up = w.open_app("uploader");
    let _ = w.app(up).read_file("/data/notes");
    w.front(None);
    w.advance_secs(30);
    for _ in 0..64 {
        let who = w.rt.subject(up);
        w.rt.shield.note_sent(clock::now_ns(), &who, 64 * 1024);
    }
    let who = w.rt.subject(up);
    assert!(w.rt.shield.is_quarantined(clock::now_ns(), who.key()));
}

#[test]
fn the_saved_mode_can_only_make_a_build_stricter() {
    // /data/.shield-mode is not authenticated: it never weakens the build's mode.
    assert_eq!(Mode::at_boot(Mode::Enforce, Some(Mode::Monitor)), Mode::Enforce);
    assert_eq!(Mode::at_boot(Mode::Enforce, Some(Mode::Strict)), Mode::Strict);
    assert_eq!(Mode::at_boot(Mode::Strict, None), Mode::Strict);
}
