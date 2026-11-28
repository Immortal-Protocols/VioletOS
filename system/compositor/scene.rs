//! The compositor's rules, with no hardware in them (host-tested:
//! tests/host.rs, run by `make test-unit`).
//!
//! The screen has two owners. The system owns the band (the top `band` rows)
//! and, while a lock screen, passcode pad or prompt is up (`covered`), every
//! row. A client (a built-in app's surface, or the Open Runtime's) owns
//! nothing: it submits rectangles of its own surface, and `submit` answers
//! with the part of the screen they may change, which is never a system row.
//! Touches are routed the same way: a touch that starts on system rows, or
//! while the screen is covered, belongs to the system until the finger lifts,
//! and a synthetic touch never reaches system UI.
//!
//! `Damage` keeps what has to be redrawn as a few rectangles that neither
//! overlap nor touch; `Budget` measures what each composition cost against
//! the frame budget.

pub const W: u32 = 720;
pub const H: u32 = 1600;
/// The most screen pixels one surface pixel may cover, each way.
pub const MAX_SCALE: u32 = 8;

/// Pixels x0..x1 of rows y0..y1.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

pub const SCREEN: Rect = Rect { x0: 0, y0: 0, x1: W, y1: H };

impl Rect {
    pub const fn new(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
    pub fn area(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            (self.x1 - self.x0) as u64 * (self.y1 - self.y0) as u64
        }
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        Rect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }
    /// The smallest rectangle holding both (an empty one adds nothing).
    pub fn bound(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    /// They overlap or share part of an edge.
    pub fn touches(&self, o: &Rect) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) >= self.x0 && (x as u32) < self.x1 && (y as u32) >= self.y0 && (y as u32) < self.y1
    }
}

/// What has changed since the last composition.
/// A power of two: the table is indexed modulo it, so no index can be out
/// of range (and no bounds check is compiled in).
pub const DAMAGE_RECTS: usize = 8;
const SLOT: usize = DAMAGE_RECTS - 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct Damage {
    rects: [Rect; DAMAGE_RECTS],
    n: usize,
}

impl Damage {
    pub const fn new() -> Damage {
        Damage { rects: [Rect::new(0, 0, 0, 0); DAMAGE_RECTS], n: 0 }
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    pub fn rects(&self) -> &[Rect] {
        self.rects.get(..self.n).unwrap_or(&[])
    }
    pub fn len(&self) -> usize {
        self.n.min(DAMAGE_RECTS)
    }
    /// Rectangle `i` (an empty one past the last).
    pub fn rect(&self, i: usize) -> Rect {
        if i < self.n {
            self.rects[i & SLOT]
        } else {
            Rect::new(0, 0, 0, 0)
        }
    }
    pub fn clear(&mut self) {
        self.n = 0;
    }
    pub fn area(&self) -> u64 {
        self.rects().iter().map(Rect::area).sum()
    }
    /// Add `r`. Rectangles that overlap or touch become one; more separate
    /// changes than the table holds become their bounding rectangle.
    pub fn add(&mut self, r: Rect) {
        let mut r = r.intersect(&SCREEN);
        if r.is_empty() {
            return;
        }
        loop {
            // Swallow everything the (growing) rectangle touches.
            let mut i = 0;
            let mut grew = false;
            while i < self.n {
                if self.rects[i & SLOT].touches(&r) {
                    r = r.bound(&self.rects[i & SLOT]);
                    self.n -= 1;
                    // The last one moves into its place (word by word: the
                    // two may be the same slot).
                    let l = &self.rects[self.n & SLOT];
                    let (x0, y0, x1, y1) = (l.x0, l.y0, l.x1, l.y1);
                    let d = &mut self.rects[i & SLOT];
                    (d.x0, d.y0, d.x1, d.y1) = (x0, y0, x1, y1);
                    grew = true;
                } else {
                    i += 1;
                }
            }
            if grew {
                continue;
            }
            if self.n < DAMAGE_RECTS {
                self.rects[self.n & SLOT] = r;
                self.n += 1;
                return;
            }
            // Full: one rectangle around them all.
            for i in 0..self.n {
                r = r.bound(&self.rects[i & SLOT]);
            }
            self.n = 0;
        }
    }
    /// Everything damaged, leaving nothing.
    pub fn take(&mut self) -> Damage {
        core::mem::replace(self, Damage::new())
    }
}

/// Why a submission changed nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// Not the client in front.
    NotFront,
    /// The screen is off.
    Off,
    /// System UI covers the screen.
    Covered,
    /// Not a rectangle of the client's surface.
    Invalid,
    /// Only system rows: nothing of it may be shown.
    SystemRows,
}

