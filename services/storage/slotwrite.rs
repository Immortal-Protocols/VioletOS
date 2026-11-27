//! Writing a full-system update into the inactive boot slot.
//!
//! Who may do what (storage.rs routes the requests):
//!
//! - The Open Runtime (badge 3, for Settings or the Agent; runtime/open/
//!   update.rs) writes and reads sectors of the INACTIVE slot only, through
//!   the 64 KiB app window, and asks Storage to hash what is on the disk, in
//!   order from sector 0. It can never address the running slot, the slot
//!   metadata or anything else on the disk, and it cannot make a slot
//!   bootable.
//! - The supervisor (badge 0) reads the manifest the update put in the
//!   inactive slot's last sector, takes the digest of what Storage read
//!   back, checks the signature itself (system/package-manager/ota.rs) and
//!   only then asks Storage to switch. Storage switches only if the digest it
//!   holds covers exactly the sectors named and equals the one the
//!   supervisor checked; any write since the hash cancels it. Once the
//!   system has run healthy the supervisor asks Storage to mark the running
//!   slot successful, so the bootloader stops spending its tries.
//!
//! The first write of a boot marks the inactive slot unbootable (and flushes)
//! before any image byte changes, so a torn or half-written slot is never
//! booted even if it was bootable before. The slot metadata is Android's
//! bootloader_control (targets/moto-g-pure/slot.rs).
//!
//! Three rules keep a fallback slot (as AOSP's update_engine does):
//!
//! - Nothing is written and nothing switched while the running slot is still
//!   a trial (not marked successful): during a trial the inactive slot is the
//!   only known-good system, and making it unbootable could leave none.
//! - The manifest is held in memory until the switch, so a bundle the
//!   supervisor refuses (`update check`) has changed nothing on the disk,
//!   the inactive slot's metadata included.
//! - A switch always makes the slot unbootable first and then active, so the
//!   activated slot is a counted trial even if it was proven before.
#![allow(dead_code)]

#[path = "sha256.rs"]
pub mod sha256;
#[path = "../../targets/moto-g-pure/slot.rs"]
pub mod slot;

use sha256::Sha256;

pub type Sector = [u8; 512];

/// Raw access to the sectors this module computes; the backend may refuse
/// anything outside the geometry it reported.
pub trait Medium {
    fn read(&mut self, lba: u32) -> Result<Sector, ()>;
    fn write(&mut self, lba: u32, sector: &mut Sector) -> Result<(), ()>;
    fn flush(&mut self) -> Result<(), ()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The sector holding bootloader_control (misc + 4 on the handset).
    pub control: u32,
    /// First sector of slot A and slot B.
    pub first: [u32; 2],
    /// Sectors of each slot that an update may use; the last holds the manifest.
    pub sectors: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No slot geometry, no valid metadata or no known running slot.
    Unavailable,
    Bounds,
    Io,
    Metadata(slot::Error),
    /// A hash step that does not continue where the last one stopped.
    Order,
    /// The supervisor's digest is not what Storage read back, or nothing was hashed.
    Digest,
    /// The running slot is still a trial: it must be marked successful first.
    Trial,
}

/// Most sectors hashed by one request, so Storage keeps answering others.
pub const HASH_STEP: u32 = 2048;

pub struct Updater {
    geo: Option<Geometry>,
    running: usize,
    /// The inactive slot was made unbootable since the last switch (or boot).
    guarded: bool,
    sha: Sha256,
    hashed: u32,
    /// The manifest sent for this update, written to the disk only by `switch`.
    pending: Option<Sector>,
}

