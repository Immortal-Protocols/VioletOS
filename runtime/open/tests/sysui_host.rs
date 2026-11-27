//! Host tests for the trusted system UI's rules (runtime/open/sysui.rs), run
//! by `make test-unit`: the band, the anti-tapjacking taps, the keypad, the
//! passcode (PBKDF2 known answers, attempts counted before they are checked,
//! the back-off), secure prompts, notifications and the sys command table.
#![allow(dead_code)]
extern crate alloc;

#[path = "../sysui.rs"]
mod sysui;
use sysui::*;

/// A plain SHA-256 for the tests (the image uses the one it links).
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let t2 = s0.wrapping_add((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn pbkdf2(key: &[u8], salt: &[u8], n: u32) -> [u8; 32] {
    let mut j = Pbkdf2::start(sha256, key, salt, n);
    loop {
        if let Some(out) = j.step(sha256, 777) {
            return out;
        }
    }
}

const S: u64 = 1_000_000_000;

#[test]
fn band_rows() {
    assert_eq!(app_rows(0, 1600, false), Some((BAND_H, 1600)));
    assert_eq!(app_rows(0, BAND_H, false), None);
    assert_eq!(app_rows(10, 20, false), None);
    assert_eq!(app_rows(100, 200, false), Some((100, 200)));
    assert_eq!(app_rows(100, 200, true), None);
    assert_eq!(app_rows(0, 1600, true), None);
}

#[test]
fn hmac_and_pbkdf2_known_answers() {
    // RFC 4231 test case 2.
    assert_eq!(hex(&hmac(sha256, b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    // PBKDF2-HMAC-SHA-256 (RFC 7914 section 11 style vectors; checked with Python's hashlib).
    assert_eq!(hex(&pbkdf2(b"password", b"salt", 1)), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
    assert_eq!(hex(&pbkdf2(b"password", b"salt", 2)), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
    assert_eq!(hex(&pbkdf2(b"password", b"salt", 4096)), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
    let salt: Vec<u8> = (0..16).collect();
    assert_eq!(hex(&pbkdf2(b"1234", &salt, ITERATIONS)), "869e6c8350c5beb0acc399fbaac3b60d220433896b26a647734d0d8f1586e1fa");
    assert!(same(&[7; 32], &[7; 32]) && !same(&[7; 32], &[[7; 31].as_slice(), &[8]].concat().try_into().unwrap()));
}

#[test]
fn backoff_schedule() {
    assert_eq!((0..5).map(backoff_secs).collect::<Vec<_>>(), [0, 0, 0, 0, 0]);
    assert_eq!(backoff_secs(5), 30);
    assert_eq!(backoff_secs(6), 60);
    assert_eq!(backoff_secs(7), 120);
    assert_eq!(backoff_secs(11), 1920);
    assert_eq!(backoff_secs(12), 3600);
    assert_eq!(backoff_secs(u32::MAX), 3600);
}

#[test]
fn record_round_trip_and_damage() {
    let r = Record { iterations: ITERATIONS, failures: 3, salt: [9; 16], hash: [4; 32] };
    assert_eq!(Record::decode(&r.encode()), Some(r));
    let mut bad = r.encode();
    bad[0] = b'X';
    assert_eq!(Record::decode(&bad), None);
    assert_eq!(Record::decode(&r.encode()[..59]), None);
    let mut weak = r;
    weak.iterations = 1;
    assert_eq!(Record::decode(&weak.encode()), None, "a record with almost no stretching is not accepted");
    let mut p = Pass::new();
    p.boot(Some(b"garbage"), true, 0);
    assert!(p.locked && p.stored == Stored::Damaged, "a damaged file keeps the phone locked");
    for k in [1, 2, 3, 4, OK] {
        p.key(k, 10 * S, sha256);
    }
    assert!(p.locked && !p.busy(), "nothing can unlock a damaged record");
    let mut none = Pass::new();
    none.boot(None, true, 0);
    assert!(!none.locked && !none.lock(), "without a passcode nothing locks");
}

fn drive(p: &mut Pass, now: u64) -> u8 {
    let mut acts = 0;
    while p.busy() {
        acts |= p.work(sha256, 5000, now);
    }
    acts
}

fn enter(p: &mut Pass, pin: &str, now: u64) -> u8 {
    let mut acts = 0;
    for c in pin.bytes() {
        acts |= p.key(c - b'0', now, sha256);
    }
    acts | p.key(OK, now, sha256)
}

/// Set a passcode through the pad as Settings would start it.
fn set_new(p: &mut Pass, pin: &str) {
    assert!(p.begin(Some([5; 16])));
    assert_eq!(p.step, Some(Step::New));
    enter(p, pin, 0);
    assert_eq!(p.step, Some(Step::Confirm));
    enter(p, pin, 0);
    let acts = drive(p, 0);
    assert_eq!(acts & (SAVE | DONE), SAVE | DONE);
    p.saved();
}

#[test]
fn set_lock_unlock_with_backoff() {
    let mut p = Pass::new();
    p.boot(None, true, 0);
    set_new(&mut p, "2468");
    let Stored::Set(r) = p.stored else { panic!("no record") };
    assert_eq!((r.iterations, r.failures, r.salt), (ITERATIONS, 0, [5; 16]));
    assert_eq!(r.hash, pbkdf2(b"2468", &[5; 16], ITERATIONS));
    let file = r.encode();

    // A restart: locked, and the pad asks.
    let mut p = Pass::new();
    p.boot(Some(&file), true, 0);
    assert!(p.locked && p.step == Some(Step::Unlock));
    assert!(!p.begin(Some([1; 16])), "no passcode change while locked");
    assert_eq!(p.key(CANCEL, 0, sha256), 0, "the lock screen cannot be cancelled");
    // Too short: not an attempt.
    assert_eq!(enter(&mut p, "12", 0) & SAVE, 0);
    assert_eq!(p.note, "AT LEAST 4 DIGITS");
    let mut now = 0;
    for n in 1..=5u32 {
        let acts = enter(&mut p, "1111", now);
        // The attempt is counted (and must be saved) before it is checked.
        assert_eq!(acts & SAVE, SAVE);
        assert!(p.busy());
        let Stored::Set(r) = p.stored else { panic!() };
        assert_eq!(r.failures, n);
        assert_eq!(drive(&mut p, now) & UNLOCKED, 0);
        assert_eq!(p.note, "WRONG PASSCODE");
        now += S;
    }
    // Five wrong: thirty seconds before anything is checked, even the right one.
    assert_eq!(p.not_before, 4 * S + 30 * S);
    assert_eq!(enter(&mut p, "2468", 10 * S), REDRAW);
    assert!(p.locked && !p.busy() && p.note == "WAIT");
    let Stored::Set(r) = p.stored else { panic!() };
    assert_eq!(r.failures, 5, "an attempt refused for the delay is not counted");
    // A restart does not shorten the wait: it starts again.
    let mut q = Pass::new();
    q.boot(Some(&r.encode()), true, 100 * S);
    assert_eq!(q.not_before, 130 * S);
    // After the wait, a sixth wrong one doubles it.
    now = 40 * S;
    enter(&mut p, "9999", now);
    drive(&mut p, now);
    assert_eq!(p.not_before, now + 60 * S);
    now += 61 * S;
    assert_eq!(enter(&mut p, "2468", now) & SAVE, SAVE);
    let acts = drive(&mut p, now);
    assert_eq!(acts & (SAVE | UNLOCKED), SAVE | UNLOCKED);
    assert!(!p.locked && p.step.is_none());
    let Stored::Set(r) = p.stored else { panic!() };
    assert_eq!(r.failures, 0, "the right passcode resets the count");
}

#[test]
fn save_failure_fails_closed() {
    let mut p = Pass::new();
    p.boot(None, true, 0);
    set_new(&mut p, "1357");
    p.lock();
    enter(&mut p, "1357", 0);
    assert!(p.busy());
    // The counter could not be written: the attempt is not checked.
    p.save_failed();
    assert!(!p.busy() && p.locked && p.note.starts_with("CANNOT SAVE"));
    assert_eq!(drive(&mut p, 0), 0);
    // A new passcode that could not be saved is not in force.
    let mut q = Pass::new();
    q.boot(None, true, 0);
    assert!(q.begin(Some([2; 16])));
    enter(&mut q, "4444", 0);
    enter(&mut q, "4444", 0);
    drive(&mut q, 0);
    q.save_failed();
    assert_eq!(q.stored, Stored::None);
}

#[test]
fn change_and_turn_off() {
    let mut p = Pass::new();
    p.boot(None, true, 0);
    assert!(!p.begin(None), "nothing to turn off");
    set_new(&mut p, "1234");
    // Change: the current one first; a mismatch on confirm starts over.
    assert!(p.begin(Some([3; 16])));
    assert_eq!(p.step, Some(Step::Current(true)));
    enter(&mut p, "0000", 0);
    drive(&mut p, 0);
    assert_eq!((p.step, p.note), (Some(Step::Current(true)), "WRONG PASSCODE"));
    enter(&mut p, "1234", 0);
    drive(&mut p, 0);
    assert_eq!(p.step, Some(Step::New));
    enter(&mut p, "5678", 0);
    enter(&mut p, "5679", 0);
    assert_eq!((p.step, p.note), (Some(Step::New), "DID NOT MATCH"));
    enter(&mut p, "5678", 0);
    enter(&mut p, "5678", 0);
    assert_eq!(drive(&mut p, 0) & DONE, DONE);
    let Stored::Set(r) = p.stored else { panic!() };
    assert_eq!(r.hash, pbkdf2(b"5678", &[3; 16], ITERATIONS));
    // Cancel leaves it as it is.
    assert!(p.begin(Some([4; 16])));
    assert_eq!(p.key(CANCEL, 0, sha256) & DONE, DONE);
    assert!(p.step.is_none() && p.stored == Stored::Set(r));
    // Off: the current passcode, then the file goes.
    assert!(p.begin(None));
    enter(&mut p, "5678", 0);
    assert_eq!(drive(&mut p, 0) & (ERASE | DONE), ERASE | DONE);
    assert_eq!(p.stored, Stored::None);
    assert!(!p.lock());
}

#[test]
fn taps_need_arming_lift_and_same_button() {
    // A finger already down when the overlay appears must lift first.
    let mut t = Taps::new(0, true);
    assert_eq!(t.touch(Some(1), true, S), None);
    assert_eq!(t.touch(Some(1), false, S), None, "the press began before the overlay");
    // Too soon after it appeared.
    let mut t = Taps::new(0, false);
    assert_eq!(t.touch(Some(1), true, ARM_NS - 1), None);
    assert_eq!(t.touch(Some(1), false, ARM_NS + 10), None);
    // Armed: down and up on the same button.
    assert_eq!(t.touch(Some(1), true, ARM_NS + 20), None);
    assert_eq!(t.touch(Some(1), false, ARM_NS + 30), Some(1));
    // Sliding to another button cancels.
    t.touch(Some(0), true, S);
    t.touch(Some(1), true, S);
    assert_eq!(t.touch(Some(1), false, S), None);
    // Lifting off any button does nothing.
    t.touch(Some(1), true, 2 * S);
    assert_eq!(t.touch(None, false, 2 * S), None);
}

#[test]
fn keypad_layout() {
    for n in 0..13 {
        let r = pad_rect(n);
        let (cx, cy) = ((r[0] + r[2] / 2) as i32, (r[1] + r[3] / 2) as i32);
        assert_eq!(pad_hit(cx, cy, true), Some(pad_key(n)));
        assert!(r[1] >= BAND_H && r[1] + r[3] <= HEIGHT && r[0] + r[2] <= WIDTH);
    }
    let c = pad_rect(12);
    assert_eq!(pad_hit((c[0] + 5) as i32, (c[1] + 5) as i32, false), None, "no Cancel on the lock screen");
    assert_eq!(pad_hit(10, 10, true), None);
    assert_eq!(prompt_hit(150, (PROMPT_Y + PROMPT_H - 120) as i32), Some(DENY));
    assert_eq!(prompt_hit(500, (PROMPT_Y + PROMPT_H - 120) as i32), Some(ALLOW));
    assert_eq!(prompt_hit(360, 600), None);
}

#[test]
fn consent_rules() {
    let mut c = Consent::default();
    // Not in front, locked, or not a promptable capability: no prompt.
    assert_eq!(c.request(7, "spy", Cap::MIC, false, false), Decision::Deny);
    assert_eq!(c.request(7, "spy", Cap::MIC, true, true), Decision::Deny);
    assert_eq!(c.request(7, "spy", Cap(1 << 20), true, false), Decision::Deny);
    assert!(c.prompt.is_none());
    assert_eq!(c.request(7, "notes", Cap::MIC, true, false), Decision::Prompt);
    assert_eq!(c.request(7, "notes", Cap::MIC, true, false), Decision::Prompt, "still asking");
    assert_eq!(c.request(8, "other", Cap::CAMERA, true, false), Decision::Deny, "one prompt at a time");
    assert_eq!(c.request(7, "notes", Cap::CAMERA, true, false), Decision::Deny);
    c.answer(false);
    assert!(c.prompt.is_none());
    assert_eq!(c.request(7, "notes", Cap::MIC, true, false), Decision::Deny, "a denied app cannot nag");
    assert_eq!(c.decided("notes", Cap::MIC), Some(false));
    assert_eq!(c.request(9, "music", Cap::MIC, true, false), Decision::Prompt);
    assert!(!c.cancel(7) && c.cancel(9));
    assert_eq!(c.decided("music", Cap::MIC), None, "a cancelled prompt keeps no answer");
    c.request(9, "music", Cap::MIC, true, false);
    c.answer(true);
    assert_eq!(c.request(10, "music", Cap::MIC, false, false), Decision::Allow, "kept for the boot");
}

#[test]
fn overlay_routes_touches() {
    let mut ui = Ui::new();
    assert_eq!(ui.mode(), Mode::App);
    assert_eq!(ui.touch(360, 800, true, S, sha256), 0);
    ui.consent.request(3, "probe", Cap::MIC, true, false);
    assert_eq!(ui.mode(), Mode::Prompt);
    ui.arm(0, false);
    let deny = prompt_rect(DENY);
    let (x, y) = ((deny[0] + 10) as i32, (deny[1] + 10) as i32);
    assert_eq!(ui.touch(x, y, true, S, sha256), 0);
    assert_eq!(ui.touch(x, y, false, S, sha256) & DONE, DONE);
    assert_eq!(ui.mode(), Mode::App);
    assert_eq!(ui.consent.decided("probe", Cap::MIC), Some(false));
    // The lock outranks a prompt.
    ui.pass.boot(Some(&Record { iterations: ITERATIONS, failures: 0, salt: [0; 16], hash: [0; 32] }.encode()), true, 0);
    ui.consent.request(4, "probe2", Cap::MIC, true, false);
    assert_eq!(ui.mode(), Mode::Lock);
}

#[test]
fn notifications_are_private_on_the_lock_screen() {
    let mut n = Notes::default();
    assert_eq!(n.summary(), "");
    n.post("Bank", false, "Your code is 123456");
    assert_eq!(n.summary(), "1 NOTIFICATION");
    for i in 0..10 {
        n.post("Chat", true, &format!("hi\n{i}\tthere"));
    }
    assert_eq!(n.list.len(), 8);
    let s = n.summary();
    assert_eq!(s, "8 NOTIFICATIONS");
    assert!(!s.contains("Chat") && !s.contains("123456"));
    assert!(n.lines().lines().all(|l| l.split('\t').count() == 2), "text cannot add fields or lines");
}

#[test]
fn sys_command_table() {
    let app = Caller::default();
    let front = Caller { front: true, ..app };
    let system = Caller { system: true, ..app };
    let opener = Caller { system: true, opener: true, ..app };
    let usb = Caller { usb: true, ..app };
    let settings = Caller { front: true, system: true, settings: true, ..app };
    let home = Caller { front: true, system: true, opener: true, home: true, ..app };
    // Synthetic touches only from the developer's cable, never onto system UI.
    for c in [app, front, system, opener, settings, home] {
        assert!(may("bench", None, &c).is_err());
    }
    assert!(may("bench", None, &usb).is_ok());
    assert!(may("bench", None, &Caller { covered: true, ..usb }).is_err());
    // Focus: the app in front, or Home and the Agent for the user.
    assert!(may("open", None, &app).is_err());
    assert!(may("launch", None, &app).is_err());
    assert!(may("open", None, &front).is_ok());
    assert!(may("open", None, &opener).is_ok());
    assert!(may("open", None, &Caller { locked: true, ..home }).is_err());
    assert!(may("open", None, &Caller { covered: true, ..opener }).is_err());
    assert!(may("home", None, &app).is_err());
    assert!(may("home", None, &front).is_ok());
    assert!(may("apps", None, &system).is_err());
    // The microphone, volume and sounds.
    assert!(may("mic", None, &system).is_err());
    assert!(may("mic", None, &front).is_ok());
    assert!(may("volume", None, &app).is_err() && may("sound", None, &app).is_err());
    assert!(may("volume", None, &system).is_ok() && may("sound", None, &front).is_ok());
    // Shared settings: where the agent connects is not for apps.
    assert!(may("set", Some("time.city"), &app).is_err());
    assert!(may("set", Some("time.city"), &front).is_ok());
    assert!(may("set", Some("relay"), &front).is_err());
    assert!(may("set", Some("relay"), &usb).is_ok() && may("set", Some("stun"), &system).is_ok());
    // The passcode: anyone may ask whether one is set; only Settings in front changes it.
    assert!(may("passcode", None, &app).is_ok());
    assert!(may("passcode", Some("set"), &front).is_err());
    assert!(may("passcode", Some("set"), &Caller { front: false, ..settings }).is_err());
    assert!(may("passcode", Some("off"), &settings).is_ok());
    // Notifications are read by Home only, never while locked.
    assert!(may("notifications", None, &front).is_err());
    assert!(may("notifications", None, &home).is_ok());
    assert!(may("notifications", None, &Caller { locked: true, ..home }).is_err());
    // Everything else is as before.
    for verb in ["status", "get", "notify", "consent", "log", "frames", "wifi", "install"] {
        assert!(may(verb, None, &app).is_ok(), "{verb}");
    }
}

#[test]
fn capability_names() {
    for (n, c) in [("mic", Cap::MIC), ("camera", Cap::CAMERA), ("bluetooth", Cap::HCI), ("net", Cap::NET)] {
        assert_eq!(Cap::named(n), Some(c));
        assert!(c.words().is_some());
    }
    assert_eq!(Cap::named("root"), None);
    // Only policy.rs's CONSENT capabilities (net 0, mic 2, camera 4, hci 5)
    // are ever prompted for: for audio out (1), wifi-admin (6), system (7) or
    // the old "mic-background" (3) policy decides from the manifest alone, so a
    // user's DENY there would be shown but not enforced.
    for bit in [1u32, 3, 6, 7] {
        assert_eq!(Cap(1 << bit).words(), None, "bit {bit}");
        let mut c = Consent::default();
        assert_eq!(c.request(7, "app", Cap(1 << bit), true, false), Decision::Deny);
        assert!(c.prompt.is_none());
    }
    assert_eq!(Cap::named("mic-background"), None);
    // policy.rs's CONSENT, as the bits a prompt may ask (kept in step by hand: policy_host.rs checks policy's side).
    let consent: u32 = [Cap::NET, Cap::MIC, Cap::CAMERA, Cap::HCI].iter().map(|c| c.0).sum();
    assert_eq!(consent, 1 | 1 << 2 | 1 << 4 | 1 << 5);
}

/// Review finding: a prompt raised and answered between two passes of the
/// loop (touches drained before the overlay was drawn) used the previous
/// overlay's armed state, so a down+up on ALLOW counted with no arming.
#[test]
fn a_prompt_counts_no_tap_before_it_is_drawn_and_armed() {
    let mut ui = Ui::new();
    // An earlier overlay (the lock screen) was armed long ago and closed.
    ui.pass.boot(Some(&Record { iterations: 1000, failures: 0, salt: [0; 16], hash: pbkdf2(b"1234", &[0; 16], 1000) }.encode()), true, 0);
    ui.arm(0, false);
    for k in [1, 2, 3, 4, OK] {
        let [x, y, w, h] = pad_rect((0..13).find(|&n| pad_key(n) == k).unwrap());
        let (x, y) = ((x + w / 2) as i32, (y + h / 2) as i32);
        ui.touch(x, y, true, 10 * S, sha256);
        ui.touch(x, y, false, 10 * S, sha256);
    }
    while ui.pass.busy() {
        ui.pass.work(sha256, 5000, 10 * S);
    }
    assert_eq!(ui.mode(), Mode::App, "unlocked");
    // The app raises a prompt; before the loop draws it, a tap lands on ALLOW.
    ui.consent.request(3, "evil", Cap::CAMERA, true, false);
    let allow = prompt_rect(ALLOW);
    let (x, y) = ((allow[0] + 10) as i32, (allow[1] + 10) as i32);
    assert_eq!(ui.touch(x, y, true, 20 * S, sha256), 0);
    assert_eq!(ui.touch(x, y, false, 20 * S + 50_000_000, sha256), 0);
    assert_eq!(ui.consent.decided("evil", Cap::CAMERA), None, "nothing counts before the prompt is shown");
    // Drawn and armed: still nothing until a quiet interval has passed.
    ui.arm(21 * S, false);
    ui.touch(x, y, true, 21 * S + 100_000_000, sha256);
    ui.touch(x, y, false, 21 * S + 150_000_000, sha256);
    assert_eq!(ui.consent.decided("evil", Cap::CAMERA), None);
    let t = 21 * S + 150_000_000 + QUIET_NS;
    ui.touch(x, y, true, t, sha256);
    assert_eq!(ui.touch(x, y, false, t + 50_000_000, sha256) & DONE, DONE);
    assert_eq!(ui.consent.decided("evil", Cap::CAMERA), Some(true));
}

/// Review finding: tapping the ALLOW spot over and over (a "tap fast" game)
/// reached it about 400 ms after the prompt came up.
#[test]
fn a_prompt_needs_a_quiet_interval_not_just_a_delay() {
    let mut t = Taps::prompt(0, false);
    // Three taps a second on ALLOW for ten seconds: none counts.
    let mut now = 0;
    for _ in 0..30 {
        assert_eq!(t.touch(Some(ALLOW), true, now), None);
        assert_eq!(t.touch(Some(ALLOW), false, now + 80_000_000), None, "at {now}");
        now += 333_000_000;
    }
    // The player stops; after a quiet interval a deliberate tap counts.
    now += QUIET_NS;
    t.touch(Some(DENY), true, now);
    assert_eq!(t.touch(Some(DENY), false, now + 60_000_000), Some(DENY));
    // The keypad keeps the plain delay, so a passcode can be typed quickly.
    let mut k = Taps::new(0, false);
    for n in 0..8u64 {
        let at = ARM_NS + n * 150_000_000;
        k.touch(Some(1), true, at);
        assert_eq!(k.touch(Some(1), false, at + 50_000_000), Some(1));
    }
}

/// Review finding: a save that failed during an unlock still counted the
/// attempt, although no passcode was checked.
#[test]
fn a_failed_save_does_not_count_an_attempt() {
    let mut p = Pass::new();
    p.boot(None, true, 0);
    set_new(&mut p, "1357");
    p.lock();
    for _ in 0..6 {
        enter(&mut p, "1357", 0);
        p.save_failed();
    }
    let Stored::Set(r) = p.stored else { panic!() };
    assert_eq!(r.failures, 0, "no check was made, so nothing failed");
    assert_eq!(p.not_before, 0);
    // One real wrong attempt counts once.
    enter(&mut p, "0000", 0);
    p.saved();
    drive(&mut p, 0);
    let Stored::Set(r) = p.stored else { panic!() };
    assert_eq!(r.failures, 1);
}

/// Review finding: a Storage listing error at boot read as "no passcode".
#[test]
fn an_unread_listing_keeps_the_phone_locked() {
    let mut p = Pass::new();
    p.boot(None, false, 0);
    assert!(p.locked && p.stored == Stored::Damaged, "fail closed");
    assert_eq!(p.note, "STORAGE UNREADABLE - RESTART");
    assert!(!p.begin(None) && !p.begin(Some([1; 16])), "and the passcode cannot be changed either");
    for k in [1, 2, 3, 4, OK] {
        p.key(k, S, sha256);
    }
    assert!(p.locked && !p.busy());
    let mut q = Pass::new();
    q.boot(None, true, 0);
    assert!(!q.locked, "every file listed and none is the record: no passcode");
    // data.rs's verdict: one slot unread (a transient error, BUSY, a bad
    // sector) is incomplete; every slot OFFLINE is Storage without a disk.
    assert!(listing_complete(0, 0, 32));
    assert!(!listing_complete(1, 0, 32));
    assert!(!listing_complete(1, 1, 32));
    assert!(!listing_complete(31, 31, 32), "some slots answered: the disk is there");
    assert!(listing_complete(32, 32, 32));
}

/// Review finding: nothing ever locked the phone again after it was unlocked.
#[test]
fn idle_auto_lock() {
    let mut p = Pass::new();
    p.boot(None, true, 0);
    assert!(!p.idle(10 * AUTO_LOCK_NS, 0), "no passcode: never");
    set_new(&mut p, "2468");
    assert!(!p.idle(AUTO_LOCK_NS - 1, 0));
    assert!(p.idle(AUTO_LOCK_NS, 0) && p.locked && p.step == Some(Step::Unlock));
    assert!(!p.idle(3 * AUTO_LOCK_NS, 0), "already locked");
    // In the middle of changing the passcode: idle also locks, and the change ends.
    let mut q = Pass::new();
    q.boot(None, true, 0);
    set_new(&mut q, "2468");
    assert!(q.begin(Some([9; 16])));
    q.key(1, 0, sha256);
    assert!(q.idle(AUTO_LOCK_NS, 0) && q.locked && q.digits.len == 0 && q.salt.is_none());
}

/// Review finding: an in-place execve kept the pid, so the band kept the old
/// title; and installed apps' notifications were not marked.
#[test]
fn band_key_and_marked_notifications() {
    let a = band_key(Mode::App, 9, "/apps/evil", 0);
    assert_ne!(a, band_key(Mode::App, 9, "/bin/python3", 0), "same pid, new program: redraw");
    assert_ne!(a, band_key(Mode::App, 9, "/apps/evil", 1));
    assert_ne!(a, band_key(Mode::Prompt, 9, "/apps/evil", 0));
    assert_eq!(a, band_key(Mode::App, 9, "/apps/evil", 0));
    let mut n = Notes::default();
    n.post("VIOLET-SYSTEM", true, "Passcode expired");
    n.post("SETTINGS", false, "Wi-Fi joined");
    let lines = n.lines();
    assert!(lines.starts_with("INSTALLED VIOLET-SYSTEM\tPasscode expired\n"), "{lines}");
    assert!(lines.ends_with("SETTINGS\tWi-Fi joined\n"));
}
