//! Host tests for the Open Runtime's fault containment (contain.rs) and the
//! process manager's frame ledger (system/supervisor/frames.rs); part of
//! `make test-unit`. Both are plain data with no kernel calls. The ledger
//! inside the real process manager, against stand-in hardware, is tested in
//! memprot_host.rs ("the frame ledger"); the supervisor's side of containment
//! (how often it restarts the runtime) in system/supervisor/test_open_recover.c.
//!
//! Every test makes its own state: they run in parallel.
#![allow(dead_code)]
extern crate alloc;

#[path = "../contain.rs"]
mod contain;
#[path = "../../../system/supervisor/frames.rs"]
mod frames;

use contain::{verdict, Entry, State, Verdict};
use frames::{Frames, Refused, OWNERS};

// ---- contain.rs ----

/// The runtime as containment sees it: an address, which slots hold apps,
/// Home and the app in front. It implements contain::Host, so `restart`
/// runs the shipped contain::recover, as mod.rs `recover` does.
struct Model {
    state: State,
    apps: [bool; 12],
    closed: Vec<usize>,
    reclaimed: Vec<usize>,
    revoked: Vec<usize>,
    home: Option<usize>,
    front: Option<usize>,
    home_tried: bool,
    homes_started: usize,
    shown: usize,
}
const RUNTIME: usize = 0x2000_4000;

impl contain::Host for Model {
    fn slots(&self) -> usize {
        self.apps.len()
    }
    fn present(&self, i: usize) -> bool {
        self.apps[i]
    }
    /// As mod.rs `reap` does when it runs to the end.
    fn close(&mut self, i: usize) {
        self.apps[i] = false;
        self.closed.push(i);
        if self.home == Some(i) {
            self.home = None;
        }
        if self.front == Some(i) {
            self.front = None;
            self.show_home();
        }
    }
    fn revoke(&mut self, i: usize) {
        self.revoked.push(i);
    }
    fn home(&mut self) -> &mut Option<usize> {
        &mut self.home
    }
    fn front(&mut self) -> &mut Option<usize> {
        &mut self.front
    }
    fn home_tried(&self) -> bool {
        self.home_tried
    }
    /// As device.rs `start_home_app`: Home into the first free slot, in front.
    fn start_home(&mut self) {
        let i = (0..self.apps.len()).find(|&i| !self.apps[i]).unwrap();
        self.apps[i] = true;
        self.home = Some(i);
        self.front = Some(i);
        self.homes_started += 1;
    }
    /// As device.rs `show_home`.
    fn show_home(&mut self) {
        self.shown += 1;
        if let Some(h) = self.home.filter(|&h| self.apps[h]) {
            self.front = Some(h);
        }
    }
}

impl Model {
    fn booted(apps: &[usize]) -> Model {
        let mut m = Model {
            state: State::new(),
            apps: [false; 12],
            closed: vec![],
            reclaimed: vec![],
            revoked: vec![],
            home: None,
            front: None,
            home_tried: false,
            homes_started: 0,
            shown: 0,
        };
        assert_eq!(m.state.enter(), Entry::Cold);
        m.state.settle(RUNTIME);
        for &a in apps {
            m.apps[a] = true;
        }
        m
    }
    /// Home in slot `h`, in front, as after the first focus.
    fn with_home(mut self, h: usize) -> Model {
        self.apps[h] = true;
        self.home = Some(h);
        self.front = Some(h);
        self.home_tried = true;
        self
    }
    /// The supervisor restarted the thread at its entry: what `run` does.
    fn restart(&mut self) -> Verdict {
        let Entry::Warm { runtime, served } = self.state.enter() else { panic!("not a warm start") };
        assert_eq!(runtime, RUNTIME, "the runtime is found where boot left it");
        self.revoked.clear();
        let v = contain::recover(self, served);
        if let Verdict::Reclaim(i) = v {
            self.reclaimed.push(i);
        }
        v
    }
    fn running(&self) -> Vec<usize> {
        (0..self.apps.len()).filter(|&i| self.apps[i]).collect()
    }
}

