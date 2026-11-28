//! VioletFS: a crash-consistent filesystem over a [`BlockDev`]. No heap, no
//! hardware, no IPC: the whole state is one `Fs` value (about 50 KiB, keep it
//! in a static), so the same code runs in Storage and in the host tests.
//!
//! Layout, in 4 KiB blocks (every metadata block ends in a CRC-32 of the rest):
//!   0, 1      superblock and its copy, written once by `format`
//!   2         journal record: which blocks the committed transaction replaces
//!   3..       journal slots: the new images of those blocks
//!   state     free counts, the allocation cursor, the unfinished-delete marker
//!             and the per-owner quota table
//!   bitmap    one bit per block
//!   inodes    31 inodes of 128 bytes per block
//!   data      file data, directory blocks and block-pointer blocks
//!
//! Consistency. Metadata is never changed in place. A changed block goes to a
//! journal slot; `commit` writes the record naming every slot with its CRC and
//! flushes, and only then copies the slots home. `mount` finishes a copy that
//! was cut short; a record whose slots do not all match was never committed,
//! and `mount` zeroes it before anything else is written, so that it cannot
//! come to match later when the slots are reused. File data
//! is copy-on-write: new data goes to blocks that are free in the last
//! committed state and is flushed before the record that points at it, and a
//! block freed by a transaction is not reused until that transaction is
//! durable. So after a power cut the volume holds exactly the state of the
//! last commit that finished, or of the one in flight.
//!
//! Each operation is all-or-nothing. Several operations share a transaction
//! until `commit` (or until the journal fills, which commits early, always
//! between operations). Three things are split over several transactions when
//! they are large: a write that touches more blocks than `atomic_write()`
//! bytes fill (a cut leaves a prefix of it; `atomic_write_at` gives the bound
//! for an offset), freeing the blocks of a big file (the name is gone, or
//! the size is changed, in the first one; `mount` frees the rest), and giving
//! back the empty blocks at the end of a directory after a name is removed.
//!
//! An `Io`, `Corrupt` or `Full` error from any call, one that only reads
//! included, stops the filesystem until it is mounted again.
//!
//! Quota. Blocks and inodes are counted per owner and checked before an
//! operation changes anything. A directory's blocks are charged to the
//! directory's owner, whoever adds the names, and `link` costs the linker
//! nothing: the caller decides who may add names to whose directory. An
//! owner takes a row of the table (`QUOTAS` rows) only while it holds
//! something or has a limit set.
//!
//! Not here: path walking, permission checks (the mode and owner are stored
//! and returned; the caller enforces them), a limit on links per owner,
//! open-file handles, checksums of file data, any repair of a block whose
//! CRC fails.
use crate::blockdev::BlockDev;

mod check;
#[allow(unused_imports)]
pub use check::Report;

pub const BLOCK: usize = 4096;
const B: u64 = BLOCK as u64;
const BODY: usize = BLOCK - 4;
const SPB: u64 = 8;
/// Block pointers in a pointer block.
const P: u64 = 1023;
const IPB: u32 = 31;
const ISIZE: usize = 128;
const BITS: u32 = (BODY * 8) as u32;
const NC: usize = 8;
const MAX_SLOTS: u32 = ((BODY - 16) / 8) as u32;
/// Owners the quota table has room for.
pub const QUOTAS: usize = 128;
const QOFF: usize = 64;
const QSIZE: usize = 24;
pub const NAME_MAX: usize = 255;
pub const ROOT: u32 = 1;
pub const S_IFMT: u16 = 0xf000;
pub const S_FILE: u16 = 0x8000;
pub const S_DIR: u16 = 0x4000;
pub const S_LINK: u16 = 0xa000;
pub const MAX_SIZE: u64 = (12 + P + P * P + P * P * P) * B;
pub const UNLIMITED: u32 = u32::MAX;

