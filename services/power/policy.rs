//! Power policy: wake locks, the suspend decision, the thermal and frequency
//! governors, shared-resource gating and a battery estimate. Pure `core` code
//! with no device access, so `make test-unit` runs it on the host; the board
//! backends (targets/*/power.rs) supply sensors and execute the decisions.
//!
//! Time is milliseconds since boot as a u64 (the backends read the 64-bit
//! counter; the 32-bit `ticks()` wraps in about a minute in QEMU).

// ---------------------------------------------------------------- wake locks

/// What a holder needs kept alive. The screen is not a kind: Display blanks
/// before Power hears about sleep, so Power could not honour it.
pub const KIND_CPU: u8 = 0;
pub const KIND_AUDIO: u8 = 1;
pub const KIND_RADIO: u8 = 2;
pub const KINDS: u8 = 3;
pub const LOCKS: usize = 16;
pub const OWNERS: usize = 16;
/// Owners at or above this are open apps (0x100 | pid): their locks expire.
pub const APP_OWNER: u32 = 0x100;
/// An app's lock lapses unless it is taken again within this time, so an app
/// that forgets (or hangs) cannot keep the phone awake for ever.
pub const APP_LOCK_MS: u64 = 10 * 60 * 1000;
/// With the screen off, an app's locks end at most this long after it went
/// off, however often they are taken again, and none is newly given: an app
/// in the background cannot hold the phone awake unseen for ever. Turning the
/// screen on (the user is there) starts the allowance again.
pub const APP_SCREEN_OFF_MS: u64 = 30 * 60 * 1000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LockError {
    /// Every lock (or every accounting row) is in use by a current holder.
    Full,
    BadKind,
    NotHeld,
    /// An app asked with the screen off: for a lock it does not hold, or
    /// after its screen-off allowance ran out.
    ScreenOff,
}

// How a refused lock request is answered: the reply's op and arg, as
// runtime/protocol.rs numbers them (process.rs checks these against it when
// the service is built, runtime/open/tests/powerctl_host.rs on the host).
// A refusal is never REPLY_PENDING: Runtime gives that to a client whose
// request is still running, and the client asks again.
pub const REPLY_DENIED: u32 = 1;
pub const REPLY_INVALID: u32 = 2;
pub const REPLY_EMPTY: u32 = 4;
pub const REPLY_PENDING: u32 = 5;
pub const REFUSED_FULL: u32 = 1;
pub const REFUSED_SCREEN_OFF: u32 = 2;

impl LockError {
    /// The (op, arg) of the reply.
    pub const fn reply(self) -> (u32, u32) {
        match self {
            LockError::Full => (REPLY_DENIED, REFUSED_FULL),
            LockError::ScreenOff => (REPLY_DENIED, REFUSED_SCREEN_OFF),
            LockError::NotHeld => (REPLY_EMPTY, 0),
            LockError::BadKind => (REPLY_INVALID, 0),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Lock {
    held: bool,
    owner: u32,
    kind: u8,
    since: u64,
    /// 0: no expiry.
    deadline: u64,
}

/// Accounting for one owner, kept after its locks are gone.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Stat {
    pub used: bool,
    pub owner: u32,
    /// Locks this owner holds now.
    pub holding: u8,
    pub acquired: u32,
    /// Locks that lapsed instead of being released.
    pub timeouts: u32,
    /// Time held over all finished locks.
    pub total_ms: u64,
    pub longest_ms: u64,
}

/// A held lock, for reports.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Held {
    pub owner: u32,
    pub kind: u8,
    pub held_ms: u64,
}

pub struct WakeLocks {
    locks: [Lock; LOCKS],
    stats: [Stat; OWNERS],
    /// When the screen went off (None: it is on).
    screen_off: Option<u64>,
}

impl WakeLocks {
    pub const fn new() -> Self {
        Self {
            locks: [Lock { held: false, owner: 0, kind: 0, since: 0, deadline: 0 }; LOCKS],
            stats: [Stat { used: false, owner: 0, holding: 0, acquired: 0, timeouts: 0, total_ms: 0, longest_ms: 0 }; OWNERS],
            screen_off: None,
        }
    }

    /// The screen went off at `since`, or (None) is on again. Locks already
    /// held keep their expiry: at most APP_LOCK_MS past the screen going off.
    pub fn screen(&mut self, off_since: Option<u64>) {
        self.screen_off = off_since;
    }

    fn stat(&mut self, owner: u32) -> Option<&mut Stat> {
        if let Some(i) = self.stats.iter().position(|s| s.used && s.owner == owner) {
            return Some(&mut self.stats[i]);
        }
        // A new owner takes a free row, or the row of whoever holds nothing
        // and has the least time on record.
        let i = match self.stats.iter().position(|s| !s.used) {
            Some(i) => i,
            None => {
                let mut best: Option<usize> = None;
                for (i, s) in self.stats.iter().enumerate() {
                    if s.holding == 0 && best.map_or(true, |b| s.total_ms < self.stats[b].total_ms) {
                        best = Some(i);
                    }
                }
                best?
            }
        };
        self.stats[i] = Stat { used: true, owner, ..Stat::default() };
        Some(&mut self.stats[i])
    }

    /// Take (or renew) `owner`'s lock of `kind`. One lock per owner and kind:
    /// taking it again only restarts its expiry.
    pub fn acquire(&mut self, owner: u32, kind: u8, now: u64) -> Result<(), LockError> {
        if kind >= KINDS {
            return Err(LockError::BadKind);
        }
        self.expire(now);
        let mut deadline = if owner >= APP_OWNER { now + APP_LOCK_MS } else { 0 };
        let held = self.locks.iter().position(|l| l.held && l.owner == owner && l.kind == kind);
        if let (true, Some(off)) = (owner >= APP_OWNER, self.screen_off) {
            let end = off.saturating_add(APP_SCREEN_OFF_MS);
            if held.is_none() || now >= end {
                return Err(LockError::ScreenOff);
            }
            deadline = deadline.min(end);
        }
        if let Some(i) = held {
            self.locks[i].deadline = deadline;
            return Ok(());
        }
        let Some(free) = self.locks.iter().position(|l| !l.held) else { return Err(LockError::Full) };
        let Some(stat) = self.stat(owner) else { return Err(LockError::Full) };
        stat.holding += 1;
        stat.acquired = stat.acquired.saturating_add(1);
        self.locks[free] = Lock { held: true, owner, kind, since: now, deadline };
        Ok(())
    }

    fn finish(&mut self, index: usize, now: u64, timed_out: bool) {
        let lock = self.locks[index];
        self.locks[index].held = false;
        let end = if timed_out { lock.deadline } else { now };
        let held = end.saturating_sub(lock.since);
        if let Some(s) = self.stats.iter_mut().find(|s| s.used && s.owner == lock.owner) {
            s.holding = s.holding.saturating_sub(1);
            s.total_ms = s.total_ms.saturating_add(held);
            s.longest_ms = s.longest_ms.max(held);
            if timed_out {
                s.timeouts = s.timeouts.saturating_add(1);
            }
        }
    }

    /// Only the owner that took a lock can release it.
    pub fn release(&mut self, owner: u32, kind: u8, now: u64) -> Result<(), LockError> {
        self.expire(now);
        match self.locks.iter().position(|l| l.held && l.owner == owner && l.kind == kind) {
            Some(i) => {
                self.finish(i, now, false);
                Ok(())
            }
            None => Err(LockError::NotHeld),
        }
    }

    /// The owner ended (an app exited, a service restarted): drop all it held.
    pub fn release_owner(&mut self, owner: u32, now: u64) -> u32 {
        self.expire(now);
        let mut n = 0;
        for i in 0..LOCKS {
            if self.locks[i].held && self.locks[i].owner == owner {
                self.finish(i, now, false);
                n += 1;
            }
        }
        n
    }

    /// Drop locks past their expiry; returns how many lapsed.
    pub fn expire(&mut self, now: u64) -> u32 {
        let mut n = 0;
        for i in 0..LOCKS {
            let l = self.locks[i];
            if l.held && l.deadline != 0 && now >= l.deadline {
                self.finish(i, now, true);
                n += 1;
            }
        }
        n
    }

    /// One bit per kind that some holder needs (after dropping lapsed locks).
    pub fn blockers(&mut self, now: u64) -> u32 {
        self.expire(now);
        self.locks.iter().filter(|l| l.held).fold(0, |bits, l| bits | 1 << l.kind)
    }

    pub fn count(&self) -> u32 {
        self.locks.iter().filter(|l| l.held).count() as u32
    }

    /// Milliseconds until `owner`'s lock of `kind` lapses: what Power tells
    /// an app that took or renewed one, so the Open Runtime's record of it
    /// ends when this one does (a renewal with the screen off ends at the
    /// screen-off bound, not ten minutes on). 0: not held, or no expiry.
    pub fn remaining(&self, owner: u32, kind: u8, now: u64) -> u32 {
        self.locks
            .iter()
            .find(|l| l.held && l.owner == owner && l.kind == kind && l.deadline > now)
            .map_or(0, |l| (l.deadline - now).min(u64::from(u32::MAX)) as u32)
    }

    /// The `index`th held lock.
    pub fn held(&self, index: usize, now: u64) -> Option<Held> {
        let l = self.locks.iter().filter(|l| l.held).nth(index)?;
        Some(Held { owner: l.owner, kind: l.kind, held_ms: now.saturating_sub(l.since) })
    }

    /// The `index`th owner's accounting. Time in locks still held is added in.
    pub fn owner_stat(&self, index: usize, now: u64) -> Option<Stat> {
        let mut s = *self.stats.iter().filter(|s| s.used).nth(index)?;
        for l in self.locks.iter().filter(|l| l.held && l.owner == s.owner) {
            let held = now.saturating_sub(l.since);
            s.total_ms = s.total_ms.saturating_add(held);
            s.longest_ms = s.longest_ms.max(held);
        }
        Some(s)
    }
}

// ------------------------------------------------------------------- suspend

/// What Power does with a sleep, wake or lock change.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SleepStep {
    /// Nothing for the board to do.
    Nothing,
    /// Tell the board to suspend now.
    Suspend,
    /// Tell the board the system is awake.
    Wake,
}

/// The screen is off (Runtime asked for sleep); the board suspends once no
/// wake lock needs the CPU. Without locks this is exactly the old behaviour.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Sleep {
    pub wanted: bool,
    pub suspended: bool,
}

impl Sleep {
    pub const fn new() -> Self {
        Self { wanted: false, suspended: false }
    }
    /// Runtime's sleep command.
    pub fn request(&mut self, blockers: u32) -> SleepStep {
        self.wanted = true;
        self.settle(blockers)
    }
    /// Runtime's wake command. Always passed on: the board's own state (the
    /// emulator's register) must clear even if the suspend never happened.
    pub fn wake(&mut self) -> SleepStep {
        self.wanted = false;
        self.suspended = false;
        SleepStep::Wake
    }
    /// Locks changed (released, lapsed, owner gone): suspend if that was
    /// all the sleep was waiting for.
    pub fn settle(&mut self, blockers: u32) -> SleepStep {
        if self.wanted && !self.suspended && blockers == 0 {
            self.suspended = true;
            return SleepStep::Suspend;
        }
        SleepStep::Nothing
    }
    pub fn deferred(&self) -> bool {
        self.wanted && !self.suspended
    }
}

