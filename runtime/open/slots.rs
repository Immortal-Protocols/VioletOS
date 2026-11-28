//! Who holds each socket slot (net.rs), and who may take another.
//!
//! The slots are one table for the whole phone, so the rules here are what
//! keeps one app from starving the rest:
//! - the last `RESERVED` slots are the system's (the Open Runtime's own
//!   sockets: the Sentinel's link, its flow observer and network time); past
//!   those the system takes only what leaves the apps their floors;
//! - an app holds at most its `share`: its floor and half of the slots that
//!   are neither a floor nor the reserve (and never more than `APP_MAX`);
//! - an app that already holds `APP_FLOOR` gets another only while every other
//!   app that may run could still reach its own floor.
//! So with `apps` apps allowed to run, each can always open `APP_FLOOR`
//! sockets whatever the others do, and because no app takes more than half of
//! what is left over, a second busy app gets nearly as much as the first
//! (9 and 8 of 32 with eight apps, 11 and 10 with six) instead of its floor.
//! A third busy app is left its floor. Pure: tested on the host (make test-unit).

/// Slots in the shared region (the server may serve fewer).
pub const SLOTS: usize = 32;
/// The owner that is the Open Runtime itself.
pub const SYSTEM: u32 = 0;
/// Slots apps can never take.
pub const RESERVED: usize = 3;
/// The most one app can hold, however few apps may run.
pub const APP_MAX: usize = 15;
/// What every app can always get.
pub const APP_FLOOR: usize = 2;
/// This app has its share.
pub const EMFILE: i32 = -24;
/// Every slot an app may use is taken.
pub const ENFILE: i32 = -23;

/// The most one app can hold of `limit` slots when `apps` apps may run: its
/// floor, and half (rounded up) of the slots left once the reserve and every
/// app's floor are set aside.
pub fn share(limit: usize, apps: usize) -> usize {
    let spare = limit.min(SLOTS).saturating_sub(RESERVED + apps * APP_FLOOR);
    (APP_FLOOR + (spare + 1) / 2).min(APP_MAX)
}

pub struct Table {
    used: u32,
    owner: [u32; SLOTS],
}

