//! The /data protocol between the Open Runtime and Storage's filesystem
//! server (vfs.rs). A request travels as an `APPSTORE` message whose `value`
//! has the `VFS` bit and whose `arg` is the request's length; the request
//! and the reply themselves are in the 64 KiB window the two share:
//!
//!   request   op, flags, offset (u64), len, mode, time, epoch,
//!             path_len, path2_len, who_len (u16 each), 0;
//!             then the path, the second path, `who` and `len` bytes of data
//!   reply     status (0 or a negative errno), epoch, value (u64), len,
//!             more; then `len` bytes from byte 32
//!
//! Paths are relative to /data ("" is /data itself, "notes/a.txt"). `who`
//! is the app a request is made for (empty: the runtime itself). `epoch`
//! counts Storage's mounts of the volume: a request with another epoch is
//! answered ESTALE and not done, because what the caller believes is stored
//! may have been rolled back; `HELLO` returns the current one.
//!
//! No heap and no IPC here: both sides and the host tests include this file.
//! Numbers are little-endian, as both sides' processors are (`encode_raw`
//! relies on it; the build is refused elsewhere).
#[cfg(not(target_endian = "little"))]
compile_error!("vfsproto.rs writes its words little-endian");

/// In `APPSTORE`'s `value`: a filesystem request (never set with the boot
/// slot bit 31; the app extent's sector numbers stay far below it).
pub const VFS: u32 = 1 << 29;
pub const WINDOW: usize = 65536;
pub const HEAD: usize = 40;
pub const REPLY: usize = 32;
/// The most data one request or reply carries.
pub const DATA_MAX: usize = 60 * 1024;
pub const PATH_MAX: usize = 1024;

pub const HELLO: u32 = 0;
/// Names in a directory from cookie `offset`: records of ino u32, mode u16,
/// name length u16, size u64, mtime u32, then the name padded to 4 bytes.
/// The reply's value is the cookie to go on from when `more` is 1.
pub const LIST: u32 = 1;
pub const READ: u32 = 2;
/// Replace a file whole: pieces in order (`FIRST` on the first, `LAST` on
/// the last) collect unseen; the last one puts the file under its name in
/// one step. After a power cut the old file or the new one is there.
pub const PUT: u32 = 3;
pub const MKDIR: u32 = 4;
/// Remove a file or an empty directory.
pub const REMOVE: u32 = 5;
/// Make everything done so far survive a power cut.
pub const COMMIT: u32 = 6;
/// mode u32, owner u32, size u64, mtime u64, ino u32, nlink u32.
pub const STAT: u32 = 7;
/// Write in place at `offset` (`CREATE`, `EXCL`, `TRUNC`).
pub const WRITE: u32 = 8;
pub const TRUNCATE: u32 = 9;
pub const RENAME: u32 = 10;
pub const CHMOD: u32 = 11;
/// blocks, free, inodes, free inodes of the volume (as `who` may use them),
/// then `who`'s (or the path's owner's) blocks, block limit, files, file limit.
pub const STATFS: u32 = 12;
/// Limit the owner of `path` to `offset` blocks and `mode` files (runtime only).
pub const QUOTA: u32 = 13;

pub const FIRST: u32 = 1;
pub const LAST: u32 = 2;
pub const CREATE: u32 = 4;
pub const EXCL: u32 = 8;
pub const TRUNC: u32 = 16;
/// With `who`: the runtime's policy shares the path's top name with it.
pub const SHARED: u32 = 32;
/// `PUT` and `MKDIR`: make the folders above the path if they are missing,
/// and take the place of what is there under the name: a folder that
/// exists is fine, a file gives way to a folder and an empty folder to a
/// file (ENOTEMPTY if it is not empty).
pub const FORCE: u32 = 64;

pub const ENOENT: i32 = -2;
pub const EIO: i32 = -5;
pub const EACCES: i32 = -13;
pub const EEXIST: i32 = -17;
pub const EXDEV: i32 = -18;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const EFBIG: i32 = -27;
pub const ENOSPC: i32 = -28;
pub const ENAMETOOLONG: i32 = -36;
pub const ENOTEMPTY: i32 = -39;
pub const ESTALE: i32 = -116;
pub const EDQUOT: i32 = -122;

#[derive(Clone, Copy, Default, Debug)]
pub struct Req {
    pub op: u32,
    pub flags: u32,
    pub offset: u64,
    pub len: u32,
    pub mode: u32,
    pub time: u32,
    pub epoch: u32,
}

