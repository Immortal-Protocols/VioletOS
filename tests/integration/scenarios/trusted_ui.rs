//! The trusted system UI: what targets/qemu/test_trusted_ui.py checked with
//! the UI Probe app across two boots, against the shipped sysui.rs (the
//! band, the keypad, the passcode and its back-off, prompts, notifications,
//! who may give which sys command), policy.rs and manifest.rs, with the fake
//! framebuffer, touch controller and clock. The 30 s back-off takes no time.
//!
//! trusted_ui.rs itself (the drawing, at the framebuffer's fixed address)
//! is not host-buildable; open/trusted_ui.rs here stands in for its joins.
use crate::open::sysui::{Mode, BAND_H};
use crate::open::trusted_ui::{ACCENT, BAND_BG, BG, INK, PANEL};
use crate::world::*;

const RED: u32 = 0xffe01020;

fn phone() -> World {
    World::boot(Image::new().app("uiprobe", ""))
}
/// Settings > Passcode > Set passcode, typed twice on the runtime's keypad.
fn set_passcode(w: &mut World, pin: &str) {
    let settings = w.open_app("settings");
    assert_eq!(w.app(settings).sys("passcode set"), "ok");
    w.settle();
    assert_eq!(w.mode(), Mode::Entry);
    w.enter(pin);
    w.enter(pin);
    assert_eq!(w.mode(), Mode::App);
    assert_eq!(w.logged("trusted ui: passcode set"), 1, "{}", w.log());
    assert_eq!(w.app(settings).sys("passcode"), "set=1 busy=0");
    w.front(None);
}
fn band_is_system(w: &World) -> bool {
    w.screen.pixel(2, 2) == ACCENT && w.screen.pixel(717, 29) == ACCENT
}

#[test]
fn the_band_stays_the_runtimes_over_a_full_screen_app() {
    let mut w = phone();
    assert!(!w.locked(), "locked with no passcode set");
    assert_eq!(w.logged("trusted ui: locked"), 0);
    // Home is an app in front: the band is drawn above it.
    assert_eq!(w.screen.pixel(700, 2), BAND_BG);
    let probe = w.open_app("uiprobe");
    for _ in 0..10 {
        w.present(probe, RED);
        assert!(w.screen.pixel(360, BAND_H as usize + 8) == RED && w.screen.pixel(360, 1500) == RED, "the probe's red below the band");
        assert!(!w.screen.any(0, BAND_H as usize, RED), "the app drew on the band");
        assert_eq!(w.screen.pixel(700, 2), BAND_BG);
        w.advance_ms(100);
    }
}

#[test]
fn an_app_gets_its_own_touches_and_none_from_the_band() {
    let mut w = phone();
    let probe = w.open_app("uiprobe");
    assert_eq!(w.tap(360, 1300), Some(probe), "touches reach the app");
    assert_eq!(w.tap(360, 10), None, "a touch on the band reached the app");
    assert_eq!(w.tap(360, BAND_H as i32 - 1), None);
    assert_eq!(w.tap(360, BAND_H as i32), Some(probe));
}

#[test]
fn an_app_may_not_give_the_systems_sys_commands() {
    let mut w = phone();
    let probe = w.open_app("uiprobe");
    for line in ["bench press 360 800 1", "set relay 10.0.0.1:1", "notifications", "passcode off", "passcode set"] {
        let answer = w.app(probe).sys(line);
        assert!(answer.starts_with("error"), "an app was allowed: '{line}' -> {answer:?}");
    }
    // Anyone may ask whether a passcode is set.
    assert_eq!(w.app(probe).sys("passcode"), "set=0 busy=0");
    // Behind Home it may not even go Home or lock.
    w.front(None);
    assert!(w.app(probe).sys("home").starts_with("error"));
    // Home may read the notifications; Settings not in front changes no passcode.
    let home = w.rt.home.unwrap();
    assert_eq!(w.app(home).sys("notifications"), "");
    let settings = w.open_app("settings");
    w.front(None);
    assert_eq!(w.app(settings).sys("passcode set"), "error not allowed");
}

