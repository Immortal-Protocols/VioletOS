//! Wake locks and Power's governors: what targets/qemu/test_power.py checked
//! with the Power Test app and the emulated sensor, against the shipped
//! runtime/open/powerctl.rs and services/power/policy.rs joined over the
//! fake IPC endpoint. The fake board is the hardware edge: the temperature
//! sensor, the load counter, the regulator and clock, and the power switch.
//!
//! One test: powerctl.rs keeps its holders in a static, as the
//! single-threaded runtime does, so nothing else in this binary may use it.
use crate::kernel::clock;
use crate::power::{self, Board, DvfsStep, Manager, Opp, Reading, Snapshot, Zone, LEVEL_HOT, LEVEL_NORMAL, LEVEL_WARM};
use crate::protocol::*;
use crate::world::*;
use std::cell::RefCell;
use std::rc::Rc;

/// targets/qemu/power.rs
const OPPS: [Opp; 4] = [Opp { khz: 600_000, uv: 650_000 }, Opp { khz: 1_000_000, uv: 750_000 }, Opp { khz: 1_500_000, uv: 850_000 }, Opp { khz: 2_000_000, uv: 950_000 }];
const ZONE: Zone = Zone { warm: 60_000, hot: 75_000, critical: 95_000, hysteresis: 5_000 };

#[derive(Default)]
struct Hardware {
    temperature: Option<Reading>,
    load: Option<u32>,
    khz: u32,
    uv: u32,
    /// Every regulator and clock step, to check the order.
    steps: Vec<DvfsStep>,
    power: Vec<u32>,
    shown: Snapshot,
}
struct Fixture(Rc<RefCell<Hardware>>);
impl Board for Fixture {
    fn temperature(&mut self) -> Reading {
        self.0.borrow().temperature.unwrap_or(Reading::Absent)
    }
    fn load_percent(&mut self) -> Option<u32> {
        self.0.borrow().load
    }
    fn opps(&self) -> &'static [Opp] {
        &OPPS
    }
    fn apply(&mut self, step: DvfsStep) -> bool {
        let mut h = self.0.borrow_mut();
        match step {
            DvfsStep::Voltage(uv) => h.uv = uv,
            DvfsStep::Frequency(khz) => h.khz = khz,
        }
        // The safety property: never a frequency its voltage cannot carry.
        let need = OPPS.iter().find(|o| o.khz >= h.khz).map_or(u32::MAX, |o| o.uv);
        assert!(h.uv >= need || h.khz == 0, "{} kHz at {} uV", h.khz, h.uv);
        h.steps.push(step);
        true
    }
    fn power(&mut self, command: u32) -> bool {
        self.0.borrow_mut().power.push(command);
        true
    }
    fn screen_peripherals(&mut self, _: bool) {}
    fn publish(&mut self, s: &Snapshot) {
        self.0.borrow_mut().shown = *s;
    }
}

