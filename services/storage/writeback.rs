//! The caller's side of the app-private files (files.rs): what the Open
//! Runtime does to keep /data in Storage. It is here, with no IPC and no
//! runtime in it, so that the host tests drive this very code against the
//! real files.rs (tests/writeback_tests.rs); runtime/open/data.rs supplies
//! the messages and the files.
//!
//! The rules it keeps:
//!   - A file is sent whole as a staged replacement, a few messages per pass
//!     of the runtime's loop (`PASS`), so a save never holds the runtime for
//!     long. Storage shows none of it before the last message.
//!   - Files to write go before files to delete, so a rename leaves both
//!     names stored for a moment, never neither. A delete is not sent while
//!     a save that failed is still being tried again. The one exception: a
//!     save refused because all of Storage's slots are taken lets a waiting
//!     delete go first, as nothing else can make room (then, for a moment,
//!     neither name is stored).
//!   - A save that fails stays queued and is tried again, with growing
//!     waits. What Storage holds for that name is then not known (the old
//!     file, or the new one if only the reply was lost), so it is not
//!     compared against and a later delete is always sent.
//!   - `sync` says what happened: an error for a file that is not stored.
use crate::protocol::{EMPTY, FILE_DELETE, FILE_STAGED, FILE_TRUNCATE, FILE_WRITE, OK};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

/// The largest file Storage keeps, and the bytes one message carries.
pub const MAX: usize = 4096;
pub const PIECE: usize = 24;
/// Messages to Storage in one pass of the runtime's loop.
pub const PASS: u32 = 16;
/// Messages one fsync, fdatasync or sync may send: more than two full files.
pub const SYNC: u32 = 400;
/// Passes before a failed save is tried again; doubles up to 16 times this.
const WAIT: u16 = 32;
/// Failed tries after which waiting deletes are no longer held back.
const TRIES: u8 = 5;

/// Why a file is not in Storage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// All of Storage's slots for open apps are taken.
    Full,
    /// Storage or its disk failed; the save is tried again.
    Io,
    /// Over `MAX` bytes: never kept.
    TooBig,
    /// Not a name Storage keeps (`key`): never kept.
    Name,
    /// Not written yet: the call's allowance ran out. Call again.
    Later,
}

/// The Storage name for a path, if the path is one /data can keep.
pub fn key(path: &str) -> Option<&str> {
    let name = path.strip_prefix("/data/")?;
    let ok = !name.is_empty() && name.len() <= 16 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    ok.then_some(name)
}

/// Storage, as the saver reaches it.
pub trait Port {
    /// One EXECUTE message (FILE_WRITE, FILE_TRUNCATE or FILE_DELETE) and the
    /// `op` of its reply.
    fn send(&mut self, key: u32, name: &str, offset: u32, data: &[u8], flags: u32) -> u32;
    /// A file was found to be one that is never kept.
    fn refused(&mut self, _path: &str, _why: Why) {}
}

/// The files, as the saver sees them.
pub trait View {
    /// Paths written, removed or renamed since they were last saved.
    fn changed(&self) -> &BTreeSet<String>;
    fn set_changed(&mut self, path: &str, on: bool);
    fn contents(&self, path: &str) -> Option<&[u8]>;
    /// What Storage was last known to hold for the path.
    fn stored(&self, path: &str) -> Option<&[u8]>;
    fn set_stored(&mut self, path: &str, data: Option<Vec<u8>>);
}

struct Stage {
    path: String,
    data: Vec<u8>,
    at: usize,
}
struct Bad {
    path: String,
    why: Why,
    tries: u8,
    /// Passes left before the next try.
    wait: u16,
}

pub struct Saver {
    /// The replacement in progress: Storage has the first `at` bytes staged.
    stage: Option<Stage>,
    /// Files that are not stored as they are now, and why.
    bad: Vec<Bad>,
    /// Paths for which Storage may hold something other than `View::stored`.
    unsure: BTreeSet<String>,
}

fn within(path: &str, scope: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(s) => path == s || (path.starts_with(s) && path.as_bytes().get(s.len()) == Some(&b'/')),
    }
}

