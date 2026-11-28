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
//   word 12 guest  1 while suspended to idle (the timekeeper holds every
//                  process's timers; the machine keeps running, unlike word 1)
//   word 13 host   wake lines, taken (and cleared) by the guest: bit 0 the
//                  power key, bit 1 a touch, bit 3 the network
//   word 14 host   a wake alarm in milliseconds from now, taken by the guest
//   word 15 guest  the last wake's sources (bits as word 13, bit 2 the alarm)
//                  in the low byte, the number of suspends above
//   word 16 guest  what Power asks of the screen: 0 nothing, 1 wake, 2 sleep
//   word 17 host   1 while the power key is held (the viewer's P key)
//   word 18 host   power key presses, counted by the host, so a press
//                  shorter than the Open Runtime's poll is still seen
use crate::process::policy::{Board, DvfsStep, Opp, Reading, Snapshot, Zone};
use crate::protocol::*;

const PAGE: usize = 0x09000000;
fn get(word: usize) -> u32 {
    unsafe { (PAGE as *const u32).add(word).read_volatile() }
}
fn set(word: usize, value: u32) {
    unsafe { (PAGE as *mut u32).add(word).write_volatile(value) }
}
/// Read a host word and clear it atomically (LDREX/STREX).
fn take(word: usize) -> u32 {
    use core::sync::atomic::{AtomicU32, Ordering};
    unsafe { (*(PAGE as *const AtomicU32).add(word)).swap(0, Ordering::AcqRel) }
}

pub fn read() -> Message {
    let mut r = Message::result(get(0).min(100));
    r.arg = (get(1) | get(12)) & 1;
    r
}
extern "C" {
    // The supervisor's timekeeper (runtime/transport.c).
    fn timekeeper_command(op: u32, arg: u32);
    fn timekeeper_state() -> u32;
    fn timekeeper_reason() -> u32;
}
const TK_OP_SUSPEND: u32 = 1;
const TK_OP_RESUME: u32 = 2;
const TK_FROZEN: u32 = 1;
/// The processes that may end a suspend to idle themselves (system_wake in
/// runtime/transport.c), by supervisor identity: Runtime, Touch, Radio and
/// the Open Runtime. None of them calls it yet; Power's own look at the wake
/// lines below is what wakes the emulator.
const WAKE_PROCESSES: u32 = 1 << 3 | 1 << 4 | 1 << 7 | 1 << 24;
/// Suspend (1) with the timekeeper is a suspend to idle inside the guest:
/// every process's timers are held and released twice a second, and the
/// machine runs on, so the wake sources work. Without the timekeeper it is
/// the old register that tools/run.py answers by pausing the machine.
/// The host's power key for the Open Runtime (runtime/open/power.rs): word
/// 17 is set while it is held, word 18 counts presses.
pub fn key() -> Message {
    let mut r = Message::result(get(17) & 1);
    r.arg = get(18);
    r
}
pub fn command(value: u32) -> Message {
    let idle = unsafe { crate::process::timekeeper_live() } != 0;
    match value {
        1 if idle => {
            unsafe { timekeeper_command(TK_OP_SUSPEND, WAKE_PROCESSES) };
            set(12, 1);
        }
        0 if idle => {
            unsafe { timekeeper_command(TK_OP_RESUME, 0) };
            set(12, 0);
            set(1, 0);
        }
        _ => set(1, value),
    }
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
        set(15, s.last_wake & 0xff | s.suspends << 8);
        set(16, u32::from(s.want));
    }
    const SUSPENDS: bool = true;
    fn wake_events(&mut self) -> u32 {
        // Taken and cleared in one exclusive access: a line the host sets
        // between a read and a separate clear would be lost.
        let mut events = take(13);
        // A process ended the suspend itself: the timekeeper says why.
        if get(12) == 1 && unsafe { crate::process::timekeeper_live() != 0 && timekeeper_state() & TK_FROZEN == 0 } {
            // Whatever it gave as the reason, the suspend is over: say so.
            events |= match unsafe { timekeeper_reason() } & 0xb {
                0 => 8,
                bits => bits,
            };
        }
        events
    }
    fn take_alarm(&mut self) -> Option<u64> {
        let ms = get(14);
        if ms == 0 {
            return None;
        }
        set(14, 0);
        Some(u64::from(ms))
    }
}
