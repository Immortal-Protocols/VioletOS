//! The compositor: the one process that writes scanout for clients. The
//! Display process (services/display/process.rs) is this code over the real
//! framebuffer; the host tests run it over arrays.
//!
//! A client's present goes scene.rs `submit` (which rows may change), then
//! one composition: every damaged rectangle is drawn from the client's
//! surface with the blits in blit.rs, the changed rows are published for the
//! QEMU viewer, and the time it took is recorded against the frame budget.
use super::blit;
use super::scene::{Budget, Rect, Refusal, Scene, H, SCREEN, STATS_WORDS, W};

/// Visible pixels, and the words after them that the panel never shows (the
/// last scanout frame is mapped whole).
pub const PIXELS: usize = (W * H) as usize;
pub const TAIL_WORDS: usize = 32;
/// In the tail: the QEMU viewer's dirty record (magic, first and last changed
/// row, a sequence number and the sequence the viewer has copied; the Open
/// Runtime keeps the same one, runtime/open/scanout.rs), then the frame
/// budget record (scene.rs `Budget::words`).
const DIRTY_MAGIC: u32 = 0x5452_4944; // "DIRT"
pub const STATS_AT: usize = 16;

/// A built-in app's surface, in its own pixels.
pub const CLASSIC_W: usize = 240;
pub const CLASSIC_H: usize = 400;
/// Each of its pixels is three screen pixels wide and four high.
const CLASSIC_SX: u32 = W / CLASSIC_W as u32;
const CLASSIC_SY: u32 = H / CLASSIC_H as u32;

pub trait Hardware {
    /// Row `y` of built-in app `client`'s surface.
    fn classic_row(&mut self, client: u32, y: usize, out: &mut [u32; CLASSIC_W]);
    /// Scanout: PIXELS pixels, then TAIL_WORDS words.
    fn scanout(&mut self) -> &mut [u32];
    /// A wrapping counter and its rate.
    fn ticks(&mut self) -> u32;
    fn ticks_per_ms(&mut self) -> u32;
}

pub struct Compositor {
    pub scene: Scene,
    pub budget: Budget,
}

impl Compositor {
    pub const fn new() -> Compositor {
        Compositor { scene: Scene::new(), budget: Budget::new() }
    }

    /// Built-in app `client` (in front: Runtime forwards a present from no
    /// other, runtime/process.rs) changed rows first..end of its surface.
    pub fn present_classic(&mut self, hw: &mut impl Hardware, client: u32, first: u32, end: u32) -> Result<Rect, Refusal> {
        // A built-in app repaints itself when it comes to the front, so the
        // rows it did not name stay as they are.
        if self.scene.front() != client || self.scene.scale() != (CLASSIC_SX, CLASSIC_SY) {
            self.scene.set_front(client, CLASSIC_SX, CLASSIC_SY)?;
            self.scene.damage.clear();
        }
        let shown = self.scene.submit(client, Rect::new(0, first, CLASSIC_W as u32, end))?;
        let t0 = hw.ticks();
        let sy = CLASSIC_SY;
        let allowed = self.scene.client_region();
        let mut src = [0u32; CLASSIC_W];
        let mut line = [0u32; W as usize];
        let mut pixels = 0;
        for i in 0..self.scene.damage.len() {
            let r = self.scene.damage.rect(i).intersect(&allowed);
            // The surface row on screen row y, and how many more screen
            // rows show it.
            let (mut row, mut left) = (r.y0 / sy, 0);
            for y in r.y0..r.y1 {
                if left == 0 {
                    hw.classic_row(client, row as usize, &mut src);
                    blit::widen3(&mut line, &src);
                    left = (row + 1) * sy - y;
                    row += 1;
                }
                left -= 1;
                draw(hw, y, r.x0, r.x1, &line);
            }
            pixels += r.area();
        }
        self.scene.damage.clear();
        self.finish(hw, t0, pixels, shown.y0, shown.y1);
        Ok(shown)
    }

    /// A client with a full-size surface (720 x 1600, as the Open Runtime's
    /// would be once it submits instead of writing scanout: README.md)
    /// changed `r` of it. Who is in front is not the client's to say:
    /// `scene.set_front` is called for whoever Runtime puts there.
    pub fn present_surface(&mut self, hw: &mut impl Hardware, client: u32, surface: &[u32], r: Rect) -> Result<Rect, Refusal> {
        if client == 0 || self.scene.front() != client {
            return Err(Refusal::NotFront);
        }
        if surface.len() < PIXELS || self.scene.scale() != (1, 1) {
            return Err(Refusal::Invalid);
        }
        let shown = self.scene.submit(client, r)?;
        self.compose_surface(hw, surface);
        Ok(shown)
    }