#[test]
fn wake_locks_and_governors() {
    let mut w = World::boot(Image::new().app("powertest", "").app("other", ""));
    let hw = Rc::new(RefCell::new(Hardware { khz: 2_000_000, uv: 950_000, ..Hardware::default() }));
    let service = Rc::new(RefCell::new((Manager::new(ZONE, 3), Fixture(hw.clone()))));
    // Power behind Runtime's endpoint: the lock request, as
    // services/power/process.rs `lock` answers it (LockError::reply).
    let served = service.clone();
    serve_through_runtime(move |m: Message| {
        if (m.op, m.key) != (EXECUTE, POWER) || m.value & !0xff != POWER_LOCK {
            return Message::error(DENIED);
        }
        let now = clock::now_ns() / 1_000_000;
        let (manager, board) = &mut *served.borrow_mut();
        let owner = power::APP_OWNER.saturating_add(m.arg);
        let (action, kind) = ((m.value >> 4) & 0xf, (m.value & 0xf) as u8);
        let result = match action {
            0 | 1 => manager.lock(board, owner, kind, action == 1, now),
            2 => {
                manager.owner_gone(board, owner, now);
                Ok(())
            }
            _ => Err(power::LockError::BadKind),
        };
        match result {
            Ok(()) => Message::result(manager.locks.blockers(now)),
            Err(e) => {
                let (op, arg) = e.reply();
                Message::error(op).with_arg(arg)
            }
        }
    });
    let tick = |w: &mut World, ms: u64| {
        // Power runs its governors when a request arrives; here, every 250 ms.
        for _ in 0..ms / 250 {
            w.advance_ms(250);
            let (manager, board) = &mut *service.borrow_mut();
            manager.tick(board, clock::now_ns() / 1_000_000);
        }
        hw.borrow().shown
    };
    let locks = || service.borrow().0.locks.count();

    // ---- wake locks: accounted to the app, renewed, ended with it ----
    assert_eq!(locks(), 0, "the fixture starts with no locks");
    let app = w.open_app("powertest");
    let pid = w.app(app).pid();
    assert_eq!(w.app(app).sys("wakelock 1"), "ok");
    assert_eq!(w.app(app).sys("wakelock 1"), "ok", "a renewal");
    assert_eq!(w.app(app).sys("wakelock 2"), "error", "a bad argument");
    assert_eq!(locks(), 1);
    assert_eq!(w.logged(&format!("wakelock: pid {pid} powertest took a lock")), 1, "{}", w.log());
    assert_eq!(tick(&mut w, 1000).locks, 1);
    // Another app's lock is its own.
    let other = w.open_app("other");
    assert_eq!(w.app(other).sys("wakelock 1"), "ok");
    assert_eq!(locks(), 2);
    assert_eq!(w.app(other).sys("wakelock 0"), "ok");
    assert_eq!(locks(), 1);

    // ---- no sensor: reported as absent, never as cool; no load figure: full speed ----
    let s = tick(&mut w, 1000);
    assert!(!s.sensor && s.level == LEVEL_NORMAL);
    assert_eq!((s.opp, s.khz, s.uv), (3, 2_000_000, 950_000));

    // ---- the operating point follows the load ----
    hw.borrow_mut().load = Some(0);
    let s = tick(&mut w, 2000);
    assert_eq!((s.opp, s.khz, s.uv), (0, 600_000, 650_000), "idle");
    hw.borrow_mut().load = Some(50);
    assert_eq!(tick(&mut w, 2000).khz, 1_500_000, "half load");
    hw.borrow_mut().load = Some(100);
    let s = tick(&mut w, 2000);
    assert_eq!((s.opp, s.uv), (3, 950_000), "full load");
    assert_eq!((hw.borrow().khz, hw.borrow().uv), (2_000_000, 950_000), "the board is where Power says");

    // ---- heat caps it at once and lets go slowly ----
    hw.borrow_mut().temperature = Some(Reading::Ok(40_000));
    let s = tick(&mut w, 1500);
    assert!(s.sensor && s.temperature_mc == 40_000 && s.level == LEVEL_NORMAL);
    hw.borrow_mut().temperature = Some(Reading::Ok(65_000));
    let s = tick(&mut w, 1500);
    assert_eq!((s.level, s.opp), (LEVEL_WARM, 2));
    hw.borrow_mut().temperature = Some(Reading::Ok(80_000));
    let s = tick(&mut w, 1500);
    assert_eq!((s.level, s.opp, s.khz), (LEVEL_HOT, 1, 1_000_000));
    hw.borrow_mut().temperature = Some(Reading::Ok(40_000));
    let cooled = clock::now_ns();
    let mut s = tick(&mut w, 250);
    while s.level != LEVEL_NORMAL {
        assert!(clock::now_ns() - cooled < 20_000_000_000, "the level never fell");
        s = tick(&mut w, 250);
    }
    let took = (clock::now_ns() - cooled) / 1_000_000;
    assert!(took >= 2 * power::DWELL_MS - 1500, "the level fell from hot to normal in {took} ms (two dwells of 5 s expected)");
    assert_eq!(tick(&mut w, 1000).opp, 3);

    // ---- a sensor that stops answering is treated as hot, says so, and powers nothing off ----
    hw.borrow_mut().temperature = Some(Reading::Failed);
    let s = tick(&mut w, 5000);
    assert!(s.sensor_failed && s.level == LEVEL_HOT && s.opp == 1);
    assert!(hw.borrow().power.is_empty(), "a failing sensor powered the machine off");
    hw.borrow_mut().temperature = Some(Reading::Ok(40_000));
    let s = tick(&mut w, 12_000);
    assert!(!s.sensor_failed && s.level < LEVEL_HOT);

    // ---- the lock ends with its app ----
    assert_eq!(locks(), 1);
    assert_eq!(w.app(app).syscall(248, [0; 6]), 0);
    assert_eq!(locks(), 0, "the lock outlived its app");
    assert_eq!(w.logged(&format!("wakelock: pid {pid} ended holding its lock")), 1);

    // ---- a critical temperature powers the machine off, once ----
    hw.borrow_mut().temperature = Some(Reading::Ok(96_000));
    tick(&mut w, 3000);
    assert_eq!(hw.borrow().power, [2]);
    assert!(service.borrow().0.powering_off());
    // Every change raised the supply before the clock and lowered the clock
    // before the supply (checked at each step by the fixture).
    assert!(hw.borrow().steps.len() >= 8);
}