// -------------------------------------------------------------- wake sources

/// What ended (or may end) a suspend: one bit each. The power key and a touch
/// mean the user is there, and the screen should come back; an alarm or the
/// network only needs the CPU for a while.
pub const WAKE_KEY: u32 = 1;
pub const WAKE_TOUCH: u32 = 2;
pub const WAKE_TIMER: u32 = 4;
pub const WAKE_NETWORK: u32 = 8;
pub const WAKE_USER: u32 = WAKE_KEY | WAKE_TOUCH;
/// After a wake the system stays up this long before it may suspend again,
/// so whatever woke it can be dealt with (and take a wake lock if it needs
/// longer). A user's wake that nothing turns into a screen-on ends the same way.
pub const LINGER_MS: u64 = 5_000;
/// How often Power looks at the wake lines while suspended.
pub const WAKE_POLL_MS: u64 = 100;
/// How often it runs while the screen is off, a lock is held or a sensor
/// needs sampling.
pub const HOUSEKEEPING_MS: u64 = 1_000;

/// What Power asks of whoever drives the screen (the launcher asks Runtime,
/// which alone can blank and unblank): nothing, wake, or sleep.
pub const WANT_NOTHING: u8 = 0;
pub const WANT_WAKE: u8 = 1;
pub const WANT_SLEEP: u8 = 2;

// ------------------------------------------------------------------- thermal

pub const LEVEL_NORMAL: u8 = 0;
pub const LEVEL_WARM: u8 = 1;
pub const LEVEL_HOT: u8 = 2;
pub const LEVEL_CRITICAL: u8 = 3;
/// A level is kept at least this long before it may fall.
pub const DWELL_MS: u64 = 5_000;
/// This many failed or implausible readings in a row count as hot.
pub const FAIL_LIMIT: u8 = 3;
/// Readings outside this range (milli-degrees C) are sensor faults.
pub const PLAUSIBLE_MC: core::ops::RangeInclusive<i32> = -40_000..=150_000;

/// Trip points in milli-degrees C: warm < hot < critical.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Zone {
    pub warm: i32,
    pub hot: i32,
    pub critical: i32,
    /// A level falls only once the temperature is this far below its trip.
    pub hysteresis: i32,
}

impl Zone {
    pub const fn valid(&self) -> bool {
        self.warm < self.hot && self.hot < self.critical && self.hysteresis > 0
    }
    fn trip(&self, level: u8) -> i32 {
        match level {
            LEVEL_WARM => self.warm,
            LEVEL_HOT => self.hot,
            _ => self.critical,
        }
    }
    fn level_for(&self, mc: i32) -> u8 {
        if mc >= self.critical {
            LEVEL_CRITICAL
        } else if mc >= self.hot {
            LEVEL_HOT
        } else if mc >= self.warm {
            LEVEL_WARM
        } else {
            LEVEL_NORMAL
        }
    }
}

pub struct Thermal {
    pub zone: Zone,
    pub level: u8,
    /// When the level last changed.
    since: u64,
    fails: u8,
    /// The last good reading.
    pub last_mc: Option<i32>,
}

impl Thermal {
    pub const fn new(zone: Zone) -> Self {
        Self { zone, level: LEVEL_NORMAL, since: 0, fails: 0, last_mc: None }
    }

    /// Feed one reading (None: the sensor did not answer). The level rises
    /// at once, falls one step at a time after the dwell and only below the
    /// trip minus hysteresis, and never falls on a failed reading. Repeated
    /// failures are treated as hot, not as cool; they never shut the phone
    /// down by themselves.
    pub fn sample(&mut self, reading: Option<i32>, now: u64) -> u8 {
        let before = self.level;
        match reading.filter(|mc| PLAUSIBLE_MC.contains(mc)) {
            Some(mc) => {
                self.fails = 0;
                self.last_mc = Some(mc);
                let target = self.zone.level_for(mc);
                if target > self.level {
                    self.level = target;
                } else if self.level > LEVEL_NORMAL
                    && mc < self.zone.trip(self.level) - self.zone.hysteresis
                    && now.saturating_sub(self.since) >= DWELL_MS
                {
                    self.level -= 1;
                }
            }
            None => {
                self.fails = self.fails.saturating_add(1);
                if self.fails >= FAIL_LIMIT && self.level < LEVEL_HOT {
                    self.level = LEVEL_HOT;
                }
            }
        }
        if self.level != before {
            self.since = now;
        }
        self.level
    }

    pub fn sensor_failed(&self) -> bool {
        self.fails >= FAIL_LIMIT
    }

    /// The highest operating point allowed at this level, for a table of
    /// `opps` points sorted slowest first.
    pub fn opp_cap(&self, opps: usize) -> usize {
        let top = opps.saturating_sub(1);
        match self.level {
            LEVEL_NORMAL => top,
            LEVEL_WARM => top - top / 3,
            LEVEL_HOT => top / 3,
            _ => 0,
        }
    }

    /// At critical the board must power off.
    pub fn must_shut_down(&self) -> bool {
        self.level >= LEVEL_CRITICAL
    }
}

// ---------------------------------------------------------------------- DVFS

/// An operating point: CPU frequency and the supply it needs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Opp {
    pub khz: u32,
    pub uv: u32,
}

/// One step of a change. The order is the safety property: raise the supply
/// before the frequency, lower the frequency before the supply.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DvfsStep {
    Voltage(u32),
    Frequency(u32),
}

/// A table is usable if it is sorted slowest first and a faster point never
/// asks for less voltage.
pub fn opps_valid(table: &[Opp]) -> bool {
    !table.is_empty()
        && table.iter().all(|o| o.khz != 0 && o.uv != 0)
        && table.windows(2).all(|w| w[0].khz < w[1].khz && w[0].uv <= w[1].uv)
}

/// The slowest point with a quarter more capacity than the load needs (as
/// Linux's schedutil does), no higher than `cap`.
pub fn opp_for_load(table: &[Opp], load_percent: u32, cap: usize) -> usize {
    let top = table.len().saturating_sub(1);
    let cap = cap.min(top);
    let need = table[top].khz as u64 * load_percent.min(100) as u64 * 5 / 400;
    let mut pick = cap;
    for (i, o) in table.iter().enumerate().take(cap + 1) {
        if o.khz as u64 >= need {
            pick = i;
            break;
        }
    }
    pick
}

/// The steps to go from point `from` to `to`, in the order to apply them.
pub fn dvfs_plan(table: &[Opp], from: usize, to: usize) -> [Option<DvfsStep>; 2] {
    let (a, b) = (table[from], table[to]);
    if to > from {
        [Some(DvfsStep::Voltage(b.uv)).filter(|_| b.uv != a.uv), Some(DvfsStep::Frequency(b.khz))]
    } else if to < from {
        [Some(DvfsStep::Frequency(b.khz)), Some(DvfsStep::Voltage(b.uv)).filter(|_| b.uv != a.uv)]
    } else {
        [None, None]
    }
}

/// How long the load must stay low before the frequency follows it down.
pub const DOWN_HOLD_MS: u64 = 500;

pub struct Dvfs {
    pub current: usize,
    /// When a slower point than the current one was first enough.
    low_since: Option<u64>,
}

impl Dvfs {
    /// `current` is the point the board runs at now.
    pub const fn new(current: usize) -> Self {
        Self { current, low_since: None }
    }

    /// The point to move to, if any. Up at once; down after the load has
    /// stayed low for DOWN_HOLD_MS; a thermal cap below the current point
    /// applies at once.
    pub fn decide(&mut self, table: &[Opp], load_percent: u32, cap: usize, now: u64) -> Option<usize> {
        let current = self.current.min(table.len() - 1);
        let want = opp_for_load(table, load_percent, cap);
        let cap = cap.min(table.len() - 1);
        let target = if want > current || current > cap {
            self.low_since = None;
            want.max(current.min(cap)).min(cap)
        } else if want < current {
            match self.low_since {
                Some(t) if now.saturating_sub(t) >= DOWN_HOLD_MS => {
                    self.low_since = None;
                    want
                }
                Some(_) => current,
                None => {
                    self.low_since = Some(now);
                    current
                }
            }
        } else {
            self.low_since = None;
            current
        };
        if target == self.current {
            return None;
        }
        Some(target)
    }
}

// -------------------------------------------------------------------- gating

pub const RESOURCES: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GateError {
    BadResource,
    BadOwner,
    /// This owner already holds the resource.
    AlreadyHeld,
    NotHeld,
}

/// What the caller must do to the hardware after a request or release.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    /// Other users remain (or already had it on): leave the hardware alone.
    None,
    /// First user: switch it on.
    On,
    /// Last user gone: switch it off (restore what was inherited).
    Off,
}

/// Users of shared supplies and clocks, one bit per component ID. The
/// hardware changes only on the first request and the last release, so one
/// consumer cannot switch off what another still uses.
pub struct Gates {
    users: [u32; RESOURCES],
}

impl Gates {
    pub const fn new() -> Self {
        Self { users: [0; RESOURCES] }
    }
    fn check(resource: usize, owner: u32) -> Result<u32, GateError> {
        if resource >= RESOURCES {
            return Err(GateError::BadResource);
        }
        if owner >= 32 {
            return Err(GateError::BadOwner);
        }
        Ok(1 << owner)
    }
    pub fn request(&mut self, resource: usize, owner: u32) -> Result<Edge, GateError> {
        let bit = Self::check(resource, owner)?;
        let users = self.users[resource];
        if users & bit != 0 {
            return Err(GateError::AlreadyHeld);
        }
        self.users[resource] = users | bit;
        Ok(if users == 0 { Edge::On } else { Edge::None })
    }
    pub fn release(&mut self, resource: usize, owner: u32) -> Result<Edge, GateError> {
        let bit = Self::check(resource, owner)?;
        let users = self.users[resource];
        if users & bit == 0 {
            return Err(GateError::NotHeld);
        }
        self.users[resource] = users & !bit;
        Ok(if users == bit { Edge::Off } else { Edge::None })
    }
    /// The owner ended: one bit per resource that must now be switched off.
    pub fn release_owner(&mut self, owner: u32) -> u32 {
        let mut off = 0;
        for r in 0..RESOURCES {
            if self.release(r, owner) == Ok(Edge::Off) {
                off |= 1 << r;
            }
        }
        off
    }
    pub fn users(&self, resource: usize) -> u32 {
        self.users.get(resource).copied().unwrap_or(0)
    }
    /// One bit per resource in use.
    pub fn active(&self) -> u32 {
        self.users.iter().enumerate().fold(0, |bits, (i, u)| bits | ((*u != 0) as u32) << i)
    }
}

/// A register bus (the PMIC wrapper on the handset, a recorder in tests).
pub trait Regs {
    fn read(&mut self, addr: u16) -> Option<u16>;
    fn write(&mut self, addr: u16, value: u16) -> Option<()>;
    /// Wait for a supply to settle after one field of a sequence.
    fn settle(&mut self);
}

/// One masked register field of a power-up sequence and its "on" value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Field {
    pub addr: u16,
    pub mask: u16,
    pub on: u16,
}