const S_FREE: usize = 4;
const S_IFREE: usize = 8;
const S_ORPHAN: usize = 12;
const S_CURSOR: usize = 16;
const S_ICURSOR: usize = 20;
const S_GEN: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device failed. The filesystem stops until it is mounted again.
    Io,
    /// A metadata block failed its checksum or a structure is impossible.
    Corrupt,
    /// The journal filled inside one operation (a bug; fails like `Io`).
    Full,
    /// An earlier `Io`, `Corrupt` or `Full`: mount again to go on.
    Failed,
    NotFormatted,
    NoSpace,
    NoInodes,
    Quota,
    Exists,
    NotFound,
    NotDir,
    IsDir,
    NotEmpty,
    Invalid,
    TooBig,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Inode {
    /// Type (`S_IFMT`) and permission bits, as Unix has them. 0: free.
    pub mode: u16,
    pub nlink: u16,
    pub owner: u32,
    /// Directories: the directory this one is in.
    pub parent: u32,
    pub flags: u32,
    pub size: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub generation: u32,
    /// Blocks charged to the owner: data and pointer blocks.
    pub blocks: u32,
    ptr: [u32; 15],
}
impl Inode {
    pub fn kind(&self) -> u16 {
        self.mode & S_IFMT
    }
    pub fn is_dir(&self) -> bool {
        self.kind() == S_DIR
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Usage {
    pub blocks: u32,
    pub block_limit: u32,
    pub inodes: u32,
    pub inode_limit: u32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stat {
    pub blocks: u32,
    pub free_blocks: u32,
    pub inodes: u32,
    pub free_inodes: u32,
}

/// One name in a directory, as `readdir` returns it.
#[derive(Clone, Copy, Debug)]
pub struct Name {
    pub ino: u32,
    /// The inode's type, `mode >> 12`.
    pub kind: u8,
    pub len: usize,
    /// Pass to `readdir` for the entry after this one.
    pub next: u64,
}

#[derive(Clone, Copy, Default)]
struct Geo {
    blocks: u32,
    journal: u32,
    slots: u32,
    state: u32,
    bitmap_start: u32,
    bitmap_blocks: u32,
    itable_start: u32,
    itable_blocks: u32,
    inodes: u32,
    data_start: u32,
}

struct Buf {
    blk: u32,
    dirty: bool,
    used: u64,
    data: [u8; BLOCK],
}
const EMPTY: Buf = Buf { blk: 0, dirty: false, used: 0, data: [0; BLOCK] };

pub struct Fs<D: BlockDev> {
    dev: D,
    g: Geo,
    cache: [Buf; NC],
    /// The journal record being built: `n` entries of (block, CRC) from byte 16.
    rec: [u8; BLOCK],
    n: usize,
    seq: u64,
    /// A bitmap block as last committed, for blocks freed by this transaction.
    home: [u8; BLOCK],
    home_blk: u32,
    tmp: [u8; BLOCK],
    tick: u64,
    pending_free: u32,
    data_dirty: bool,
    mounted: bool,
    failed: bool,
    /// Stamped on what an operation changes; the caller keeps it current.
    pub now: u64,
    /// Transactions made durable since `mount`.
    pub commits: u64,
    /// Source line of the last `Corrupt`, for a diagnostic.
    pub why: u32,
    /// Tests only: break a rule the design needs (1: no flush of data before
    /// the record, 2: none after copying home, 3: reuse blocks freed by the
    /// running transaction), to show the power-cut sweep notices.
    #[cfg(test)]
    pub fault: u8,
}

macro_rules! bad {
    ($s:expr) => {{
        $s.why = line!();
        return Err(Error::Corrupt);
    }};
}
pub(crate) use bad;

const CRC: [u32; 16] = [
    0x00000000, 0x1db71064, 0x3b6e20c8, 0x26d930ac, 0x76dc4190, 0x6b6b51f4, 0x4db26158, 0x5005713c,
    0xedb88320, 0xf00f9344, 0xd6d6a3e8, 0xcb61b38c, 0x9b64c2b0, 0x86d3d2d4, 0xa00ae278, 0xbdbdf21c,
];
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        c = CRC[(c & 15) as usize] ^ (c >> 4);
        c = CRC[(c & 15) as usize] ^ (c >> 4);
    }
    !c
}
fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn le64(b: &[u8], at: usize) -> u64 {
    le32(b, at) as u64 | (le32(b, at + 4) as u64) << 32
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn seal(b: &mut [u8; BLOCK]) -> u32 {
    let c = crc32(&b[..BODY]);
    put32(b, BODY, c);
    c
}
fn sealed(b: &[u8; BLOCK]) -> bool {
    crc32(&b[..BODY]) == le32(b, BODY)
}
fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

impl Geo {
    fn layout(blocks: u64, inodes: u32, slots: u32) -> Option<Geo> {
        if blocks >= u32::MAX as u64 || !(16..=MAX_SLOTS).contains(&slots) {
            return None;
        }
        let blocks = blocks as u32;
        let itable_blocks = inodes.max(IPB).div_ceil(IPB);
        let state = 3 + slots;
        let bitmap_blocks = blocks.div_ceil(BITS);
        let itable_start = state as u64 + 1 + bitmap_blocks as u64;
        let data_start = itable_start + itable_blocks as u64;
        if data_start + 8 > blocks as u64 {
            return None;
        }
        Some(Geo {
            blocks,
            journal: 2,
            slots,
            state,
            bitmap_start: state + 1,
            bitmap_blocks,
            itable_start: itable_start as u32,
            itable_blocks,
            inodes: itable_blocks.checked_mul(IPB)?,
            data_start: data_start as u32,
        })
    }
    fn encode(&self, b: &mut [u8; BLOCK]) {
        b.fill(0);
        b[..8].copy_from_slice(b"VIOLETFS");
        put32(b, 8, 1);
        put32(b, 12, self.blocks);
        put32(b, 16, self.slots);
        put32(b, 20, self.inodes);
        seal(b);
    }
    /// The layout follows from three numbers; anything else is not ours.
    fn parse(b: &[u8; BLOCK], sectors: u64) -> Option<Geo> {
        if &b[..8] != b"VIOLETFS" || !sealed(b) || le32(b, 8) != 1 {
            return None;
        }
        let g = Geo::layout(le32(b, 12) as u64, le32(b, 20), le32(b, 16))?;
        (g.inodes == le32(b, 20) && g.blocks as u64 * SPB <= sectors).then_some(g)
    }
}

/// Where a file block's pointer lives: in the inode or in a pointer block.
#[derive(Clone, Copy)]
enum Loc {
    Direct(usize),
    Ind(u32, usize),
}

/// A directory record: `ino` 0 is free room.
#[derive(Clone, Copy)]
struct Rec {
    blk: u32,
    off: usize,
    /// The record before it in the same block, or `usize::MAX`.
    prev: usize,
    len: usize,
    ino: u32,
    kind: u8,
    name_len: usize,
    next: u64,
}

fn path(f: u64) -> Result<(usize, [usize; 4]), Error> {
    if f < 12 {
        return Ok((0, [f as usize, 0, 0, 0]));
    }
    let f = f - 12;
    if f < P {
        return Ok((1, [12, f as usize, 0, 0]));
    }
    let f = f - P;
    if f < P * P {
        return Ok((2, [13, (f / P) as usize, (f % P) as usize, 0]));
    }
    let f = f - P * P;
    if f < P * P * P {
        return Ok((3, [14, (f / (P * P)) as usize, (f / P % P) as usize, (f % P) as usize]));
    }
    Err(Error::TooBig)
}

fn name_ok(name: &[u8]) -> Result<(), Error> {
    if name.is_empty() || name.len() > NAME_MAX || name == b"." || name == b".." || name.iter().any(|&c| c == 0 || c == b'/') {
        return Err(Error::Invalid);
    }
    Ok(())
}

impl<D: BlockDev> Fs<D> {
    pub const fn new(dev: D) -> Self {
        Fs {
            dev,
            g: Geo { blocks: 0, journal: 0, slots: 0, state: 0, bitmap_start: 0, bitmap_blocks: 0, itable_start: 0, itable_blocks: 0, inodes: 0, data_start: 0 },
            cache: [EMPTY; NC],
            rec: [0; BLOCK],
            n: 0,
            seq: 0,
            home: [0; BLOCK],
            home_blk: 0,
            tmp: [0; BLOCK],
            tick: 0,
            pending_free: 0,
            data_dirty: false,
            mounted: false,
            failed: false,
            now: 0,
            commits: 0,
            why: 0,
            #[cfg(test)]
            fault: 0,
        }
    }
    pub fn device(&mut self) -> &mut D {
        &mut self.dev
    }
    fn reset(&mut self) {
        for b in &mut self.cache {
            b.blk = 0;
            b.dirty = false;
        }
        self.n = 0;
        self.home_blk = 0;
        self.pending_free = 0;
        self.data_dirty = false;
        self.mounted = false;
        self.failed = false;
    }

    // ---- device ----
    fn dread(dev: &mut D, blk: u32, buf: &mut [u8]) -> Result<(), Error> {
        dev.read(blk as u64 * SPB, buf).map_err(|_| Error::Io)
    }
    fn dwrite(dev: &mut D, blk: u32, buf: &[u8]) -> Result<(), Error> {
        dev.write(blk as u64 * SPB, buf).map_err(|_| Error::Io)
    }
    fn dflush(&mut self) -> Result<(), Error> {
        self.dev.flush().map_err(|_| Error::Io)
    }

    // ---- format and mount ----

    /// Make an empty filesystem over the whole device and mount it. `inodes`
    /// 0 gives one per 64 KiB; `journal_slots` 0 gives 64 to 256 by size.
    /// Not atomic: a cut during it leaves a device that does not mount.
    pub fn format(&mut self, inodes: u32, journal_slots: u32) -> Result<(), Error> {
        self.reset();
        let blocks = self.dev.sectors() / SPB;
        let slots = if journal_slots == 0 { ((blocks / 64) as u32).clamp(64, 256) } else { journal_slots };
        let inodes = if inodes == 0 { (blocks / 16).min(u32::MAX as u64 / 2) as u32 } else { inodes };
        let g = Geo::layout(blocks, inodes, slots).ok_or(Error::Invalid)?;
        self.tmp.fill(0);
        for blk in 0..3 {
            Self::dwrite(&mut self.dev, blk, &self.tmp)?;
        }
        self.dflush()?;
        self.tmp[..4].copy_from_slice(b"VST1");
        put32(&mut self.tmp, S_FREE, g.blocks - g.data_start);
        put32(&mut self.tmp, S_IFREE, g.inodes - 1);
        put32(&mut self.tmp, S_CURSOR, g.data_start);
        put32(&mut self.tmp, S_GEN, 1);
        // The root directory belongs to owner 0.
        put32(&mut self.tmp, QOFF + 4, 1);
        put32(&mut self.tmp, QOFF + 12, UNLIMITED);
        put32(&mut self.tmp, QOFF + 16, 1);
        put32(&mut self.tmp, QOFF + 20, UNLIMITED);
        seal(&mut self.tmp);
        Self::dwrite(&mut self.dev, g.state, &self.tmp)?;
        for k in 0..g.bitmap_blocks {
            self.tmp.fill(0);
            for bit in 0..BITS {
                let b = k as u64 * BITS as u64 + bit as u64;
                if b < g.data_start as u64 || b >= g.blocks as u64 {
                    self.tmp[(bit / 8) as usize] |= 1 << (bit % 8);
                }
            }
            seal(&mut self.tmp);
            Self::dwrite(&mut self.dev, g.bitmap_start + k, &self.tmp)?;
        }
        for k in 0..g.itable_blocks {
            self.tmp.fill(0);
            if k == 0 {
                let root = Inode { mode: S_DIR | 0o755, nlink: 1, generation: 1, ..Inode::default() };
                Self::iencode(&root, &mut self.tmp[..ISIZE]);
            }
            seal(&mut self.tmp);
            Self::dwrite(&mut self.dev, g.itable_start + k, &self.tmp)?;
        }
        self.dflush()?;
        g.encode(&mut self.tmp);
        Self::dwrite(&mut self.dev, 0, &self.tmp)?;
        Self::dwrite(&mut self.dev, 1, &self.tmp)?;
        self.dflush()?;
        self.mount()
    }

    /// Recover and open the filesystem: finish the committed transaction if
    /// its copy home was cut short, then finish an interrupted delete.
    pub fn mount(&mut self) -> Result<(), Error> {
        self.reset();
        let r = self.mount_inner();
        self.mounted = r.is_ok();
        r
    }
    fn mount_inner(&mut self) -> Result<(), Error> {
        let mut found = Err(Error::NotFormatted);
        for copy in 0..2 {
            match Self::dread(&mut self.dev, copy, &mut self.tmp) {
                Err(e) => found = Err(e),
                Ok(()) => {
                    if let Some(g) = Geo::parse(&self.tmp, self.dev.sectors()) {
                        found = Ok(g);
                        break;
                    }
                }
            }
        }
        self.g = found?;
        self.replay()?;
        let i = self.load(self.g.state, false)?;
        if &self.cache[i].data[..4] != b"VST1" {
            bad!(self);
        }
        self.mounted = true;
        let orphan = self.st(S_ORPHAN)?;
        if orphan != 0 {
            self.reap(orphan)?;
            self.commit_inner()?;
        }
        Ok(())
    }
    fn replay(&mut self) -> Result<(), Error> {
        let g = self.g;
        Self::dread(&mut self.dev, g.journal, &mut self.rec)?;
        let n = le32(&self.rec, 4) as usize;
        if &self.rec[..4] != b"VJ01" || !sealed(&self.rec) || n > g.slots as usize {
            return self.wipe_record();
        }
        self.seq = le64(&self.rec, 8);
        // Committed only if every slot is the block the record names.
        for pass in 0..2 {
            for e in 0..n {
                let target = le32(&self.rec, 16 + 8 * e);
                if target == 0 {
                    continue;
                }
                Self::dread(&mut self.dev, g.journal + 1 + e as u32, &mut self.tmp)?;
                if pass == 0 {
                    if target < g.state || target >= g.blocks || !sealed(&self.tmp) || le32(&self.tmp, BODY) != le32(&self.rec, 20 + 8 * e) {
                        return self.wipe_record();
                    }
                } else {
                    Self::dwrite(&mut self.dev, target, &self.tmp)?;
                }
            }
        }
        self.dflush()
    }
    /// A record that is not a committed transaction must not stay on the
    /// medium: the next transaction reuses the slots, and if it wrote the
    /// missing ones back with the same bytes, a later mount would take the
    /// old record for committed and replay what was rolled back here. So it
    /// is zeroed, durably, before anything else is written. (A record that
    /// was replayed stays: replaying it again changes nothing.)
    fn wipe_record(&mut self) -> Result<(), Error> {
        if self.rec.iter().all(|&b| b == 0) {
            return Ok(());
        }
        self.rec.fill(0);
        Self::dwrite(&mut self.dev, self.g.journal, &self.rec)?;
        self.dflush()
    }

    fn live(&self) -> Result<(), Error> {
        if self.failed || !self.mounted {
            return Err(Error::Failed);
        }
        Ok(())
    }
    fn guard<T>(&mut self, r: Result<T, Error>) -> Result<T, Error> {
        if matches!(r, Err(Error::Io | Error::Corrupt | Error::Full)) {
            self.failed = true;
        }
        r
    }

    // ---- block cache and journal ----

    fn slot_of(&self, blk: u32) -> Option<usize> {
        (0..self.n).find(|&e| le32(&self.rec, 16 + 8 * e) == blk)
    }
    /// Bring a metadata block into the cache (`fresh`: a new, zeroed one).
    /// The index is good until the next `load`.
    fn load(&mut self, blk: u32, fresh: bool) -> Result<usize, Error> {
        if blk < self.g.state || blk >= self.g.blocks {
            bad!(self);
        }
        self.tick += 1;
        let mut v = 0;
        for i in 0..NC {
            if self.cache[i].blk == blk {
                self.cache[i].used = self.tick;
                if fresh {
                    self.cache[i].data.fill(0);
                }
                return Ok(i);
            }
            if self.cache[i].blk == 0 || (self.cache[v].blk != 0 && self.cache[i].used < self.cache[v].used) {
                v = i;
            }
        }
        self.writeback(v)?;
        self.cache[v].blk = 0;
        if fresh {
            self.cache[v].data.fill(0);
        } else {
            let (at, want) = match self.slot_of(blk) {
                Some(s) => (self.g.journal + 1 + s as u32, Some(le32(&self.rec, 20 + 8 * s))),
                None => (blk, None),
            };
            Self::dread(&mut self.dev, at, &mut self.cache[v].data)?;
            let c = crc32(&self.cache[v].data[..BODY]);
            if c != le32(&self.cache[v].data, BODY) || want.is_some_and(|w| w != c) {
                bad!(self);
            }
        }
        self.cache[v].blk = blk;
        self.cache[v].dirty = false;
        self.cache[v].used = self.tick;
        Ok(v)
    }
    /// Put the block in the running transaction before changing it.
    fn dirty(&mut self, i: usize) -> Result<(), Error> {
        if self.cache[i].dirty {
            return Ok(());
        }
        if self.slot_of(self.cache[i].blk).is_none() {
            if self.n >= self.g.slots as usize {
                return Err(Error::Full);
            }
            put32(&mut self.rec, 16 + 8 * self.n, self.cache[i].blk);
            put32(&mut self.rec, 20 + 8 * self.n, 0);
            self.n += 1;
        }
        self.cache[i].dirty = true;
        Ok(())
    }
    fn writeback(&mut self, i: usize) -> Result<(), Error> {
        if !self.cache[i].dirty {
            return Ok(());
        }
        let c = seal(&mut self.cache[i].data);
        let Some(s) = self.slot_of(self.cache[i].blk) else { bad!(self) };
        put32(&mut self.rec, 20 + 8 * s, c);
        Self::dwrite(&mut self.dev, self.g.journal + 1 + s as u32, &self.cache[i].data)?;
        self.cache[i].dirty = false;
        Ok(())
    }
    /// A freed block is nobody's metadata any more.
    fn forget(&mut self, blk: u32) {
        for b in &mut self.cache {
            if b.blk == blk {
                b.blk = 0;
                b.dirty = false;
            }
        }
        if let Some(s) = self.slot_of(blk) {
            put32(&mut self.rec, 16 + 8 * s, 0);
        }
    }
    fn rd(&mut self, blk: u32, at: usize) -> Result<u32, Error> {
        let i = self.load(blk, false)?;
        Ok(le32(&self.cache[i].data, at))
    }
    fn wr(&mut self, blk: u32, at: usize, v: u32) -> Result<(), Error> {
        let i = self.load(blk, false)?;
        self.dirty(i)?;
        put32(&mut self.cache[i].data, at, v);
        Ok(())
    }
    fn st(&mut self, at: usize) -> Result<u32, Error> {
        self.rd(self.g.state, at)
    }
    fn st_set(&mut self, at: usize, v: u32) -> Result<(), Error> {
        self.wr(self.g.state, at, v)
    }
    /// Make room in the journal for an operation that changes up to `n` blocks.
    fn reserve(&mut self, n: usize) -> Result<(), Error> {
        if n > self.g.slots as usize {
            return Err(Error::Full);
        }
        if self.n + n > self.g.slots as usize {
            self.commit_inner()?;
        }
        Ok(())
    }

    /// Make everything done so far durable. Returns once it would survive a
    /// power cut.
    pub fn commit(&mut self) -> Result<(), Error> {
        self.live()?;
        let r = self.commit_inner();
        self.guard(r)
    }
    fn commit_inner(&mut self) -> Result<(), Error> {
        if self.n == 0 {
            return Ok(());
        }
        for i in 0..NC {
            self.writeback(i)?;
        }
        // New file data must be on the medium before the record that uses it.
        #[cfg(test)]
        let skip = self.fault;
        #[cfg(not(test))]
        let skip = 0u8;
        if self.data_dirty && skip != 1 {
            self.dflush()?;
        }
        self.seq += 1;
        self.rec[..4].copy_from_slice(b"VJ01");
        put32(&mut self.rec, 4, self.n as u32);
        put64(&mut self.rec, 8, self.seq);
        seal(&mut self.rec);
        Self::dwrite(&mut self.dev, self.g.journal, &self.rec)?;
        self.dflush()?;
        self.commits += 1;
        // Durable. Copy the blocks home; mount repeats this if it is cut.
        for e in 0..self.n {
            let target = le32(&self.rec, 16 + 8 * e);
            if target == 0 {
                continue;
            }
            if let Some(i) = (0..NC).find(|&i| self.cache[i].blk == target) {
                Self::dwrite(&mut self.dev, target, &self.cache[i].data)?;
            } else {
                Self::dread(&mut self.dev, self.g.journal + 1 + e as u32, &mut self.tmp)?;
                Self::dwrite(&mut self.dev, target, &self.tmp)?;
            }
        }
        // The slots are reused next: home must be durable first.
        if skip != 2 {
            self.dflush()?;
        }
        self.n = 0;
        self.pending_free = 0;
        self.home_blk = 0;
        self.data_dirty = false;
        Ok(())
    }

    // ---- quota ----

    /// The owner's row in the quota table and a free row, if there are such.
    fn quota_row(&mut self, owner: u32) -> Result<(Option<usize>, Option<usize>), Error> {
        let i = self.load(self.g.state, false)?;
        let mut spare = None;
        for k in 0..QUOTAS {
            let at = QOFF + k * QSIZE;
            let d = &self.cache[i].data;
            if le32(d, at + 4) & 1 == 0 {
                spare = spare.or(Some(at));
            } else if le32(d, at) == owner {
                return Ok((Some(at), spare));
            }
        }
        Ok((None, spare))
    }
    fn quota_find(&mut self, owner: u32, make: bool) -> Result<Option<usize>, Error> {
        let (found, spare) = self.quota_row(owner)?;
        if found.is_some() {
            return Ok(found);
        }
        let Some(at) = spare.filter(|_| make) else { return Ok(None) };
        let i = self.load(self.g.state, false)?;
        self.dirty(i)?;
        let d = &mut self.cache[i].data;
        d[at..at + QSIZE].fill(0);
        put32(d, at, owner);
        put32(d, at + 4, 1);
        put32(d, at + 12, UNLIMITED);
        put32(d, at + 20, UNLIMITED);
        Ok(Some(at))
    }
    /// Would `owner` stay within its limits with this much more? Changes
    /// nothing: an owner without a row has no limits and gets its row when
    /// it is first charged (`Quota` here when the table has no room for one),
    /// so an operation that is refused leaves no row behind.
    fn quota_ok(&mut self, owner: u32, blocks: u32, inodes: u32) -> Result<(), Error> {
        let at = match self.quota_row(owner)? {
            (Some(at), _) => at,
            (None, Some(_)) => return Ok(()),
            (None, None) => return Err(Error::Quota),
        };
        let i = self.load(self.g.state, false)?;
        let d = &self.cache[i].data;
        let fits = |used: u32, more: u32, limit: u32| used.checked_add(more).is_some_and(|t| t <= limit);
        if !fits(le32(d, at + 8), blocks, le32(d, at + 12)) || !fits(le32(d, at + 16), inodes, le32(d, at + 20)) {
            return Err(Error::Quota);
        }
        Ok(())
    }
    fn charge(&mut self, owner: u32, blocks: i64, inodes: i64) -> Result<(), Error> {
        let Some(at) = self.quota_find(owner, true)? else { bad!(self) };
        let i = self.load(self.g.state, false)?;
        self.dirty(i)?;
        let b = le32(&self.cache[i].data, at + 8) as i64 + blocks;
        let n = le32(&self.cache[i].data, at + 16) as i64 + inodes;
        if b < 0 || n < 0 || b > u32::MAX as i64 || n > u32::MAX as i64 {
            bad!(self);
        }
        put32(&mut self.cache[i].data, at + 8, b as u32);
        put32(&mut self.cache[i].data, at + 16, n as u32);
        self.quota_release(at)
    }
    /// A row that says nothing (no usage, no limits) is free for another
    /// owner: the table has `QUOTAS` rows, and owners come and go.
    fn quota_release(&mut self, at: usize) -> Result<(), Error> {
        let i = self.load(self.g.state, false)?;
        let d = &self.cache[i].data;
        if le32(d, at + 8) == 0 && le32(d, at + 16) == 0 && le32(d, at + 12) == UNLIMITED && le32(d, at + 20) == UNLIMITED {
            self.dirty(i)?;
            self.cache[i].data[at..at + QSIZE].fill(0);
        }
        Ok(())
    }
    /// Limit what `owner` may hold, in 4 KiB blocks and in files. A limit
    /// keeps a row of the table (`Quota` when all `QUOTAS` are taken) until
    /// both limits are set back to `UNLIMITED` and the owner holds nothing.
    pub fn set_quota(&mut self, owner: u32, blocks: u32, inodes: u32) -> Result<(), Error> {
        self.live()?;
        let r = (|| {
            self.reserve(2)?;
            let make = blocks != UNLIMITED || inodes != UNLIMITED;
            let Some(at) = self.quota_find(owner, make)? else {
                return if make { Err(Error::Quota) } else { Ok(()) };
            };
            self.st_set(at + 12, blocks)?;
            self.st_set(at + 20, inodes)?;
            self.quota_release(at)
        })();
        self.guard(r)
    }
    pub fn usage(&mut self, owner: u32) -> Result<Usage, Error> {
        self.live()?;
        let r = (|| {
            let Some(at) = self.quota_find(owner, false)? else {
                return Ok(Usage { block_limit: UNLIMITED, inode_limit: UNLIMITED, ..Usage::default() });
            };
            let i = self.load(self.g.state, false)?;
            let d = &self.cache[i].data;
            Ok(Usage { blocks: le32(d, at + 8), block_limit: le32(d, at + 12), inodes: le32(d, at + 16), inode_limit: le32(d, at + 20) })
        })();
        self.guard(r)
    }

    // ---- block allocation ----

    fn okp(&self, b: u32) -> bool {
        b >= self.g.data_start && b < self.g.blocks
    }
    /// Is the block in use in the last committed state?
    fn home_bit(&mut self, b: u32) -> Result<bool, Error> {
        let hb = self.g.bitmap_start + b / BITS;
        let bit = (b % BITS) as usize;
        if self.slot_of(hb).is_none() {
            let i = self.load(hb, false)?;
            return Ok(self.cache[i].data[bit / 8] & 1 << (bit % 8) != 0);
        }
        if self.home_blk != hb {
            self.home_blk = 0;
            Self::dread(&mut self.dev, hb, &mut self.home)?;
            if !sealed(&self.home) {
                bad!(self);
            }
            self.home_blk = hb;
        }
        Ok(self.home[bit / 8] & 1 << (bit % 8) != 0)
    }
    /// Are `n` blocks free? Blocks this transaction freed do not count until
    /// it is durable, so commit first if they are needed.
    fn space(&mut self, n: u32) -> Result<(), Error> {
        let free = self.st(S_FREE)?;
        if n > free {
            return Err(Error::NoSpace);
        }
        if n > free - self.pending_free.min(free) {
            self.commit_inner()?;
        }
        Ok(())
    }
    /// Take a block that is free now and was free at the last commit, so
    /// writing it cannot hurt the committed state. The search starts where the
    /// last one ended and goes round the volume: no block is rewritten more
    /// often than its neighbours just for being near the front.
    fn alloc(&mut self, owner: u32) -> Result<u32, Error> {
        let (lo, hi) = (self.g.data_start, self.g.blocks);
        let mut c = self.st(S_CURSOR)?;
        if c < lo || c >= hi {
            c = lo;
        }
        let mut left = hi - lo;
        #[cfg(test)]
        let reuse = self.fault == 3;
        #[cfg(not(test))]
        let reuse = false;
        loop {
            if left == 0 {
                bad!(self);
            }
            let bit = (c % BITS) as usize;
            let i = self.load(self.g.bitmap_start + c / BITS, false)?;
            let byte = self.cache[i].data[bit / 8];
            if bit % 8 == 0 && byte == 0xff && left >= 8 && hi - c >= 8 {
                c += 8;
                left -= 8;
            } else {
                if byte & 1 << (bit % 8) == 0 && (reuse || !self.home_bit(c)?) {
                    break;
                }
                c += 1;
                left -= 1;
            }
            if c >= hi {
                c = lo;
            }
        }
        let bit = (c % BITS) as usize;
        let i = self.load(self.g.bitmap_start + c / BITS, false)?;
        self.dirty(i)?;
        self.cache[i].data[bit / 8] |= 1 << (bit % 8);
        let free = self.st(S_FREE)?;
        if free == 0 {
            bad!(self);
        }
        self.st_set(S_FREE, free - 1)?;
        self.st_set(S_CURSOR, c + 1)?;
        self.charge(owner, 1, 0)?;
        Ok(c)
    }
    fn free_block(&mut self, b: u32, owner: u32) -> Result<(), Error> {
        if !self.okp(b) {
            bad!(self);
        }
        let was = self.home_bit(b)?;
        let bit = (b % BITS) as usize;
        let i = self.load(self.g.bitmap_start + b / BITS, false)?;
        if self.cache[i].data[bit / 8] & 1 << (bit % 8) == 0 {
            bad!(self);
        }
        self.dirty(i)?;
        self.cache[i].data[bit / 8] &= !(1 << (bit % 8));
        if was {
            self.pending_free += 1;
        }
        self.forget(b);
        let free = self.st(S_FREE)?;
        self.st_set(S_FREE, free + 1)?;
        self.charge(owner, -1, 0)
    }
    fn new_meta(&mut self, owner: u32) -> Result<u32, Error> {
        let b = self.alloc(owner)?;
        let i = self.load(b, true)?;
        self.dirty(i)?;
        Ok(b)
    }

    // ---- inodes ----

    fn iat(&mut self, ino: u32) -> Result<(u32, usize), Error> {
        if ino == 0 || ino > self.g.inodes {
            return Err(Error::Invalid);
        }
        Ok((self.g.itable_start + (ino - 1) / IPB, ((ino - 1) % IPB) as usize * ISIZE))
    }
    fn iencode(n: &Inode, b: &mut [u8]) {
        b[..ISIZE].fill(0);
        put16(b, 0, n.mode);
        put16(b, 2, n.nlink);
        put32(b, 4, n.owner);
        put32(b, 8, n.parent);
        put32(b, 12, n.flags);
        put64(b, 16, n.size);
        put64(b, 24, n.mtime);
        put64(b, 32, n.ctime);
        put32(b, 40, n.generation);
        put32(b, 44, n.blocks);
        for (k, p) in n.ptr.iter().enumerate() {
            put32(b, 48 + 4 * k, *p);
        }
    }
    fn iget(&mut self, ino: u32) -> Result<Inode, Error> {
        let (blk, at) = self.iat(ino)?;
        let i = self.load(blk, false)?;
        let b = &self.cache[i].data[at..at + ISIZE];
        let mut n = Inode {
            mode: le16(b, 0),
            nlink: le16(b, 2),
            owner: le32(b, 4),
            parent: le32(b, 8),
            flags: le32(b, 12),
            size: le64(b, 16),
            mtime: le64(b, 24),
            ctime: le64(b, 32),
            generation: le32(b, 40),
            blocks: le32(b, 44),
            ptr: [0; 15],
        };
        for k in 0..15 {
            n.ptr[k] = le32(b, 48 + 4 * k);
        }
        Ok(n)
    }
    /// An inode that is in use.
    fn inode(&mut self, ino: u32) -> Result<Inode, Error> {
        let n = self.iget(ino)?;
        if n.mode == 0 {
            return Err(Error::NotFound);
        }
        Ok(n)
    }
    fn iput(&mut self, ino: u32, n: &Inode) -> Result<(), Error> {
        let (blk, at) = self.iat(ino)?;
        let i = self.load(blk, false)?;
        self.dirty(i)?;
        Self::iencode(n, &mut self.cache[i].data[at..at + ISIZE]);
        Ok(())
    }
    fn ialloc(&mut self, mut n: Inode) -> Result<u32, Error> {
        let count = self.g.inodes;
        let start = self.st(S_ICURSOR)? % count;
        for k in 0..count {
            let ino = (start + k) % count + 1;
            let (blk, at) = self.iat(ino)?;
            if self.rd(blk, at)? as u16 != 0 {
                continue;
            }
            let free = self.st(S_IFREE)?;
            if free == 0 {
                bad!(self);
            }
            n.generation = self.st(S_GEN)?;
            self.st_set(S_GEN, n.generation.wrapping_add(1))?;
            self.st_set(S_IFREE, free - 1)?;
            self.st_set(S_ICURSOR, ino % count)?;
            self.charge(n.owner, 0, 1)?;
            self.iput(ino, &n)?;
            return Ok(ino);
        }
        bad!(self)
    }
    fn ifree(&mut self, ino: u32, owner: u32) -> Result<(), Error> {
        self.iput(ino, &Inode::default())?;
        let free = self.st(S_IFREE)?;
        self.st_set(S_IFREE, free + 1)?;
        self.charge(owner, 0, -1)
    }

    // ---- file block map ----

    /// Where the pointer to file block `f` lives. With `grow`, missing pointer
    /// blocks are made (and counted in the inode, which the caller writes
    /// back); without, a hole in the way gives `None`.
    fn map(&mut self, n: &mut Inode, f: u64, grow: bool) -> Result<Option<Loc>, Error> {
        let (depth, ix) = path(f)?;
        if depth == 0 {
            return Ok(Some(Loc::Direct(ix[0])));
        }
        let mut cur = n.ptr[ix[0]];
        if cur == 0 {
            if !grow {
                return Ok(None);
            }
            cur = self.new_meta(n.owner)?;
            n.ptr[ix[0]] = cur;
            n.blocks += 1;
        }
        for level in 1..depth {
            if !self.okp(cur) {
                bad!(self);
            }
            let mut next = self.rd(cur, ix[level] * 4)?;
            if next == 0 {
                if !grow {
                    return Ok(None);
                }
                next = self.new_meta(n.owner)?;
                self.wr(cur, ix[level] * 4, next)?;
                n.blocks += 1;
            }
            cur = next;
        }
        if !self.okp(cur) {
            bad!(self);
        }
        Ok(Some(Loc::Ind(cur, ix[depth])))
    }
    fn ptr(&mut self, n: &Inode, loc: Loc) -> Result<u32, Error> {
        let p = match loc {
            Loc::Direct(k) => n.ptr[k],
            Loc::Ind(blk, k) => self.rd(blk, k * 4)?,
        };
        if p != 0 && !self.okp(p) {
            bad!(self);
        }
        Ok(p)
    }
    fn set_ptr(&mut self, n: &mut Inode, loc: Loc, v: u32) -> Result<(), Error> {
        match loc {
            Loc::Direct(k) => n.ptr[k] = v,
            Loc::Ind(blk, k) => self.wr(blk, k * 4, v)?,
        }
        Ok(())
    }
    /// The disk block of file block `f`, or 0 for a hole.
    fn bmap(&mut self, n: &Inode, f: u64) -> Result<u32, Error> {
        let mut copy = *n;
        match self.map(&mut copy, f, false)? {
            Some(loc) => self.ptr(n, loc),
            None => Ok(0),
        }
    }
    /// How many pointer blocks mapping file block `f` would add.
    fn missing(&mut self, n: &Inode, f: u64) -> Result<u32, Error> {
        let (depth, ix) = path(f)?;
        if depth == 0 {
            return Ok(0);
        }
        let mut cur = n.ptr[ix[0]];
        for level in 1..=depth {
            if cur == 0 {
                return Ok((depth + 1 - level) as u32);
            }
            if !self.okp(cur) {
                bad!(self);
            }
            if level < depth {
                cur = self.rd(cur, ix[level] * 4)?;
            }
        }
        Ok(0)
    }

    // ---- files ----

    pub fn stat(&mut self, ino: u32) -> Result<Inode, Error> {
        self.live()?;
        let r = self.inode(ino);
        self.guard(r)
    }
    pub fn statfs(&mut self) -> Result<Stat, Error> {
        self.live()?;
        let r = (|| {
            Ok(Stat {
                blocks: self.g.blocks - self.g.data_start,
                free_blocks: self.st(S_FREE)?,
                inodes: self.g.inodes,
                free_inodes: self.st(S_IFREE)?,
            })
        })();
        self.guard(r)
    }
    /// The largest write that is all-or-nothing across a power cut when it
    /// starts on a block boundary. What counts is the blocks a write touches:
    /// use `atomic_write_at` for a write that starts anywhere else.
    pub fn atomic_write(&self) -> usize {
        self.chunk() as usize * BLOCK
    }
    /// The largest write at offset `off` that is all-or-nothing across a
    /// power cut: one that touches no more than `atomic_write() / BLOCK`
    /// blocks. A longer one is done in pieces of that many blocks, each
    /// all-or-nothing, and a cut leaves a prefix of it.
    pub fn atomic_write_at(&self, off: u64) -> usize {
        self.atomic_write() - (off % B) as usize
    }
    fn chunk(&self) -> u64 {
        ((self.g.slots as u64 - 14) / 2).clamp(1, 64)
    }

    /// Read from a file (or a symbolic link's target). Holes read as zeros.
    pub fn read(&mut self, ino: u32, off: u64, buf: &mut [u8]) -> Result<usize, Error> {
        self.live()?;
        let r = self.read_inner(ino, off, buf);
        self.guard(r)
    }
    fn read_inner(&mut self, ino: u32, off: u64, buf: &mut [u8]) -> Result<usize, Error> {
        let n = self.inode(ino)?;
        if n.is_dir() {
            return Err(Error::IsDir);
        }
        if off >= n.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(n.size - off) as usize;
        let mut done = 0;
        while done < len {
            let pos = off + done as u64;
            let (f, at) = (pos / B, (pos % B) as usize);
            let take = (BLOCK - at).min(len - done);
            let p = self.bmap(&n, f)?;
            let out = &mut buf[done..done + take];
            if p == 0 {
                out.fill(0);
            } else if take == BLOCK {
                Self::dread(&mut self.dev, p, out)?;
            } else {
                Self::dread(&mut self.dev, p, &mut self.tmp)?;
                out.copy_from_slice(&self.tmp[at..at + take]);
            }
            done += take;
        }
        Ok(len)
    }

    /// Write to a file. Returns how much was written: less than asked when
    /// space or quota ran out part way, an error when nothing fitted. Old
    /// blocks are never overwritten, so a write of up to
    /// `atomic_write_at(off)` bytes changes all together or not at all.
    pub fn write(&mut self, ino: u32, off: u64, data: &[u8]) -> Result<usize, Error> {
        self.live()?;
        let r = self.write_inner(ino, off, data);
        self.guard(r)
    }
    fn write_inner(&mut self, ino: u32, off: u64, data: &[u8]) -> Result<usize, Error> {
        let mut n = self.inode(ino)?;
        if n.is_dir() {
            return Err(Error::IsDir);
        }
        let end = off.checked_add(data.len() as u64).filter(|e| *e <= MAX_SIZE).ok_or(Error::TooBig)?;
        if data.is_empty() {
            return Ok(0);
        }
        if off / B > n.size / B {
            self.seal_tail(ino, &mut n)?;
        }
        let mut done = 0usize;
        while done < data.len() {
            let pos = off + done as u64;
            let f0 = pos / B;
            let f1 = ((end - 1) / B).min(f0 + self.chunk() - 1);
            let stop = end.min((f1 + 1) * B);
            let count = (f1 - f0 + 1) as u32;
            // Everything that can refuse comes before anything changes.
            let fits = self.write_fits(&n, f0, f1, count);
            match fits {
                Err(Error::NoSpace | Error::Quota) if done > 0 => return Ok(done),
                other => other?,
            }
            for f in f0..=f1 {
                let lo = pos.max(f * B);
                let hi = stop.min((f + 1) * B);
                let loc = match self.map(&mut n, f, true)? {
                    Some(loc) => loc,
                    None => bad!(self),
                };
                let old = self.ptr(&n, loc)?;
                let new = self.alloc(n.owner)?;
                let src = &data[(lo - off) as usize..(hi - off) as usize];
                if src.len() == BLOCK {
                    Self::dwrite(&mut self.dev, new, src)?;
                } else {
                    // Keep the rest of the old block, but nothing past the
                    // old end of the file (a truncate leaves old bytes there).
                    let keep = n.size.saturating_sub(f * B).min(B) as usize;
                    if old != 0 && keep > 0 {
                        Self::dread(&mut self.dev, old, &mut self.tmp)?;
                    }
                    self.tmp[if old != 0 { keep } else { 0 }..].fill(0);
                    self.tmp[(lo - f * B) as usize..(hi - f * B) as usize].copy_from_slice(src);
                    Self::dwrite(&mut self.dev, new, &self.tmp)?;
                }
                self.set_ptr(&mut n, loc, new)?;
                if old != 0 {
                    self.free_block(old, n.owner)?;
                } else {
                    n.blocks += 1;
                }
            }
            n.size = n.size.max(stop);
            n.mtime = self.now;
            self.iput(ino, &n)?;
            self.data_dirty = true;
            done = (stop - off) as usize;
        }
        Ok(done)
    }
    fn write_fits(&mut self, n: &Inode, f0: u64, f1: u64, count: u32) -> Result<(), Error> {
        self.reserve(2 * count as usize + 14)?;
        let (mut holes, mut pointers) = (0, 0);
        for f in f0..=f1 {
            let (depth, ix) = path(f)?;
            if f == f0 || (depth > 0 && ix[depth] == 0) {
                pointers += self.missing(n, f)?;
            }
            if self.bmap(n, f)? == 0 {
                holes += 1;
            }
        }
        self.quota_ok(n.owner, holes + pointers, 0)?;
        self.space(count + pointers)
    }
    /// Zero what lies past the end of the file in its last block, before the
    /// file grows over it.
    fn seal_tail(&mut self, ino: u32, n: &mut Inode) -> Result<(), Error> {
        let tail = (n.size % B) as usize;
        if tail == 0 {
            return Ok(());
        }
        let f = n.size / B;
        let Some(loc) = self.map(n, f, false)? else { return Ok(()) };
        let old = self.ptr(n, loc)?;
        if old == 0 {
            return Ok(());
        }
        Self::dread(&mut self.dev, old, &mut self.tmp)?;
        if self.tmp[tail..].iter().all(|&b| b == 0) {
            return Ok(());
        }
        self.reserve(8)?;
        self.space(1)?;
        Self::dread(&mut self.dev, old, &mut self.tmp)?;
        self.tmp[tail..].fill(0);
        let new = self.alloc(n.owner)?;
        Self::dwrite(&mut self.dev, new, &self.tmp)?;
        self.set_ptr(n, loc, new)?;
        self.free_block(old, n.owner)?;
        self.iput(ino, n)?;
        self.data_dirty = true;
        Ok(())
    }

    /// Set a file's size. Growing leaves a hole that reads as zeros.
    pub fn truncate(&mut self, ino: u32, size: u64) -> Result<(), Error> {
        self.live()?;
        let r = self.truncate_inner(ino, size);
        self.guard(r)
    }
    fn truncate_inner(&mut self, ino: u32, size: u64) -> Result<(), Error> {
        let mut n = self.inode(ino)?;
        if n.is_dir() {
            return Err(Error::IsDir);
        }
        if size > MAX_SIZE {
            return Err(Error::TooBig);
        }
        if size > n.size {
            self.seal_tail(ino, &mut n)?;
            self.reserve(2)?;
            n.size = size;
            n.mtime = self.now;
            return self.iput(ino, &n);
        }
        if size < n.size {
            self.reserve(6)?;
            n.size = size;
            n.mtime = self.now;
            self.iput(ino, &n)?;
            self.st_set(S_ORPHAN, ino)?;
            self.trim(ino)?;
            self.st_set(S_ORPHAN, 0)?;
        }
        Ok(())
    }

    /// Between two frees: commit if the journal is nearly full.
    fn step(&mut self) -> Result<(), Error> {
        if self.n + 6 > self.g.slots as usize {
            self.commit_inner()?;
        }
        Ok(())
    }
    /// Free every block of the file past its size. Each free is complete in
    /// itself, so this may commit part way; the marker in the state block
    /// makes `mount` finish it.
    fn trim(&mut self, ino: u32) -> Result<(), Error> {
        let mut n = self.iget(ino)?;
        let keep = n.size.div_ceil(B);
        for k in 0..12 {
            if k as u64 >= keep && n.ptr[k] != 0 {
                if n.blocks == 0 {
                    bad!(self);
                }
                self.step()?;
                self.free_block(n.ptr[k], n.owner)?;
                n.ptr[k] = 0;
                n.blocks -= 1;
                self.iput(ino, &n)?;
            }
        }
        let mut base = 12u64;
        let mut span = P;
        for k in 12..15 {
            if n.ptr[k] != 0 && base + span > keep {
                if !self.okp(n.ptr[k]) {
                    bad!(self);
                }
                let top = n.ptr[k];
                self.trim_tree(ino, &mut n, top, k - 11, base, keep)?;
                if base >= keep {
                    if n.blocks == 0 {
                        bad!(self);
                    }
                    self.step()?;
                    self.free_block(n.ptr[k], n.owner)?;
                    n.ptr[k] = 0;
                    n.blocks -= 1;
                    self.iput(ino, &n)?;
                }
            }
            base += span;
            span *= P;
        }
        Ok(())
    }
    fn trim_tree(&mut self, ino: u32, n: &mut Inode, blk: u32, level: usize, base: u64, keep: u64) -> Result<(), Error> {
        let span = if level == 1 { 1 } else if level == 2 { P } else { P * P };
        for k in 0..P as usize {
            let first = base + k as u64 * span;
            if first + span <= keep {
                continue;
            }
            let child = self.rd(blk, k * 4)?;
            if child == 0 {
                continue;
            }
            if !self.okp(child) || n.blocks == 0 {
                bad!(self);
            }
            if level > 1 {
                self.trim_tree(ino, n, child, level - 1, first, keep)?;
            }
            if first >= keep {
                self.step()?;
                self.free_block(child, n.owner)?;
                self.wr(blk, k * 4, 0)?;
                n.blocks -= 1;
                self.iput(ino, n)?;
            }
        }
        Ok(())
    }
    /// Finish a delete or truncate that a power cut interrupted.
    fn reap(&mut self, ino: u32) -> Result<(), Error> {
        let n = self.iget(ino)?;
        if n.mode != 0 {
            self.trim(ino)?;
            if n.nlink == 0 {
                self.ifree(ino, n.owner)?;
            }
        }
        self.st_set(S_ORPHAN, 0)
    }

    // ---- directories ----

    fn dir(&mut self, ino: u32) -> Result<Inode, Error> {
        let n = self.inode(ino)?;
        if !n.is_dir() {
            return Err(Error::NotDir);
        }
        Ok(n)
    }
    /// The record at `*pos` (free room included), stepping `*pos` and `*prev`.
    fn dnext(&mut self, d: &Inode, pos: &mut u64, prev: &mut usize) -> Result<Option<Rec>, Error> {
        loop {
            if *pos >= d.size {
                return Ok(None);
            }
            let (f, off) = (*pos / B, (*pos % B) as usize);
            if off >= BODY {
                *pos = (f + 1) * B;
                *prev = usize::MAX;
                continue;
            }
            if off == 0 {
                *prev = usize::MAX;
            }
            if off % 4 != 0 || off + 8 > BODY {
                bad!(self);
            }
            let blk = self.bmap(d, f)?;
            if blk == 0 {
                bad!(self);
            }
            let i = self.load(blk, false)?;
            let b = &self.cache[i].data;
            let r = Rec {
                blk,
                off,
                prev: *prev,
                len: le16(b, off + 4) as usize,
                ino: le32(b, off),
                kind: b[off + 6],
                name_len: b[off + 7] as usize,
                next: *pos + le16(b, off + 4) as u64,
            };
            if r.len < 8 || r.len % 4 != 0 || off + r.len > BODY || (r.ino != 0 && (r.name_len == 0 || 8 + r.name_len > r.len)) {
                bad!(self);
            }
            *prev = off;
            *pos = r.next;
            return Ok(Some(r));
        }
    }
    fn find(&mut self, d: &Inode, name: &[u8]) -> Result<Option<Rec>, Error> {
        let (mut pos, mut prev) = (0, usize::MAX);
        while let Some(r) = self.dnext(d, &mut pos, &mut prev)? {
            if r.ino != 0 && r.name_len == name.len() {
                let i = self.load(r.blk, false)?;
                if &self.cache[i].data[r.off + 8..r.off + 8 + r.name_len] == name {
                    return Ok(Some(r));
                }
            }
        }
        Ok(None)
    }
    /// Room for a record of `need` bytes: after a name, or in a free record.
    fn dslot(&mut self, d: &Inode, need: usize) -> Result<Option<Rec>, Error> {
        let (mut pos, mut prev) = (0, usize::MAX);
        while let Some(r) = self.dnext(d, &mut pos, &mut prev)? {
            let used = if r.ino == 0 { 0 } else { 8 + pad4(r.name_len) };
            if r.len - used >= need {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }
    /// Blocks adding `name` would take: 0 when there is room already.
    fn dneed(&mut self, d: &Inode, name: &[u8]) -> Result<u32, Error> {
        if self.dslot(d, 8 + pad4(name.len()))?.is_some() {
            return Ok(0);
        }
        Ok(1 + self.missing(d, d.size / B)?)
    }
    fn dadd(&mut self, dino: u32, d: &mut Inode, name: &[u8], ino: u32, kind: u8) -> Result<(), Error> {
        let need = 8 + pad4(name.len());
        let (blk, off, len) = match self.dslot(d, need)? {
            Some(r) => {
                let used = if r.ino == 0 { 0 } else { 8 + pad4(r.name_len) };
                if used != 0 {
                    let i = self.load(r.blk, false)?;
                    self.dirty(i)?;
                    put16(&mut self.cache[i].data, r.off + 4, used as u16);
                }
                (r.blk, r.off + used, r.len - used)
            }
            None => {
                let Some(loc) = self.map(d, d.size / B, true)? else { bad!(self) };
                let blk = self.new_meta(d.owner)?;
                self.set_ptr(d, loc, blk)?;
                d.blocks += 1;
                d.size += B;
                (blk, 0, BODY)
            }
        };
        let i = self.load(blk, false)?;
        self.dirty(i)?;
        let b = &mut self.cache[i].data;
        put32(b, off, ino);
        put16(b, off + 4, len as u16);
        b[off + 6] = kind;
        b[off + 7] = name.len() as u8;
        b[off + 8..off + 8 + name.len()].copy_from_slice(name);
        d.mtime = self.now;
        self.iput(dino, d)
    }
    fn dremove(&mut self, r: Rec) -> Result<(), Error> {
        let i = self.load(r.blk, false)?;
        self.dirty(i)?;
        let b = &mut self.cache[i].data;
        put32(b, r.off, 0);
        if r.prev != usize::MAX {
            let joined = le16(b, r.prev + 4) as usize + r.len;
            put16(b, r.prev + 4, joined as u16);
        }
        Ok(())
    }
    /// Give back the blocks at the end of a directory that hold no name
    /// (removing names joins their room, so such a block is one free record).
    /// Its own step after the removal that emptied them, and like a truncate
    /// it may take more than one transaction; `mount` finishes it. Blocks
    /// before the last name stay: a directory is not compacted.
    fn dshrink(&mut self, dino: u32) -> Result<(), Error> {
        let mut d = self.iget(dino)?;
        let mut size = d.size;
        while size >= B {
            let blk = self.bmap(&d, size / B - 1)?;
            if blk == 0 {
                bad!(self);
            }
            let i = self.load(blk, false)?;
            if le32(&self.cache[i].data, 0) != 0 || le16(&self.cache[i].data, 4) as usize != BODY {
                break;
            }
            size -= B;
        }
        if size == d.size {
            return Ok(());
        }
        self.step()?;
        d.size = size;
        self.iput(dino, &d)?;
        self.st_set(S_ORPHAN, dino)?;
        self.trim(dino)?;
        self.st_set(S_ORPHAN, 0)
    }
    fn dempty(&mut self, d: &Inode) -> Result<bool, Error> {
        let (mut pos, mut prev) = (0, usize::MAX);
        while let Some(r) = self.dnext(d, &mut pos, &mut prev)? {
            if r.ino != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn lookup(&mut self, dir: u32, name: &[u8]) -> Result<u32, Error> {
        self.live()?;
        let r = (|| {
            let d = self.dir(dir)?;
            Ok(self.find(&d, name)?.ok_or(Error::NotFound)?.ino)
        })();
        self.guard(r)
    }
    /// The name at `cookie` (0 to start) copied into `name`, or `None` at the
    /// end. A cookie is where a record starts in the directory, and the
    /// caller's word is not taken for it: the records of its block are walked
    /// from the block's start, and a cookie that is not one of them (made up,
    /// or left behind when names were removed and their room joined) gives
    /// `Invalid`: start again from 0. So what is returned is always a name
    /// that is in the directory, never bytes of a name read as a record. A
    /// cookie kept across changes may still skip or repeat names.
    pub fn readdir(&mut self, dir: u32, cookie: u64, name: &mut [u8; NAME_MAX]) -> Result<Option<Name>, Error> {
        self.live()?;
        let r = self.readdir_inner(dir, cookie, name);
        self.guard(r)
    }
    fn readdir_inner(&mut self, dir: u32, cookie: u64, name: &mut [u8; NAME_MAX]) -> Result<Option<Name>, Error> {
        let d = self.dir(dir)?;
        if cookie >= d.size {
            return Ok(None);
        }
        let (mut pos, mut prev) = (cookie - cookie % B, usize::MAX);
        while pos < cookie {
            if pos % B >= BODY as u64 || self.dnext(&d, &mut pos, &mut prev)?.is_none() {
                break;
            }
        }
        if pos != cookie {
            return Err(Error::Invalid);
        }
        while let Some(r) = self.dnext(&d, &mut pos, &mut prev)? {
            if r.ino != 0 {
                let i = self.load(r.blk, false)?;
                name[..r.name_len].copy_from_slice(&self.cache[i].data[r.off + 8..r.off + 8 + r.name_len]);
                return Ok(Some(Name { ino: r.ino, kind: r.kind, len: r.name_len, next: r.next }));
            }
        }
        Ok(None)
    }

    /// Make a file, directory or symbolic link (`mode` carries the type; a
    /// link's target is written as its data). Returns the new inode.
    pub fn create(&mut self, dir: u32, name: &[u8], mode: u16, owner: u32) -> Result<u32, Error> {
        self.live()?;
        let r = self.create_inner(dir, name, mode, owner);
        self.guard(r)
    }
    fn create_inner(&mut self, dir: u32, name: &[u8], mode: u16, owner: u32) -> Result<u32, Error> {
        name_ok(name)?;
        if !matches!(mode & S_IFMT, S_FILE | S_DIR | S_LINK) {
            return Err(Error::Invalid);
        }
        let mut d = self.dir(dir)?;
        if self.find(&d, name)?.is_some() {
            return Err(Error::Exists);
        }
        self.reserve(14)?;
        let need = self.dneed(&d, name)?;
        self.quota_ok(d.owner, need, 0)?;
        self.quota_ok(owner, 0, 1)?;
        if self.st(S_IFREE)? == 0 {
            return Err(Error::NoInodes);
        }
        self.space(need)?;
        let n = Inode {
            mode,
            nlink: 1,
            owner,
            parent: if mode & S_IFMT == S_DIR { dir } else { 0 },
            mtime: self.now,
            ctime: self.now,
            ..Inode::default()
        };
        let ino = self.ialloc(n)?;
        self.dadd(dir, &mut d, name, ino, (mode >> 12) as u8)?;
        Ok(ino)
    }

    /// Give a file a second name.
    pub fn link(&mut self, dir: u32, name: &[u8], ino: u32) -> Result<(), Error> {
        self.live()?;
        let r = (|| {
            name_ok(name)?;
            let mut d = self.dir(dir)?;
            let mut n = self.inode(ino)?;
            if n.is_dir() || n.nlink == u16::MAX {
                return Err(Error::Invalid);
            }
            if self.find(&d, name)?.is_some() {
                return Err(Error::Exists);
            }
            self.reserve(14)?;
            let need = self.dneed(&d, name)?;
            self.quota_ok(d.owner, need, 0)?;
            self.space(need)?;
            n.nlink += 1;
            n.ctime = self.now;
            self.iput(ino, &n)?;
            self.dadd(dir, &mut d, name, ino, (n.mode >> 12) as u8)
        })();
        self.guard(r)
    }

    /// Remove a file's name; the file goes with its last name.
    pub fn unlink(&mut self, dir: u32, name: &[u8]) -> Result<(), Error> {
        self.live()?;
        let r = self.remove(dir, name, false);
        self.guard(r)
    }
    /// Remove an empty directory.
    pub fn rmdir(&mut self, dir: u32, name: &[u8]) -> Result<(), Error> {
        self.live()?;
        let r = self.remove(dir, name, true);
        self.guard(r)
    }
    fn remove(&mut self, dir: u32, name: &[u8], want_dir: bool) -> Result<(), Error> {
        let mut d = self.dir(dir)?;
        let r = self.find(&d, name)?.ok_or(Error::NotFound)?;
        let mut t = self.inode(r.ino)?;
        if want_dir {
            if !t.is_dir() {
                return Err(Error::NotDir);
            }
            if !self.dempty(&t)? {
                return Err(Error::NotEmpty);
            }
        } else if t.is_dir() {
            return Err(Error::IsDir);
        }
        self.reserve(10)?;
        self.dremove(r)?;
        d.mtime = self.now;
        self.iput(dir, &d)?;
        self.unref(r.ino, &mut t)?;
        self.dshrink(dir)
    }
    /// Drop one name of an inode; free it with the last.
    fn unref(&mut self, ino: u32, t: &mut Inode) -> Result<(), Error> {
        if t.nlink == 0 {
            bad!(self);
        }
        t.nlink -= 1;
        t.ctime = self.now;
        if t.nlink > 0 {
            return self.iput(ino, t);
        }
        t.size = 0;
        self.iput(ino, t)?;
        self.st_set(S_ORPHAN, ino)?;
        self.trim(ino)?;
        self.ifree(ino, t.owner)?;
        self.st_set(S_ORPHAN, 0)
    }

    /// Move a name, replacing what the new name held (a file by anything but
    /// a directory, an empty directory by a directory). One transaction: after
    /// a cut the old name or the new one exists, never both or neither.
    pub fn rename(&mut self, from: u32, name: &[u8], to: u32, new: &[u8]) -> Result<(), Error> {
        self.live()?;
        let r = self.rename_inner(from, name, to, new);
        self.guard(r)
    }
    fn rename_inner(&mut self, from: u32, name: &[u8], to: u32, new: &[u8]) -> Result<(), Error> {
        name_ok(new)?;
        let d1 = self.dir(from)?;
        let mut d2 = self.dir(to)?;
        let src = self.find(&d1, name)?.ok_or(Error::NotFound)?;
        let mut s = self.inode(src.ino)?;
        if from == to && name == new {
            return Ok(());
        }
        if s.is_dir() {
            // A directory must not move into itself.
            let mut up = to;
            let mut hops = 0;
            while up != ROOT {
                if up == src.ino {
                    return Err(Error::Invalid);
                }
                up = self.dir(up)?.parent;
                hops += 1;
                if hops > self.g.inodes {
                    bad!(self);
                }
            }
        }
        let dst = self.find(&d2, new)?;
        let mut old = None;
        if let Some(r) = dst {
            if r.ino == src.ino {
                return Ok(());
            }
            let t = self.inode(r.ino)?;
            match (s.is_dir(), t.is_dir()) {
                (true, false) => return Err(Error::NotDir),
                (false, true) => return Err(Error::IsDir),
                (true, true) if !self.dempty(&t)? => return Err(Error::NotEmpty),
                _ => {}
            }
            old = Some((r, t));
        }
        self.reserve(16)?;
        if old.is_none() {
            let need = self.dneed(&d2, new)?;
            self.quota_ok(d2.owner, need, 0)?;
            self.space(need)?;
        }
        match old {
            Some((r, _)) => {
                let i = self.load(r.blk, false)?;
                self.dirty(i)?;
                put32(&mut self.cache[i].data, r.off, src.ino);
                self.cache[i].data[r.off + 6] = (s.mode >> 12) as u8;
                d2.mtime = self.now;
                self.iput(to, &d2)?;
            }
            None => self.dadd(to, &mut d2, new, src.ino, (s.mode >> 12) as u8)?,
        }
        // Adding may have shortened the old name's record: look it up again.
        let mut d1 = self.iget(from)?;
        let Some(src) = self.find(&d1, name)? else { bad!(self) };
        self.dremove(src)?;
        d1.mtime = self.now;
        self.iput(from, &d1)?;
        if s.is_dir() && from != to {
            s.parent = to;
        }
        s.ctime = self.now;
        self.iput(src.ino, &s)?;
        if let Some((r, mut t)) = old {
            self.unref(r.ino, &mut t)?;
        }
        self.dshrink(from)
    }

    /// Change the permission bits (the type stays).
    pub fn chmod(&mut self, ino: u32, perm: u16) -> Result<(), Error> {
        self.live()?;
        let r = (|| {
            let mut n = self.inode(ino)?;
            self.reserve(2)?;
            n.mode = n.mode & S_IFMT | perm & !S_IFMT;
            n.ctime = self.now;
            self.iput(ino, &n)
        })();
        self.guard(r)
    }
    /// Give the inode, and what it is charged for, to another owner.
    pub fn chown(&mut self, ino: u32, owner: u32) -> Result<(), Error> {
        self.chown_over(ino, owner, 0)
    }
    /// `chown` of a file that is about to replace the file `old` (0: none)
    /// by a rename: what `old` holds for the new owner counts as given back,
    /// so that a file can be replaced by one as large within the quota. If
    /// the rename never comes, the caller removes the file.
    pub fn chown_over(&mut self, ino: u32, owner: u32, old: u32) -> Result<(), Error> {
        self.live()?;
        let r = (|| {
            let mut n = self.inode(ino)?;
            if n.owner == owner {
                return Ok(());
            }
            self.reserve(4)?;
            let mut back = (0, 0);
            if old != 0 {
                let o = self.inode(old)?;
                if o.owner == owner && o.nlink == 1 && !o.is_dir() {
                    back = (o.blocks, 1);
                }
            }
            self.quota_ok(owner, n.blocks.saturating_sub(back.0), 1 - back.1)?;
            self.charge(n.owner, -(n.blocks as i64), -1)?;
            self.charge(owner, n.blocks as i64, 1)?;
            n.owner = owner;
            n.ctime = self.now;
            self.iput(ino, &n)
        })();
        self.guard(r)
    }
    pub fn utime(&mut self, ino: u32, mtime: u64) -> Result<(), Error> {
        self.live()?;
        let r = (|| {
            let mut n = self.inode(ino)?;
            self.reserve(2)?;
            n.mtime = mtime;
            self.iput(ino, &n)
        })();
        self.guard(r)
    }

    /// Tell the device which blocks are free (eMMC TRIM), so its own wear
    /// levelling can use them. Commits first; returns the blocks discarded.
    pub fn discard_free(&mut self) -> Result<u64, Error> {
        self.commit()?;
        let r = self.discard_inner();
        self.guard(r)
    }
    fn discard_inner(&mut self) -> Result<u64, Error> {
        let (mut run, mut total) = (0u64, 0u64);
        for b in self.g.data_start..=self.g.blocks {
            let used = b == self.g.blocks || {
                let bit = (b % BITS) as usize;
                let i = self.load(self.g.bitmap_start + b / BITS, false)?;
                self.cache[i].data[bit / 8] & 1 << (bit % 8) != 0
            };
            if !used {
                run += 1;
            } else if run > 0 {
                self.dev.discard((b as u64 - run) * SPB, run * SPB).map_err(|_| Error::Io)?;
                total += run;
                run = 0;
            }
        }
        Ok(total)
    }
}
