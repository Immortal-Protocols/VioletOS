//! /data: files that survive a restart. Each file directly in /data is kept
//! by the Storage service in the open apps' private directory (32 files of
//! up to 4 KiB, names of up to 16 letters, digits, '.', '_' or '-'). The
//! runtime loads them at startup and writes a file back after it changes:
//! whole, in one commit, so a power cut leaves the old file or the new one.
//! fsync, fdatasync and sync write back at once and say whether it worked
//! (`fsync`). A rename is two commits, the new name first: a cut between
//! them leaves both names.
//!
//! What is sent when, what happens when a save fails and what a sync
//! reports is decided in services/storage/writeback.rs, which the host tests
//! run against the real Storage files; this file is its link to the runtime.
use crate::log;
use crate::protocol::*;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use super::fs::Fs;

#[path = "../../services/storage/writeback.rs"]
mod writeback;
use writeback::{Limiter, Port, Saver, View, Why};

pub const DIR: &str = "/data";
/// How many files Storage keeps for open apps (services/storage/files.rs).
const FILES: u32 = 32;

// The runtime is one thread; these are touched only from its loop.
static mut SAVER: Saver = Saver::new();
static mut LIMIT: Limiter = Limiter::new();
fn saver() -> &'static mut Saver {
    unsafe { &mut *core::ptr::addr_of_mut!(SAVER) }
}

struct Storage;
impl Port for Storage {
    fn send(&mut self, key: u32, name: &str, offset: u32, data: &[u8], flags: u32) -> u32 {
        send(EXECUTE, key, name, offset, data, flags).op
    }
    fn refused(&mut self, path: &str, why: Why) {
        match why {
            Why::TooBig => log!("{} is over 4 KiB and is not kept", path),
            _ => log!("{} is not kept across restarts (names in /data: up to 16 of A-Z a-z 0-9 . _ -)", path),
        }
    }
}
impl View for Fs {
    fn changed(&self) -> &BTreeSet<String> {
        &self.changed
    }
    fn set_changed(&mut self, path: &str, on: bool) {
        if on {
            self.changed.insert(String::from(path));
        } else {
            self.changed.remove(path);
        }
    }
    fn contents(&self, path: &str) -> Option<&[u8]> {
        Fs::contents(self, path)
    }
    fn stored(&self, path: &str) -> Option<&[u8]> {
        self.stored.get(path).map(|d| &d[..])
    }
    fn set_stored(&mut self, path: &str, data: Option<Vec<u8>>) {
        match data {
            Some(d) => self.stored.insert(String::from(path), d),
            None => self.stored.remove(path),
        };
    }
}

fn file(op: u32, key: u32, name: &str, offset: u32, data: &[u8]) -> Message {
    send(op, key, name, offset, data, 0)
}