/// The most fields one sequence may have.
pub const FIELDS: usize = 8;

/// An ordered power-up sequence that remembers what each field held before,
/// so switching off restores the inherited state (a rail the bootloader left
/// on stays on) in reverse order. A failure part-way leaves `count` at the
/// fields already touched, and `restore` undoes exactly those.
pub struct Sequence {
    saved: [u16; FIELDS],
    count: usize,
}

impl Sequence {
    pub const fn new() -> Self {
        Self { saved: [0; FIELDS], count: 0 }
    }
    /// Fields switched on and not yet restored.
    pub fn active(&self) -> usize {
        self.count
    }
    pub fn enable(&mut self, fields: &[Field], regs: &mut impl Regs) -> Option<()> {
        if self.count != 0 || fields.len() > FIELDS {
            return None;
        }
        for (i, f) in fields.iter().enumerate() {
            let original = regs.read(f.addr)?;
            self.saved[i] = original & f.mask;
            self.count = i + 1;
            if original & f.mask != f.on {
                regs.write(f.addr, (original & !f.mask) | f.on)?;
                if regs.read(f.addr)? & f.mask != f.on {
                    return None;
                }
            }
            regs.settle();
        }
        Some(())
    }
    pub fn restore(&mut self, fields: &[Field], regs: &mut impl Regs) -> Option<()> {
        while self.count != 0 {
            let i = self.count - 1;
            let f = fields.get(i)?;
            let original = self.saved[i];
            let now = regs.read(f.addr)?;
            if now & f.mask != original {
                regs.write(f.addr, (now & !f.mask) | original)?;
                if regs.read(f.addr)? & f.mask != original {
                    return None;
                }
            }
            self.count = i;
        }
        Some(())
    }
}

// ------------------------------------------------------------------- battery

/// Resting voltage of one lithium-ion cell against charge, in (mV, percent).
/// A generic curve, not measured on any particular battery, and with no
/// correction for load or temperature: an estimate for a status line.
const CELL_CURVE: [(u32, u32); 11] = [
    (3300, 0), (3600, 5), (3700, 15), (3750, 25), (3790, 35), (3830, 45),
    (3870, 55), (3920, 65), (3980, 75), (4080, 87), (4200, 100),
];

/// An estimated charge for a cell voltage, or None for a reading no single
/// cell produces (a failed or disconnected measurement).
pub fn percent_from_millivolts(mv: u32) -> Option<u32> {
    if !(2500..=4600).contains(&mv) {
        return None;
    }
    let (low, high) = (CELL_CURVE[0], CELL_CURVE[CELL_CURVE.len() - 1]);
    if mv <= low.0 {
        return Some(0);
    }
    if mv >= high.0 {
        return Some(100);
    }
    for w in CELL_CURVE.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if mv < v1 {
            return Some(p0 + (mv - v0) * (p1 - p0) / (v1 - v0));
        }
    }
    Some(100)
}

// ------------------------------------------------------------------- manager

/// What a board's temperature sensor said.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reading {
    /// The board has no sensor Power can read: thermal control is off and
    /// reported as unsupported, never as "cool".
    Absent,
    /// There is a sensor and it did not answer.
    Failed,
    Ok(i32),
}

/// What Power last decided, for the board to show and for status queries.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Snapshot {
    pub sensor: bool,
    pub sensor_failed: bool,
    pub temperature_mc: i32,
    pub level: u8,
    /// False when the board has no operating-point table.
    pub dvfs: bool,
    pub opp: u32,
    pub khz: u32,
    pub uv: u32,
    pub screen_on: bool,
    pub sleep_deferred: bool,
    pub blockers: u32,
    pub locks: u32,
    /// Suspended to idle now.
    pub suspended: bool,
    /// WANT_*: what the screen's driver should ask Runtime for.
    pub want: u8,
    /// WAKE_* bits of the last wake, and how often the board was suspended.
    pub last_wake: u32,
    pub suspends: u32,
}

/// The hardware side of the decisions (targets/*/power.rs).
pub trait Board {
    fn temperature(&mut self) -> Reading;
    /// CPU load in percent, if the board can measure it.
    fn load_percent(&mut self) -> Option<u32>;
    /// Operating points, slowest first; empty when frequency scaling is not
    /// implemented for the board.
    fn opps(&self) -> &'static [Opp];
    /// Apply one step of a frequency change; false if it did not take.
    fn apply(&mut self, step: DvfsStep) -> bool;
    /// 0 awake, 1 suspend, 2 power off; false if the board refused.
    fn power(&mut self, command: u32) -> bool;
    /// Peripherals that are only needed while the screen is on.
    fn screen_peripherals(&mut self, on: bool);
    fn publish(&mut self, snapshot: &Snapshot);
    /// Whether `power(1)` really suspends and the two calls below can end
    /// it. False (the handset today): a sleep only blanks the screen, and
    /// Power neither watches wake lines nor suspends again by itself.
    const SUSPENDS: bool = false;
    /// Wake events since the last call, as WAKE_* bits: the power key, a
    /// touch, the network. 0 on a board that can see none of them.
    fn wake_events(&mut self) -> u32 {
        0
    }
    /// A wake alarm the board was asked for since the last call, in
    /// milliseconds from now.
    fn take_alarm(&mut self) -> Option<u64> {
        None
    }
}

/// Thermal samples are taken at most this often.
pub const SAMPLE_MS: u64 = 1_000;

pub struct Manager {
    pub locks: WakeLocks,
    pub sleep: Sleep,
    pub thermal: Thermal,
    pub dvfs: Dvfs,
    pub gates: Gates,
    sensor: bool,
    sampled: Option<u64>,
    screen_on: bool,
    off_sent: bool,
    /// No suspend before this time (a wake's linger).
    linger_until: u64,
    /// A wake alarm, in milliseconds since boot.
    alarm: Option<u64>,
    want: u8,
    last_wake: u32,
    suspends: u32,
}

impl Manager {
    /// `opp` is the operating point the board starts at.
    pub const fn new(zone: Zone, opp: usize) -> Self {
        Self {
            locks: WakeLocks::new(),
            sleep: Sleep::new(),
            thermal: Thermal::new(zone),
            dvfs: Dvfs::new(opp),
            gates: Gates::new(),
            sensor: false,
            sampled: None,
            screen_on: true,
            off_sent: false,
            linger_until: 0,
            alarm: None,
            want: WANT_NOTHING,
            last_wake: 0,
            suspends: 0,
        }
    }

    fn settle(&mut self, board: &mut impl Board, now: u64) {
        let blockers = self.locks.blockers(now);
        if self.off_sent || now < self.linger_until {
            return;
        }
        if self.sleep.settle(blockers) == SleepStep::Suspend {
            self.suspend(board);
        }
    }

    /// The suspend decision is made: tell the board. False if it refused;
    /// the next change tries again.
    fn suspend(&mut self, board: &mut impl Board) -> bool {
        if board.power(1) {
            self.suspends = self.suspends.wrapping_add(1);
            // A wake that was never turned into a screen-on is forgotten.
            self.want = WANT_NOTHING;
            true
        } else {
            self.sleep.suspended = false;
            false
        }
    }

    /// Wake at `at` (milliseconds since boot) if the system is suspended
    /// then; None: no alarm.
    pub fn set_alarm(&mut self, at: Option<u64>) {
        self.alarm = at;
    }

    /// The wake sources. Suspended, any of them resumes the CPU; the screen
    /// stays off, and for a user's wake `want` asks its driver to turn it on.
    /// Awake with the screen on, the power key asks for sleep.
    fn wake_sources<B: Board>(&mut self, board: &mut B, now: u64) {
        if !B::SUSPENDS {
            return;
        }
        if let Some(ms) = board.take_alarm() {
            self.alarm = Some(now.saturating_add(ms));
        }
        let mut reason = board.wake_events() & (WAKE_USER | WAKE_NETWORK);
        if self.alarm.is_some_and(|at| now >= at) {
            self.alarm = None;
            // An alarm that passes while the system is up needs no wake.
            if self.sleep.wanted {
                reason |= WAKE_TIMER;
            }
        }
        if reason == 0 || self.off_sent {
            return;
        }
        if !self.sleep.wanted {
            if reason & WAKE_KEY != 0 {
                self.want = WANT_SLEEP;
            }
            return;
        }
        // The screen is off: suspended, or waiting to be.
        self.last_wake = reason;
        self.linger_until = now.saturating_add(LINGER_MS);
        if reason & WAKE_USER != 0 {
            self.want = WANT_WAKE;
        }
        if self.sleep.suspended {
            self.sleep.suspended = false;
            board.power(0);
        }
    }

    /// Milliseconds until Power should run again without a request, or None
    /// when nothing is pending (it then sleeps until one arrives).
    pub fn next_tick<B: Board>(&self, _board: &B, now: u64) -> Option<u64> {
        if self.off_sent {
            return None;
        }
        // u64::MAX: nothing.
        let mut next = u64::MAX;
        if self.sensor || self.locks.count() != 0 {
            next = HOUSEKEEPING_MS;
        }
        if B::SUSPENDS {
            if self.sleep.suspended {
                next = WAKE_POLL_MS;
            } else if self.sleep.wanted && now < self.linger_until {
                // Lingering after a wake: suspend when it ends.
                next = next.min(self.linger_until.saturating_sub(now).clamp(WAKE_POLL_MS, HOUSEKEEPING_MS));
            } else if self.sleep.wanted {
                // Deferred by a lock: a release is a request and settles at
                // once; a lapse needs only the housekeeping second.
                next = next.min(HOUSEKEEPING_MS);
            }
            if let Some(at) = self.alarm {
                next = next.min(at.saturating_sub(now).max(1));
            }
        }
        (next != u64::MAX).then_some(next)
    }

    /// Run the governors. Power has no timer of its own, so this runs when a
    /// request arrives; the dwell and hold times are in real time, so uneven
    /// calls only delay a decision.
    pub fn tick(&mut self, board: &mut impl Board, now: u64) {
        if self.sampled.map_or(true, |t| now.saturating_sub(t) >= SAMPLE_MS) {
            self.sampled = Some(now);
            match board.temperature() {
                Reading::Absent => self.sensor = false,
                Reading::Failed => {
                    self.sensor = true;
                    self.thermal.sample(None, now);
                }
                Reading::Ok(mc) => {
                    self.sensor = true;
                    self.thermal.sample(Some(mc), now);
                }
            }
        }
        let table = board.opps();
        if opps_valid(table) {
            let cap = if self.sensor { self.thermal.opp_cap(table.len()) } else { table.len() - 1 };
            // Without a load figure, run as fast as the temperature allows.
            let load = board.load_percent().unwrap_or(100);
            if let Some(to) = self.dvfs.decide(table, load, cap, now) {
                let from = self.dvfs.current.min(table.len() - 1);
                let mut frequency_changed = false;
                let mut ok = true;
                for step in dvfs_plan(table, from, to).into_iter().flatten() {
                    if !board.apply(step) {
                        ok = false;
                        break;
                    }
                    frequency_changed |= matches!(step, DvfsStep::Frequency(_));
                }
                // The frequency decides which point we are at: a failed
                // voltage step leaves the supply higher than needed (safe),
                // and is retried by the next change.
                if ok || frequency_changed {
                    self.dvfs.current = to;
                }
            }
        }
        if self.sensor && self.thermal.must_shut_down() && !self.off_sent {
            self.off_sent = board.power(2);
        }
        self.wake_sources(board, now);
        self.settle(board, now);
        self.publish(board, now);
    }