fn le16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}
pub fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
pub fn le64(b: &[u8], at: usize) -> u64 {
    le32(b, at) as u64 | (le32(b, at + 4) as u64) << 32
}
pub fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Write a request into the window. Returns its length (the message's
/// `arg`), or None if it does not fit.
pub fn encode(win: &mut [u8], r: &Req, path: &[u8], path2: &[u8], who: &[u8], data: &[u8]) -> Option<usize> {
    let total = HEAD + path.len() + path2.len() + who.len() + data.len();
    if path.len() > PATH_MAX || path2.len() > PATH_MAX || who.len() > 255 || data.len() > DATA_MAX || total > win.len() {
        return None;
    }
    put32(win, 0, r.op);
    put32(win, 4, r.flags);
    put64(win, 8, r.offset);
    put32(win, 16, data.len() as u32);
    put32(win, 20, r.mode);
    put32(win, 24, r.time);
    put32(win, 28, r.epoch);
    for (k, n) in [path.len(), path2.len(), who.len(), 0].into_iter().enumerate() {
        win[32 + 2 * k..34 + 2 * k].copy_from_slice(&(n as u16).to_le_bytes());
    }
    let mut at = HEAD;
    for part in [path, path2, who, data] {
        win[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
    Some(total)
}

/// The client's way to write a request with one path and no `who`, and to
/// read the answer's head, without the checks of slices: the Open Runtime
/// has little room for code. Same bytes as `encode` and `answer` (the host
/// tests compare them). Returns the request's length, 0 if it does not fit.
///
/// # Safety
/// `win` must be valid for `WINDOW` bytes.
pub unsafe fn encode_raw(win: *mut u8, r: &Req, path: &[u8], data: &[u8]) -> usize {
    if path.len() > PATH_MAX || data.len() > DATA_MAX {
        return 0;
    }
    let head = [r.op, r.flags, r.offset as u32, (r.offset >> 32) as u32, data.len() as u32, r.mode, r.time, r.epoch, path.len() as u32, 0];
    core::ptr::copy_nonoverlapping(head.as_ptr() as *const u8, win, HEAD);
    core::ptr::copy_nonoverlapping(path.as_ptr(), win.add(HEAD), path.len());
    core::ptr::copy_nonoverlapping(data.as_ptr(), win.add(HEAD + path.len()), data.len());
    HEAD + path.len() + data.len()
}
/// # Safety
/// `win` must be valid for `WINDOW` bytes.
pub unsafe fn answer_raw(win: *const u8) -> Answer {
    let mut head = [0u32; 6];
    core::ptr::copy_nonoverlapping(win, head.as_mut_ptr() as *mut u8, 24);
    Answer { status: head[0] as i32, epoch: head[1], value: head[2] as u64 | (head[3] as u64) << 32, len: (head[4] as usize).min(WINDOW - REPLY), more: head[5] != 0 }
}

/// The request in the first `len` bytes of the window, with where its
/// path, second path, `who` and data lie. None: not a request.
pub fn decode(win: &[u8], len: usize) -> Option<(Req, [(usize, usize); 4])> {
    if len < HEAD || len > win.len() {
        return None;
    }
    let r = Req { op: le32(win, 0), flags: le32(win, 4), offset: le64(win, 8), len: le32(win, 16), mode: le32(win, 20), time: le32(win, 24), epoch: le32(win, 28) };
    let sizes = [le16(win, 32), le16(win, 34), le16(win, 36), r.len as usize];
    if sizes[0] > PATH_MAX || sizes[1] > PATH_MAX || sizes[2] > 255 || sizes[3] > DATA_MAX {
        return None;
    }
    let mut parts = [(0, 0); 4];
    let mut at = HEAD;
    for (k, n) in sizes.into_iter().enumerate() {
        parts[k] = (at, at + n);
        at += n;
    }
    (at == len).then_some((r, parts))
}

/// Write a reply's head; its data (if any) is already at `REPLY`.
pub fn reply(win: &mut [u8], status: i32, epoch: u32, value: u64, len: usize, more: bool) {
    put32(win, 0, status as u32);
    put32(win, 4, epoch);
    put64(win, 8, value);
    put32(win, 16, len as u32);
    put32(win, 20, more as u32);
    win[24..REPLY].fill(0);
}

#[derive(Clone, Copy, Debug)]
pub struct Answer {
    pub status: i32,
    pub epoch: u32,
    pub value: u64,
    pub len: usize,
    pub more: bool,
}
pub fn answer(win: &[u8]) -> Answer {
    let len = (le32(win, 16) as usize).min(win.len() - REPLY);
    Answer { status: le32(win, 0) as i32, epoch: le32(win, 4), value: le64(win, 8), len, more: le32(win, 20) != 0 }
}

/// One record of a `LIST` reply.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub ino: u32,
    pub mode: u32,
    pub size: u64,
    pub mtime: u32,
    pub name: &'a [u8],
}
pub const ENTRY: usize = 20;
/// The record at `at` in a `LIST` reply's data and where the next one starts.
pub fn entry(data: &[u8], at: usize) -> Option<(Entry<'_>, usize)> {
    let head: &[u8; ENTRY] = data.get(at..at + ENTRY)?.try_into().ok()?;
    let word = |k: usize| u32::from_le_bytes([head[k], head[k + 1], head[k + 2], head[k + 3]]);
    let end = at + ENTRY + (word(4) >> 16) as usize;
    let e = Entry { ino: word(0), mode: word(4) & 0xffff, size: word(8) as u64 | (word(12) as u64) << 32, mtime: word(16), name: data.get(at + ENTRY..end)? };
    Some((e, (end + 3) & !3))
}