impl Saver {
    pub const fn new() -> Self {
        Saver { stage: None, bad: Vec::new(), unsure: BTreeSet::new() }
    }
    /// Is a file part way to Storage? (The loop should come round soon.)
    pub fn busy(&self) -> bool {
        self.stage.is_some()
    }
    /// One pass of the runtime's loop: at most `PASS` messages.
    pub fn pass(&mut self, v: &mut dyn View, port: &mut dyn Port) -> u32 {
        for b in &mut self.bad {
            b.wait = b.wait.saturating_sub(1);
        }
        self.run(v, port, PASS, None)
    }
    /// Write back now what is in `scope` (a file, a directory and all under
    /// it, or everything): what fsync, fdatasync and sync wait for. At most
    /// `SYNC` messages; returns how many were sent, and `Ok` only if every
    /// file in scope is in Storage as it is now (or gone from it, if it was
    /// removed). For a directory or everything, files that are never kept
    /// (`TooBig`, `Name`) do not count: asked about by name, they do.
    pub fn sync(&mut self, v: &mut dyn View, port: &mut dyn Port, scope: Option<&str>) -> (Result<(), Why>, u32) {
        // What the caller waits for is tried now, however long it had to wait.
        for b in &mut self.bad {
            if within(&b.path, scope) {
                b.wait = 0;
            }
        }
        // A replacement of an older version, or of a file nobody is waiting
        // for while there is one somebody is, starts over later.
        if let Some(st) = &self.stage {
            let stale = v.contents(&st.path) != Some(&st.data[..]);
            if stale || (!within(&st.path, scope) && self.pending(v, scope)) {
                // Nothing of it shows in Storage: the stored file is as it was.
                v.set_changed(&st.path, true);
                self.stage = None;
            }
        }
        let used = self.run(v, port, SYNC, scope);
        let exact = |p: &str| scope == Some(p);
        let r = match self.bad.iter().find(|b| within(&b.path, scope) && (exact(&b.path) || matches!(b.why, Why::Full | Why::Io))) {
            Some(b) => Err(b.why),
            // Nothing could be sent: a delete held back by a save that failed.
            None if self.pending(v, scope) && used == 0 => Err(Why::Io),
            None if self.pending(v, scope) => Err(Why::Later),
            None => Ok(()),
        };
        (r, used)
    }
    /// Why the path is not stored as it is now, if a save found that out.
    pub fn failure(&self, path: &str) -> Option<Why> {
        self.bad.iter().find(|b| b.path == path).map(|b| b.why)
    }