    /// Show the board the current state (after a request changed it).
    pub fn publish(&mut self, board: &mut impl Board, now: u64) {
        let snapshot = self.snapshot(board, now);
        board.publish(&snapshot);
    }

    pub fn snapshot(&mut self, board: &impl Board, now: u64) -> Snapshot {
        let table = board.opps();
        let dvfs = opps_valid(table);
        let point = if dvfs { table[self.dvfs.current.min(table.len() - 1)] } else { Opp { khz: 0, uv: 0 } };
        Snapshot {
            sensor: self.sensor,
            sensor_failed: self.sensor && self.thermal.sensor_failed(),
            temperature_mc: self.thermal.last_mc.unwrap_or(0),
            level: if self.sensor { self.thermal.level } else { LEVEL_NORMAL },
            dvfs,
            opp: self.dvfs.current as u32,
            khz: point.khz,
            uv: point.uv,
            screen_on: self.screen_on,
            sleep_deferred: self.sleep.deferred(),
            blockers: self.locks.blockers(now),
            locks: self.locks.count(),
            suspended: self.sleep.suspended,
            want: self.want,
            last_wake: self.last_wake,
            suspends: self.suspends,
        }
    }

    /// Runtime blanked the screen and asks for sleep. Ok(0): suspended.
    /// Ok(bits): the screen is off but these lock kinds keep the CPU up; the
    /// suspend follows when they go. Err: the board refused to suspend, or
    /// is powering off.
    pub fn sleep(&mut self, board: &mut impl Board, now: u64) -> Result<u32, ()> {
        // A power-off for heat is not replaced by a later command: on a
        // board with one power register, sleep or wake would cancel it.
        if self.off_sent {
            return Err(());
        }
        if self.screen_on {
            self.screen_on = false;
            self.locks.screen(Some(now));
            board.screen_peripherals(false);
        }
        // Runtime's own command is not held back by a wake's linger.
        self.linger_until = 0;
        self.want = WANT_NOTHING;
        let blockers = self.locks.blockers(now);
        if self.sleep.request(blockers) == SleepStep::Suspend && !self.suspend(board) {
            return Err(());
        }
        Ok(blockers)
    }

    /// False if the board refused, or is powering off (see `sleep`).
    pub fn wake(&mut self, board: &mut impl Board) -> bool {
        if self.off_sent {
            return false;
        }
        self.sleep.wake();
        self.linger_until = 0;
        self.want = WANT_NOTHING;
        if !self.screen_on {
            self.screen_on = true;
            self.locks.screen(None);
            board.screen_peripherals(true);
        }
        board.power(0)
    }

    /// The board accepted a power-off for heat: nothing else is commanded.
    pub fn powering_off(&self) -> bool {
        self.off_sent
    }

    pub fn lock(&mut self, board: &mut impl Board, owner: u32, kind: u8, on: bool, now: u64) -> Result<(), LockError> {
        let result = if on { self.locks.acquire(owner, kind, now) } else { self.locks.release(owner, kind, now) };
        self.settle(board, now);
        result
    }

