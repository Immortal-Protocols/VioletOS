//! The Open Runtime's side of /data: the client of Storage's filesystem
//! server (vfs.rs, over the requests of vfsproto.rs). It is here, with no
//! IPC and no runtime in it, so that the host tests drive this very code
//! against the real server (tests/writeback_tests.rs); runtime/open/data.rs
//! supplies the window and the files.
//!
//! The runtime works on its files in memory. This loads /data from the
//! volume at start (every folder and file, any size that fits in memory)
//! and writes back what changed:
//!   - A file is sent whole as a replacement (`PUT`), a piece per pass of the
//!     runtime's loop (`PASS`), so a save never holds the runtime for long.
//!     The volume shows none of it before the last piece; then it is the new
//!     file. A folder is made, a removed name removed.
//!   - Files and folders to write go before names to remove, so a rename
//!     leaves both names for a moment, never neither. The name a rename
//!     took a file from is not removed while any save is failing, for
//!     whatever reason (it may be the other half); other removals are held
//!     back only while a failed save is being tried again (they may be what
//!     makes room for it).
//!   - A file is not copied to be sent: its pieces are read from the view
//!     as they go, and a file written to meanwhile starts over.
//!   - What was sent survives a power cut once it is committed: when a pass
//!     has nothing left to send, and before `sync` says yes.
//!   - A save that fails stays queued and is tried again: after an error,
//!     with growing waits; when the volume or the quota was full, only when
//!     a removal has made room, the file changed size, or a sync asks (not
//!     sent again and again for nothing). What the volume holds for that
//!     name is then not known, so the file is sent again even if it goes
//!     back to its old contents.
//!   - Storage numbers its mounts of the volume. When it had to mount again
//!     (after a device error), what was sent since the last commit is gone
//!     and requests are answered ESTALE: from then on nothing is written
//!     until the restart, and `sync` says EIO (as when the disk is lost).
//!   - Nothing is ever sent before /data was loaded whole: a volume that
//!     could not be read is not written over with what little was read. A
//!     file is read into memory sized once (no regrowth); one the runtime
//!     has no memory for is left on the volume unseen if it is an app's,
//!     and fails the load if it is the system's (a dot name).
//!   - `sync` says what happened: an error for a file that is not stored.
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

#[path = "vfsproto.rs"]
pub mod proto;
use proto::*;

/// The bytes of a file one request carries.
pub const PIECE: usize = 8192;
/// Requests to Storage in one pass of the runtime's loop.
pub const PASS: u32 = 1;
/// Requests one fsync, fdatasync or sync may send: a megabyte of file.
pub const SYNC: u32 = 128;
/// Files up to this size are remembered as stored, to skip a save that
/// would change nothing; a larger one is sent whenever it was opened to write.
pub const KEEP: usize = 65536;
/// Passes before a failed save is tried again; doubles up to 16 times this.
const WAIT: u16 = 32;
/// Passes a change may wait for its commit while more keep coming.
const LINGER: u16 = 64;
/// In a reply's status: Storage has no data volume at all (data.rs's
/// translation of an OFFLINE answer). Nothing can have been kept.
pub const ENXIO: i32 = -6;
/// The runtime has no memory for a system file of /data.
pub const ENOMEM: i32 = -12;
pub const DIR: &str = "/data";

/// Why a file is not on the volume; as a number, the errno fsync returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Why {
    /// The volume is full (as far as apps may fill it), or a folder that is
    /// not empty yet is where the file goes. ENOSPC.
    Full = -28,
    /// The file's owner is at its quota. EDQUOT.
    Quota = -122,
    /// Storage or its disk failed; the save is tried again. EIO.
    Io = -5,
    /// Not written yet: the call's allowance ran out. Call again. EINTR.
    Later = -4,
}

pub struct Reply {
    pub status: i32,
    pub epoch: u32,
    pub value: u64,
    pub more: bool,
}

/// Storage, as the client reaches it.
pub trait Port {
    /// One request (`path` relative to /data) and its answer. A request
    /// that got no answer at all is `EIO`.
    fn call(&mut self, r: &Req, path: &[u8], data: &[u8]) -> Reply;
    /// The data of the last answer.
    fn data(&self) -> &[u8];
}