#[test]
fn consent_is_policys_answer_for_an_image_app_and_no_prompt_comes_up() {
    // The probe is from the image and has no manifest: the microphone,
    // Bluetooth and the camera are refused at once. A prompt there would be
    // shown but not enforced.
    let mut w = phone();
    let probe = w.open_app("uiprobe");
    assert_eq!(w.app(probe).sys("consent mic"), "deny");
    assert_eq!(w.app(probe).sys("consent hci"), "deny");
    assert_eq!(w.app(probe).open("/dev/violet/camera", O_RDONLY), Err(EACCES));
    assert_eq!(w.app(probe).sys("consent everything"), "error usage: consent net|mic|camera|hci");
    assert_eq!(w.logged("trusted ui: prompt"), 0);
    assert_eq!(w.mode(), Mode::App);
}

#[test]
fn a_prompt_takes_the_touches_and_the_screen_and_only_the_user_answers_it() {
    let mut w = phone();
    w.install("recorder", "caps: mic\n");
    let app = w.open_app("recorder");
    assert_eq!(w.app(app).sys("consent mic"), "pending");
    w.settle();
    assert_eq!(w.mode(), Mode::Prompt);
    assert_eq!(w.logged("trusted ui: prompt"), 1);
    assert!(band_is_system(&w), "the band says VIOLET SYSTEM");
    // The app presents over everything: the prompt and the band stay.
    w.present(app, RED);
    assert!(band_is_system(&w) && w.screen.pixel(360, 700) == PANEL, "the app drew over the prompt");
    assert!(!w.screen.any(0, crate::kernel::HEIGHT, RED), "nothing of the app's is shown under a prompt");
    // No touch reaches the app while it is up, wherever it lands.
    for (x, y) in [(360, 1300), (360, 200), (210, 965), (360, 10)] {
        w.finger(x, y, true);
        assert_eq!(w.finger(x, y, false), None);
    }
    // A tap that lands the instant the prompt appears answers nothing
    // (anti-tapjacking: sysui.rs `Taps`), and the app cannot answer for the user.
    assert_eq!(w.mode(), Mode::Prompt);
    assert_eq!(w.app(app).sys("consent mic"), "pending");
    assert_eq!(w.app(app).sys("bench press 510 965 1"), "error busy", "no synthetic touch onto system UI");
    // The user allows it.
    w.answer_prompt(true);
    assert_eq!(w.mode(), Mode::App);
    assert_eq!(w.app(app).sys("consent mic"), "allow");
    assert!(w.app(app).open("/dev/violet/mic", O_RDONLY).is_ok());
    // An app that is not in front raises no prompt: deny, and nothing remembered.
    w.install("lurker", "caps: camera\n");
    let lurker = w.open_app("lurker");
    w.front(None);
    assert_eq!(w.app(lurker).open("/dev/violet/camera", O_RDONLY), Err(EACCES));
    assert_eq!(w.mode(), Mode::App);
    w.front(Some(lurker));
    assert_eq!(w.app(lurker).open("/dev/violet/camera", O_RDONLY), Err(EAGAIN), "asked once it is in front");
    // A prompt whose app leaves the screen is withdrawn.
    w.front(None);
    assert_eq!(w.mode(), Mode::App);
}

