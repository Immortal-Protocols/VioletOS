//! Tear-free presents and VSync (docs/8ms-frame-pipeline-rfc.md, phase 1).
//!
//! On the handset the panel scans the framebuffer top to bottom 60 times a
//! second, and DISP_RDMA0 reports the line it is reading (display.rs). A
//! present copies rows at about 500 lines a millisecond, cleaning each band
//! to memory as it goes, while the panel reads about 96: a copy that starts
//! below the beam, or above it, finishes before the beam reaches anything it
//! changed. Only a copy that would start inside the damaged rows waits, and
//! the presenting thread waits for it (Wait::Present), not the Open Runtime.
//!
//! In QEMU there is no beam: presents never wait, VSync is a 60 Hz clock,
//! and a few words after the frame tell the Mac viewer which rows changed.
use super::sys;

/// Rows ahead of the scan line the display engine has already fetched.
const MARGIN: u32 = 32;
/// A scan line that has not moved for this long means the panel is off.
const STALLED_NS: u64 = 5_000_000;
/// A present never waits longer than this, whatever the beam says.
pub const MAX_WAIT_NS: u64 = 40_000_000;
pub const LINES: u32 = 1600;
const QEMU_PERIOD_NS: u64 = 16_666_667;

#[derive(Default)]
pub struct Beam {
    line: u32,
    moved_ns: u64,
    /// Frames scanned since start: the VSync sequence.
    pub frame: u64,
    pub frame_ns: u64,
    /// Measured frame period.
    pub period_ns: u64,
}

impl Beam {
    /// The line the panel reads now, or None without a readable beam (QEMU,
    /// or a panel that is off).
    #[cfg(moto_board)]
    fn read(&mut self, now: u64) -> Option<u32> {
        let line = super::display::scan_line();
        if line != self.line {
            if line + 200 < self.line {
                if self.frame_ns != 0 {
                    let p = now - self.frame_ns;
                    // A missed frame (the Open Runtime was busy) is not a period.
                    if p < 25_000_000 {
                        self.period_ns = if self.period_ns == 0 { p } else { (self.period_ns * 7 + p) / 8 };
                    }
                }
                self.frame += 1;
                self.frame_ns = now;
            }
            self.line = line;
            self.moved_ns = now;
        }
        (now - self.moved_ns < STALLED_NS).then_some(line)
    }
    #[cfg(not(moto_board))]
    fn read(&mut self, now: u64) -> Option<u32> {
        let frame = now / QEMU_PERIOD_NS;
        if frame != self.frame {
            self.frame = frame;
            self.frame_ns = frame * QEMU_PERIOD_NS;
            self.period_ns = QEMU_PERIOD_NS;
        }
        None
    }

    /// Track the beam (call often: every pass of the main loop).
    pub fn tick(&mut self) {
        let now = sys::now_ns();
        self.read(now);
    }

    /// Whether rows y0..y1 can be replaced now without the panel showing
    /// half of the change: the beam is above them (the copy, top down, stays
    /// ahead of it), or near enough their end that it leaves them before the
    /// copy, at about five times its speed, reaches the rows it has left.
    pub fn clear_of(&mut self, y0: u32, y1: u32) -> bool {
        match self.read(sys::now_ns()) {
            Some(line) => line >= Self::leave_line(y0, y1) || line + MARGIN < y0,
            None => true,
        }
    }

    /// The line from which a copy of y0..y1 may start behind the beam.
    fn leave_line(y0: u32, y1: u32) -> u32 {
        y1 - (y1 - y0) * 3 / 20
    }

    /// How long until rows y0..y1 are clear (0 if they are now): the time
    /// the beam needs to reach their leave line.
    pub fn wait_ns(&mut self, y0: u32, y1: u32) -> u64 {
        let Some(line) = self.read(sys::now_ns()) else { return 0 };
        let leave = Self::leave_line(y0, y1);
        if line >= leave || line + MARGIN < y0 {
            return 0;
        }
        // About 1640 lines a frame, counting the blanking.
        let per_line = if self.period_ns == 0 { 10_200 } else { self.period_ns / 1640 };
        u64::from(leave - line) * per_line
    }

    pub fn period_us(&self) -> u32 {
        (if self.period_ns == 0 { QEMU_PERIOD_NS } else { self.period_ns } / 1000) as u32
    }
}

/// Words just after the visible frame (in the last scanout page, which the
/// panel never shows): magic, first and last changed row, a sequence number
/// the Open Runtime bumps, and the sequence the viewer has copied.
const DIRTY: *mut u32 = (sys::FRAMEBUFFER + 720 * 1600 * 4) as *mut u32;
const DIRTY_MAGIC: u32 = 0x5452_4944; // "DIRT"

/// Tell the QEMU viewer rows y0..y1 changed (merged with any it has not copied yet).
pub fn mark_dirty(y0: u32, y1: u32) {
    if cfg!(moto_board) || y0 >= y1 {
        return;
    }
    unsafe {
        let seq = DIRTY.add(3).read_volatile();
        let seen = DIRTY.add(4).read_volatile();
        let (mut a, mut b) = (y0, y1);
        if DIRTY.read_volatile() == DIRTY_MAGIC && seen != seq {
            a = a.min(DIRTY.add(1).read_volatile());
            b = b.max(DIRTY.add(2).read_volatile());
        }
        DIRTY.add(1).write_volatile(a);
        DIRTY.add(2).write_volatile(b.min(LINES));
        DIRTY.write_volatile(DIRTY_MAGIC);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        DIRTY.add(3).write_volatile(seq.wrapping_add(1));
    }
}
