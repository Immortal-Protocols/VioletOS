//! /data's volume on Storage's disk: where it is (once and for all), the
//! filesystem server on it (vfs.rs), and the disk driver lent to them.
//! Compiled into Storage (storage.rs) and, unchanged, into the host tests
//! over the stand-in driver (tests/block.rs), so the adapter that runs on
//! the device is the one the tests drive.
//!
//! The record. Two copies, in sectors `RECORD` and `RECORD + 1` of the
//! first megabyte (after the flat files of files.rs and the small volume's
//! header, and unused on a disk whose volume is elsewhere): "the data
//! volume is at this place", and once the open apps' old files went into
//! it, "done". It is written before the volume is first formatted, so a
//! format that was cut short is done again in the same place, and it is
//! never changed after "done". From then on (extent.rs `decide`):
//!   - /data is served from that place only. A boot that cannot reach it
//!     (a failed read of the claimed userdata, a smaller disk) serves
//!     nothing: ENODEV, the phone stays locked, nothing is written.
//!   - That volume is never formatted again and the old files are never
//!     moved in again: a volume that is gone is "DATA LOST", not a new
//!     empty /data (no passcode) or the old slots' stale secrets.
//!   - A volume made in the first megabyte stays there when a larger
//!     extent is offered later (userdata claimed): moving /data is not done.
//! Losing both copies of the record and the volume's header and superblock
//! together brings back the first-boot path; the slots themselves are left
//! readable for an older system image (README, "What is not done").
//!
//! A disk that is there but has no room for a volume, or whose record
//! cannot be read, answers ENODEV too: only a disk that is not there at all
//! answers OFFLINE (which the runtime reads as "nothing was ever kept").
use super::violetfs::crc32;
use super::{legacy, vfs};
use crate::block::extent::{self, Place};
use crate::block::Disk;
use crate::blockdev::BlockDev;

pub const RECORD: usize = extent::SMALL_FIRST + 2;
const TAG: &[u8] = b"VioletOS data place v1\n";

pub fn record(p: Place, done: bool) -> [u8; 512] {
    let mut s = [0u8; 512];
    s[..TAG.len()].copy_from_slice(TAG);
    for (k, v) in [p.first, p.count, p.raw as u32, done as u32].into_iter().enumerate() {
        s[24 + 4 * k..28 + 4 * k].copy_from_slice(&v.to_le_bytes());
    }
    let sum = crc32(&s[..508]);
    s[508..].copy_from_slice(&sum.to_le_bytes());
    s
}
pub fn parse(s: &[u8; 512]) -> Option<(Place, bool)> {
    let w = |k: usize| u32::from_le_bytes([s[24 + 4 * k], s[25 + 4 * k], s[26 + 4 * k], s[27 + 4 * k]]);
    let sum = u32::from_le_bytes([s[508], s[509], s[510], s[511]]);
    if &s[..TAG.len()] != TAG || crc32(&s[..508]) != sum || w(1) == 0 || w(2) > 1 || w(3) > 1 {
        return None;
    }
    Some((Place { first: w(0), count: w(1), raw: w(2) == 1 }, w(3) == 1))
}
/// The record (the first copy that is one: it is written first). `Err`: a
/// copy could not be read.
fn read_record(d: &mut Disk) -> Result<Option<(Place, bool)>, ()> {
    let a = d.read(RECORD)?;
    let b = d.read(RECORD + 1)?;
    Ok(parse(&a).or(parse(&b)))
}
fn write_record(d: &mut Disk, p: Place, done: bool) -> Result<(), ()> {
    for copy in 0..2 {
        d.commit(RECORD + copy, &mut record(p, done))?;
    }
    Ok(())
}

/// The disk as the filesystem sees it: the data extent, through the driver
/// lent for the length of one call (null between calls: every access fails).
#[derive(Clone)]
pub struct Extent(*mut *mut Disk);
impl Extent {
    fn disk(&self) -> Result<&mut Disk, ()> {
        unsafe { (*self.0).as_mut() }.ok_or(())
    }
}
impl BlockDev for Extent {
    fn sectors(&self) -> u64 {
        self.disk().map_or(0, |d| d.data_sectors() as u64)
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        if buf.len() % 512 != 0 {
            return Err(());
        }
        let d = self.disk()?;
        for (k, sector) in buf.chunks_exact_mut(512).enumerate() {
            sector.copy_from_slice(&d.data_read(lba as usize + k)?);
        }
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        if buf.len() % 512 != 0 {
            return Err(());
        }
        let d = self.disk()?;
        for (k, sector) in buf.chunks_exact(512).enumerate() {
            let mut s = [0u8; 512];
            s.copy_from_slice(sector);
            d.data_write(lba as usize + k, &mut s)?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.disk()?.flush()
    }
}
/// The open apps' old files, for the one move into the filesystem.
struct Slots(*mut *mut Disk);
impl vfs::Legacy for Slots {
    fn file(&mut self, slot: u32, out: &mut [u8]) -> Result<Option<([u8; 16], usize)>, ()> {
        legacy::file(Extent(self.0).disk()?, slot, out)
    }
}

/// The volume's key. The cipher layer is not wired in (volume.rs `crypt`
/// passes sectors through): there is none to unlock, and the volume is
/// plaintext (README, "What is not done").
const NO_KEY: [u8; 32] = [0; 32];

/// Keep it where it does not move (a static; a Box in the tests): the
/// filesystem's device points at `lent`.
pub struct Mount {
    pub vfs: vfs::Vfs<Extent>,
    lent: *mut Disk,
}
impl Mount {
    pub const fn new() -> Self {
        Mount { vfs: vfs::Vfs::new(), lent: core::ptr::null_mut() }
    }
    fn lend<T>(&mut self, d: &mut Disk, work: impl FnOnce(&mut vfs::Vfs<Extent>, *mut *mut Disk) -> T) -> T {
        self.lent = d;
        let at = core::ptr::addr_of_mut!(self.lent);
        let r = work(&mut self.vfs, at);
        self.lent = core::ptr::null_mut();
        r
    }
    /// At startup: find or place the volume, open it (the first time:
    /// format it and move the old files in) and start serving. Returns what
    /// is wrong for the diagnostic, empty when /data is served.
    pub fn open(&mut self, d: &mut Disk) -> &[u8] {
        match place(d) {
            Err(why) => self.vfs.refuse(why),
            Ok((p, fresh)) => {
                d.use_data(p);
                let ok = self.lend(d, |v, at| {
                    let mut old = Slots(at);
                    v.attach(Extent(at), &NO_KEY, if fresh { Some(&mut old) } else { None })
                });
                // The old files are in: they never go into another volume.
                // (If this write fails the next boot finds them moved anyway.)
                if ok && fresh {
                    let _ = write_record(d, p, true);
                }
            }
        }
        self.vfs.diag()
    }
    /// Answer the request in the first `len` bytes of `win`, in `win`.
    pub fn serve(&mut self, d: &mut Disk, win: &mut [u8], len: usize) {
        self.lend(d, |v, _| v.serve(win, len));
    }
}
/// Where /data is this boot, and whether it may be made and filled there.
fn place(d: &mut Disk) -> Result<(Place, bool), &'static [u8]> {
    let small = extent::small(d.sectors);
    let known = if small.count == 0 { None } else { read_record(d).map_err(|_| b"DATA PLACE READ" as &[u8])? };
    let (p, fresh) = extent::decide(known, d.data_offer(), small)?;
    if known.is_none() {
        write_record(d, p, false).map_err(|_| b"DATA PLACE WRITE" as &[u8])?;
    }
    Ok((p, fresh))
}
