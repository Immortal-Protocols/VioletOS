//! /data as a service: VioletFS (violetfs/) mounted on the data volume
//! (volume.rs) and reached by path through the requests of vfsproto.rs.
//! Paths, owners, permissions, quotas and the one move of the old files
//! are decided here, in Storage; the Open Runtime keeps a client.
//!
//! Owners. /data is flat at the top, as the runtime's policy has it
//! (runtime/open/policy.rs `owns_data`): the app NAME owns /data/NAME (a
//! file, or a folder and all in it) and /data/NAME.ANYTHING. So the owner of
//! every file follows from the first part of its path: what is before the
//! first dot. Names that start with a dot are the system's (owner 0). Each
//! owner gets a number (kept in /.vfs/owners, at most `OWNERS` at a time,
//! given back when the owner holds nothing) which VioletFS stores on its
//! inodes and counts quota by.
//!
//! Permissions. A request made for an app (`who`) is held to that rule a
//! second time here, on Storage's own reading of the path: another owner's
//! names and dot names are ENOENT, unless the runtime says its policy shares
//! the name (`SHARED`). On top of it the owner's mode bits are checked (read
//! 0400, write 0200; adding or removing names needs write on the folder).
//! Requests without `who` are the runtime's own and are not restricted.
//! Storage has one client, so `who` is the runtime's word: this guards
//! against the runtime's mistakes, not against the runtime.
//!
//! Quota. An app owner may hold a quarter of the volume's blocks and of its
//! files unless `QUOTA` set other limits (EDQUOT beyond), and nothing once
//! the volume's free blocks are down to the system's reserve of a sixteenth
//! (ENOSPC), so that settings can still be saved on a full volume. The
//! system's own names have no limit. Who is charged: violetfs/mod.rs.
//!
//! The old files. Before VioletFS, /data was 32 slots in files.rs. The first
//! time the volume is opened they are copied in (same names, at the top),
//! and /.vfs/migrated is made in the transaction that ends the copy.
//! Nothing is served before that marker is durable, and a copy that was cut
//! short is done again from the slots, which are never changed: so the
//! volume shows either no /data at all (not yet available) or every old
//! file. The slots are left as they were (an older system image in the
//! other boot slot still reads them); they are not kept up to date.
//!
//! No heap, no hardware, no IPC: one `Vfs` value (keep it in a static).
use crate::blockdev::BlockDev;
use super::vfsproto::*;
use super::violetfs::{crc32, Error, Fs, Inode, Usage, NAME_MAX, ROOT, S_DIR, S_FILE, S_IFMT, UNLIMITED};
use super::volume::{self, Stack};

/// The 32 slots of files.rs, as the move reads them (legacy.rs).
pub trait Legacy {
    /// Name and length of the file in `slot`, its bytes in `out`; `Ok(None)`
    /// for an empty slot. `Err`: it could not be read whole (or is longer
    /// than `out`).
    fn file(&mut self, slot: u32, out: &mut [u8]) -> Result<Option<([u8; 16], usize)>, ()>;
}

const KEY: usize = 24;
/// Owners that hold something at one time (one fewer than VioletFS has
/// quota rows for: the system takes one).
pub const OWNERS: usize = 120;
const REC: usize = 32;
const SYS: &[u8] = b".vfs";
/// Names the runtime keeps for Settings (wifi.rs; shared to it by its
/// manifest), not an app's own: the system's, like the dot names, so that
/// the reserve keeps saving them on a full volume.
const SYSTEM: [&[u8]; 2] = [b"networks", b"relay"];
const LEGACY_SLOTS: u32 = 32;

type R<T> = Result<T, i32>;

fn errno(e: Error) -> i32 {
    match e {
        Error::Io | Error::Corrupt | Error::Full | Error::Failed => EIO,
        Error::NotFormatted => ENODEV,
        Error::NoSpace | Error::NoInodes => ENOSPC,
        Error::Quota => EDQUOT,
        Error::Exists => EEXIST,
        Error::NotFound => ENOENT,
        Error::NotDir => ENOTDIR,
        Error::IsDir => EISDIR,
        Error::NotEmpty => ENOTEMPTY,
        Error::Invalid => EINVAL,
        Error::TooBig => EFBIG,
    }
}