    /// An app exited or a service restarted.
    pub fn owner_gone(&mut self, board: &mut impl Board, owner: u32, now: u64) -> u32 {
        let n = self.locks.release_owner(owner, now);
        self.settle(board, now);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: u32 = APP_OWNER | 7;
    const AUDIO: u32 = 22;

    #[test]
    fn lock_acquire_release_accounting() {
        let mut w = WakeLocks::new();
        assert_eq!(w.blockers(0), 0);
        w.acquire(APP, KIND_CPU, 1_000).unwrap();
        w.acquire(AUDIO, KIND_AUDIO, 2_000).unwrap();
        assert_eq!(w.blockers(2_000), 1 << KIND_CPU | 1 << KIND_AUDIO);
        assert_eq!(w.count(), 2);
        assert_eq!(w.held(0, 5_000), Some(Held { owner: APP, kind: KIND_CPU, held_ms: 4_000 }));
        assert_eq!(w.held(2, 5_000), None);
        w.release(APP, KIND_CPU, 6_000).unwrap();
        assert_eq!(w.blockers(6_000), 1 << KIND_AUDIO);
        let s = w.owner_stat(0, 6_000).unwrap();
        assert_eq!((s.owner, s.holding, s.acquired, s.total_ms, s.longest_ms, s.timeouts), (APP, 0, 1, 5_000, 5_000, 0));
        // A held lock's time shows in the report before it is released.
        let s = w.owner_stat(1, 12_000).unwrap();
        assert_eq!((s.owner, s.holding, s.total_ms), (AUDIO, 1, 10_000));
        w.acquire(APP, KIND_CPU, 7_000).unwrap();
        w.release(APP, KIND_CPU, 8_000).unwrap();
        let s = w.owner_stat(0, 8_000).unwrap();
        assert_eq!((s.acquired, s.total_ms, s.longest_ms), (2, 6_000, 5_000));
    }

    #[test]
    fn lock_only_owner_releases() {
        let mut w = WakeLocks::new();
        w.acquire(APP, KIND_CPU, 0).unwrap();
        assert_eq!(w.release(APP + 1, KIND_CPU, 1), Err(LockError::NotHeld));
        assert_eq!(w.release(APP, KIND_AUDIO, 1), Err(LockError::NotHeld));
        assert_eq!(w.acquire(APP, KINDS, 1), Err(LockError::BadKind));
        assert_eq!(w.blockers(1), 1 << KIND_CPU);
        assert_eq!(w.release(APP, KIND_CPU, 2), Ok(()));
        assert_eq!(w.release(APP, KIND_CPU, 3), Err(LockError::NotHeld));
    }

    #[test]
    fn lock_renewal_is_not_a_second_lock() {
        let mut w = WakeLocks::new();
        w.acquire(APP, KIND_CPU, 0).unwrap();
        w.acquire(APP, KIND_CPU, APP_LOCK_MS - 1).unwrap();
        assert_eq!(w.count(), 1);
        // Renewed: still held past the first deadline.
        assert_eq!(w.blockers(APP_LOCK_MS + 5), 1);
        let s = w.owner_stat(0, APP_LOCK_MS + 5).unwrap();
        assert_eq!((s.acquired, s.holding), (1, 1));
        w.release(APP, KIND_CPU, APP_LOCK_MS + 10).unwrap();
        assert_eq!(w.count(), 0);
    }

    #[test]
    fn app_lock_times_out_service_lock_does_not() {
        let mut w = WakeLocks::new();
        w.acquire(APP, KIND_CPU, 100).unwrap();
        w.acquire(AUDIO, KIND_AUDIO, 100).unwrap();
        assert_eq!(w.blockers(100 + APP_LOCK_MS - 1), 0b11);
        // A day later only the service's lock is left.
        assert_eq!(w.blockers(86_400_000), 1 << KIND_AUDIO);
        let s = w.owner_stat(0, 86_400_000).unwrap();
        // The lapsed lock is charged up to its deadline, not to when it was noticed.
        assert_eq!((s.timeouts, s.holding, s.total_ms), (1, 0, APP_LOCK_MS));
        assert_eq!(w.release(APP, KIND_CPU, 86_400_001), Err(LockError::NotHeld));
    }

    #[test]
    fn owner_death_releases_everything_it_held() {
        let mut w = WakeLocks::new();
        w.acquire(APP, KIND_CPU, 0).unwrap();
        w.acquire(APP, KIND_AUDIO, 0).unwrap();
        w.acquire(AUDIO, KIND_AUDIO, 0).unwrap();
        assert_eq!(w.release_owner(APP, 50), 2);
        assert_eq!(w.release_owner(APP, 60), 0);
        assert_eq!(w.blockers(60), 1 << KIND_AUDIO);
        assert_eq!(w.owner_stat(0, 60).unwrap().total_ms, 100);
    }

    #[test]
    fn full_table_is_refused_not_dropped() {
        let mut w = WakeLocks::new();
        for i in 0..LOCKS as u32 {
            w.acquire(APP_OWNER | i, KIND_CPU, 0).unwrap();
        }
        assert_eq!(w.acquire(APP_OWNER | 99, KIND_CPU, 1), Err(LockError::Full));
        assert_eq!(w.count(), LOCKS as u32);
        // A current holder can still renew.
        assert_eq!(w.acquire(APP_OWNER | 3, KIND_CPU, 1), Ok(()));
        w.release(APP_OWNER | 3, KIND_CPU, 2).unwrap();
        assert_eq!(w.acquire(APP_OWNER | 99, KIND_CPU, 3), Ok(()));
    }

    #[test]
    fn accounting_rows_are_recycled_from_idle_owners() {
        let mut w = WakeLocks::new();
        // Many short-lived apps: more owners than rows, none holding.
        for i in 0..100u32 {
            w.acquire(APP_OWNER | i, KIND_CPU, i as u64 * 10).unwrap();
            w.release(APP_OWNER | i, KIND_CPU, i as u64 * 10 + 1 + (i == 5) as u64 * 1000).unwrap();
        }
        assert_eq!(w.count(), 0);
        // The owner with the most time on record survives the recycling.
        let kept = (0..OWNERS).filter_map(|i| w.owner_stat(i, 0)).any(|s| s.owner == APP_OWNER | 5);
        assert!(kept);
        // Rows of current holders are never taken.
        let mut w = WakeLocks::new();
        for i in 0..OWNERS as u32 {
            w.acquire(i + 1, KIND_CPU, 0).unwrap();
        }
        for i in 0..OWNERS {
            assert_eq!(w.owner_stat(i, 0).unwrap().holding, 1);
        }
    }

    #[test]
    fn counters_saturate() {
        let mut w = WakeLocks::new();
        w.acquire(AUDIO, KIND_AUDIO, 0).unwrap();
        w.release(AUDIO, KIND_AUDIO, u64::MAX).unwrap();
        w.acquire(AUDIO, KIND_AUDIO, 0).unwrap();
        w.release(AUDIO, KIND_AUDIO, u64::MAX).unwrap();
        let s = w.owner_stat(0, 0).unwrap();
        assert_eq!((s.total_ms, s.longest_ms), (u64::MAX, u64::MAX));
        // A clock that reads earlier than the acquisition charges nothing.
        w.acquire(APP, KIND_CPU, 500).unwrap();
        w.release(APP, KIND_CPU, 100).unwrap();
        assert_eq!(w.owner_stat(1, 0).unwrap().total_ms, 0);
    }

    #[test]
    fn sleep_waits_for_locks() {
        let mut w = WakeLocks::new();
        let mut s = Sleep::new();
        // No locks: suspend at once, as before wake locks existed.
        assert_eq!(s.request(w.blockers(0)), SleepStep::Suspend);
        assert_eq!(s.settle(0), SleepStep::Nothing); // only once
        assert_eq!(s.wake(), SleepStep::Wake);
        // A lock defers it; the release completes it.
        w.acquire(APP, KIND_CPU, 10).unwrap();
        assert_eq!(s.request(w.blockers(10)), SleepStep::Nothing);
        assert!(s.deferred());
        w.release(APP, KIND_CPU, 20).unwrap();
        assert_eq!(s.settle(w.blockers(20)), SleepStep::Suspend);
        assert!(!s.deferred());
        assert_eq!(s.wake(), SleepStep::Wake);
        // A lapsed lock completes it too; a wake in between cancels it.
        w.acquire(APP, KIND_CPU, 100).unwrap();
        assert_eq!(s.request(w.blockers(100)), SleepStep::Nothing);
        assert_eq!(s.wake(), SleepStep::Wake);
        assert_eq!(s.settle(w.blockers(100 + APP_LOCK_MS)), SleepStep::Nothing);
        w.acquire(APP, KIND_CPU, 200).unwrap();
        assert_eq!(s.request(w.blockers(200)), SleepStep::Nothing);
        assert_eq!(s.settle(w.blockers(200 + APP_LOCK_MS)), SleepStep::Suspend);
        // Releasing a lock while awake never suspends.
        let mut s = Sleep::new();
        assert_eq!(s.settle(0), SleepStep::Nothing);
    }

    const ZONE: Zone = Zone { warm: 60_000, hot: 75_000, critical: 95_000, hysteresis: 5_000 };

    #[test]
    fn thermal_rises_at_once_and_falls_with_hysteresis_and_dwell() {
        assert!(ZONE.valid());
        let mut t = Thermal::new(ZONE);
        assert_eq!(t.sample(Some(40_000), 0), LEVEL_NORMAL);
        assert_eq!(t.sample(Some(59_999), 10), LEVEL_NORMAL);
        assert_eq!(t.sample(Some(60_000), 20), LEVEL_WARM);
        // Straight to hot, skipping nothing on the way up.
        assert_eq!(t.sample(Some(80_000), 30), LEVEL_HOT);
        // Below the trip but inside the hysteresis band: stays.
        assert_eq!(t.sample(Some(72_000), 30 + DWELL_MS), LEVEL_HOT);
        // Far below, but the dwell (restarted by nothing) has passed: one step only.
        assert_eq!(t.sample(Some(30_000), 31 + DWELL_MS), LEVEL_WARM);
        // The next step waits for its own dwell.
        assert_eq!(t.sample(Some(30_000), 32 + DWELL_MS), LEVEL_WARM);
        assert_eq!(t.sample(Some(30_000), 31 + 2 * DWELL_MS), LEVEL_NORMAL);
        // Cooling inside the dwell does nothing.
        assert_eq!(t.sample(Some(76_000), 100_000), LEVEL_HOT);
        assert_eq!(t.sample(Some(20_000), 100_000 + DWELL_MS - 1), LEVEL_HOT);
        assert_eq!(t.sample(Some(20_000), 100_000 + DWELL_MS), LEVEL_WARM);
    }

    #[test]
    fn thermal_does_not_flap_around_a_trip() {
        let mut t = Thermal::new(ZONE);
        let mut changes = 0;
        let mut last = t.level;
        for i in 0..1000u64 {
            let mc = if i % 2 == 0 { 60_100 } else { 59_900 };
            let level = t.sample(Some(mc), i * 1000);
            changes += (level != last) as u32;
            last = level;
        }
        assert_eq!((changes, last), (1, LEVEL_WARM));
    }

    #[test]
    fn thermal_sensor_failure_is_hot_never_cool() {
        let mut t = Thermal::new(ZONE);
        assert_eq!(t.sample(Some(30_000), 0), LEVEL_NORMAL);
        assert_eq!(t.sample(None, 1), LEVEL_NORMAL);
        assert_eq!(t.sample(None, 2), LEVEL_NORMAL);
        assert!(!t.sensor_failed());
        assert_eq!(t.sample(None, 3), LEVEL_HOT);
        assert!(t.sensor_failed());
        assert!(!t.must_shut_down());
        // Implausible readings are failures, not temperatures.
        let mut t = Thermal::new(ZONE);
        for now in 0..3 {
            t.sample(Some(-273_000), now);
        }
        assert_eq!(t.level, LEVEL_HOT);
        let mut t = Thermal::new(ZONE);
        for now in 0..3 {
            t.sample(Some(900_000), now);
        }
        assert_eq!((t.level, t.must_shut_down()), (LEVEL_HOT, false));
        // Failures never lower a level, however long they last.
        let mut t = Thermal::new(ZONE);
        t.sample(Some(96_000), 0);
        for now in 1..100 {
            assert_eq!(t.sample(None, now * DWELL_MS), LEVEL_CRITICAL);
        }
        // A good reading after failures recovers by the normal rules.
        let mut t = Thermal::new(ZONE);
        for now in 0..3 {
            t.sample(None, now);
        }
        assert_eq!(t.sample(Some(30_000), 10), LEVEL_HOT);
        assert_eq!(t.sample(Some(30_000), 10 + DWELL_MS), LEVEL_WARM);
        assert_eq!(t.last_mc, Some(30_000));
    }

    #[test]
    fn thermal_critical_and_caps() {
        let mut t = Thermal::new(ZONE);
        assert_eq!(t.opp_cap(4), 3);
        t.sample(Some(61_000), 0);
        assert_eq!(t.opp_cap(4), 2);
        t.sample(Some(76_000), 1);
        assert_eq!(t.opp_cap(4), 1);
        assert!(!t.must_shut_down());
        t.sample(Some(95_000), 2);
        assert_eq!(t.opp_cap(4), 0);
        assert!(t.must_shut_down());
        // Degenerate tables.
        assert_eq!(t.opp_cap(1), 0);
        assert_eq!(t.opp_cap(0), 0);
        assert_eq!(Thermal::new(ZONE).opp_cap(1), 0);
        assert!(!Zone { warm: 70_000, hot: 60_000, critical: 95_000, hysteresis: 1 }.valid());
    }

    const OPPS: [Opp; 4] = [
        Opp { khz: 600_000, uv: 650_000 },
        Opp { khz: 1_000_000, uv: 750_000 },
        Opp { khz: 1_500_000, uv: 850_000 },
        Opp { khz: 2_000_000, uv: 950_000 },
    ];

    #[test]
    fn opp_selection() {
        assert!(opps_valid(&OPPS));
        assert!(!opps_valid(&[]));
        assert!(!opps_valid(&[OPPS[1], OPPS[0]]));
        assert!(!opps_valid(&[OPPS[0], Opp { khz: 700_000, uv: 600_000 }]));
        assert_eq!(opp_for_load(&OPPS, 0, 3), 0);
        // 20% of 2 GHz with a quarter of headroom is 500 MHz.
        assert_eq!(opp_for_load(&OPPS, 20, 3), 0);
        assert_eq!(opp_for_load(&OPPS, 30, 3), 1);
        assert_eq!(opp_for_load(&OPPS, 60, 3), 2);
        assert_eq!(opp_for_load(&OPPS, 61, 3), 3);
        assert_eq!(opp_for_load(&OPPS, 100, 3), 3);
        assert_eq!(opp_for_load(&OPPS, 250, 3), 3);
        // The thermal cap wins over the load.
        assert_eq!(opp_for_load(&OPPS, 100, 1), 1);
        assert_eq!(opp_for_load(&OPPS, 100, 0), 0);
        assert_eq!(opp_for_load(&OPPS, 100, 99), 3);
    }

    #[test]
    fn dvfs_voltage_leads_up_and_trails_down() {
        assert_eq!(dvfs_plan(&OPPS, 0, 3), [Some(DvfsStep::Voltage(950_000)), Some(DvfsStep::Frequency(2_000_000))]);
        assert_eq!(dvfs_plan(&OPPS, 3, 1), [Some(DvfsStep::Frequency(1_000_000)), Some(DvfsStep::Voltage(750_000))]);
        assert_eq!(dvfs_plan(&OPPS, 2, 2), [None, None]);
        // Points sharing a supply only change frequency.
        let shared = [Opp { khz: 500_000, uv: 700_000 }, Opp { khz: 800_000, uv: 700_000 }];
        assert_eq!(dvfs_plan(&shared, 0, 1), [None, Some(DvfsStep::Frequency(800_000))]);
        assert_eq!(dvfs_plan(&shared, 1, 0), [Some(DvfsStep::Frequency(500_000)), None]);
        // Every transition keeps the supply at or above what the running
        // frequency needs after each step.
        for from in 0..OPPS.len() {
            for to in 0..OPPS.len() {
                let (mut khz, mut uv) = (OPPS[from].khz, OPPS[from].uv);
                for step in dvfs_plan(&OPPS, from, to).into_iter().flatten() {
                    match step {
                        DvfsStep::Voltage(v) => uv = v,
                        DvfsStep::Frequency(f) => khz = f,
                    }
                    let needed = OPPS.iter().find(|o| o.khz == khz).unwrap().uv;
                    assert!(uv >= needed, "{from}->{to}: {khz} kHz at {uv} uV");
                }
                assert_eq!((khz, uv), (OPPS[to].khz, OPPS[to].uv));
            }
        }
    }

    #[test]
    fn dvfs_governor_up_fast_down_slow_cap_at_once() {
        let mut d = Dvfs::new(0);
        assert_eq!(d.decide(&OPPS, 10, 3, 0), None);
        assert_eq!(d.decide(&OPPS, 90, 3, 10), Some(3));
        d.current = 3;
        // Load drops: wait out the hold.
        assert_eq!(d.decide(&OPPS, 10, 3, 20), None);
        assert_eq!(d.decide(&OPPS, 10, 3, 20 + DOWN_HOLD_MS - 1), None);
        assert_eq!(d.decide(&OPPS, 10, 3, 20 + DOWN_HOLD_MS), Some(0));
        // A burst during the hold cancels it.
        assert_eq!(d.decide(&OPPS, 10, 3, 2_000), None);
        assert_eq!(d.decide(&OPPS, 100, 3, 2_100), None);
        assert_eq!(d.decide(&OPPS, 10, 3, 2_000 + DOWN_HOLD_MS), None);
        assert_eq!(d.decide(&OPPS, 10, 3, 2_000 + 2 * DOWN_HOLD_MS), Some(0));
        // Thermal cap below the current point: down immediately, even under full load.
        assert_eq!(d.decide(&OPPS, 100, 1, 5_000), Some(1));
        d.current = 1;
        assert_eq!(d.decide(&OPPS, 100, 1, 5_001), None);
        assert_eq!(d.decide(&OPPS, 100, 0, 5_002), Some(0));
        d.current = 0;
        // Cap lifted: back up with the load.
        assert_eq!(d.decide(&OPPS, 100, 3, 5_003), Some(3));
        // A capped governor under light load still goes no higher than needed.
        let mut d = Dvfs::new(3);
        assert_eq!(d.decide(&OPPS, 0, 2, 0), Some(2));
    }

    #[test]
    fn gates_count_users() {
        let mut g = Gates::new();
        assert_eq!(g.request(0, 7), Ok(Edge::On));
        assert_eq!(g.request(0, 22), Ok(Edge::None));
        assert_eq!(g.request(0, 7), Err(GateError::AlreadyHeld));
        assert_eq!(g.active(), 1);
        assert_eq!(g.release(0, 7), Ok(Edge::None));
        assert_eq!(g.release(0, 7), Err(GateError::NotHeld));
        assert_eq!(g.users(0), 1 << 22);
        assert_eq!(g.release(0, 22), Ok(Edge::Off));
        assert_eq!(g.active(), 0);
        assert_eq!(g.request(RESOURCES, 7), Err(GateError::BadResource));
        assert_eq!(g.request(0, 32), Err(GateError::BadOwner));
        // A dead owner frees only what nobody else uses.
        g.request(1, 7).unwrap();
        g.request(2, 7).unwrap();
        g.request(2, 25).unwrap();
        assert_eq!(g.release_owner(7), 1 << 1);
        assert_eq!(g.active(), 1 << 2);
        assert_eq!(g.release_owner(7), 0);
    }

    /// A register file that records every access.
    struct Bus {
        regs: [(u16, u16); 6],
        log: std::vec::Vec<(char, u16, u16)>,
        /// Fail the nth write (1-based), or make it not stick.
        fail_write: usize,
        stuck_write: usize,
        writes: usize,
    }
    impl Bus {
        fn new(values: [u16; 5]) -> Self {
            let mut regs = [(0, 0); 6];
            for (i, f) in RADIO.iter().enumerate() {
                regs[i] = (f.addr, values[i]);
            }
            Bus { regs, log: std::vec::Vec::new(), fail_write: 0, stuck_write: 0, writes: 0 }
        }
        fn value(&self, addr: u16) -> u16 {
            self.regs.iter().find(|r| r.0 == addr).unwrap().1
        }
    }
    impl Regs for Bus {
        fn read(&mut self, addr: u16) -> Option<u16> {
            let v = self.value(addr);
            self.log.push(('r', addr, v));
            Some(v)
        }
        fn write(&mut self, addr: u16, value: u16) -> Option<()> {
            self.writes += 1;
            self.log.push(('w', addr, value));
            if self.writes == self.fail_write {
                return None;
            }
            if self.writes != self.stuck_write {
                self.regs.iter_mut().find(|r| r.0 == addr).unwrap().1 = value;
            }
            Some(())
        }
        fn settle(&mut self) {
            self.log.push(('s', 0, 0));
        }
    }

    /// The handset's Wi-Fi rails (targets/moto-g-pure/power.rs RADIO_FIELDS).
    const RADIO: [Field; 5] = [
        Field { addr: 0x1a58, mask: 1, on: 1 },
        Field { addr: 0x1c1c, mask: 3 << 8, on: 1 << 8 },
        Field { addr: 0x1b1c, mask: 1, on: 1 },
        Field { addr: 0x0788, mask: 7 << 3, on: 1 << 5 },
        Field { addr: 0x1b88, mask: 1, on: 1 },
    ];

    #[test]
    fn sequence_writes_in_order_and_restores_in_reverse() {
        // Other bits of each register (0x8000, 0x00f0, 0x0004) must survive.
        let before = [0x8000, 0x00f0, 0x0000, 0x0004, 0x0000];
        let mut bus = Bus::new(before);
        let mut seq = Sequence::new();
        assert_eq!(seq.enable(&RADIO, &mut bus), Some(()));
        assert_eq!(seq.active(), 5);
        // Exactly the accesses the handset code made before this was shared:
        // read, write if needed, read back, settle; field by field.
        let expected: &[(char, u16, u16)] = &[
            ('r', 0x1a58, 0x8000), ('w', 0x1a58, 0x8001), ('r', 0x1a58, 0x8001), ('s', 0, 0),
            ('r', 0x1c1c, 0x00f0), ('w', 0x1c1c, 0x01f0), ('r', 0x1c1c, 0x01f0), ('s', 0, 0),
            ('r', 0x1b1c, 0x0000), ('w', 0x1b1c, 0x0001), ('r', 0x1b1c, 0x0001), ('s', 0, 0),
            ('r', 0x0788, 0x0004), ('w', 0x0788, 0x0024), ('r', 0x0788, 0x0024), ('s', 0, 0),
            ('r', 0x1b88, 0x0000), ('w', 0x1b88, 0x0001), ('r', 0x1b88, 0x0001), ('s', 0, 0),
        ];
        assert_eq!(bus.log, expected);
        // A second enable while on is refused without touching the bus.
        bus.log.clear();
        assert_eq!(seq.enable(&RADIO, &mut bus), None);
        assert!(bus.log.is_empty());
        assert_eq!(seq.restore(&RADIO, &mut bus), Some(()));
        assert_eq!(seq.active(), 0);
        let order: std::vec::Vec<u16> = bus.log.iter().filter(|e| e.0 == 'w').map(|e| e.1).collect();
        assert_eq!(order, [0x1b88, 0x0788, 0x1b1c, 0x1c1c, 0x1a58]);
        for (i, f) in RADIO.iter().enumerate() {
            assert_eq!(bus.value(f.addr), before[i]);
        }
        // Restoring again is a no-op.
        bus.log.clear();
        assert_eq!(seq.restore(&RADIO, &mut bus), Some(()));
        assert!(bus.log.is_empty());
    }

    #[test]
    fn sequence_preserves_inherited_rails() {
        // The bootloader left VCN18 and VCN33 on and the selector already right.
        let before = [0x0001, 0x0100, 0x0001, 0x0000, 0x0000];
        let mut bus = Bus::new(before);
        let mut seq = Sequence::new();
        seq.enable(&RADIO, &mut bus).unwrap();
        let written: std::vec::Vec<u16> = bus.log.iter().filter(|e| e.0 == 'w').map(|e| e.1).collect();
        assert_eq!(written, [0x0788, 0x1b88]);
        bus.log.clear();
        seq.restore(&RADIO, &mut bus).unwrap();
        let written: std::vec::Vec<u16> = bus.log.iter().filter(|e| e.0 == 'w').map(|e| e.1).collect();
        assert_eq!(written, [0x1b88, 0x0788]);
        // What was on before is still on.
        assert_eq!(bus.value(0x1a58) & 1, 1);
        assert_eq!(bus.value(0x1b1c) & 1, 1);
        assert_eq!(bus.value(0x1c1c), 0x0100);
    }

    #[test]
    fn sequence_failure_restores_only_what_it_touched() {
        let before = [0, 0, 0, 0, 0];
        // The third write fails on the bus.
        let mut bus = Bus::new(before);
        bus.fail_write = 3;
        let mut seq = Sequence::new();
        assert_eq!(seq.enable(&RADIO, &mut bus), None);
        assert_eq!(seq.active(), 3);
        seq.restore(&RADIO, &mut bus).unwrap();
        for f in RADIO.iter() {
            assert_eq!(bus.value(f.addr), 0);
        }
        // A write that does not stick is a failure too.
        let mut bus = Bus::new(before);
        bus.stuck_write = 2;
        let mut seq = Sequence::new();
        assert_eq!(seq.enable(&RADIO, &mut bus), None);
        assert_eq!(seq.active(), 2);
        seq.restore(&RADIO, &mut bus).unwrap();
        assert_eq!((bus.value(0x1a58), bus.value(0x1c1c)), (0, 0));
        // Too many fields for the saved state: refused before any access.
        let long = [RADIO[0]; FIELDS + 1];
        let mut bus = Bus::new(before);
        assert_eq!(Sequence::new().enable(&long, &mut bus), None);
        assert!(bus.log.is_empty());
    }

    #[test]
    fn battery_estimate() {
        assert_eq!(percent_from_millivolts(0), None);
        assert_eq!(percent_from_millivolts(5400), None);
        assert_eq!(percent_from_millivolts(3000), Some(0));
        assert_eq!(percent_from_millivolts(3300), Some(0));
        assert_eq!(percent_from_millivolts(4200), Some(100));
        assert_eq!(percent_from_millivolts(4350), Some(100));
        assert_eq!(percent_from_millivolts(3830), Some(45));
        assert_eq!(percent_from_millivolts(3850), Some(50));
        let mut last = 0;
        for mv in 2500..=4600 {
            let p = percent_from_millivolts(mv).unwrap();
            assert!(p >= last && p <= 100, "{mv} mV -> {p}%");
            last = p;
        }
    }
    struct Fake {
        reading: Reading,
        load: Option<u32>,
        table: &'static [Opp],
        log: std::vec::Vec<std::string::String>,
        refuse_power: bool,
        fail_step: Option<DvfsStep>,
        last: Snapshot,
        events: u32,
        alarm: Option<u64>,
    }
    impl Fake {
        fn new(table: &'static [Opp]) -> Self {
            Fake { reading: Reading::Absent, load: None, table, log: std::vec::Vec::new(), refuse_power: false, fail_step: None, last: Snapshot::default(), events: 0, alarm: None }
        }
        fn take(&mut self) -> std::vec::Vec<std::string::String> {
            core::mem::take(&mut self.log)
        }
    }
    impl Board for Fake {
        fn temperature(&mut self) -> Reading {
            self.reading
        }
        fn load_percent(&mut self) -> Option<u32> {
            self.load
        }
        fn opps(&self) -> &'static [Opp] {
            self.table
        }
        fn apply(&mut self, step: DvfsStep) -> bool {
            if self.fail_step == Some(step) {
                self.log.push(std::format!("fail {step:?}"));
                return false;
            }
            self.log.push(std::format!("{step:?}"));
            true
        }
        fn power(&mut self, command: u32) -> bool {
            self.log.push(std::format!("power {command}"));
            !self.refuse_power
        }
        fn screen_peripherals(&mut self, on: bool) {
            self.log.push(std::format!("screen {on}"));
        }
        fn publish(&mut self, snapshot: &Snapshot) {
            self.last = *snapshot;
        }
        const SUSPENDS: bool = true;
        fn wake_events(&mut self) -> u32 {
            core::mem::take(&mut self.events)
        }
        fn take_alarm(&mut self) -> Option<u64> {
            self.alarm.take()
        }
    }
    /// A board whose sleep only blanks the screen (the handset today).
    struct Blank(std::vec::Vec<u32>);
    impl Board for Blank {
        fn temperature(&mut self) -> Reading {
            Reading::Absent
        }
        fn load_percent(&mut self) -> Option<u32> {
            None
        }
        fn opps(&self) -> &'static [Opp] {
            &[]
        }
        fn apply(&mut self, _: DvfsStep) -> bool {
            false
        }
        fn power(&mut self, command: u32) -> bool {
            self.0.push(command);
            true
        }
        fn screen_peripherals(&mut self, _: bool) {}
        fn publish(&mut self, _: &Snapshot) {}
        // Even if it reported events, they are not acted on.
        fn wake_events(&mut self) -> u32 {
            WAKE_KEY | WAKE_TOUCH
        }
        fn take_alarm(&mut self) -> Option<u64> {
            Some(1)
        }
    }
    #[test]
    fn a_board_that_cannot_suspend_is_left_alone() {
        let mut b = Blank(std::vec::Vec::new());
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        assert_eq!(m.next_tick(&b, 0), None);
        assert_eq!(m.sleep(&mut b, 10), Ok(0));
        m.tick(&mut b, 20);
        m.tick(&mut b, 20 + 10 * LINGER_MS);
        // One sleep command reached it, no wake and no second suspend; no polling.
        assert_eq!(b.0, [1]);
        assert_eq!(m.next_tick(&b, 30), None);
        // A held lock is still looked at once a second, so it lapses on time.
        m.wake(&mut b);
        m.lock(&mut b, APP, KIND_CPU, true, 100).unwrap();
        assert_eq!(m.next_tick(&b, 100), Some(HOUSEKEEPING_MS));
    }
    static TABLE: [Opp; 4] = OPPS;

    #[test]
    fn manager_without_sensor_or_table_only_reports_unsupported() {
        // The handset today: no readable sensor, no operating points.
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        for now in 0..20 {
            m.tick(&mut b, now * 1000);
        }
        assert!(b.take().is_empty());
        assert_eq!(b.last, Snapshot { screen_on: true, ..Snapshot::default() });
        assert!(!b.last.sensor && !b.last.dvfs);
    }

    #[test]
    fn manager_throttles_and_shuts_down() {
        let mut b = Fake::new(&TABLE);
        b.reading = Reading::Ok(40_000);
        b.load = Some(100);
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        assert_eq!(b.take(), ["Voltage(950000)", "Frequency(2000000)"]);
        assert_eq!((b.last.level, b.last.opp, b.last.khz, b.last.temperature_mc), (0, 3, 2_000_000, 40_000));
        // Hot: capped at once, frequency first.
        b.reading = Reading::Ok(80_000);
        m.tick(&mut b, 1_000);
        assert_eq!(b.take(), ["Frequency(1000000)", "Voltage(750000)"]);
        assert_eq!((b.last.level, b.last.opp), (LEVEL_HOT, 1));
        // A second reading inside the sampling interval is not taken.
        b.reading = Reading::Ok(20_000);
        m.tick(&mut b, 1_500);
        assert_eq!(b.last.temperature_mc, 80_000);
        // Critical: lowest point and power off, asked once.
        b.reading = Reading::Ok(96_000);
        m.tick(&mut b, 2_000);
        assert_eq!(b.take(), ["Frequency(600000)", "Voltage(650000)", "power 2"]);
        m.tick(&mut b, 3_000);
        assert!(b.take().is_empty());
        // A board that refuses to power off is asked again.
        let mut b = Fake::new(&TABLE);
        b.reading = Reading::Ok(120_000);
        b.refuse_power = true;
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        m.tick(&mut b, 1_000);
        assert_eq!(b.take(), ["power 2", "power 2"]);
    }

    #[test]
    fn manager_power_off_is_not_cancelled() {
        // The emulator has one power register: a wake (0) or sleep (1)
        // written after the power-off (2) would replace it.
        let mut b = Fake::new(&[]);
        b.reading = Reading::Ok(96_000);
        let mut m = Manager::new(ZONE, 0);
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        m.tick(&mut b, 0);
        assert_eq!(b.take(), ["power 2"]);
        assert!(m.powering_off());
        // A touch wake and a sleep that were already on their way.
        assert!(!m.wake(&mut b));
        assert_eq!(m.sleep(&mut b, 10), Err(()));
        // Lock changes that would have let a postponed suspend through.
        m.lock(&mut b, APP, KIND_CPU, false, 20).unwrap();
        m.owner_gone(&mut b, APP, 30);
        // It cools down: still off, and not asked twice.
        b.reading = Reading::Ok(30_000);
        for i in 1..=20 {
            m.tick(&mut b, i * SAMPLE_MS);
        }
        assert!(!m.wake(&mut b));
        assert!(b.take().is_empty(), "nothing may follow the power-off");
        // Asleep with a suspend postponed when it gets too hot: the lock's
        // end does not turn the power-off into a suspend.
        let mut b = Fake::new(&[]);
        b.reading = Reading::Ok(40_000);
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        assert_eq!(m.sleep(&mut b, 10), Ok(1));
        b.reading = Reading::Ok(96_000);
        m.tick(&mut b, SAMPLE_MS);
        m.tick(&mut b, APP_LOCK_MS + 1);
        assert_eq!(b.take(), ["screen false", "power 2"]);
        // A board that refused the power-off still sleeps and wakes.
        let mut b = Fake::new(&[]);
        b.reading = Reading::Ok(96_000);
        b.refuse_power = true;
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        b.refuse_power = false;
        assert!(m.wake(&mut b));
        assert_eq!(b.take(), ["power 2", "power 0"]);
    }

    #[test]
    fn app_locks_are_bounded_with_the_screen_off() {
        // The app that never lets go: it takes its lock every nine minutes.
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        // Screen on, it may go on as long as it likes.
        let mut now = 0;
        while now < 3 * APP_SCREEN_OFF_MS {
            now += 9 * 60 * 1000;
            assert_eq!(m.lock(&mut b, APP, KIND_CPU, true, now), Ok(()));
            m.tick(&mut b, now);
        }
        assert!(b.take().is_empty());
        // The user turns the screen off.
        let off = now + 1000;
        assert_eq!(m.sleep(&mut b, off), Ok(1 << KIND_CPU));
        assert_eq!(b.take(), ["screen false"]);
        let mut refused_at = None;
        let mut suspended_at = None;
        for i in 1..=20 {
            let t = off + i * 9 * 60 * 1000;
            m.tick(&mut b, t);
            if suspended_at.is_none() && b.take().iter().any(|l| l == "power 1") {
                suspended_at = Some(t);
            }
            let r = m.lock(&mut b, APP, KIND_CPU, true, t);
            if r.is_err() && refused_at.is_none() {
                assert_eq!(r, Err(LockError::ScreenOff));
                refused_at = Some(t);
            }
            assert_eq!(r.is_err(), refused_at.is_some(), "refused once, refused until the screen is on");
        }
        // Renewals stopped counting at the allowance; the suspend followed
        // on the first request after it.
        assert_eq!(refused_at, Some(off + 36 * 60 * 1000));
        assert_eq!(suspended_at, Some(off + 36 * 60 * 1000));
        assert_eq!(m.locks.count(), 0);
        // The lock ended at the allowance, not when it was noticed.
        let stat = m.locks.owner_stat(0, off + 10 * APP_SCREEN_OFF_MS).unwrap();
        assert_eq!((stat.timeouts, stat.holding), (1, 0));
        assert_eq!(stat.total_ms, off + APP_SCREEN_OFF_MS);
        // Another app cannot start one behind a dark screen either.
        assert_eq!(m.lock(&mut b, APP + 1, KIND_CPU, true, off + 5), Err(LockError::ScreenOff));
        // A service's lock is not an app's: no limit.
        assert_eq!(m.lock(&mut b, AUDIO, KIND_AUDIO, true, off + 400 * 60 * 1000), Ok(()));
        m.lock(&mut b, AUDIO, KIND_AUDIO, false, off + 400 * 60 * 1000).unwrap();
        // The user is back: the app may hold a lock again, for a new allowance.
        let on = off + 500 * 60 * 1000;
        assert!(m.wake(&mut b));
        assert_eq!(m.lock(&mut b, APP, KIND_CPU, true, on), Ok(()));
        assert_eq!(m.locks.remaining(APP, KIND_CPU, on), APP_LOCK_MS as u32);
        assert_eq!(m.sleep(&mut b, on + 1), Ok(1 << KIND_CPU));
        for minutes in [9, 18, 27, 30] {
            assert_eq!(m.lock(&mut b, APP, KIND_CPU, true, on + minutes * 60 * 1000), Ok(()));
        }
        // What the app is told it has left: ten minutes awake, the screen-off
        // bound once that comes first (renewed at 27: three minutes, not ten).
        let at = on + 27 * 60 * 1000;
        m.lock(&mut b, APP, KIND_CPU, true, at).unwrap();
        assert_eq!(m.locks.remaining(APP, KIND_CPU, at), (on + 1 + APP_SCREEN_OFF_MS - at) as u32);
        assert_eq!(m.locks.remaining(APP, KIND_CPU, on + 1 + APP_SCREEN_OFF_MS), 0, "lapsed");
        assert_eq!(m.locks.remaining(APP + 1, KIND_CPU, at), 0, "not its lock");
        assert_eq!(m.locks.remaining(AUDIO, KIND_AUDIO, at), 0, "a service's lock has no expiry");
        assert_eq!(m.lock(&mut b, APP, KIND_CPU, true, on + 1 + APP_SCREEN_OFF_MS), Err(LockError::ScreenOff));
        // A lock that lapsed in the dark is not taken again in the dark.
        let mut m = Manager::new(ZONE, 0);
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        m.sleep(&mut b, 1).unwrap();
        assert_eq!(m.lock(&mut b, APP, KIND_CPU, true, APP_LOCK_MS + 5), Err(LockError::ScreenOff));
        // Releasing is always allowed.
        let mut m = Manager::new(ZONE, 0);
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        m.sleep(&mut b, 1).unwrap();
        assert_eq!(m.lock(&mut b, APP, KIND_CPU, false, 2), Ok(()));
    }

    #[test]
    fn refusals_are_never_the_pending_reply() {
        for e in [LockError::Full, LockError::BadKind, LockError::NotHeld, LockError::ScreenOff] {
            assert_ne!(e.reply().0, REPLY_PENDING, "{e:?}");
            assert_ne!(e.reply().0, 0, "{e:?} must not read as success");
        }
        // The two refusals an app can do something about are told apart.
        assert_ne!(LockError::Full.reply(), LockError::ScreenOff.reply());
    }

    #[test]
    fn manager_sensor_failure_throttles_but_never_powers_off() {
        let mut b = Fake::new(&TABLE);
        b.reading = Reading::Ok(30_000);
        let mut m = Manager::new(ZONE, 3);
        m.tick(&mut b, 0);
        b.reading = Reading::Failed;
        for i in 1..=10 {
            m.tick(&mut b, i * SAMPLE_MS);
        }
        assert_eq!(b.take(), ["Frequency(1000000)", "Voltage(750000)"]);
        assert!(b.last.sensor && b.last.sensor_failed);
        assert_eq!(b.last.level, LEVEL_HOT);
    }

    #[test]
    fn manager_failed_steps_leave_a_safe_state() {
        // Going up, the frequency step fails: still at the old point (with
        // the supply already raised), and the next tick tries again.
        let mut b = Fake::new(&TABLE);
        b.load = Some(100);
        b.fail_step = Some(DvfsStep::Frequency(2_000_000));
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        assert_eq!(b.take(), ["Voltage(950000)", "fail Frequency(2000000)"]);
        assert_eq!(m.dvfs.current, 0);
        b.fail_step = None;
        m.tick(&mut b, 10);
        assert_eq!(b.take(), ["Voltage(950000)", "Frequency(2000000)"]);
        assert_eq!(m.dvfs.current, 3);
        // Going down, the voltage step fails after the frequency fell: we
        // are at the new point with more supply than it needs.
        b.fail_step = Some(DvfsStep::Voltage(650_000));
        b.load = Some(0);
        m.tick(&mut b, 20);
        m.tick(&mut b, 20 + DOWN_HOLD_MS);
        assert_eq!(b.take(), ["Frequency(600000)", "fail Voltage(650000)"]);
        assert_eq!(m.dvfs.current, 0);
        // Going down, the frequency step fails: nothing changed, the supply stays up.
        let mut b = Fake::new(&TABLE);
        b.reading = Reading::Ok(80_000);
        b.fail_step = Some(DvfsStep::Frequency(1_000_000));
        let mut m = Manager::new(ZONE, 3);
        m.tick(&mut b, 0);
        assert_eq!(b.take(), ["fail Frequency(1000000)"]);
        assert_eq!(m.dvfs.current, 3);
    }

    #[test]
    fn manager_sleep_wake_and_locks() {
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        // No locks: as before.
        assert_eq!(m.sleep(&mut b, 0), Ok(0));
        assert_eq!(b.take(), ["screen false", "power 1"]);
        assert!(m.wake(&mut b));
        assert_eq!(b.take(), ["screen true", "power 0"]);
        // An app holds the CPU: screen peripherals go off, the suspend waits.
        m.lock(&mut b, APP, KIND_CPU, true, 10).unwrap();
        assert_eq!(m.sleep(&mut b, 20), Ok(1 << KIND_CPU));
        assert_eq!(b.take(), ["screen false"]);
        m.tick(&mut b, 30);
        assert!(b.last.sleep_deferred && !b.last.screen_on);
        assert_eq!((b.last.blockers, b.last.locks), (1, 1));
        assert_eq!(m.lock(&mut b, APP + 1, KIND_CPU, false, 35), Err(LockError::NotHeld));
        assert!(b.take().is_empty());
        m.lock(&mut b, APP, KIND_CPU, false, 40).unwrap();
        assert_eq!(b.take(), ["power 1"]);
        m.wake(&mut b);
        b.take();
        // The app dies holding the lock.
        m.lock(&mut b, APP, KIND_AUDIO, true, 50).unwrap();
        assert_eq!(m.sleep(&mut b, 60), Ok(1 << KIND_AUDIO));
        assert_eq!(m.owner_gone(&mut b, APP, 70), 1);
        assert_eq!(b.take(), ["screen false", "power 1"]);
        m.wake(&mut b);
        b.take();
        // The lock lapses while the screen is off: the next tick suspends.
        m.lock(&mut b, APP, KIND_CPU, true, 100).unwrap();
        m.sleep(&mut b, 110).unwrap();
        m.tick(&mut b, 100 + APP_LOCK_MS - 1);
        assert_eq!(b.take(), ["screen false"]);
        m.tick(&mut b, 100 + APP_LOCK_MS);
        assert_eq!(b.take(), ["power 1"]);
        // A wake always reaches the board, and a second sleep does not gate twice.
        let mut m = Manager::new(ZONE, 0);
        assert!(m.wake(&mut b));
        assert_eq!(b.take(), ["power 0"]);
        // A board that refuses to suspend: reported, and retried later.
        b.refuse_power = true;
        assert_eq!(m.sleep(&mut b, 0), Err(()));
        assert!(m.sleep.deferred());
        b.refuse_power = false;
        b.take();
        m.tick(&mut b, 5);
        assert_eq!(b.take(), ["power 1"]);
    }
    #[test]
    fn a_background_wake_resumes_the_cpu_and_suspends_again() {
        for source in [WAKE_NETWORK, WAKE_TIMER] {
            let mut b = Fake::new(&[]);
            let mut m = Manager::new(ZONE, 0);
            if source == WAKE_TIMER {
                m.set_alarm(Some(1_000));
            }
            assert_eq!(m.sleep(&mut b, 0), Ok(0));
            assert_eq!(b.take(), ["screen false", "power 1"]);
            m.tick(&mut b, 999);
            assert!(b.take().is_empty() && b.last.suspended);
            if source == WAKE_NETWORK {
                b.events = WAKE_NETWORK;
            }
            m.tick(&mut b, 1_000);
            // The CPU is up; the screen is not asked for.
            assert_eq!(b.take(), ["power 0"]);
            assert!(!b.last.suspended && !b.last.screen_on);
            assert_eq!((b.last.want, b.last.last_wake, b.last.suspends), (WANT_NOTHING, source, 1));
            // It lingers, then suspends again by itself.
            m.tick(&mut b, 1_000 + LINGER_MS - 1);
            assert!(b.take().is_empty());
            m.tick(&mut b, 1_000 + LINGER_MS);
            assert_eq!(b.take(), ["power 1"]);
            assert_eq!(b.last.suspends, 2);
        }
    }

    #[test]
    fn a_user_wake_asks_for_the_screen() {
        for source in [WAKE_KEY, WAKE_TOUCH] {
            let mut b = Fake::new(&[]);
            let mut m = Manager::new(ZONE, 0);
            m.sleep(&mut b, 0).unwrap();
            b.take();
            b.events = source;
            m.tick(&mut b, 50);
            assert_eq!(b.take(), ["power 0"]);
            assert_eq!((b.last.want, b.last.last_wake), (WANT_WAKE, source));
            // Runtime's wake command completes it.
            assert!(m.wake(&mut b));
            assert_eq!(b.take(), ["screen true", "power 0"]);
            m.tick(&mut b, 60);
            assert_eq!(b.last.want, WANT_NOTHING);
            assert!(b.last.screen_on && !b.last.suspended);
            // Nothing suspends it again.
            m.tick(&mut b, 60 + 10 * LINGER_MS);
            assert!(b.take().is_empty());
        }
        // A wake nobody turns into a screen-on: suspended again, the request forgotten.
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        m.sleep(&mut b, 0).unwrap();
        b.events = WAKE_TOUCH;
        m.tick(&mut b, 10);
        b.take();
        m.tick(&mut b, 10 + LINGER_MS);
        assert_eq!(b.take(), ["power 1"]);
        assert_eq!(b.last.want, WANT_NOTHING);
    }

    #[test]
    fn the_power_key_asks_for_sleep_when_awake() {
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        // A touch or the network while awake asks for nothing.
        b.events = WAKE_TOUCH | WAKE_NETWORK;
        m.tick(&mut b, 0);
        assert_eq!(b.last.want, WANT_NOTHING);
        b.events = WAKE_KEY;
        m.tick(&mut b, 10);
        assert_eq!(b.last.want, WANT_SLEEP);
        assert!(b.take().is_empty());
        // The request stands until Runtime's sleep arrives.
        m.tick(&mut b, 20);
        assert_eq!(b.last.want, WANT_SLEEP);
        assert_eq!(m.sleep(&mut b, 30), Ok(0));
        m.tick(&mut b, 31);
        assert_eq!(b.last.want, WANT_NOTHING);
        assert!(b.last.suspended);
        // Bits a board must not send are ignored.
        let mut m = Manager::new(ZONE, 0);
        b.events = 0xffff_fff0;
        m.tick(&mut b, 0);
        assert_eq!(b.last.want, WANT_NOTHING);
    }

    #[test]
    fn a_wake_lock_taken_after_a_wake_holds_the_suspend() {
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        m.sleep(&mut b, 0).unwrap();
        b.events = WAKE_NETWORK;
        m.tick(&mut b, 100);
        b.take();
        // Services may still take a lock with the screen off (apps may not).
        m.lock(&mut b, AUDIO, KIND_RADIO, true, 200).unwrap();
        m.tick(&mut b, 100 + 3 * LINGER_MS);
        assert!(b.take().is_empty());
        assert!(b.last.sleep_deferred);
        m.lock(&mut b, AUDIO, KIND_RADIO, false, 100 + 4 * LINGER_MS).unwrap();
        assert_eq!(b.take(), ["power 1"]);
        // A user's touch while the suspend is only deferred still asks for the screen.
        let mut m = Manager::new(ZONE, 0);
        m.lock(&mut b, AUDIO, KIND_AUDIO, true, 0).unwrap();
        assert_eq!(m.sleep(&mut b, 10), Ok(1 << KIND_AUDIO));
        b.take();
        b.events = WAKE_TOUCH;
        m.tick(&mut b, 20);
        assert_eq!(b.last.want, WANT_WAKE);
        assert!(b.take().is_empty(), "nothing to resume: it never suspended");
    }

    #[test]
    fn alarms() {
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        // One that passes while the system is up does nothing.
        m.set_alarm(Some(100));
        m.tick(&mut b, 150);
        assert_eq!(b.last.last_wake, 0);
        assert_eq!(m.next_tick(&b, 150), None);
        // The board's alarm is relative to now.
        b.alarm = Some(400);
        m.tick(&mut b, 1_000);
        assert_eq!(m.next_tick(&b, 1_000), Some(400));
        m.sleep(&mut b, 1_100).unwrap();
        b.take();
        m.tick(&mut b, 1_399);
        assert!(b.take().is_empty());
        m.tick(&mut b, 1_400);
        assert_eq!(b.take(), ["power 0"]);
        assert_eq!(b.last.last_wake, WAKE_TIMER);
        // Fired once.
        m.tick(&mut b, 1_400 + LINGER_MS);
        assert_eq!(b.take(), ["power 1"]);
        m.tick(&mut b, 1_400 + 3 * LINGER_MS);
        assert!(b.take().is_empty());
    }

    #[test]
    fn power_runs_only_when_something_is_pending() {
        let mut b = Fake::new(&[]);
        let mut m = Manager::new(ZONE, 0);
        m.tick(&mut b, 0);
        assert_eq!(m.next_tick(&b, 0), None, "awake with nothing held: wait for a request");
        m.lock(&mut b, APP, KIND_CPU, true, 0).unwrap();
        assert_eq!(m.next_tick(&b, 0), Some(HOUSEKEEPING_MS), "a lock can lapse");
        assert_eq!(m.sleep(&mut b, 10), Ok(1));
        assert_eq!(m.next_tick(&b, 10), Some(HOUSEKEEPING_MS), "deferred by a lock: once a second, for its lapse");
        assert_eq!(m.next_tick(&b, 10 + 9 * 60_000), Some(HOUSEKEEPING_MS), "and still, while it is held");
        m.lock(&mut b, APP, KIND_CPU, false, 20).unwrap();
        assert_eq!(m.next_tick(&b, 20), Some(WAKE_POLL_MS), "suspended: watch the wake lines");
        b.events = WAKE_NETWORK;
        m.tick(&mut b, 1_000);
        assert_eq!(m.next_tick(&b, 1_000), Some(HOUSEKEEPING_MS), "lingering: no sooner than needed");
        assert_eq!(m.next_tick(&b, 1_000 + LINGER_MS - 300), Some(300));
        m.wake(&mut b);
        assert_eq!(m.next_tick(&b, 9_000), None);
        // A sensor is sampled once a second.
        b.reading = Reading::Ok(30_000);
        m.tick(&mut b, 10_000);
        assert_eq!(m.next_tick(&b, 10_000), Some(HOUSEKEEPING_MS));
    }

    #[test]
    fn a_power_off_for_heat_ignores_wake_sources() {
        let mut b = Fake::new(&TABLE);
        let mut m = Manager::new(ZONE, 0);
        b.reading = Reading::Ok(ZONE.critical + 1);
        m.tick(&mut b, 0);
        assert!(m.powering_off());
        b.take();
        b.events = WAKE_KEY;
        m.tick(&mut b, 10);
        assert_eq!(b.last.want, WANT_NOTHING);
        assert!(b.take().is_empty());
        assert_eq!(m.next_tick(&b, 10), None);
    }
}
