// QEMU power fixture: one page shared with the host (guest-physical
// 0x49002000, the file offset 0x09002000 of guest.ram). The host supplies the
// battery, a temperature and a CPU load; the guest writes its decisions back,
// where tools/run.py (sleep, shutdown) and targets/qemu/test_power.py read them.
//
//   word 0  host   battery percent
//   word 1  guest  0 awake, 1 suspend, 2 power off
//   word 2  host   temperature in milli-degrees C; 0: no sensor (thermal
//                  control off), 0xffffffff: the sensor fails to answer
//   word 3  host   CPU load percent + 1; 0: not supplied (assume full load)
//   word 4  guest  thermal level (0 normal, 1 warm, 2 hot, 3 critical)
//   word 5  guest  operating point index
//   word 6  guest  operating point kHz (written by the frequency step)
//   word 7  guest  operating point microvolts (written by the voltage step)
//   word 8  guest  bit 0: screen peripherals gated off, bit 1: suspend
//                  deferred by a wake lock, bit 2: sensor failed
//   word 9  guest  wake locks held
//   word 10 guest  wake-lock kinds held, one bit each
//   word 11 guest  times the governors ran
use crate::process::policy::{Board, DvfsStep, Opp, Reading, Snapshot, Zone};
use crate::protocol::*;

const PAGE: usize = 0x09000000;
fn get(word: usize) -> u32 {
    unsafe { (PAGE as *const u32).add(word).read_volatile() }
}
fn set(word: usize, value: u32) {
    unsafe { (PAGE as *mut u32).add(word).write_volatile(value) }
}

pub fn read() -> Message {
    let mut r = Message::result(get(0).min(100));
    r.arg = get(1) & 1;
    r
}
pub fn command(value: u32) -> Message {
    set(1, value);
    Message::result(value)
}
pub fn battery_percent() -> i32 {
    get(0).min(100) as i32
}

pub fn radio_rails(_: bool) -> Message {
    Message::error(INVALID)
}

/// The emulated phone's trip points.
pub const ZONE: Zone = Zone { warm: 60_000, hot: 75_000, critical: 95_000, hysteresis: 5_000 };
/// Emulated operating points: they change nothing about how fast QEMU runs,
/// only what the fixture records, so the governor can be tested end to end.
static OPPS: [Opp; 4] = [
    Opp { khz: 600_000, uv: 650_000 },
    Opp { khz: 1_000_000, uv: 750_000 },
    Opp { khz: 1_500_000, uv: 850_000 },
    Opp { khz: 2_000_000, uv: 950_000 },
];

pub struct Hardware;
impl Board for Hardware {
    fn temperature(&mut self) -> Reading {
        match get(2) {
            0 => Reading::Absent,
            u32::MAX => Reading::Failed,
            mc => Reading::Ok(mc as i32),
        }
    }
    fn load_percent(&mut self) -> Option<u32> {
        get(3).checked_sub(1)
    }
    fn opps(&self) -> &'static [Opp] {
        &OPPS
    }
    fn apply(&mut self, step: DvfsStep) -> bool {
        match step {
            DvfsStep::Frequency(khz) => set(6, khz),
            DvfsStep::Voltage(uv) => set(7, uv),
        }
        true
    }
    fn power(&mut self, value: u32) -> bool {
        command(value).op == OK
    }
    fn screen_peripherals(&mut self, on: bool) {
        set(8, (get(8) & !1) | u32::from(!on));
    }
    fn publish(&mut self, s: &Snapshot) {
        set(4, u32::from(s.level));
        set(5, s.opp);
        set(8, (get(8) & 1) | u32::from(s.sleep_deferred) << 1 | u32::from(s.sensor_failed) << 2);
        set(9, s.locks);
        set(10, s.blockers);
        set(11, get(11).wrapping_add(1));
    }
}