/// The files, as the client sees them. Paths are absolute ("/data/...").
/// Names the volume cannot hold (a part over 255 bytes, a path over 1024)
/// are the view's to refuse: runtime/open/fs.rs does not make them.
pub trait View {
    /// Paths written, made, removed or renamed since they were last saved.
    fn changed(&self) -> &BTreeSet<String>;
    fn set_changed(&mut self, path: &str, on: bool);
    fn contents(&self, path: &str) -> Option<&[u8]>;
    fn is_dir(&self, path: &str) -> bool;
    /// When the file was last written, in seconds (kept with it).
    fn mtime(&self, path: &str) -> u32;
    /// What the volume was last known to hold for the path.
    fn stored(&self, path: &str) -> Option<&[u8]>;
    /// A rename took a file away from this name, which is to be removed.
    fn renamed(&self, path: &str) -> bool;
    /// The removal of that name is on the volume.
    fn removed(&mut self, path: &str);
    fn set_stored(&mut self, path: &str, data: Option<Vec<u8>>);
    /// At start: a folder or a file as the volume has it.
    fn loaded(&mut self, path: &str, file: Option<Vec<u8>>, mtime: u32);
}

/// The name under /data, if the path is one the volume keeps. Pieces of a
/// full-system update (runtime/open/update.rs `staging`) are not: each is
/// replaced by the next within the second, is worth nothing after a restart,
/// and would take the app's quota and wear the disk.
pub fn key(path: &str) -> Option<&str> {
    let name = path.strip_prefix("/data/")?;
    (!name.is_empty() && !name.ends_with(".ota-staging")).then_some(name)
}

/// The path as Storage wants it: without "/data/".
fn rel(path: &str) -> &[u8] {
    path.as_bytes().get(DIR.len() + 1..).unwrap_or(&[])
}

/// An empty buffer that holds `size` bytes, if there is the memory: sized
/// once, so that loading a file never needs two copies of it at a time.
fn room(size: u64) -> Option<Vec<u8>> {
    if size == 0 || size > isize::MAX as u64 {
        return (size == 0).then(Vec::new);
    }
    // SAFETY: a size of 1 to isize::MAX with alignment 1 is a valid layout,
    // and `p` is that many bytes from the global allocator, as Vec's own.
    let p = unsafe { alloc::alloc::alloc(core::alloc::Layout::from_size_align_unchecked(size as usize, 1)) };
    (!p.is_null()).then(|| unsafe { Vec::from_raw_parts(p, 0, size as usize) })
}

struct Stage {
    path: String,
    at: usize,
    time: u32,
}
struct Bad {
    path: String,
    why: Why,
    tries: u8,
    /// Passes left before the next try.
    wait: u16,
    /// The file's size then (a full volume or quota: tried again when it changes).
    size: usize,
}

pub struct Saver {
    /// The replacement in progress: Storage has the first `at` bytes.
    stage: Option<Stage>,
    /// Files that are not stored as they are now, and why.
    bad: Vec<Bad>,
    epoch: u32,
    /// /data was loaded whole and Storage has not lost what was sent: only
    /// then is anything sent.
    ready: bool,
    /// Something was sent since the last commit.
    dirty: bool,
    /// Passes since something was sent that is not committed yet.
    age: u16,
}

fn within(path: &str, scope: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(s) => path == s || (path.starts_with(s) && path.as_bytes().get(s.len()) == Some(&b'/')),
    }
}

impl Saver {
    pub const fn new() -> Self {
        Saver { stage: None, bad: Vec::new(), epoch: 0, ready: false, dirty: false, age: 0 }
    }
    /// Is something on its way to Storage? (The loop should come round soon.)
    pub fn busy(&self) -> bool {
        self.ready && (self.stage.is_some() || self.dirty)
    }
    /// Is what changes written back? (/data was loaded whole, Storage is well.)
    pub fn ready(&self) -> bool {
        self.ready
    }