/// The path without its leading slash.
fn rel(path: &[u8]) -> &[u8] {
    path.strip_prefix(b"/").unwrap_or(path)
}
fn top(path: &[u8]) -> &[u8] {
    rel(path).split(|&c| c == b'/').next().unwrap_or(b"")
}
/// `name` is `owner` or `owner.ANYTHING` (policy.rs `covers`).
fn covers(owner: &[u8], name: &[u8]) -> bool {
    name.strip_prefix(owner).is_some_and(|rest| rest.is_empty() || rest[0] == b'.')
}
/// Who owns a path: None for the system (dot names, /data itself, and a
/// first part too long to be an app's name).
pub fn owner_key(path: &[u8]) -> Option<&[u8]> {
    let name = top(path);
    if SYSTEM.contains(&name) {
        return None;
    }
    let key = name.split(|&c| c == b'.').next().unwrap_or(b"");
    (!key.is_empty() && key.len() <= KEY).then_some(key)
}

/// May the app `who` reach `path` at all? (The mode bits come after.)
fn may(who: &[u8], flags: u32, path: &[u8], write: bool) -> R<()> {
    if who.is_empty() {
        return Ok(());
    }
    if who.len() > KEY || who.iter().any(|&c| c == b'.' || c == b'/' || c == 0) {
        return Err(EACCES);
    }
    let name = top(path);
    if name.is_empty() {
        return if write { Err(EACCES) } else { Ok(()) };
    }
    if name[0] != b'.' && (covers(who, name) || flags & SHARED != 0) {
        Ok(())
    } else {
        Err(ENOENT)
    }
}

pub struct Vfs<D: BlockDev> {
    fs: Fs<Stack<D>>,
    ready: bool,
    /// The filesystem stopped (a device error or damaged metadata): it is
    /// mounted again before the next request, which is then answered ESTALE.
    broken: bool,
    pub epoch: u32,
    /// Why /data is not available (`diag`). Kept by value so that the whole
    /// server starts as zeros.
    why: [u8; 24],
    why_len: u8,
    sys: u32,
    owners_ino: u32,
    owners: [[u8; KEY]; OWNERS],
    /// The replacement in progress: /.vfs/put holds its first `size` bytes.
    put_active: bool,
    put_crc: u32,
    put_size: u64,
    put_owner: u32,
    /// Blocks of the file the replacement takes the place of, when its
    /// owner's (they come back with the last piece).
    put_old: u32,
    reserve: u32,
    def_blocks: u32,
    def_inodes: u32,
    name: [u8; NAME_MAX],
    buf: [u8; 4096],
}

macro_rules! fs {
    ($s:ident . $($call:tt)+) => {{
        let r = $s.fs.$($call)+;
        $s.ck(r)?
    }};
}