    fn pending(&self, v: &dyn View, scope: Option<&str>) -> bool {
        self.stage.as_ref().is_some_and(|st| within(&st.path, scope)) || v.changed().iter().any(|p| within(p, scope))
    }
    fn run(&mut self, v: &mut dyn View, port: &mut dyn Port, budget: u32, scope: Option<&str>) -> u32 {
        let mut used = 0;
        while used < budget && self.pending(v, scope) {
            let Some(st) = &mut self.stage else {
                match self.pick(v, scope) {
                    Some(path) => self.begin(v, port, path, &mut used),
                    None => break,
                }
                continue;
            };
            let Some(name) = key(&st.path) else {
                self.stage = None;
                continue;
            };
            let first = st.at == 0;
            let end = (st.at + PIECE).min(st.data.len());
            let commit = st.at == st.data.len();
            let r = if commit {
                port.send(FILE_TRUNCATE, name, st.at as u32, &[], FILE_STAGED)
            } else {
                port.send(FILE_WRITE, name, st.at as u32, &st.data[st.at..end], FILE_STAGED)
            };
            used += 1;
            if r == OK && !commit {
                st.at = end;
                continue;
            }
            let Some(st) = self.stage.take() else { break };
            if r == OK {
                self.unsure.remove(&st.path);
                self.forget(&st.path);
                v.set_stored(&st.path, Some(st.data));
                continue;
            }
            v.set_changed(&st.path, true);
            if r == EMPTY && first {
                // No slot is free and nothing was changed. A removal that is
                // waiting frees one: let it go first, then try again at once.
                let gone = v.changed().iter().find(|p| v.contents(p).is_none() && self.in_storage(v, p)).cloned();
                if let Some(gone) = gone {
                    if self.remove(v, port, gone, &mut used) {
                        continue;
                    }
                }
                self.fail(st.path, Why::Full);
            } else {
                // The old file, or the new one if only the reply was lost.
                self.unsure.insert(st.path.clone());
                self.fail(st.path, Why::Io);
            }
        }
        used
    }
    /// The next path to work on: a file to write (one the caller waits for
    /// first), else a file to delete, unless a failed save holds those back.
    fn pick(&self, v: &dyn View, scope: Option<&str>) -> Option<String> {
        let (mut write, mut gone): (Option<&String>, Option<&String>) = (None, None);
        let mut held = false;
        for p in v.changed() {
            let bad = self.bad.iter().find(|b| b.path == *p);
            let waiting = bad.is_some_and(|b| b.wait > 0);
            let wanted = scope.is_some() && within(p, scope);
            if v.contents(p).is_some() {
                if waiting {
                    held |= bad.is_some_and(|b| b.why == Why::Io && b.tries < TRIES);
                } else if write.is_none() || (wanted && !write.is_some_and(|w| within(w, scope))) {
                    write = Some(p);
                }
            } else if !waiting && (gone.is_none() || (wanted && !gone.is_some_and(|g| within(g, scope)))) {
                gone = Some(p);
            }
        }
        write.or(if held { None } else { gone }).cloned()
    }
    fn begin(&mut self, v: &mut dyn View, port: &mut dyn Port, path: String, used: &mut u32) {
        if v.contents(&path).is_none() {
            self.remove(v, port, path, used);
            return;
        }
        v.set_changed(&path, false);
        let Some(data) = v.contents(&path) else { return };
        let never = if key(&path).is_none() {
            Some(Why::Name)
        } else if data.len() > MAX {
            Some(Why::TooBig)
        } else {
            None
        };
        if let Some(why) = never {
            if self.failure(&path) != Some(why) {
                port.refused(&path, why);
            }
            self.forget(&path);
            self.bad.push(Bad { path, why, tries: 0, wait: 0 });
        } else if v.stored(&path) == Some(data) && !self.unsure.contains(&path) {
            self.forget(&path); // opened for writing, not changed
        } else {
            self.stage = Some(Stage { data: data.to_vec(), path, at: 0 });
        }
    }
    fn in_storage(&self, v: &dyn View, path: &str) -> bool {
        key(path).is_some() && (v.stored(path).is_some() || self.unsure.contains(path))
    }
    /// Delete a removed file from Storage. True if Storage has it no more.
    fn remove(&mut self, v: &mut dyn View, port: &mut dyn Port, path: String, used: &mut u32) -> bool {
        v.set_changed(&path, false);
        if self.in_storage(v, &path) {
            let r = port.send(FILE_DELETE, key(&path).unwrap_or(""), 0, &[], 0);
            *used += 1;
            // EMPTY: Storage has no such file, which is what was wanted.
            if r != OK && r != EMPTY {
                v.set_changed(&path, true);
                self.fail(path, Why::Io);
                return false;
            }
            // A slot is free: saves that found none need not wait.
            for b in &mut self.bad {
                if b.why == Why::Full {
                    b.wait = 0;
                }
            }
        }
        self.unsure.remove(&path);
        self.forget(&path);
        v.set_stored(&path, None);
        true
    }
    fn forget(&mut self, path: &str) {
        self.bad.retain(|b| b.path != path);
    }
    fn fail(&mut self, path: String, why: Why) {
        let tries = self.bad.iter().find(|b| b.path == path).map_or(0, |b| b.tries).saturating_add(1);
        self.forget(&path);
        self.bad.push(Bad { path, why, tries, wait: WAIT << (tries - 1).min(4) });
    }
}

/// Keeps one app's fsync and sync calls from holding the runtime: each app
/// may send `BURST` messages at once and one more every `REFILL_NS` after.
/// An app that has used more than that is not served (`ready`) until it is
/// back within its allowance; the call that went over is answered late
/// (`charge` says how late).
pub struct Limiter {
    level: [i32; APPS],
    last: [u64; APPS],
}
const APPS: usize = 16;
pub const BURST: i32 = 1024;
pub const REFILL_NS: u64 = 4_000_000;

impl Limiter {
    pub const fn new() -> Self {
        Limiter { level: [BURST; APPS], last: [0; APPS] }
    }
    pub fn ready(&mut self, app: usize, now_ns: u64) -> bool {
        let a = app % APPS;
        let add = now_ns.saturating_sub(self.last[a]) / REFILL_NS;
        if add >= (BURST - self.level[a]) as u64 {
            self.level[a] = BURST;
            self.last[a] = now_ns;
        } else {
            self.level[a] += add as i32;
            self.last[a] += add * REFILL_NS;
        }
        self.level[a] > 0
    }
    /// Count `used` messages; returns for how long the app is over.
    pub fn charge(&mut self, app: usize, used: u32) -> u64 {
        let a = app % APPS;
        self.level[a] = self.level[a].saturating_sub(used.min(i32::MAX as u32) as i32).max(-(1 << 20));
        if self.level[a] >= 0 {
            0
        } else {
            (-self.level[a]) as u64 * REFILL_NS
        }
    }
}