    /// At start: put every folder and file of the volume into the view;
    /// returns how many files. The caller clears the view's `changed`
    /// afterwards. An error is a request's status: `ENXIO` when Storage has
    /// no data volume at all (nothing was ever kept, nothing will be); any
    /// other means the volume is there and could not be read whole. Either
    /// way nothing is written this run.
    pub fn load(&mut self, v: &mut dyn View, port: &mut dyn Port) -> Result<usize, i32> {
        self.ready = false;
        self.epoch = 0;
        self.ask(port, HELLO, 0, 0, 0, DIR, &[], &mut 0)?;
        let mut dirs = alloc::vec![String::from(DIR)];
        let mut files = 0;
        while let Some(dir) = dirs.pop() {
            let mut cookie = 0;
            loop {
                let r = self.ask(port, LIST, 0, cookie, 0, &dir, &[], &mut 0)?;
                // The window is used again for each file read below.
                let page = port.data().to_vec();
                let mut at = 0;
                while let Some((e, to)) = entry(&page, at) {
                    at = to;
                    // A name that is not text was not made by this client.
                    let Ok(name) = core::str::from_utf8(e.name) else { continue };
                    let mut path = dir.clone();
                    path.push('/');
                    path.push_str(name);
                    if e.mode & 0xf000 == 0x4000 {
                        v.loaded(&path, None, e.mtime);
                        dirs.push(path);
                        continue;
                    }
                    let Some(mut data) = room(e.size) else {
                        if dir == DIR && name.starts_with('.') {
                            return Err(ENOMEM);
                        }
                        continue;
                    };
                    while (data.len() as u64) < e.size {
                        // Kept short would be worse than not there (a passcode record).
                        if self.ask(port, READ, 0, data.len() as u64, 0, &path, &[], &mut 0)?.value == 0 {
                            return Err(EIO);
                        }
                        data.extend_from_slice(port.data());
                    }
                    if data.len() <= KEEP {
                        v.set_stored(&path, Some(data.clone()));
                    }
                    v.loaded(&path, Some(data), e.mtime);
                    files += 1;
                }
                if !r.more {
                    break;
                }
                cookie = r.value;
            }
        }
        self.ready = true;
        self.dirty = false;
        Ok(files)
    }

    /// One pass of the runtime's loop: at most `PASS` requests, and a commit
    /// when due.
    pub fn pass(&mut self, v: &mut dyn View, port: &mut dyn Port) -> u32 {
        for b in &mut self.bad {
            b.wait = b.wait.saturating_sub(1);
        }
        let mut used = self.run(v, port, PASS, None);
        self.age = if self.dirty { self.age.saturating_add(1) } else { 0 };
        // Between files, not in the middle of one: when nothing is left to
        // send, or `LINGER` passes after the first change not committed.
        if self.dirty && self.ready && self.stage.is_none() && (self.age >= LINGER || v.changed().is_empty()) {
            self.commit(port, &mut used);
        }
        used
    }
    /// Write back now what is in `scope` (a file, a folder and all under
    /// it, or everything) and commit: what fsync, fdatasync and sync wait
    /// for. At most `SYNC` requests; returns how many were sent, and `Ok`
    /// only if every file in scope is on the volume as it is now (or gone
    /// from it, if it was removed) and would survive a power cut.
    pub fn sync(&mut self, v: &mut dyn View, port: &mut dyn Port, scope: Option<&str>) -> (Result<(), Why>, u32) {
        // What the caller waits for is tried now, however long it had to wait.
        for b in &mut self.bad {
            if within(&b.path, scope) {
                b.wait = 0;
            }
        }
        // A replacement of a file nobody is waiting for while there is one
        // somebody is starts over later (one written to since does anyway).
        if let Some(st) = &self.stage {
            if !within(&st.path, scope) && self.pending(v, scope) {
                // Nothing of it shows on the volume: the file there is as it was.
                v.set_changed(&st.path, true);
                self.stage = None;
            }
        }
        let mut used = self.run(v, port, SYNC, scope);
        let bad = self.bad.iter().find(|b| within(&b.path, scope)).map(|b| b.why);
        let r = match bad {
            _ if !self.ready => Err(Why::Io),
            Some(why) => Err(why),
            // Nothing could be sent: a removal held back by a save that failed.
            None if self.pending(v, scope) && used == 0 => Err(Why::Io),
            None if self.pending(v, scope) => Err(Why::Later),
            None if self.dirty && !self.commit(port, &mut used) => Err(Why::Io),
            None => Ok(()),
        };
        (r, used)
    }
    /// Why the path is not stored as it is now, if a save found that out.
    pub fn failure(&self, path: &str) -> Option<Why> {
        self.bad.iter().find(|b| b.path == path).map(|b| b.why)
    }