#[test]
fn a_fault_while_serving_an_app_closes_that_app_only() {
    let mut m = Model::booted(&[0, 3, 5]);
    m.state.serve(3);
    // ... the runtime faults in app 3's system call ...
    assert_eq!(m.restart(), Verdict::Close(3));
    assert_eq!(m.running(), vec![0, 5], "Home and the other app go on");
    assert_eq!(m.state.recovered(), 1);
}

#[test]
fn a_fault_outside_any_call_is_not_contained() {
    let mut m = Model::booted(&[0, 3]);
    m.state.serve(3);
    m.state.done();
    // ... the fault comes in the runtime's own housekeeping (poll, Shield,
    // lifecycle ticks): no app can be blamed, so `run` halts on Nothing, and
    // recover touched nothing it could have half done.
    assert_eq!(m.restart(), Verdict::Nothing);
    assert_eq!(m.running(), vec![0, 3]);
    assert!(m.revoked.is_empty() && m.closed.is_empty() && m.homes_started == 0);
}

#[test]
fn a_fault_in_the_middle_of_reap_leaves_no_stale_home_or_front() {
    // Home in slot 0 is being reaped (low memory): reap took it out of the
    // table and faulted before it cleared `home` and `foreground`.
    let mut m = Model::booted(&[4]).with_home(0);
    m.state.serve(0);
    m.apps[0] = false;
    assert_eq!(m.restart(), Verdict::Reclaim(0));
    assert!(m.revoked.contains(&0), "what was left of it is revoked");
    // Home starts again, as a new app; the old slot is not taken for Home
    // by whatever launches next.
    assert_eq!(m.homes_started, 1);
    assert_eq!(m.home, m.front);
    assert!(m.home.is_some_and(|h| m.apps[h]), "home names a running app");
    assert_eq!(m.running().len(), 2);
}

#[test]
fn a_stale_front_with_home_alive_shows_home() {
    let mut m = Model::booted(&[]).with_home(0);
    m.apps[5] = true;
    m.front = Some(5);
    m.state.serve(5);
    m.apps[5] = false; // caught mid-reap
    assert_eq!(m.restart(), Verdict::Reclaim(5));
    assert_eq!(m.front, Some(0), "Home comes to the front");
    assert_eq!(m.home, Some(0));
    assert_eq!(m.homes_started, 0, "Home is still running: not started again");
}

#[test]
fn closing_home_restarts_home_once_and_nothing_else() {
    // The agent runs behind Home in slot 2. Home's call faults.
    let mut m = Model::booted(&[2]).with_home(0);
    m.state.serve(0);
    assert_eq!(m.restart(), Verdict::Close(0));
    assert_eq!(m.homes_started, 1);
    assert_eq!(m.running().len(), 2, "Home again and the one agent: no second agent");
    // A second fault in Home's call: again one Home, still two apps.
    m.state.serve(m.home.unwrap());
    assert!(matches!(m.restart(), Verdict::Close(_)));
    assert_eq!(m.homes_started, 2);
    assert_eq!(m.running().len(), 2);
}

#[test]
fn home_is_not_started_before_it_ever_was() {
    // Before the open apps first came to the front, a contained fault does
    // not start Home (it starts on first focus).
    let mut m = Model::booted(&[3]);
    m.state.serve(3);
    assert_eq!(m.restart(), Verdict::Close(3));
    assert_eq!(m.homes_started, 0);
    assert_eq!(m.revoked, (0..12).collect::<Vec<_>>(), "every empty slot is revoked");
}

#[test]
fn a_second_fault_while_closing_does_not_close_again() {
    let m = Model::booted(&[0, 3]);
    m.state.serve(3);
    // The first warm start took the record; suppose closing app 3 itself faults.
    let Entry::Warm { served, .. } = m.state.enter() else { panic!() };
    assert_eq!(served, Some(3));
    assert_eq!(m.state.enter(), Entry::Warm { runtime: RUNTIME, served: None });
    assert_eq!(m.state.recovered(), 2);
}

