//! The data volume: where VioletFS sits on Storage's disk, and the rule for
//! formatting it. Hardware-free (the host tests run it on the test disk).
//!
//! The disk driver hands over one extent of sectors (block.rs and emmc.rs say
//! which). Its first eight sectors are the volume's header; the filesystem
//! takes the rest, through `crypt::open(dev, &volume_key)`.
//!
//! The header (sector 0 and a copy in sector 1) says "a filesystem was made
//! here, this many sectors". It is written after `format` finished, and it
//! is what makes formatting happen once:
//!   - no header and nothing that mounts: format, then write the header
//!     (a format that was cut short is simply done again);
//!   - a header: mount. If that fails, nothing is formatted: with a cipher
//!     underneath a wrong key looks like an empty disk, and the files of a
//!     volume that only failed to open must not be erased. /data is then
//!     unavailable and Storage's diagnostic says so.
use crate::blockdev::BlockDev;
use super::violetfs::{crc32, Error, Fs};

/// The block cipher under the filesystem. The crypt cluster's layer goes
/// here; until it lands this has its signature and passes sectors through,
/// so the volume is NOT encrypted yet.
pub mod crypt {
    use super::BlockDev;
    pub struct Dev<D: BlockDev>(D);
    pub fn open<D: BlockDev>(dev: D, _volume_key: &[u8; 32]) -> Dev<D> {
        Dev(dev)
    }
    impl<D: BlockDev> BlockDev for Dev<D> {
        fn sectors(&self) -> u64 {
            self.0.sectors()
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
            self.0.read(lba, buf)
        }
        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
            self.0.write(lba, buf)
        }
        fn flush(&mut self) -> Result<(), ()> {
            self.0.flush()
        }
        fn discard(&mut self, lba: u64, sectors: u64) -> Result<(), ()> {
            self.0.discard(lba, sectors)
        }
    }
}

/// Sectors `first..first + count` of a device, and nothing else of it.
pub struct Part<D: BlockDev> {
    dev: D,
    first: u64,
    count: u64,
}
impl<D: BlockDev> Part<D> {
    fn at(&self, lba: u64, bytes: usize) -> Result<u64, ()> {
        let n = (bytes / 512) as u64;
        if bytes % 512 != 0 || lba.checked_add(n).map_or(true, |end| end > self.count) {
            return Err(());
        }
        Ok(self.first + lba)
    }
}
impl<D: BlockDev> BlockDev for Part<D> {
    fn sectors(&self) -> u64 {
        self.count
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        let at = self.at(lba, buf.len())?;
        self.dev.read(at, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        let at = self.at(lba, buf.len())?;
        self.dev.write(at, buf)
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.dev.flush()
    }
    fn discard(&mut self, lba: u64, sectors: u64) -> Result<(), ()> {
        if lba.checked_add(sectors).map_or(true, |end| end > self.count) {
            return Err(());
        }
        self.dev.discard(self.first + lba, sectors)
    }
}

/// A device that may not be there yet: the filesystem is a static built
/// before the disk is known, and gets its device when the volume opens.
pub struct Slot<D: BlockDev>(pub Option<D>);
impl<D: BlockDev> BlockDev for Slot<D> {
    fn sectors(&self) -> u64 {
        self.0.as_ref().map_or(0, |d| d.sectors())
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        self.0.as_mut().ok_or(())?.read(lba, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        self.0.as_mut().ok_or(())?.write(lba, buf)
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.0.as_mut().ok_or(())?.flush()
    }
    fn discard(&mut self, lba: u64, sectors: u64) -> Result<(), ()> {
        self.0.as_mut().ok_or(())?.discard(lba, sectors)
    }
}

/// What the filesystem sits on.
pub type Stack<D> = Slot<crypt::Dev<Part<D>>>;

pub const HEADER: u64 = 8;
const TAG: &[u8] = b"VioletOS data volume v1\n";
/// The smallest extent worth a filesystem: 64 blocks after the header.
pub const MIN_SECTORS: u64 = HEADER + 64 * 8;

fn header(sectors: u64) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..TAG.len()].copy_from_slice(TAG);
    h[24..28].copy_from_slice(&CIPHER.to_le_bytes());
    h[28..36].copy_from_slice(&sectors.to_le_bytes());
    let sum = crc32(&h[..508]);
    h[508..].copy_from_slice(&sum.to_le_bytes());
    h
}
/// The filesystem's size in a header sector, if it is one: `Ok(None)` when
/// it is not, `Err` when it is one of another cipher.
fn parse(h: &[u8; 512]) -> Result<Option<u64>, &'static [u8]> {
    let sum = u32::from_le_bytes([h[508], h[509], h[510], h[511]]);
    if &h[..TAG.len()] != TAG || crc32(&h[..508]) != sum {
        return Ok(None);
    }
    if h[24..28] != CIPHER.to_le_bytes() {
        return Err(b"DATA CIPHER");
    }
    let mut n = [0u8; 8];
    n.copy_from_slice(&h[28..36]);
    Ok(Some(u64::from_le_bytes(n)))
}