impl<D: BlockDev + Clone> Vfs<D> {
    pub const fn new() -> Self {
        Vfs {
            fs: Fs::new(volume::Slot(None)),
            ready: false,
            broken: false,
            epoch: 0,
            why: [0; 24],
            why_len: 0,
            sys: 0,
            owners_ino: 0,
            owners: [[0; KEY]; OWNERS],
            put_active: false,
            put_crc: 0,
            put_size: 0,
            put_owner: 0,
            put_old: 0,
            reserve: 0,
            def_blocks: 0,
            def_inodes: 0,
            name: [0; NAME_MAX],
            buf: [0; 4096],
        }
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    /// Why /data is not available, for Storage's diagnostic. Empty: it is
    /// (or no volume was ever attached).
    pub fn diag(&self) -> &[u8] {
        &self.why[..self.why_len as usize]
    }
    /// For the checks of the host tests.
    pub fn fs(&mut self) -> &mut Fs<Stack<D>> {
        &mut self.fs
    }

    /// Open the volume on `raw` and start serving. With `legacy` this is
    /// the volume the old files go into (mount.rs's record says so): it is
    /// formatted if nothing was ever made there, and the old files are moved
    /// in if that has not been done. Without, they went into a volume before:
    /// a volume that is not there is lost, not new, and is never formatted
    /// or filled with them again. On failure `diag` says why and every
    /// request is refused.
    pub fn attach(&mut self, raw: D, key: &[u8; 32], legacy: Option<&mut dyn Legacy>) -> bool {
        self.broken = false;
        let fresh = legacy.is_some();
        let why: &[u8] = match volume::open(&mut self.fs, raw, key, fresh) {
            Err(why) => why,
            Ok(_) => match self.setup(legacy) {
                Ok(()) => b"",
                Err(why) => why,
            },
        };
        self.refuse(why);
        self.ready
    }
    /// Serve nothing (ENODEV) and say `why`; an empty `why` serves.
    pub fn refuse(&mut self, why: &[u8]) {
        self.why_len = why.len().min(self.why.len()) as u8;
        self.why[..self.why_len as usize].copy_from_slice(&why[..self.why_len as usize]);
        self.ready = why.is_empty();
    }

    fn ck<T>(&mut self, r: Result<T, Error>) -> R<T> {
        r.map_err(|e| {
            if matches!(e, Error::Io | Error::Corrupt | Error::Full | Error::Failed) {
                self.broken = true;
            }
            errno(e)
        })
    }
    /// The inode of `name` in `dir`, made if it is not there.
    fn own(&mut self, dir: u32, name: &[u8], mode: u16) -> R<u32> {
        match self.fs.lookup(dir, name) {
            Err(Error::NotFound) => Ok(fs!(self.create(dir, name, mode, 0))),
            r => self.ck(r),
        }
    }

    /// After a mount: the system's own files, the mount count, and the move.
    fn setup(&mut self, legacy: Option<&mut dyn Legacy>) -> Result<(), &'static [u8]> {
        self.put_active = false;
        (|| -> R<()> {
            self.sys = self.own(ROOT, SYS, S_DIR | 0o700)?;
            self.owners_ino = self.own(self.sys, b"owners", S_FILE | 0o600)?;
            self.buf.fill(0);
            fs!(self.read(self.owners_ino, 0, &mut self.buf[..OWNERS * REC]));
            for (k, o) in self.owners.iter_mut().enumerate() {
                o.copy_from_slice(&self.buf[k * REC..k * REC + KEY]);
            }
            match self.fs.unlink(self.sys, b"put") {
                Ok(()) | Err(Error::NotFound) => {}
                Err(e) => return Err(self.ck::<()>(Err(e)).unwrap_err()),
            }
            let s = fs!(self.statfs());
            self.reserve = (s.blocks / 16).max(4);
            self.def_blocks = (s.blocks / 4).max(16);
            self.def_inodes = (s.inodes / 4).max(8);
            // Every mount gets a number no earlier one had.
            let at = self.own(self.sys, b"epoch", S_FILE | 0o600)?;
            let mut e = [0u8; 4];
            fs!(self.read(at, 0, &mut e));
            self.epoch = u32::from_le_bytes(e).wrapping_add(1).max(1);
            if fs!(self.write(at, 0, &self.epoch.to_le_bytes())) != 4 {
                return Err(ENOSPC);
            }
            Ok(())
        })()
        .map_err(|_| b"DATA SETUP" as &[u8])?;
        if matches!(self.fs.lookup(self.sys, b"migrated"), Err(Error::NotFound)) {
            let Some(legacy) = legacy else { return Err(b"DATA NOT MIGRATED") };
            self.migrate(legacy).map_err(|_| b"DATA MIGRATE" as &[u8])?;
        }
        self.fs.commit().map_err(|_| b"DATA COMMIT" as &[u8])
    }