#[test]
fn the_record_is_the_last_app_served() {
    let mut m = Model::booted(&[0, 1, 2]);
    for i in [0, 1, 2, 1] {
        m.state.serve(i);
    }
    assert_eq!(m.restart(), Verdict::Close(1));
    // After a recovery the record is clear until the next call is served.
    assert_eq!(m.restart(), Verdict::Nothing);
    m.state.serve(0);
    assert_eq!(m.restart(), Verdict::Close(0), "even Home, if the fault came in its call");
    assert_eq!(m.closed, vec![1, 0]);
}

#[test]
fn an_app_caught_half_torn_down_is_reclaimed_not_closed() {
    // execve takes the app out of the table while it replaces its image.
    let mut m = Model::booted(&[0, 4]);
    m.state.serve(4);
    m.apps[4] = false;
    assert_eq!(m.restart(), Verdict::Reclaim(4));
    assert_eq!(m.running(), vec![0]);
    assert!(m.closed.is_empty());
}

#[test]
fn a_record_that_names_no_slot_is_ignored() {
    assert_eq!(verdict(Some(12), 12, |_| true), Verdict::Nothing);
    assert_eq!(verdict(Some(usize::MAX), 12, |_| true), Verdict::Nothing);
    assert_eq!(verdict(Some(0), 0, |_| true), Verdict::Nothing);
    assert_eq!(verdict(None, 12, |_| true), Verdict::Nothing);
    // `present` is asked only about slots in the table.
    assert_eq!(verdict(Some(11), 12, |i| [false; 12][i]), Verdict::Reclaim(11));
}

#[test]
fn a_fault_during_boot_stays_down() {
    let s = State::new();
    assert_eq!(s.enter(), Entry::Cold);
    // Boot faults before `settle`: every later start is a boot fault.
    assert_eq!(s.enter(), Entry::Boot);
    assert_eq!(s.enter(), Entry::Boot);
    assert_eq!(s.recovered(), 0);
    // A served app recorded before the runtime exists changes nothing.
    s.serve(2);
    assert_eq!(s.enter(), Entry::Boot);
}

#[test]
fn settle_takes_only_a_real_address() {
    for bad in [0usize, 1] {
        let s = State::new();
        assert_eq!(s.enter(), Entry::Cold);
        s.settle(bad);
        assert_eq!(s.enter(), Entry::Boot, "settle({bad}) must not look like a runtime");
    }
    let s = State::new();
    assert_eq!(s.enter(), Entry::Cold);
    s.settle(0x2000_0000);
    assert_eq!(s.enter(), Entry::Warm { runtime: 0x2000_0000, served: None });
}

#[test]
fn every_app_slot_round_trips_through_the_record() {
    let s = State::new();
    s.enter();
    s.settle(RUNTIME);
    for i in 0..4096 {
        s.serve(i);
        assert_eq!(s.enter(), Entry::Warm { runtime: RUNTIME, served: Some(i) });
    }
}

// ---- frames.rs ----

#[test]
fn one_owner_per_page() {
    let mut f = Frames::new(0, 100);
    assert_eq!(f.owner(7), 0);
    assert_eq!(f.claim(41, 7), Ok(()));
    assert_eq!(f.owner(7), 41);
    assert_eq!(f.claim(42, 7), Err(Refused::Held));
    assert_eq!(f.refused, 1);
    assert_eq!(f.owner(7), 41, "a refused claim changes nothing");
    assert!(!f.release(42, 7), "nor can another space release it");
    assert_eq!(f.owner(7), 41);
    assert!(f.release(41, 7));
    assert_eq!(f.owner(7), 0);
    assert_eq!(f.claim(42, 7), Ok(()));
    assert_eq!(f.owner(7), 42);
}