    /// One request. `Err`: its status, when that is not 0.
    fn ask(&mut self, port: &mut dyn Port, op: u32, flags: u32, offset: u64, time: u32, path: &str, data: &[u8], used: &mut u32) -> Result<Reply, i32> {
        *used += 1;
        // `mode`: a file's or folder's permission bits; for READ, how much.
        let mode = match op {
            READ => DATA_MAX as u32,
            MKDIR => 0o755,
            _ => 0o644,
        };
        let r = port.call(&Req { op, flags, offset, len: 0, mode, time, epoch: self.epoch }, rel(path), data);
        if op == HELLO {
            self.epoch = r.epoch;
        }
        match r.status {
            0 => {
                self.dirty |= op >= PUT && op != COMMIT;
                Ok(r)
            }
            e => {
                // ESTALE: Storage mounted the volume again. It is as it was
                // at some earlier commit, and what was sent since is not there.
                self.ready &= e != ESTALE;
                Err(e)
            }
        }
    }
    fn commit(&mut self, port: &mut dyn Port, used: &mut u32) -> bool {
        let ok = self.ask(port, COMMIT, 0, 0, 0, DIR, &[], used).is_ok();
        self.dirty = !ok;
        ok
    }

    fn pending(&self, v: &dyn View, scope: Option<&str>) -> bool {
        self.stage.as_ref().is_some_and(|st| within(&st.path, scope)) || v.changed().iter().any(|p| within(p, scope))
    }
    fn run(&mut self, v: &mut dyn View, port: &mut dyn Port, budget: u32, scope: Option<&str>) -> u32 {
        let mut used = 0;
        while self.ready && used < budget && self.pending(v, scope) {
            let Some(mut st) = self.stage.take() else {
                let Some(path) = self.pick(v, scope) else { break };
                v.set_changed(&path, false);
                if key(&path).is_none() {
                    continue; // never kept: nothing to send, nothing to remove
                }
                let r = if v.is_dir(&path) {
                    self.ask(port, MKDIR, FORCE, 0, 0, &path, &[], &mut used).err()
                } else if let Some(data) = v.contents(&path) {
                    if v.stored(&path) == Some(data) {
                        self.forget(&path); // opened for writing, not changed
                    } else {
                        self.stage = Some(Stage { time: v.mtime(&path), path, at: 0 });
                    }
                    continue;
                } else {
                    // ENOENT: the volume has no such name, which is what was wanted.
                    let r = self.ask(port, REMOVE, 0, 0, 0, &path, &[], &mut used).err().filter(|&e| e != ENOENT);
                    if r.is_none() {
                        v.set_stored(&path, None);
                        v.removed(&path);
                        // There is room again: saves that found none need not wait.
                        for b in &mut self.bad {
                            if b.why != Why::Io {
                                b.wait = 0;
                            }
                        }
                    }
                    r
                };
                self.done(v, path, r);
                continue;
            };
            // Written to (or removed) since it was staged: it is queued
            // again, and what was sent of it shows nowhere.
            let Some(data) = v.contents(&st.path).filter(|_| !v.changed().contains(st.path.as_str())) else { continue };
            let end = (st.at + PIECE).min(data.len());
            let last = end == data.len();
            let flags = FORCE | if st.at == 0 { FIRST } else { 0 } | if last { LAST } else { 0 };
            let r = self.ask(port, PUT, flags, st.at as u64, st.time, &st.path, &data[st.at..end], &mut used).err();
            if r.is_none() && !last {
                st.at = end;
                self.stage = Some(st);
                continue;
            }
            if r.is_none() {
                let keep = (data.len() <= KEEP).then(|| data.to_vec());
                v.set_stored(&st.path, keep);
            }
            self.done(v, st.path, r);
        }
        used
    }
    /// A request for `path` was answered `r`. A failure keeps the path
    /// queued: nothing of a replacement shows on the volume.
    fn done(&mut self, v: &mut dyn View, path: String, r: Option<i32>) {
        let tries = self.bad.iter().find(|b| b.path == path).map_or(0, |b| b.tries);
        self.forget(&path);
        let Some(r) = r else { return };
        v.set_changed(&path, true);
        let (why, wait) = match r {
            ESTALE => return,
            ENOSPC | ENOTEMPTY => (Why::Full, u16::MAX),
            EDQUOT => (Why::Quota, u16::MAX),
            _ => {
                // The old file, or the new one if only the reply was lost.
                v.set_stored(&path, None);
                (Why::Io, WAIT << tries.min(4))
            }
        };
        let size = v.contents(&path).map_or(0, |d| d.len());
        self.bad.push(Bad { path, why, tries: tries.saturating_add(1), wait, size });
    }
    /// The next path to work on: a file or folder to write (one the caller
    /// waits for first; a folder sorts before what is in it), else a name
    /// to remove (the last in order: what is in a folder before the
    /// folder). While any save is failing the names a rename left are held
    /// back; other removals do not wait (they may be what makes room).
    fn pick(&self, v: &dyn View, scope: Option<&str>) -> Option<String> {
        let (mut write, mut gone, mut free): (Option<&String>, Option<&String>, Option<&String>) = (None, None, None);
        let mut failing = false;
        for p in v.changed() {
            let bad = self.bad.iter().find(|b| b.path == *p);
            let waiting = bad.is_some_and(|b| b.wait > 0 && (b.why == Why::Io || Some(b.size) == v.contents(p).map(<[u8]>::len)));
            let wanted = scope.is_some() && within(p, scope);
            if v.is_dir(p) || v.contents(p).is_some() {
                failing |= bad.is_some();
                if !waiting && (write.is_none() || (wanted && !write.is_some_and(|w| within(w, scope)))) {
                    write = Some(p);
                }
            } else if !waiting {
                gone = Some(p);
                if !v.renamed(p) {
                    free = Some(p);
                }
            }
        }
        write.or(if failing { free } else { gone }).cloned()
    }
    fn forget(&mut self, path: &str) {
        // There is at most one (`done` forgets before it adds).
        if let Some(i) = self.bad.iter().position(|b| b.path == path) {
            self.bad.swap_remove(i);
        }
    }
}