    /// Copy the open apps' old files into the top of the volume and mark it
    /// done. Runs again from the start if it did not get to the mark. Every
    /// old file comes over whole or the move stops: one left out could be
    /// the passcode record, and /data would then say there is none.
    fn migrate(&mut self, legacy: &mut dyn Legacy) -> R<()> {
        for slot in 0..LEGACY_SLOTS {
            let Some((raw, len)) = legacy.file(slot, &mut self.buf).map_err(|_| EIO)? else { continue };
            let n = raw.iter().position(|&b| b == 0).unwrap_or(16);
            let name = &raw[..n];
            if name == b"." || name == b".." || name == SYS || len > self.buf.len() {
                return Err(EIO); // not a name the old store made: damaged
            }
            match self.fs.unlink(ROOT, name) {
                Ok(()) | Err(Error::NotFound) => {}
                Err(e) => return self.ck(Err(e)),
            }
            // No limit is set for the owner here: what was kept is kept.
            let owner = self.owner_id(owner_key(name), true)?.unwrap_or(0);
            let ino = fs!(self.create(ROOT, name, S_FILE | 0o644, owner));
            let mut done = 0;
            while done < len {
                let n = fs!(self.write(ino, done as u64, &self.buf[done..len]));
                if n == 0 {
                    return Err(ENOSPC);
                }
                done += n;
            }
        }
        fs!(self.create(self.sys, b"migrated", S_FILE | 0o600, 0));
        fs!(self.commit());
        Ok(())
    }

    // ---- owners and quota ----

    /// The number of the owner with this key (None: the system, 0). With
    /// `make` an owner not yet known gets one.
    fn owner_id(&mut self, key: Option<&[u8]>, make: bool) -> R<Option<u32>> {
        let Some(key) = key else { return Ok(Some(0)) };
        let mut rec = [0u8; REC];
        rec[..key.len()].copy_from_slice(key);
        if let Some(i) = self.owners.iter().position(|o| o[..] == rec[..KEY]) {
            return Ok(Some(i as u32 + 1));
        }
        if !make {
            return Ok(None);
        }
        let mut free = self.owners.iter().position(|o| o[0] == 0);
        if free.is_none() {
            // Numbers taken by owners that never came to hold anything.
            for i in 0..OWNERS {
                self.release(i as u32 + 1)?;
            }
            free = self.owners.iter().position(|o| o[0] == 0);
        }
        let i = free.ok_or(ENOSPC)?;
        if fs!(self.write(self.owners_ino, (i * REC) as u64, &rec)) != REC {
            return Err(ENOSPC);
        }
        self.owners[i].copy_from_slice(&rec[..KEY]);
        Ok(Some(i as u32 + 1))
    }
    /// An app owner without limits gets the default ones before it grows.
    fn limit(&mut self, owner: u32) -> R<()> {
        if owner == 0 {
            return Ok(());
        }
        let u = fs!(self.usage(owner));
        if u.block_limit == UNLIMITED && u.inode_limit == UNLIMITED {
            fs!(self.set_quota(owner, self.def_blocks, self.def_inodes));
        }
        Ok(())
    }
    /// An owner that holds nothing gives back its number and its quota row
    /// (unless limits of its own were set for it).
    fn release(&mut self, owner: u32) -> R<()> {
        if owner == 0 || owner as usize > OWNERS || self.owners[owner as usize - 1][0] == 0 {
            return Ok(());
        }
        let u = fs!(self.usage(owner));
        let default = u.block_limit == self.def_blocks && u.inode_limit == self.def_inodes;
        let none = u.block_limit == UNLIMITED && u.inode_limit == UNLIMITED;
        if u.blocks != 0 || u.inodes != 0 || !(default || none) || (self.put_active && self.put_owner == owner) {
            return Ok(());
        }
        if default {
            fs!(self.set_quota(owner, UNLIMITED, UNLIMITED));
        }
        let at = (owner as usize - 1) * REC;
        if fs!(self.write(self.owners_ino, at as u64, &[0u8; REC])) == REC {
            self.owners[owner as usize - 1] = [0; KEY];
        }
        Ok(())
    }
    /// Is there room for `bytes` more of an app's data above the reserve?
    fn room(&mut self, owner: u32, bytes: usize) -> R<()> {
        if owner == 0 {
            return Ok(());
        }
        let need = self.reserve + (bytes / 4096) as u32 + 2;
        if fs!(self.statfs()).free_blocks >= need {
            return Ok(());
        }
        // Blocks freed since the last commit count only after it.
        fs!(self.commit());
        if fs!(self.statfs()).free_blocks >= need {
            Ok(())
        } else {
            Err(ENOSPC)
        }
    }

    // ---- paths ----