#[test]
fn a_page_stays_held_until_its_last_mapping_goes() {
    let mut f = Frames::new(0, 100);
    for _ in 0..3 {
        assert_eq!(f.claim(9, 50), Ok(()));
    }
    assert!(f.release(9, 50));
    assert!(f.release(9, 50));
    assert_eq!(f.claim(10, 50), Err(Refused::Held));
    assert!(f.release(9, 50));
    assert!(!f.release(9, 50), "released more often than mapped");
    assert_eq!(f.claim(10, 50), Ok(()));
}

#[test]
fn only_the_apps_range() {
    // Pages 0..768 are the runtime's heap; 5000.. hold the unpacked image.
    let mut f = Frames::new(768, 5000);
    for outside in [0, 767, 5000, 5001, u32::MAX] {
        assert_eq!(f.claim(3, outside), Err(Refused::Range), "page {outside}");
        assert_eq!(f.owner(outside), 0);
        assert!(!f.release(3, outside));
    }
    assert_eq!(f.claim(3, 768), Ok(()));
    assert_eq!(f.claim(3, 4999), Ok(()));
    assert_eq!(f.held_by(3), 2);
    // The range can shrink (the image is unpacked at boot); what is held stays held.
    f.limit(768, 4000);
    assert_eq!(f.claim(4, 4999), Err(Refused::Range));
    assert_eq!(f.owner(4999), 3);
    assert!(f.release(3, 4999));
}

#[test]
fn closed_until_the_range_is_set() {
    let mut f = Frames::new(0, 0);
    for index in [0, 1, 768, u32::MAX] {
        assert_eq!(f.claim(5, index), Err(Refused::Range));
    }
    f.limit(10, 20);
    assert_eq!(f.claim(5, 10), Ok(()));
    assert_eq!(f.claim(5, 20), Err(Refused::Range));
    // An empty or inverted range admits nothing.
    f.limit(20, 10);
    assert_eq!(f.claim(5, 15), Err(Refused::Range));
}

#[test]
fn owners_and_counts_cannot_overflow_into_each_other() {
    let mut f = Frames::new(0, 8);
    assert_eq!(f.claim(0, 1), Err(Refused::Count), "0 means nobody");
    assert_eq!(f.claim(OWNERS, 1), Err(Refused::Count));
    assert_eq!(f.claim(u32::MAX, 1), Err(Refused::Count));
    assert_eq!(f.owner(1), 0);
    let top = OWNERS - 1;
    for n in 0..4095 {
        assert_eq!(f.claim(top, 1), Ok(()), "mapping {n}");
    }
    // The count is full: one more is refused rather than wrapping into the owner.
    assert_eq!(f.claim(top, 1), Err(Refused::Count));
    assert_eq!(f.owner(1), top);
    assert_eq!(f.claim(1, 1), Err(Refused::Held));
    for _ in 0..4095 {
        assert!(f.release(top, 1));
    }
    assert_eq!(f.owner(1), 0);
}

/// Random claims and releases by a few spaces against a plain model.
#[test]
fn random_operations_match_a_model() {
    let mut f = Frames::new(4, 60);
    let mut model = [(0u32, 0u32); 64];
    let mut seed = 0x1234_5678u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..200_000 {
        let (owner, index, release) = (1 + next() % 5, next() % 64, next() % 3 == 0);
        let (held_by, count) = model[index as usize];
        if release {
            let ok = held_by == owner && count > 0;
            assert_eq!(f.release(owner, index), ok);
            if ok {
                model[index as usize] = if count == 1 { (0, 0) } else { (held_by, count - 1) };
            }
        } else {
            let want = if !(4..60).contains(&index) {
                Err(Refused::Range)
            } else if count > 0 && held_by != owner {
                Err(Refused::Held)
            } else {
                Ok(())
            };
            assert_eq!(f.claim(owner, index), want);
            if want.is_ok() {
                model[index as usize] = (owner, count + 1);
            }
        }
        assert_eq!(f.owner(index), model[index as usize].0);
    }
    for owner in 1..=5 {
        assert_eq!(f.held_by(owner), model.iter().filter(|m| m.0 == owner).count());
    }
}