/// Where a touch goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// The compositor's own UI (band, lock screen, passcode pad, prompt).
    System,
    Client(u32),
    Nowhere,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grab {
    None,
    System,
    Client(u32),
    /// A client's gesture that system UI interrupted: the rest of it reaches
    /// no one (system UI must never be handed the tail of an app's gesture).
    Dead,
}

#[derive(Clone, Copy, Debug)]
pub struct Scene {
    /// The client in front (0: none), and how many screen pixels one pixel
    /// of its surface covers, across and down.
    front: u32,
    scale: (u32, u32),
    /// Rows at the top that are the system's.
    band: u32,
    covered: bool,
    on: bool,
    grab: Grab,
    pub damage: Damage,
}

impl Scene {
    pub const fn new() -> Scene {
        Scene { front: 0, scale: (1, 1), band: 0, covered: false, on: true, grab: Grab::None, damage: Damage::new() }
    }
    pub fn front(&self) -> u32 {
        self.front
    }
    pub fn covered(&self) -> bool {
        self.covered
    }
    pub fn is_on(&self) -> bool {
        self.on
    }
    /// The rows a client may change now (empty while covered or off).
    pub fn client_region(&self) -> Rect {
        if self.covered || !self.on {
            Rect::default()
        } else {
            Rect::new(0, self.band.min(H), W, H)
        }
    }
    /// The rows only the system draws.
    pub fn system_region(&self) -> Rect {
        if self.covered {
            SCREEN
        } else {
            Rect::new(0, 0, W, self.band.min(H))
        }
    }

    /// `client` is in front now; each pixel of its surface is shown sx
    /// screen pixels wide and sy high (1 to MAX_SCALE). Err, changing
    /// nothing, for any other scale. The client's rows
    /// are damaged: they show someone else's pixels. A gesture in progress
    /// on the client that was in front reaches no one from here (`Some(old)`:
    /// tell it the gesture is cancelled).
    pub fn set_front(&mut self, client: u32, sx: u32, sy: u32) -> Result<Option<u32>, Refusal> {
        let scale = if client == 0 { (1, 1) } else { (sx, sy) };
        if scale.0 == 0 || scale.1 == 0 || scale.0 > MAX_SCALE || scale.1 > MAX_SCALE {
            return Err(Refusal::Invalid);
        }
        if client == self.front && scale == self.scale {
            return Ok(None);
        }
        let changed = client != self.front;
        self.front = client;
        self.scale = scale;
        // The whole client region shows someone else's pixels now.
        let all = self.client_region();
        self.damage.add(all);
        Ok(if changed { self.cancel_client_gesture() } else { None })
    }

    /// The system keeps the top `rows` rows (the band).
    pub fn set_band(&mut self, rows: u32) {
        let rows = rows.min(H);
        if rows != self.band {
            let changed = Rect::new(0, self.band.min(rows), W, self.band.max(rows));
            self.band = rows;
            self.damage.add(changed);
        }
    }

    /// System UI covers the screen (true) or has gone (false). Covering
    /// cancels a client's gesture in progress (`Some(client)`); uncovering
    /// damages the client's rows, which the client must be shown again.
    pub fn set_cover(&mut self, on: bool) -> Option<u32> {
        if on == self.covered {
            return None;
        }
        self.covered = on;
        self.damage.add(Rect::new(0, self.band.min(H), W, H));
        if on {
            self.cancel_client_gesture()
        } else {
            None
        }
    }

    /// The screen went off or came on. Off drops every gesture.
    pub fn set_power(&mut self, on: bool) {
        if on != self.on {
            self.on = on;
            self.damage.add(SCREEN);
            if !on {
                self.grab = Grab::None;
            }
        }
    }

    fn cancel_client_gesture(&mut self) -> Option<u32> {
        match self.grab {
            Grab::Client(c) => {
                self.grab = Grab::Dead;
                Some(c)
            }
            _ => None,
        }
    }