    /// Draw every damaged client row from `surface` (the front client's).
    pub fn compose_surface(&mut self, hw: &mut impl Hardware, surface: &[u32]) {
        let t0 = hw.ticks();
        let allowed = self.scene.client_region();
        let (mut pixels, mut lo, mut hi) = (0, H, 0);
        for i in 0..self.scene.damage.len() {
            let r = self.scene.damage.rect(i).intersect(&allowed);
            if r.is_empty() {
                continue;
            }
            for y in r.y0..r.y1 {
                if let Some(row) = surface.get((y * W) as usize..((y + 1) * W) as usize) {
                    draw(hw, y, r.x0, r.x1, row);
                }
            }
            pixels += r.area();
            lo = lo.min(r.y0);
            hi = hi.max(r.y1);
        }
        self.scene.damage.clear();
        self.finish(hw, t0, pixels, lo, hi);
    }

    /// The system's own drawing: `r` filled with `color`, where it is the
    /// system's to draw (the band, or anywhere while the screen is covered).
    pub fn system_fill(&mut self, hw: &mut impl Hardware, r: Rect, color: u32) -> Rect {
        let r = self.scene.system_rect(r);
        if !r.is_empty() {
            let t0 = hw.ticks();
            for y in r.y0..r.y1 {
                if let Some(d) = hw.scanout().get_mut((y * W + r.x0) as usize..(y * W + r.x1) as usize) {
                    blit::fill(d, color);
                }
            }
            self.finish(hw, t0, r.area(), r.y0, r.y1);
        }
        r
    }

    /// The screen goes dark (the phone sleeps or wakes: the app in front
    /// draws again when it is shown).
    pub fn clear(&mut self, hw: &mut impl Hardware) {
        let t0 = hw.ticks();
        if let Some(d) = hw.scanout().get_mut(..PIXELS) {
            blit::fill(d, 0);
        }
        self.scene.damage.clear();
        self.finish(hw, t0, SCREEN.area(), 0, H);
    }

    fn finish(&mut self, hw: &mut impl Hardware, t0: u32, pixels: u64, y0: u32, y1: u32) {
        if y0 < y1 {
            if let Some(tail) = hw.scanout().get_mut(PIXELS..) {
                mark_dirty(tail, y0, y1);
            }
        }
        let (t1, rate) = (hw.ticks(), hw.ticks_per_ms());
        self.budget.record(t0, t1, rate, pixels);
        let words = self.budget.words();
        if let Some(stats) = hw.scanout().get_mut(PIXELS + STATS_AT..PIXELS + STATS_AT + STATS_WORDS) {
            // The magic last: a reader that sees it sees a whole record.
            for (i, d) in stats.iter_mut().enumerate().skip(1) {
                unsafe { core::ptr::write_volatile(d, words[i % STATS_WORDS]) };
            }
            if let Some(d) = stats.first_mut() {
                unsafe { core::ptr::write_volatile(d, words[0]) };
            }
        }
    }
}

/// Columns x0..x1 of screen row `y`, from a row of 720 pixels.
fn draw(hw: &mut impl Hardware, y: u32, x0: u32, x1: u32, row: &[u32]) {
    let at = (y * W) as usize;
    if let (Some(d), Some(s)) = (hw.scanout().get_mut(at + x0 as usize..at + x1 as usize), row.get(x0 as usize..x1 as usize)) {
        blit::copy(d, s);
    }
}

/// Tell the QEMU viewer rows y0..y1 changed, merged with any it has not
/// copied yet. The viewer reads these words while they are written, so they
/// are written in this order and the sequence number last.
fn mark_dirty(tail: &mut [u32], y0: u32, y1: u32) {
    if tail.len() < 5 {
        return;
    }
    let dirty = tail.as_mut_ptr();
    unsafe {
        let (seq, seen) = (dirty.add(3).read_volatile(), dirty.add(4).read_volatile());
        let (mut a, mut b) = (y0, y1);
        if dirty.read_volatile() == DIRTY_MAGIC && seen != seq {
            a = a.min(dirty.add(1).read_volatile());
            b = b.max(dirty.add(2).read_volatile());
        }
        dirty.add(1).write_volatile(a);
        dirty.add(2).write_volatile(b.min(H));
        dirty.write_volatile(DIRTY_MAGIC);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        dirty.add(3).write_volatile(seq.wrapping_add(1));
    }
}
