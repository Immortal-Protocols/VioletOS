//! Touch reports Touch reads on the controller's interrupt (handset): a ring
//! Touch writes (services/touch/events.c) and the supervisor maps here
//! read-only at 0x09807000. Word 1 is "LIVE" once Touch's interrupt runs,
//! word 2 counts samples written; from word 4, four words a sample: x, y,
//! down, and the counter's low word when Touch read it. Each report signals
//! this process's notification (sys::TOUCH_BADGE), so a touch reaches the
//! app in front without waiting for a poll. In QEMU there is no ring.
use super::sys;

const CAP: u32 = 64;
const LIVE: u32 = 0x4556_494c;

#[derive(Default)]
pub struct Ring {
    read: u32,
    started: bool,
    /// The last report: a finger is on the glass.
    pub down: bool,
    /// When the last report arrived.
    pub last_ns: u64,
    /// When Runtime was last polled for touch (the watchdog's liveness poll).
    pub polled_ns: u64,
}

impl Ring {
    #[cfg(moto_board)]
    fn word(i: usize) -> u32 {
        unsafe { (0x0980_7000 as *const u32).add(i).read_volatile() }
    }
    #[cfg(not(moto_board))]
    fn word(_: usize) -> u32 {
        0
    }

    pub fn live(&self) -> bool {
        Self::word(1) == LIVE
    }

    /// The next report: x, y, down, and how long ago Touch read it.
    pub fn next(&mut self) -> Option<(i32, i32, bool, u64)> {
        if !self.live() {
            return None;
        }
        let written = Self::word(2);
        if !self.started {
            // Reports from before this process looked are stale.
            self.started = true;
            self.read = written;
        }
        if written.wrapping_sub(self.read) > CAP {
            self.read = written.wrapping_sub(CAP); // fell behind: the newest CAP
        }
        if self.read == written {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let at = 4 + 4 * (self.read % CAP) as usize;
        let (x, y, down, stamp) = (Self::word(at), Self::word(at + 1), Self::word(at + 2) != 0, Self::word(at + 3));
        self.read = self.read.wrapping_add(1);
        let now = sys::now_ns();
        self.down = down;
        self.last_ns = now;
        let ticks = (sys::counter() as u32).wrapping_sub(stamp) as u64;
        Some((x as i32, y as i32, down, ticks * 1_000_000_000 / sys::counter_hz()))
    }
}

/// "ring=live irqs=N" (or "ring=off"), for the frames report.
pub fn describe() -> alloc::string::String {
    if Ring::word(1) == LIVE {
        alloc::format!("ring=live irqs={} reports={}", Ring::word(3), Ring::word(2))
    } else {
        alloc::format!("ring=off mark={:x} irqs={}", Ring::word(0), Ring::word(3))
    }
}