impl Updater {
    /// `running` is the slot the bootloader started; without it nothing is allowed.
    pub fn new(geo: Option<Geometry>, running: Option<usize>) -> Updater {
        let ok = running.is_some_and(|r| r < 2) && geo.is_some_and(|g| g.sectors >= 2);
        Updater { geo: if ok { geo } else { None }, running: running.unwrap_or(0), guarded: false, sha: Sha256::new(), hashed: 0, pending: None }
    }
    /// The QEMU model: the slot the bootloader recorded in slot_suffix.
    pub fn from_control(geo: Option<Geometry>, m: &mut impl Medium) -> Updater {
        let running = geo.and_then(|g| m.read(g.control).ok()).and_then(|s| slot::Control::decode(&s).ok()).and_then(|c| c.suffix());
        Updater::new(geo, running)
    }
    pub fn running(&self) -> Option<usize> {
        self.geo.map(|_| self.running)
    }
    fn inactive(&self) -> usize {
        1 - self.running
    }
    /// Sectors of the inactive slot, 0 when updates are unavailable.
    pub fn capacity(&self) -> u32 {
        self.geo.map_or(0, |g| g.sectors)
    }
    fn lba(&self, first: u32, count: u32) -> Result<u32, Error> {
        let g = self.geo.ok_or(Error::Unavailable)?;
        if count == 0 || first.checked_add(count).map_or(true, |end| end > g.sectors) {
            return Err(Error::Bounds);
        }
        Ok(g.first[self.inactive()] + first)
    }
    fn metadata(&self, m: &mut impl Medium, change: impl FnOnce(&mut slot::Control) -> Result<bool, slot::Error>) -> Result<bool, Error> {
        let g = self.geo.ok_or(Error::Unavailable)?;
        let cell = core::cell::RefCell::new(&mut *m);
        let changed = slot::update(|| cell.borrow_mut().read(g.control), |s| cell.borrow_mut().write(g.control, s), change)
            .map_err(Error::Metadata)?;
        if changed {
            m.flush().map_err(|_| Error::Io)?;
        }
        Ok(changed)
    }
    /// Whether the running slot is marked successful, read from the disk now.
    pub fn proven(&self, m: &mut impl Medium) -> Result<bool, Error> {
        let g = self.geo.ok_or(Error::Unavailable)?;
        let s = m.read(g.control).map_err(|_| Error::Io)?;
        let c = slot::Control::decode(&s).map_err(Error::Metadata)?;
        Ok(c.slot(self.running).successful)
    }
    /// Make the inactive slot unbootable (once per update) after checking that
    /// the running slot is proven.
    fn guard(&mut self, m: &mut impl Medium) -> Result<(), Error> {
        if !self.proven(m)? {
            return Err(Error::Trial);
        }
        if !self.guarded {
            let inactive = self.inactive();
            self.metadata(m, |c| c.set_unbootable(inactive))?;
            self.guarded = true;
        }
        Ok(())
    }
    /// Write sectors of the inactive slot from `first`. A single sector at
    /// the manifest's place is the manifest: it is kept in memory (the disk
    /// is not touched) until `switch`. Image writes never cover that sector.
    pub fn write(&mut self, m: &mut impl Medium, first: u32, sectors: &mut [Sector]) -> Result<(), Error> {
        let at = self.lba(first, sectors.len() as u32)?;
        let last = self.capacity() - 1;
        // Whatever was hashed no longer describes the slot.
        self.sha = Sha256::new();
        self.hashed = 0;
        if first == last && sectors.len() == 1 {
            self.pending = Some(sectors[0]);
            return Ok(());
        }
        if first + sectors.len() as u32 > last {
            return Err(Error::Bounds);
        }
        if !self.guarded {
            self.guard(m)?;
        }
        for (k, s) in sectors.iter_mut().enumerate() {
            m.write(at + k as u32, s).map_err(|_| Error::Io)?;
        }
        Ok(())
    }
    pub fn read(&self, m: &mut impl Medium, first: u32, out: &mut [Sector]) -> Result<(), Error> {
        let at = self.lba(first, out.len() as u32)?;
        for (k, s) in out.iter_mut().enumerate() {
            *s = m.read(at + k as u32).map_err(|_| Error::Io)?;
        }
        Ok(())
    }
    /// Hash `count` sectors from `first` as they read back from the disk.
    /// Steps go in order from sector 0 and never cover the manifest sector.
    /// Returns the sectors hashed so far.
    pub fn hash(&mut self, m: &mut impl Medium, first: u32, count: u32) -> Result<u32, Error> {
        let cap = self.capacity();
        if first != self.hashed {
            return Err(Error::Order);
        }
        if count > HASH_STEP || cap == 0 || first.saturating_add(count) > cap - 1 {
            return Err(Error::Bounds);
        }
        let at = self.lba(first, count)?;
        // Data written must be on the medium before it counts as read back.
        m.flush().map_err(|_| Error::Io)?;
        for k in 0..count {
            match m.read(at + k) {
                Ok(s) => self.sha.update(&s),
                Err(()) => {
                    self.sha = Sha256::new();
                    self.hashed = 0;
                    return Err(Error::Io);
                }
            }
        }
        self.hashed += count;
        Ok(self.hashed)
    }
    /// The manifest sector: the last sector of the inactive slot.
    /// The one sent in this boot if there is one, else what the disk holds.
    pub fn manifest(&self, m: &mut impl Medium) -> Result<Sector, Error> {
        if let Some(p) = self.pending.filter(|_| self.geo.is_some()) {
            return Ok(p);
        }
        let mut out = [[0u8; 512]; 1];
        self.read(m, self.capacity().saturating_sub(1), &mut out)?;
        Ok(out[0])
    }
    /// The digest of exactly `sectors` sectors read back from sector 0.
    pub fn digest(&self, sectors: u32) -> Result<[u8; 32], Error> {
        if sectors == 0 || self.hashed != sectors {
            return Err(Error::Digest);
        }
        Ok(self.sha.clone().finish())
    }
    /// Make the inactive slot the one the bootloader tries next (the
    /// supervisor has checked the manifest's signature and `expect`).
    /// The slot is made unbootable first (if no write did so), so it is
    /// activated as a counted trial whatever its successful bit said, and
    /// never while the running slot is itself a trial.
    pub fn switch(&mut self, m: &mut impl Medium, sectors: u32, expect: &[u8; 32]) -> Result<(), Error> {
        if &self.digest(sectors)? != expect {
            return Err(Error::Digest);
        }
        self.guard(m)?;
        if let Some(mut p) = self.pending {
            let at = self.lba(self.capacity() - 1, 1)?;
            m.write(at, &mut p).map_err(|_| Error::Io)?;
        }
        m.flush().map_err(|_| Error::Io)?;
        let (inactive, running) = (self.inactive(), self.running);
        self.metadata(m, |c| c.set_active(inactive, running).map(|_| true))?;
        // A later write must make the slot unbootable again first.
        self.guarded = false;
        self.pending = None;
        self.sha = Sha256::new();
        self.hashed = 0;
        Ok(())
    }
    /// The running system is healthy: stop the bootloader spending tries on it.
    /// Ok(false) when it was already marked (nothing written).
    pub fn mark_successful(&mut self, m: &mut impl Medium) -> Result<bool, Error> {
        let running = self.running;
        self.metadata(m, |c| c.mark_successful(running))
    }
}