#[test]
fn a_passcode_locks_the_next_boot_with_a_back_off() {
    // First boot: set 1357 in Settings on the runtime's keypad.
    let mut w = phone();
    let settings = w.open_app("settings");
    assert_eq!(w.app(settings).sys("passcode set"), "ok");
    w.settle();
    assert!(band_is_system(&w) && w.screen.pixel(45, 805) == PANEL, "the passcode pad is not on screen");
    assert_eq!(w.logged("trusted ui: passcode pad"), 1);
    // The pad's digits are not an app's: no touch on it reaches Settings.
    assert_eq!(w.tap(140, 870), None);
    w.key(9); // DEL
    w.enter("1357");
    w.enter("1357");
    assert_eq!(w.logged("trusted ui: passcode set"), 1, "{}", w.log());
    // The record is the runtime's file: no app can read or remove it.
    assert!(w.rt.fs.contents("/data/.lock").is_some());
    assert_eq!(w.app(settings).read_file("/data/.lock"), Err(ENOENT));
    assert_eq!(w.app(settings).unlink("/data/.lock"), Err(ENOENT));

    // Second boot on the same disk: locked from the start.
    let mut w = w.reboot();
    assert!(w.locked());
    assert_eq!(w.logged("trusted ui: locked"), 1);
    assert!(band_is_system(&w) && w.screen.pixel(45, 805) == PANEL && w.screen.pixel(360, 700) == BG, "lock screen drawn");
    // A tap where Home's Settings icon is opens nothing.
    assert_eq!(w.tap(360, 400), None);
    let home = w.rt.home.unwrap();
    assert_eq!(w.app(home).sys("open /apps/settings"), "error locked");
    assert_eq!(w.rt.apps.iter().flatten().count(), 1);
    // Five wrong passcodes, each counted (and saved before it is checked).
    for n in 1..=5 {
        w.enter("0000");
        assert_eq!(w.logged("trusted ui: wrong passcode"), n);
    }
    // During the back-off the right one is not even checked.
    w.enter("1357");
    assert_eq!(w.logged("trusted ui: passcode not checked: waiting"), 1, "{}", w.log());
    assert!(w.locked(), "unlocked during the back-off");
    assert_eq!(w.logged("trusted ui: unlocked"), 0);
    // After it, the right one unlocks.
    w.advance_secs(31);
    w.enter("1357");
    assert!(!w.locked());
    assert_eq!(w.logged("trusted ui: unlocked"), 1);
    assert_eq!(w.screen.pixel(700, 2), BAND_BG);

    // Home's Lock locks the phone at once; the passcode unlocks it.
    assert_eq!(w.app(home).sys("lock"), "ok");
    w.settle();
    assert!(w.locked());
    assert_eq!(w.logged("trusted ui: locked"), 2);
    w.enter("1357");
    assert!(!w.locked());

    // A notification, then the app locks the phone: only a count is shown.
    let probe = w.open_app("uiprobe");
    assert_eq!(w.app(probe).sys("notify secret code 4321"), "ok");
    assert_eq!(w.app(probe).sys("lock"), "ok");
    w.settle();
    assert!(w.locked());
    assert!((200..520).step_by(2).any(|x| (440..462).any(|y| w.screen.pixel(x, y) == INK)), "the lock screen does not show the notification count");
    crate::open::trusted_ui::with(|s| assert_eq!(s.ui.notes.summary(), "1 NOTIFICATION"));
    assert_eq!(w.app(probe).sys("notifications"), "error locked", "its text is not shown or handed out while locked");
    w.enter("1357");
    assert!(!w.locked());
    assert_eq!(w.logged("trusted ui: unlocked"), 3);

    // The failure count survives a restart: wrong tries are not reset by rebooting.
    w.app(home).sys("lock");
    w.settle();
    for _ in 0..5 {
        w.enter("0000");
    }
    let mut w = w.reboot();
    w.enter("1357");
    assert!(w.locked(), "a restart cleared the back-off");
    assert_eq!(w.logged("trusted ui: passcode not checked: waiting"), 1);
}

#[test]
fn a_data_volume_that_cannot_be_read_stays_locked() {
    // Fail closed: a missing /data/.lock proves nothing if Storage could not list its files.
    let (image, mut kept) = phone().shutdown();
    kept.unreadable = true;
    let mut w = World::boot_with(image, kept, 3);
    assert!(w.locked());
    w.enter("0000");
    w.enter("1357");
    assert!(w.locked(), "no passcode opens a phone whose passcode file could not be read");
}

#[test]
fn the_phone_locks_itself_when_left_alone() {
    let mut w = phone();
    set_passcode(&mut w, "2468");
    assert!(!w.locked());
    w.advance_secs(crate::open::sysui::AUTO_LOCK_NS / 1_000_000_000 - 30);
    assert!(!w.locked());
    w.tap(360, 800);
    w.advance_secs(60);
    assert!(!w.locked(), "a touch restarts the count");
    w.advance_secs(crate::open::sysui::AUTO_LOCK_NS / 1_000_000_000);
    assert!(w.locked());
    assert_eq!(w.logged("trusted ui: idle"), 1);
}
