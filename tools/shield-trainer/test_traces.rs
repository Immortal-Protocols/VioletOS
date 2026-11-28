//! The false-positive budget over REAL recorded traces: complete Sentinel
//! records of an emulator run of the shipped image (Home, the notes demo app
//! and the two attack demos; targets/qemu/shield-traces/*/qemu-enforce-*.log,
//! split per process by tools/shield-trainer/collect.py). They are replayed
//! through the real policy and scorer (runtime/open/scorer_replay.rs).
//! Built by tools/shield-trainer/test_shield_data.sh, part of `make test-unit`.
use super::policy::{AccessMode, Action};
use super::scorer_replay::{replay, Origin};
use super::shield::{self, Mode, ShieldPolicy, ShieldVerdict, Subject};

const HOME: &str = include_str!("../../targets/qemu/shield-traces/benign/qemu-enforce-home-1.log");
const NOTES: &str = include_str!("../../targets/qemu/shield-traces/benign/qemu-enforce-notes-1.log");
const PHOTOSYNC: &str = include_str!("../../targets/qemu/shield-traces/attack/qemu-enforce-photosync-1.log");
const THEMES: &str = include_str!("../../targets/qemu/shield-traces/attack/qemu-enforce-themes-1.log");


/// Nothing a real ordinary app did is contained, refused, reported or even
/// warned about: in every mode, as part of the image or as a user's app.
#[test]
fn real_benign_runs_have_no_false_positives() {
    for (name, text, records) in [("home", HOME, 16), ("notes", NOTES, 8)] {
        for mode in [Mode::Monitor, Mode::Enforce, Mode::Strict] {
            for origin in [Origin::Recorded, Origin::Added] {
                let r = replay(text.lines(), mode, origin);
                assert_eq!(r.apps.len(), 1, "{}", name);
                let a = &r.apps[0];
                assert!(a.records == records && r.scored >= 2, "{} was not replayed: {:?} scored {}", name, a, r.scored);
                assert_eq!((a.contained(), a.denied, a.warned, a.shadow), (false, 0, 0, 0), "{} in {:?}/{:?}: {:?}", name, mode, origin, a);
            }
        }
    }
}

/// The false warning this trace exposed. Home reads /apps/.home (the image's
/// list of app titles) every two seconds; the phone used to mark that as a
/// read of another app's files, so Home's small weather request looked like
/// "read someone's data, then sent it": a warning on every boot. The trace
/// keeps the flags as they were recorded; the policy no longer sets them.
#[test]
fn reading_the_images_app_list_is_not_reading_another_apps_data() {
    let recorded = HOME.lines().filter(|l| l.contains("\tOPEN_R\t2\t") && l.contains("/apps/.home")).count();
    assert!(recorded >= 5, "the trace was recorded with the old cross-app mark");
    let home = Subject::derive(100, "/apps/home", "home", [1, 2, 3, 4], true, false);
    let added = Subject::derive(101, "/apps/notes", "notes", [5, 6, 7, 8], false, false);
    for who in [&home, &added] {
        let mut p = ShieldPolicy::with_mode(Mode::Enforce);
        assert_eq!(p.evaluate(1_000, who, &Action::OpenPath { path: "/apps/.home", mode: AccessMode::Read }), ShieldVerdict::Allow);
        let ev = p.ring.drain();
        assert_eq!(ev.len(), 1, "still reported to the companion");
        assert_eq!(ev[0].flags & shield::FLAG_CROSS_APP, 0, "but not as a cross-app read");
        // Another app's program or data is still one.
        p.evaluate(2_000, who, &Action::OpenPath { path: "/data/weather/city", mode: AccessMode::Read });
        p.evaluate(3_000, who, &Action::OpenPath { path: "/apps/settings/main", mode: AccessMode::Read });
        let ev = p.ring.drain();
        assert!(ev.len() == 2 && ev.iter().all(|e| e.flags & shield::FLAG_CROSS_APP != 0), "{:?}", ev.len());
    }
    // Writing the list is still a write to the system's files: refused and marked.
    let mut p = ShieldPolicy::with_mode(Mode::Enforce);
    assert!(matches!(p.evaluate(1_000, &added, &Action::OpenPath { path: "/apps/.home", mode: AccessMode::Write }), ShieldVerdict::Deny { .. }));
    let ev = p.ring.drain();
    assert!(ev.iter().any(|e| e.kind == shield::EventKind::OpenWrite as u8 && e.flags & shield::FLAG_CROSS_APP != 0));
}

/// The two attack demos, from the same run: what the phone does by itself.
#[test]
fn real_attack_runs_are_contained() {
    // photosync reads the notes app's keys and sends 64 KiB from the background.
    let r = replay(PHOTOSYNC.lines(), Mode::Enforce, Origin::Added);
    assert!(r.apps[0].quarantined && r.apps[0].contained_at_ms.unwrap() < 12_600, "{:?}", r.apps);
    let r = replay(PHOTOSYNC.lines(), Mode::Strict, Origin::Recorded);
    assert!(r.apps[0].quarantined, "{:?}", r.apps);
    // As part of the demo image it is reported, not contained; monitoring says what it would do.
    let r = replay(PHOTOSYNC.lines(), Mode::Enforce, Origin::Recorded);
    assert!(!r.apps[0].contained() && r.apps[0].warned > 0, "{:?}", r.apps);
    let r = replay(PHOTOSYNC.lines(), Mode::Monitor, Origin::Added);
    assert!(!r.apps[0].contained() && r.apps[0].shadow > 0, "{:?}", r.apps);
    // themes maps W+X memory and opens Home's program for writing, twice:
    // both writes are refused whoever it is, and a user's app is frozen.
    for origin in [Origin::Recorded, Origin::Added] {
        let r = replay(THEMES.lines(), Mode::Enforce, origin);
        assert_eq!(r.apps[0].denied, 2, "{:?}", r.apps);
        assert_eq!(r.apps[0].frozen, origin == Origin::Added, "{:?}", r.apps);
    }
    let r = replay(THEMES.lines(), Mode::Monitor, Origin::Added);
    assert!(!r.apps[0].contained() && r.apps[0].denied == 0 && r.apps[0].shadow >= 2, "{:?}", r.apps);
}