impl Table {
    pub const fn new() -> Table {
        Table { used: 0, owner: [0; SLOTS] }
    }
    pub fn in_use(&self, slot: usize) -> bool {
        slot < SLOTS && self.used & (1 << slot) != 0
    }
    pub fn owner(&self, slot: usize) -> Option<u32> {
        if self.in_use(slot) { Some(self.owner[slot]) } else { None }
    }
    /// Slots `owner` holds.
    pub fn held(&self, owner: u32) -> usize {
        (0..SLOTS).filter(|&s| self.in_use(s) && self.owner[s] == owner).count()
    }
    /// Slots the other apps are still owed below their floors: each app seen
    /// in the table, and each of the seats (`apps - 1` in all) nobody has yet.
    fn owed_to_others(&self, owner: u32, apps: usize) -> usize {
        let (mut seen, mut owed) = (0, 0);
        for s in 0..SLOTS {
            if !self.in_use(s) || self.owner[s] == owner || self.owner[s] == SYSTEM {
                continue;
            }
            // Count each owner once, at its first slot.
            if (0..s).any(|p| self.in_use(p) && self.owner[p] == self.owner[s]) {
                continue;
            }
            seen += 1;
            owed += APP_FLOOR.saturating_sub(self.held(self.owner[s]));
        }
        owed + apps.saturating_sub(1).saturating_sub(seen) * APP_FLOOR
    }
    /// Take a slot below `limit` (what the server serves) for `owner`.
    /// `apps` is how many apps can run at once.
    pub fn alloc(&mut self, owner: u32, limit: usize, apps: usize) -> Result<usize, i32> {
        let limit = limit.min(SLOTS);
        let free = (0..limit).filter(|&s| !self.in_use(s)).count();
        if free == 0 {
            return Err(ENFILE);
        }
        if owner != SYSTEM {
            let mine = self.held(owner);
            if mine >= share(limit, apps) {
                return Err(EMFILE);
            }
            let system = RESERVED.saturating_sub(self.held(SYSTEM));
            if mine >= APP_FLOOR && free - 1 < system + self.owed_to_others(owner, apps) {
                return Err(EMFILE);
            }
            if free - 1 < system {
                return Err(ENFILE);
            }
        } else if self.held(SYSTEM) >= RESERVED && free - 1 < self.owed_to_others(SYSTEM, apps + 1) {
            // Past its reserve the system, too, leaves the apps their floors.
            return Err(ENFILE);
        }
        let slot = (0..limit).find(|&s| !self.in_use(s)).ok_or(ENFILE)?;
        self.used |= 1 << slot;
        self.owner[slot] = owner;
        Ok(slot)
    }
    pub fn free(&mut self, slot: usize) {
        if slot < SLOTS {
            self.used &= !(1 << slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_app_cannot_take_everything() {
        let mut t = Table::new();
        let mut got = 0;
        while t.alloc(100, SLOTS, 8).is_ok() {
            got += 1;
        }
        // 32 slots, 3 for the system, 8 apps' floors of 2: 13 over, of which
        // one app gets 7 on top of its floor.
        assert_eq!(got, share(SLOTS, 8));
        assert_eq!(got, 9);
        assert_eq!(t.alloc(100, SLOTS, 8), Err(EMFILE));
        // The other apps take all they can, one after another: none is left
        // below its floor, and the system's reserve is still there.
        let mut held = [0usize; 7];
        for (k, pid) in (101..108).enumerate() {
            while t.alloc(pid, SLOTS, 8).is_ok() {
                held[k] += 1;
            }
        }
        assert_eq!(held, [8, 2, 2, 2, 2, 2, 2]);
        assert_eq!(t.alloc(107, SLOTS, 8), Err(EMFILE));
        for _ in 0..RESERVED {
            assert!(t.alloc(SYSTEM, SLOTS, 8).is_ok());
        }
        assert_eq!(t.alloc(SYSTEM, SLOTS, 8), Err(ENFILE));
        assert_eq!(t.alloc(108, SLOTS, 8), Err(ENFILE));
    }

    /// The reviewers' case: a background app holds all it can, then the
    /// browser starts. It must get a usable number, not its floor of 2.
    #[test]
    fn a_second_busy_app_is_not_left_its_floor() {
        for (apps, first, second) in [(8, 9, 8), (6, 11, 10)] {
            let mut t = Table::new();
            let mut a = 0;
            while t.alloc(100, SLOTS, apps).is_ok() {
                a += 1;
            }
            let mut b = 0;
            while t.alloc(101, SLOTS, apps).is_ok() {
                b += 1;
            }
            assert_eq!((a, b), (first, second), "{} apps", apps);
            // The same with the system's three sockets already open.
            let mut t = Table::new();
            for _ in 0..RESERVED {
                t.alloc(SYSTEM, SLOTS, apps).unwrap();
            }
            let (mut a, mut b) = (0, 0);
            while t.alloc(100, SLOTS, apps).is_ok() {
                a += 1;
            }
            while t.alloc(101, SLOTS, apps).is_ok() {
                b += 1;
            }
            assert_eq!((a, b), (first, second), "{} apps, system up", apps);
            // And every other app still has its floor, the two busy ones notwithstanding.
            for pid in 102..100 + apps as u32 {
                assert!(t.alloc(pid, SLOTS, apps).is_ok() && t.alloc(pid, SLOTS, apps).is_ok());
                assert!(t.alloc(pid, SLOTS, apps).is_err());
            }
            assert_eq!(t.alloc(SYSTEM, SLOTS, apps), Err(ENFILE));
        }
        assert_eq!(share(SLOTS, 1), APP_MAX);
        assert_eq!(share(7, 8), APP_FLOOR);
        assert_eq!(share(0, 8), APP_FLOOR);
    }

    #[test]
    fn app_max_holds_with_few_apps() {
        let mut t = Table::new();
        let mut got = 0;
        while t.alloc(100, SLOTS, 2).is_ok() {
            got += 1;
        }
        assert_eq!(got, APP_MAX);
    }

    #[test]
    fn apps_cannot_take_the_reserve() {
        let mut t = Table::new();
        // Many apps, one slot at a time: only the reserve is left.
        let mut taken = 0;
        'outer: loop {
            for pid in 100..132 {
                if t.alloc(pid, SLOTS, 32).is_err() {
                    break 'outer;
                }
                taken += 1;
            }
        }
        assert_eq!(taken, SLOTS - RESERVED);
        assert_eq!(t.alloc(131, SLOTS, 32), Err(ENFILE));
        for _ in 0..RESERVED {
            assert!(t.alloc(SYSTEM, SLOTS, 32).is_ok());
        }
        assert_eq!(t.alloc(SYSTEM, SLOTS, 32), Err(ENFILE));
    }

    #[test]
    fn a_reserve_the_system_holds_is_not_counted_twice() {
        let mut t = Table::new();
        for _ in 0..RESERVED {
            assert!(t.alloc(SYSTEM, SLOTS, 1).is_ok());
        }
        let mut got = 0;
        while t.alloc(100, SLOTS, 1).is_ok() {
            got += 1;
        }
        assert_eq!(got, APP_MAX);
        // The system may go past its reserve while slots are free.
        assert!(t.alloc(SYSTEM, SLOTS, 1).is_ok());
    }

    #[test]
    fn free_returns_the_slot_and_the_share() {
        let mut t = Table::new();
        const MINE: usize = 9;
        assert_eq!(share(SLOTS, 8), MINE);
        let slots: [usize; MINE] = core::array::from_fn(|_| t.alloc(7, SLOTS, 8).unwrap());
        assert_eq!(t.alloc(7, SLOTS, 8), Err(EMFILE));
        t.free(slots[3]);
        assert!(!t.in_use(slots[3]));
        assert_eq!(t.owner(slots[3]), None);
        assert_eq!(t.alloc(7, SLOTS, 8), Ok(slots[3]));
        assert_eq!(t.owner(slots[3]), Some(7));
        t.free(slots[3]);
        t.free(slots[3]);
        t.free(SLOTS + 5);
        assert_eq!(t.held(7), MINE - 1);
    }

    #[test]
    fn a_smaller_server_limits_the_table() {
        let mut t = Table::new();
        assert_eq!(t.alloc(5, 0, 8), Err(ENFILE));
        // Eight slots served: an app gets its floor, not the others'.
        assert!(t.alloc(5, 8, 3).is_ok());
        assert!(t.alloc(5, 8, 3).is_ok());
        assert_eq!(t.alloc(5, 8, 3), Err(EMFILE));
        for s in 0..SLOTS {
            assert!(!t.in_use(s) || s < 8);
        }
        assert!(t.alloc(SYSTEM, 1000, 3).is_ok());
    }

    /// Any order of opens and closes: the promises hold.
    #[test]
    fn floors_and_reserve_survive_random_use() {
        let mut seed = 0x2545_f491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for apps in [1usize, 2, 6, 8] {
            let mut t = Table::new();
            let mut held: [alloc::vec::Vec<usize>; 9] = Default::default();
            for _ in 0..20_000 {
                let who = next() as usize % (apps + 1);
                let owner = if who == apps { SYSTEM } else { 100 + who as u32 };
                if next() % 3 != 0 {
                    if let Ok(s) = t.alloc(owner, SLOTS, apps) {
                        assert!(!held.iter().any(|h| h.contains(&s)));
                        held[who].push(s);
                    }
                } else if !held[who].is_empty() {
                    let k = next() as usize % held[who].len();
                    t.free(held[who].swap_remove(k));
                }
                for a in 0..apps {
                    let pid = 100 + a as u32;
                    assert_eq!(t.held(pid), held[a].len());
                    assert!(t.held(pid) <= share(SLOTS, apps));
                    if t.held(pid) < APP_FLOOR {
                        let mut probe = Table { used: t.used, owner: t.owner };
                        assert!(probe.alloc(pid, SLOTS, apps).is_ok(), "an app below its floor was refused");
                    }
                }
                if t.held(SYSTEM) < RESERVED {
                    let mut probe = Table { used: t.used, owner: t.owner };
                    assert!(probe.alloc(SYSTEM, SLOTS, apps).is_ok(), "the system was refused its reserve");
                }
            }
        }
    }
}