/// `flags`: FILE_STAGED for the pieces and the commit of a whole-file replacement.
fn send(op: u32, key: u32, name: &str, offset: u32, data: &[u8], flags: u32) -> Message {
    let mut m = Message::new(op, key, offset);
    m.arg = data.len() as u32 | flags;
    let mut bytes = [0u8; 40];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    bytes[16..16 + data.len()].copy_from_slice(data);
    m.put_text(&bytes);
    let r = m.call(super::sys::SLOT_RUNTIME);
    if r.op != OK {
        return r;
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(super::sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        super::sys::pause();
    }
}

/// Put every stored file into /data. True if every file Storage keeps was
/// read (or Storage has no disk at all): only then does a missing file mean
/// it is not stored, which the passcode depends on (trusted_ui.rs `boot`).
pub fn load(fs: &mut Fs) -> bool {
    let _ = fs.mkdir(DIR);
    let mut count = 0;
    let (mut failed, mut offline) = (0, 0);
    for slot in 0..FILES {
        let mut entry = file(GET, FILE_LIST, "", slot, &[]);
        // An error is tried again, unless every slot so far was OFFLINE (no disk).
        for _ in 0..2 {
            if entry.op == OK || entry.op == INVALID || (slot > 0 && offline == slot) {
                break;
            }
            super::sys::pause();
            entry = file(GET, FILE_LIST, "", slot, &[]);
        }
        // INVALID: no such slot (a small disk holds fewer files).
        if entry.op != OK && entry.op != INVALID {
            failed += 1;
            offline += (entry.op == OFFLINE) as u32;
        }
        if entry.op != OK || entry.value == 0 {
            continue;
        }
        let raw = entry.text();
        let end = raw[..16].iter().position(|&b| b == 0).unwrap_or(16);
        let Ok(name) = core::str::from_utf8(&raw[..end]) else { continue };
        let name = String::from(name);
        let len = entry.value as usize;
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let r = file(GET, FILE_READ, &name, data.len() as u32, &[]);
            if r.op != OK || r.value == 0 {
                failed += 1; // kept short: a short passcode record keeps the phone locked
                break;
            }
            data.extend_from_slice(&r.text()[..r.value as usize]);
        }
        let path = alloc::format!("{}/{}", DIR, name);
        if let Some(f) = fs.writable(&path, true, true, 0) {
            *f = data.clone();
            fs.stored.insert(path, data);
            count += 1;
        }
    }
    fs.changed.clear();
    log!("/data: {} saved files, {} unreadable", count, failed);
    super::sysui::listing_complete(failed, offline, FILES)
}

/// Write back every changed file now, however long it takes (the runtime's
/// own saves, at most the 32 files Storage keeps). True if all of them are
/// in Storage when it returns.
pub fn sync(fs: &mut Fs) -> bool {
    loop {
        match saver().sync(fs, &mut Storage, None).0 {
            Err(Why::Later) => {}
            r => return r.is_ok(),
        }
    }
}

/// fsync and fdatasync of the file or directory at `scope`, or sync (None),
/// for app `app`: write back what is in scope and changed, now. Returns the
/// call's result and for how long to hold the answer back.
///   0        every file in scope is in Storage as it is now
///   EIO      Storage failed; ENOSPC: its 32 files are taken; EFBIG: over
///            4 KiB; EINVAL: not a name /data keeps. The file is not stored
///            (an older version may be), and a failed save is tried again.
///   EINTR    not finished: one call sends a bounded number of messages, and
///            an app that has used up its allowance is not served until it is
///            back within it. Call again.
/// The wait is what keeps one app's syncs from holding the runtime for
/// more than a part of the time (writeback::Limiter).
pub fn fsync(fs: &mut Fs, scope: Option<&str>, app: usize, now_ns: u64) -> (i32, u64) {
    use super::syscall::{EINTR, EINVAL, EIO};
    if scope.is_some_and(|s| s != DIR && !s.starts_with("/data/")) {
        return (0, 0); // nothing outside /data is ever stored
    }
    let limit = unsafe { &mut *core::ptr::addr_of_mut!(LIMIT) };
    if !limit.ready(app, now_ns) {
        return (EINTR, 0);
    }
    let (r, used) = saver().sync(fs, &mut Storage, scope);
    let wait = limit.charge(app, used);
    match r {
        Ok(()) => (0, wait),
        Err(Why::Io) => (EIO, 0),
        Err(Why::Full) => (-28, 0),
        Err(Why::TooBig) => (-27, 0),
        Err(Why::Name) => (EINVAL, 0),
        Err(Why::Later) => (EINTR, 0),
    }
}

/// Is a file part way to Storage? The loop should come round again soon.
pub fn busy() -> bool {
    saver().busy()
}

/// One pass of the runtime's loop: a few messages of the file being written
/// back (or a delete), so a save never holds the runtime for long. A 4 KiB
/// file takes 11 passes.
pub fn flush(fs: &mut Fs) {
    saver().pass(fs, &mut Storage);
}