    /// `client` says rectangle `r` of its surface (in its own pixels) has
    /// changed. Ok: the screen rectangle to compose from it, already added
    /// to `damage`; it never includes a system row.
    pub fn submit(&mut self, client: u32, r: Rect) -> Result<Rect, Refusal> {
        if client == 0 || client != self.front {
            return Err(Refusal::NotFront);
        }
        // Multiplied, not divided: the components have no divide instruction
        // to rely on. Coordinates beyond the screen cannot overflow this.
        let (sx, sy) = self.scale;
        if r.is_empty() || r.x1 > W || r.y1 > H || r.x1 * sx > W || r.y1 * sy > H {
            return Err(Refusal::Invalid);
        }
        if !self.on {
            return Err(Refusal::Off);
        }
        if self.covered {
            return Err(Refusal::Covered);
        }
        let screen = Rect::new(r.x0 * sx, r.y0 * sy, r.x1 * sx, r.y1 * sy).intersect(&self.client_region());
        if screen.is_empty() {
            return Err(Refusal::SystemRows);
        }
        self.damage.add(screen);
        Ok(screen)
    }

    /// The part of `r` that is the system's to draw (empty if none of it is).
    pub fn system_rect(&self, r: Rect) -> Rect {
        let r = r.intersect(&self.system_region());
        if r.is_empty() {
            Rect::default()
        } else {
            r
        }
    }

    /// How many screen pixels one of the front client's pixels covers.
    pub fn scale(&self) -> (u32, u32) {
        self.scale
    }

    /// Where a touch report goes. `synthetic`: made by software (a test's
    /// `bench`), not read from the panel. `trusted`: the software was the
    /// developer at the cable, not an app.
    pub fn route(&mut self, x: i32, y: i32, down: bool, synthetic: bool, trusted: bool) -> Route {
        if synthetic {
            // Never to system UI, never while it is up or holds a finger,
            // never from an app, and never onto the band.
            let ok = trusted && self.on && !self.covered && self.grab != Grab::System && self.front != 0 && self.client_region().contains(x, y);
            return if ok { Route::Client(self.front) } else { Route::Nowhere };
        }
        if self.grab == Grab::None {
            if !down {
                return Route::Nowhere;
            }
            // A finger landing decides who has the whole gesture.
            self.grab = if !self.on || self.covered || self.front == 0 || !self.client_region().contains(x, y) {
                Grab::System
            } else {
                Grab::Client(self.front)
            };
        }
        let to = match self.grab {
            Grab::System => Route::System,
            Grab::Client(c) => Route::Client(c),
            _ => Route::Nowhere,
        };
        if !down {
            self.grab = Grab::None;
        }
        to
    }
}

/// What compositions cost, against a budget for one frame. Times are kept
/// in ticks of the board's counter, as they are read (no division on the
/// way); `ticks_per_ms` is published with them for whoever reads the record.
pub const FRAME_BUDGET_MS: u32 = 8;
pub const STATS_MAGIC: u32 = 0x5350_4d43; // "CMPS"
pub const STATS_WORDS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub ticks_per_ms: u32,
    /// Compositions measured, and how many took longer than the budget.
    pub frames: u32,
    pub over: u32,
    pub last: u32,
    pub max: u32,
    /// All of them together (it stops counting when the word is full).
    pub total: u32,
    /// Screen pixels written.
    pub pixels: u64,
}

impl Budget {
    pub const fn new() -> Budget {
        Budget { ticks_per_ms: 0, frames: 0, over: 0, last: 0, max: 0, total: 0, pixels: 0 }
    }
    /// The budget for one composition, in ticks.
    pub fn budget(&self) -> u32 {
        FRAME_BUDGET_MS.saturating_mul(self.ticks_per_ms)
    }
    /// A composition ran from counter reading t0 to t1 (the counter wraps).
    pub fn record(&mut self, t0: u32, t1: u32, ticks_per_ms: u32, pixels: u64) {
        let ticks = t1.wrapping_sub(t0);
        self.ticks_per_ms = ticks_per_ms;
        self.frames = self.frames.wrapping_add(1);
        self.over += (ticks > self.budget()) as u32;
        self.last = ticks;
        self.max = self.max.max(ticks);
        self.total = self.total.saturating_add(ticks);
        self.pixels += pixels;
    }
    /// The record published after the frame (display.rs `STATS_AT`): magic,
    /// ticks a millisecond, frames, frames over budget, the last, the worst
    /// and the total time in ticks (the mean is the total over the frames),
    /// and the pixels written, in units of 1024.
    pub fn words(&self) -> [u32; STATS_WORDS] {
        [STATS_MAGIC, self.ticks_per_ms, self.frames, self.over, self.last, self.max, self.total, (self.pixels >> 10) as u32]
    }
}

/// Ticks as microseconds, for reports (not used on the way to the screen).
pub fn ticks_us(ticks: u32, ticks_per_ms: u32) -> u32 {
    let per = ticks_per_ms.max(1);
    (ticks / per).saturating_mul(1000).saturating_add((ticks % per).saturating_mul(1000) / per)
}