/// Keeps one app's fsync and sync calls from holding the runtime: each app
/// may send `BURST` requests at once and one more every `REFILL_NS` after
/// (2 MiB, then 1 MiB a second). An app that has used more than that is not
/// served (`ready`) until it is back within its allowance; the call that
/// went over is answered late (`charge` says how late).
pub struct Limiter {
    /// Requests used beyond what has come back (the allowance is `BURST`
    /// minus this): zero to start with, so the static takes no image bytes.
    spent: [i32; APPS],
    last: [u64; APPS],
}
const APPS: usize = 16;
pub const BURST: i32 = 256;
pub const REFILL_NS: u64 = 8_000_000;

impl Limiter {
    pub const fn new() -> Self {
        Limiter { spent: [0; APPS], last: [0; APPS] }
    }
    pub fn ready(&mut self, app: usize, now_ns: u64) -> bool {
        let a = app % APPS;
        let add = now_ns.saturating_sub(self.last[a]) / REFILL_NS;
        if add >= self.spent[a] as u64 {
            self.spent[a] = 0;
            self.last[a] = now_ns;
        } else {
            self.spent[a] -= add as i32;
            self.last[a] += add * REFILL_NS;
        }
        self.spent[a] < BURST
    }
    /// Count `used` requests; returns for how long the app is over.
    pub fn charge(&mut self, app: usize, used: u32) -> u64 {
        let a = app % APPS;
        self.spent[a] = self.spent[a].saturating_add(used.min(i32::MAX as u32) as i32).min(BURST + (1 << 20));
        (self.spent[a] - BURST).max(0) as u64 * REFILL_NS
    }
}