    /// Walk `path`: the folder its last part is in, the inode of that part
    /// (0 if there is none) and the part. /data itself: (0, ROOT, "").
    fn walk<'p>(&mut self, path: &'p [u8]) -> R<(u32, u32, &'p [u8])> {
        self.walk_to(path, false)
    }
    /// `make`: folders above the last part that are missing are made.
    fn walk_to<'p>(&mut self, path: &'p [u8], make: bool) -> R<(u32, u32, &'p [u8])> {
        let p = rel(path);
        if p.is_empty() {
            return Ok((0, ROOT, p));
        }
        if top(p) == SYS {
            return Err(ENOENT);
        }
        let mut dir = ROOT;
        let mut parts = p.split(|&c| c == b'/').peekable();
        while let Some(part) = parts.next() {
            if part.len() > NAME_MAX {
                return Err(ENAMETOOLONG);
            }
            if part.is_empty() || part == b"." || part == b".." || part.contains(&0) {
                return Err(EINVAL);
            }
            let found = self.fs.lookup(dir, part);
            if parts.peek().is_none() {
                return match found {
                    Ok(ino) => Ok((dir, ino, part)),
                    Err(Error::NotFound) => Ok((dir, 0, part)),
                    Err(e) => self.ck(Err(e)),
                };
            }
            dir = match found {
                Err(Error::NotFound) if make => {
                    let owner = self.owner_id(owner_key(path), true)?.unwrap_or(0);
                    self.limit(owner)?;
                    self.room(owner, 4096)?;
                    fs!(self.create(dir, part, S_DIR | 0o755, owner))
                }
                found => self.ck(found)?,
            };
        }
        Err(EINVAL)
    }
    /// The inode at `path`, which must exist.
    fn find(&mut self, path: &[u8]) -> R<(u32, u32, Inode)> {
        let (parent, ino, _) = self.walk(path)?;
        if ino == 0 {
            return Err(ENOENT);
        }
        Ok((parent, ino, fs!(self.stat(ino))))
    }
    /// May `who` change the names in folder `dir`? (/data itself: `may` decided.)
    fn dir_writable(&mut self, who: &[u8], dir: u32) -> R<()> {
        if who.is_empty() || dir == ROOT {
            return Ok(());
        }
        if fs!(self.stat(dir)).mode & 0o200 == 0 {
            return Err(EACCES);
        }
        Ok(())
    }

    // ---- requests ----

    /// Answer the request in the first `len` bytes of `win`, in `win`.
    pub fn serve(&mut self, win: &mut [u8], len: usize) {
        let Some((r, parts)) = decode(win, len) else {
            return reply(win, EINVAL, self.epoch, 0, 0, false);
        };
        let mut names = [0u8; 2 * PATH_MAX + 256];
        let mut cut = [0usize; 4];
        for k in 0..3 {
            let n = parts[k].1 - parts[k].0;
            names[cut[k]..cut[k] + n].copy_from_slice(&win[parts[k].0..parts[k].1]);
            cut[k + 1] = cut[k] + n;
        }
        let (path, path2, who) = (&names[..cut[1]], &names[cut[1]..cut[2]], &names[cut[2]..cut[3]]);
        if !self.ready {
            return reply(win, ENODEV, self.epoch, 0, 0, false);
        }
        if self.broken {
            // Back to the last transaction that was made durable.
            let again = self.fs.mount().is_ok() && self.setup(None).is_ok();
            self.broken = !again;
            if !again {
                return reply(win, EIO, self.epoch, 0, 0, false);
            }
        }
        if r.op != HELLO && r.epoch != self.epoch {
            return reply(win, ESTALE, self.epoch, 0, 0, false);
        }
        // A request without a time (a removal, a commit) keeps the last one
        // seen, so that folders are not dated 1970.
        if r.time != 0 {
            self.fs.now = r.time as u64;
        }
        let out = self.run(&r, path, path2, who, win, parts[3]);
        if out.is_err() && r.op == PUT {
            self.drop_put();
        }
        match out {
            Ok((value, len, more)) => reply(win, 0, self.epoch, value, len, more),
            Err(e) => reply(win, e, self.epoch, 0, 0, false),
        }
    }

    fn drop_put(&mut self) {
        if self.put_active {
            self.put_active = false;
            let _ = self.fs.unlink(self.sys, b"put");
        }
    }

    fn run(&mut self, r: &Req, path: &[u8], path2: &[u8], who: &[u8], win: &mut [u8], data: (usize, usize)) -> R<(u64, usize, bool)> {
        let done = |v: u64| Ok((v, 0, false));
        // Only for the runtime itself: an app's request makes and removes
        // nothing it did not name.
        let force = r.flags & FORCE != 0 && who.is_empty();
        let write = !matches!(r.op, HELLO | LIST | READ | STAT | STATFS | COMMIT);
        may(who, r.flags, path, write)?;
        match r.op {
            HELLO => done(self.fs.atomic_write() as u64),
            COMMIT => {
                fs!(self.commit());
                done(0)
            }
            LIST => self.list(r, path, who, win),
            READ => {
                let (_, ino, n) = self.find(path)?;
                if n.mode & S_IFMT == S_DIR {
                    return Err(EISDIR);
                }
                if !who.is_empty() && n.mode & 0o400 == 0 {
                    return Err(EACCES);
                }
                let want = (r.mode as usize).min(DATA_MAX);
                let got = fs!(self.read(ino, r.offset, &mut win[REPLY..REPLY + want]));
                Ok((got as u64, got, false))
            }
            STAT => {
                let (_, ino, n) = self.find(path)?;
                let out = &mut win[REPLY..REPLY + 32];
                put32(out, 0, n.mode as u32);
                put32(out, 4, n.owner);
                put64(out, 8, n.size);
                put64(out, 16, n.mtime);
                put32(out, 24, ino);
                put32(out, 28, n.nlink as u32);
                Ok((n.size, 32, false))
            }
            STATFS => {
                let s = fs!(self.statfs());
                let key = if who.is_empty() { owner_key(path) } else { Some(who) };
                let (mut free, mut ifree) = (s.free_blocks, s.free_inodes);
                let mut u = Usage { block_limit: UNLIMITED, inode_limit: UNLIMITED, ..Usage::default() };
                if key.is_some() {
                    u = match self.owner_id(key, false)? {
                        Some(id) => fs!(self.usage(id)),
                        None => u,
                    };
                    if u.block_limit == UNLIMITED && u.inode_limit == UNLIMITED {
                        (u.block_limit, u.inode_limit) = (self.def_blocks, self.def_inodes);
                    }
                    free = free.saturating_sub(self.reserve).min(u.block_limit.saturating_sub(u.blocks));
                    ifree = ifree.min(u.inode_limit.saturating_sub(u.inodes));
                }
                for (k, v) in [s.blocks, free, s.inodes, ifree, u.blocks, u.block_limit, u.inodes, u.inode_limit].into_iter().enumerate() {
                    put32(win, REPLY + 4 * k, v);
                }
                Ok((free as u64, 32, false))
            }
            PUT => self.put(r, path, who, force, &win[data.0..data.1]),
            MKDIR => {
                let (parent, ino, name) = self.walk_to(path, force)?;
                if ino != 0 {
                    if !force || parent == 0 {
                        return Err(EEXIST);
                    }
                    if fs!(self.stat(ino)).mode & S_IFMT == S_DIR {
                        return done(0);
                    }
                    fs!(self.unlink(parent, name));
                }
                self.dir_writable(who, parent)?;
                let owner = self.owner_id(owner_key(path), true)?.unwrap_or(0);
                self.limit(owner)?;
                self.room(owner, 4096)?;
                fs!(self.create(parent, name, S_DIR | (r.mode as u16 & 0o7777), owner));
                done(0)
            }
            REMOVE => {
                let (parent, _, n) = self.find(path)?;
                if parent == 0 {
                    return Err(EACCES);
                }
                self.dir_writable(who, parent)?;
                let name = rel(path).rsplit(|&c| c == b'/').next().unwrap_or(b"");
                if n.mode & S_IFMT == S_DIR {
                    fs!(self.rmdir(parent, name));
                } else {
                    fs!(self.unlink(parent, name));
                }
                self.release(n.owner)?;
                done(0)
            }
            WRITE => {
                let (parent, mut ino, name) = self.walk(path)?;
                if parent == 0 {
                    return Err(EISDIR);
                }
                let owner = if ino == 0 {
                    if r.flags & CREATE == 0 {
                        return Err(ENOENT);
                    }
                    self.dir_writable(who, parent)?;
                    let owner = self.owner_id(owner_key(path), true)?.unwrap_or(0);
                    self.limit(owner)?;
                    ino = fs!(self.create(parent, name, S_FILE | (r.mode as u16 & 0o7777), owner));
                    owner
                } else {
                    if r.flags & EXCL != 0 {
                        return Err(EEXIST);
                    }
                    let n = fs!(self.stat(ino));
                    if n.mode & S_IFMT == S_DIR {
                        return Err(EISDIR);
                    }
                    if !who.is_empty() && n.mode & 0o200 == 0 {
                        return Err(EACCES);
                    }
                    if r.flags & TRUNC != 0 {
                        fs!(self.truncate(ino, 0));
                    }
                    self.limit(n.owner)?;
                    n.owner
                };
                let bytes = &win[data.0..data.1];
                self.room(owner, bytes.len())?;
                done(self.write_all(ino, r.offset, bytes, true)? as u64)
            }
            TRUNCATE => {
                let (_, ino, n) = self.find(path)?;
                if !who.is_empty() && n.mode & 0o200 == 0 {
                    return Err(EACCES);
                }
                fs!(self.truncate(ino, r.offset));
                done(0)
            }
            RENAME => {
                may(who, r.flags, path2, true)?;
                // The owner of a file follows from its path: it cannot move
                // to another owner's names by a rename.
                if owner_key(path) != owner_key(path2) {
                    return Err(EXDEV);
                }
                let (from, ino, name) = self.walk(path)?;
                let (to, _, new) = self.walk(path2)?;
                if ino == 0 {
                    return Err(ENOENT);
                }
                if from == 0 || to == 0 {
                    return Err(EINVAL);
                }
                self.dir_writable(who, from)?;
                self.dir_writable(who, to)?;
                fs!(self.rename(from, name, to, new));
                done(0)
            }
            CHMOD => {
                if !who.is_empty() && !covers(who, top(path)) {
                    return Err(EACCES);
                }
                let (_, ino, _) = self.find(path)?;
                fs!(self.chmod(ino, r.mode as u16 & 0o7777));
                done(0)
            }
            QUOTA => {
                if !who.is_empty() {
                    return Err(EACCES);
                }
                let Some(owner) = self.owner_id(owner_key(path), true)?.filter(|&id| id != 0) else { return Err(EINVAL) };
                fs!(self.set_quota(owner, r.offset.min(u32::MAX as u64) as u32, r.mode));
                self.release(owner)?;
                done(0)
            }
            _ => Err(EINVAL),
        }
    }

    /// Write all of `data`, or (`partly`) what fitted if that is something.
    fn write_all(&mut self, ino: u32, off: u64, data: &[u8], partly: bool) -> R<usize> {
        let (mut at, mut retried) = (0, false);
        while at < data.len() {
            match self.fs.write(ino, off + at as u64, &data[at..]) {
                Ok(n) if n > 0 => at += n,
                Ok(_) | Err(Error::NoSpace) if !retried => {
                    // Blocks freed since the last commit are usable after it.
                    retried = true;
                    fs!(self.commit());
                }
                Err(Error::NoSpace | Error::Quota) | Ok(_) if partly && at > 0 => break,
                Ok(_) => return Err(ENOSPC),
                Err(e) => return self.ck(Err(e)),
            }
        }
        Ok(at)
    }

    /// The folder a replacement goes into, with the way cleared: a folder
    /// under the file's own name is in the way unless `force` can remove it.
    fn put_target<'p>(&mut self, path: &'p [u8], who: &[u8], force: bool) -> R<(u32, &'p [u8])> {
        let (parent, ino, name) = self.walk_to(path, force)?;
        if parent == 0 {
            return Err(EISDIR);
        }
        self.dir_writable(who, parent)?;
        if ino != 0 {
            let n = fs!(self.stat(ino));
            if n.mode & S_IFMT == S_DIR {
                if !force {
                    return Err(EISDIR);
                }
                fs!(self.rmdir(parent, name));
            } else if !who.is_empty() && n.mode & 0o200 == 0 {
                return Err(EACCES);
            }
        }
        Ok((parent, name))
    }
    fn put(&mut self, r: &Req, path: &[u8], who: &[u8], force: bool, data: &[u8]) -> R<(u64, usize, bool)> {
        let crc = crc32(path);
        if r.flags & FIRST != 0 {
            self.drop_put();
            if r.offset != 0 {
                return Err(EINVAL);
            }
            let (parent, name) = self.put_target(path, who, force)?;
            let owner = self.owner_id(owner_key(path), true)?.unwrap_or(0);
            self.limit(owner)?;
            // The new copy collects as the system's, charged to the owner
            // only for what it adds to the file it replaces (checked below
            // with each piece, and by VioletFS with the last).
            let mut old = 0;
            if let Ok(at) = self.fs.lookup(parent, name) {
                let n = fs!(self.stat(at));
                if n.owner == owner && n.nlink == 1 && n.mode & S_IFMT == S_FILE {
                    old = n.blocks;
                }
            }
            fs!(self.create(self.sys, b"put", S_FILE | (r.mode as u16 & 0o7777), 0));
            (self.put_active, self.put_crc, self.put_size, self.put_owner, self.put_old) = (true, crc, 0, owner, old);
        } else if !self.put_active || self.put_crc != crc || r.offset != self.put_size {
            return Err(EINVAL);
        }
        self.room(self.put_owner, data.len())?;
        let ino = fs!(self.lookup(self.sys, b"put"));
        self.write_all(ino, self.put_size, data, false)?;
        self.put_size += data.len() as u64;
        if self.put_owner != 0 {
            let u = fs!(self.usage(self.put_owner));
            let staged = fs!(self.stat(ino)).blocks;
            if (u.blocks.saturating_sub(self.put_old) as u64) + staged as u64 > u.block_limit as u64 {
                return Err(EDQUOT);
            }
        }
        if r.flags & LAST != 0 {
            let (parent, name) = self.put_target(path, who, force)?;
            let old = self.fs.lookup(parent, name).unwrap_or(0);
            fs!(self.chown_over(ino, self.put_owner, old));
            fs!(self.rename(self.sys, b"put", parent, name));
            self.put_active = false;
        }
        Ok((self.put_size, 0, false))
    }

    fn list(&mut self, r: &Req, path: &[u8], who: &[u8], win: &mut [u8]) -> R<(u64, usize, bool)> {
        let (_, dir, n) = self.find(path)?;
        if n.mode & S_IFMT != S_DIR {
            return Err(ENOTDIR);
        }
        if !who.is_empty() && n.mode & 0o400 == 0 {
            return Err(EACCES);
        }
        let (mut cookie, mut at) = (r.offset, REPLY);
        loop {
            let Some(e) = fs!(self.readdir(dir, cookie, &mut self.name)) else {
                return Ok((0, at - REPLY, false));
            };
            let name = &self.name[..e.len];
            let hidden = dir == ROOT && (name == SYS || (!who.is_empty() && (name[0] == b'.' || !(covers(who, name) || r.flags & SHARED != 0))));
            if !hidden {
                let need = ENTRY + ((e.len + 3) & !3);
                if at + need > REPLY + DATA_MAX {
                    return Ok((cookie, at - REPLY, true));
                }
                win[at + ENTRY..at + need].fill(0);
                win[at + ENTRY..at + ENTRY + e.len].copy_from_slice(name);
                let n = fs!(self.stat(e.ino));
                put32(win, at, e.ino);
                win[at + 4..at + 6].copy_from_slice(&n.mode.to_le_bytes());
                win[at + 6..at + 8].copy_from_slice(&(e.len as u16).to_le_bytes());
                put64(win, at + 8, n.size);
                put32(win, at + 16, n.mtime.min(u32::MAX as u64) as u32);
                at += need;
            }
            cookie = e.next;
        }
    }
}