/// Inodes and journal slots for a new filesystem of `blocks` blocks: half
/// as many files as blocks (62 to 8192), and a small journal on a small
/// volume, where the default (64 slots and up) would take much of it. With
/// 16 slots a write is all-or-nothing a block at a time, with 32 nine.
pub fn shape(blocks: u64) -> (u32, u32) {
    let slots = match blocks {
        0..=255 => 16,
        256..=1023 => 32,
        _ => 0,
    };
    ((blocks / 2).clamp(62, 8192) as u32, slots)
}

/// The cipher this build puts under the filesystem, written into the
/// header: 1 is none (`crypt` above passes sectors through). The build that
/// mounts through services/storage/crypt must say another number here, so
/// that a volume made without a cipher is refused ("DATA CIPHER") instead
/// of being read through one, and the other way round.
const CIPHER: u32 = 1;

/// Open the volume on `raw` and mount its filesystem into `fs`, formatting
/// it only the first time (the rule is at the top of this file), and only
/// if `fresh`: a volume that was made and filled before (mount.rs's record
/// says so) and is not there now is lost, "DATA LOST", not made again
/// empty. `Ok(true)` when it was formatted now. An error is a short text
/// for the diagnostic; the filesystem is then not mounted and nothing was
/// erased.
pub fn open<D: BlockDev + Clone>(fs: &mut Fs<Stack<D>>, raw: D, key: &[u8; 32], fresh: bool) -> Result<bool, &'static [u8]> {
    let mut head = raw.clone();
    let total = raw.sectors();
    if total < MIN_SECTORS {
        return Err(b"DATA EXTENT SMALL");
    }
    let mut known = None;
    for copy in 0..2 {
        let mut h = [0u8; 512];
        head.read(copy, &mut h).map_err(|_| b"DATA HEADER READ" as &[u8])?;
        known = known.or(parse(&h)?);
    }
    let room = (total - HEADER) / 8 * 8;
    let sectors = match known {
        Some(n) if n % 8 == 0 && n >= MIN_SECTORS - HEADER && n <= room => n,
        Some(_) => return Err(b"DATA HEADER BOUNDS"),
        None => room,
    };
    *fs.device() = Slot(Some(crypt::open(Part { dev: raw, first: HEADER, count: sectors }, key)));
    let mounted = fs.mount();
    let formatted = match (mounted, known) {
        (Ok(()), _) => false,
        // Nothing was ever made here (or making it was cut short).
        (Err(Error::NotFormatted), None) if !fresh => return Err(b"DATA LOST"),
        (Err(Error::NotFormatted), None) => {
            let (inodes, slots) = shape(sectors / 8);
            fs.format(inodes, slots).map_err(|_| b"DATA FORMAT" as &[u8])?;
            true
        }
        (Err(Error::Io), _) => return Err(b"DATA READ"),
        (Err(_), _) => return Err(b"DATA MOUNT"),
    };
    if known.is_none() {
        let h = header(sectors);
        for copy in 0..2 {
            head.write(copy, &h).and_then(|_| head.flush()).map_err(|_| b"DATA HEADER WRITE" as &[u8])?;
        }
    }
    Ok(formatted)
}
