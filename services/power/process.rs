//! Power: the battery, sleep and shutdown, wake locks, the thermal and
//! frequency governors and the supplies other services share. The decisions
//! are in policy.rs (host-tested); the board's registers are in
//! targets/*/power.rs.
#[path = "policy.rs"]
pub mod policy;

use crate::{power_backend, protocol::*};
use policy::{Edge, LockError, Manager};

/// The shared-supply table (policy::Gates): the Wi-Fi rails are resource 0.
const RADIO_RAILS_RESOURCE: usize = 0;
/// The badge of Radio's lane worker.
const RADIO_BADGE: u32 = 7;

/// Milliseconds since boot from the 64-bit virtual counter (`ticks()` is its
/// low half, which wraps in about a minute in QEMU).
pub fn now_ms() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("isb", "mrrc p15, 1, {lo}, {hi}, c14", lo = out(reg) lo, hi = out(reg) hi,
            options(nomem, nostack, preserves_flags));
        (u64::from(hi) << 32 | u64::from(lo)) / u64::from(ticks_per_ms.max(1))
    }
}

fn put(m: &mut Message, at: usize, value: u32) {
    m.data[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
}

/// The Wi-Fi rails through the user table: the hardware changes on the first
/// request and the last release. With Radio as the only user this is what the
/// backend did alone: a repeated enable is refused and the rails restored.
fn radio_rails(m: &mut Manager, on: bool) -> Message {
    let edge = if on { m.gates.request(RADIO_RAILS_RESOURCE, RADIO_BADGE) } else { m.gates.release(RADIO_RAILS_RESOURCE, RADIO_BADGE) };
    if edge == Ok(Edge::None) {
        return Message::result(u32::from(on));
    }
    let reply = power_backend::radio_rails(on);
    if on && reply.op != OK {
        // The backend restored the rails.
        let _ = m.gates.release(RADIO_RAILS_RESOURCE, RADIO_BADGE);
    }
    reply
}

/// GET POWER POWER_INFO: value is the thermal level (0 normal, 1 warm, 2 hot,
/// 3 critical), arg the POWER_INFO_* flags, then seven words: temperature in
/// milli-degrees C, operating point, its kHz, the wake-lock kinds held (one
/// bit each), the number of locks, the battery percentage (-1 unknown) and
/// milliseconds since boot (low half).
fn info(m: &mut Manager, board: &power_backend::Hardware, now: u64) -> Message {
    let s = m.snapshot(board, now);
    let mut r = Message::result(u32::from(s.level));
    r.arg = u32::from(s.sensor) * POWER_INFO_SENSOR
        | u32::from(s.sensor_failed) * POWER_INFO_SENSOR_FAILED
        | u32::from(s.dvfs) * POWER_INFO_DVFS
        | u32::from(s.sleep_deferred) * POWER_INFO_SLEEP_DEFERRED
        | u32::from(!s.screen_on) * POWER_INFO_SCREEN_OFF;
    let words = [s.temperature_mc as u32, s.opp, s.khz, s.blockers, s.locks, power_backend::battery_percent() as u32, now as u32];
    for (i, w) in words.into_iter().enumerate() {
        put(&mut r, i, w);
    }
    r
}

/// GET POWER POWER_LOCKS|index. arg 0: the index'th held lock (value owner,
/// arg kind, then milliseconds held as a u64). arg 1: the index'th owner's
/// accounting (value owner, arg locks held now, then total and longest
/// milliseconds as u64s, locks taken and locks that lapsed as u32s).
fn locks(m: &Manager, index: usize, stats: bool, now: u64) -> Message {
    if stats {
        let Some(s) = m.locks.owner_stat(index, now) else { return Message::error(EMPTY) };
        let mut r = Message::result(s.owner);
        r.arg = u32::from(s.holding);
        r.data[..8].copy_from_slice(&s.total_ms.to_le_bytes());
        r.data[8..16].copy_from_slice(&s.longest_ms.to_le_bytes());
        put(&mut r, 4, s.acquired);
        put(&mut r, 5, s.timeouts);
        r
    } else {
        let Some(h) = m.locks.held(index, now) else { return Message::error(EMPTY) };
        let mut r = Message::result(h.owner);
        r.arg = u32::from(h.kind);
        r.data[..8].copy_from_slice(&h.held_ms.to_le_bytes());
        r
    }
}

/// EXECUTE POWER POWER_LOCK|action<<4|kind with the owner in arg. Actions:
/// 0 release, 1 take (or renew), 2 the owner is gone. The reply's value is
/// the lock kinds still held by anyone. A refusal is DENIED with the reason
/// in arg (POWER_LOCK_FULL, POWER_LOCK_SCREEN_OFF), EMPTY for a lock not
/// held, INVALID for a bad request.
fn lock(m: &mut Manager, board: &mut power_backend::Hardware, owner: u32, request: u32, now: u64) -> Message {
    let (action, kind) = ((request >> 4) & 0xf, (request & 0xf) as u8);
    let result = match action {
        0 | 1 => m.lock(board, owner, kind, action == 1, now),
        2 => {
            m.owner_gone(board, owner, now);
            Ok(())
        }
        _ => Err(LockError::BadKind),
    };
    match result {
        Ok(()) => Message::result(m.locks.blockers(now)),
        Err(e) => {
            let (op, arg) = e.reply();
            Message::error(op).with_arg(arg)
        }
    }
}

// policy.rs numbers the refusals itself (it is built alone for the host
// tests): they must be protocol.rs's. A refusal is never BUSY, which a
// client of Runtime reads as "still running, ask again".
const _: () = assert!(
    policy::REPLY_DENIED == DENIED
        && policy::REPLY_INVALID == INVALID
        && policy::REPLY_EMPTY == EMPTY
        && policy::REPLY_PENDING == BUSY
        && policy::REFUSED_FULL == POWER_LOCK_FULL
        && policy::REFUSED_SCREEN_OFF == POWER_LOCK_SCREEN_OFF
);

static mut MANAGER: Manager = Manager::new(power_backend::ZONE, 0);

pub fn run() -> ! {
    // Only this thread uses the manager.
    let manager = unsafe { &mut *core::ptr::addr_of_mut!(MANAGER) };
    let mut board = power_backend::Hardware;
    loop {
        let (caller,m) = Message::receive(SERVER);
        let now = now_ms();
        // Power has no timer: the governors run when a request arrives.
        manager.tick(&mut board, now);
        let reply = match (caller,m.key,m.op) {
            #[cfg(moto_board)]
            (21,0x4057,EXECUTE) => power_backend::usb_supply(),
            #[cfg(moto_board)]
            (21,0x4062,EXECUTE) if m.value <= 1 => power_backend::fastboot_mark(m.value != 0),
            #[cfg(moto_board)]
            (AUDIO_COMPONENT,PMIC_ACCESS,GET|EXECUTE) => power_backend::audio_pmic(m.op,m.value,m.arg),
            #[cfg(moto_board)]
            (CAMERA_COMPONENT,PMIC_ACCESS,GET|EXECUTE) => power_backend::camera_pmic(m.op,m.value,m.arg),
            (RADIO_BADGE,RADIO_RAILS,EXECUTE) if m.value <= 1 => radio_rails(manager, m.value != 0),
            (3,POWER,GET) if m.value == POWER_INFO => info(manager, &board, now),
            (3,POWER,GET) if m.value & !0xff == POWER_LOCKS => locks(manager, (m.value & 0xff) as usize, m.arg != 0, now),
            (3,POWER,GET) => power_backend::read(),
            // Runtime has blanked the screen. With a wake lock held the
            // screen stays off and the suspend waits: arg names the kinds.
            // Sleep and wake are refused (OFFLINE) once Power has commanded a
            // power-off for heat, so they cannot replace it.
            (3,POWER,EXECUTE) if m.value == 1 => match manager.sleep(&mut board, now) {
                Ok(blockers) => Message::result(1).with_arg(blockers),
                Err(()) => Message::error(OFFLINE),
            },
            (3,POWER,EXECUTE) if m.value == 0 => {
                if manager.wake(&mut board) { Message::result(0) } else { Message::error(OFFLINE) }
            }
            (3,POWER,EXECUTE) if m.value == 2 => power_backend::command(2),
            // Open apps' locks, through Runtime, which accepts them only from
            // the Open Runtime; their owner IDs never collide with a service's.
            (3,POWER,EXECUTE) if m.value & !0xff == POWER_LOCK =>
                lock(manager, &mut board, policy::APP_OWNER.saturating_add(m.arg), m.value, now),
            // A service's own lock, under its component ID. Nothing sends
            // this yet. Radio has no endpoint here of its own (badge 7 is
            // Runtime's worker for Radio's requests), so it cannot.
            (AUDIO_COMPONENT|CAMERA_COMPONENT,POWER,EXECUTE) if m.value & !0xff == POWER_LOCK =>
                lock(manager, &mut board, caller, m.value, now),
            _ => Message::error(DENIED),
        };
        manager.publish(&mut board, now);
        reply.reply();
    }
}
