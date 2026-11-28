//! /data: files that survive a restart. They are kept by the Storage
//! service in a filesystem of its own (services/storage/vfs.rs on VioletFS):
//! folders, names of up to 255 bytes, files as large as memory holds. The runtime loads
//! them at startup and writes a file back after it changes: whole, so a
//! power cut leaves the old file or the new one. fsync, fdatasync and sync
//! write back and commit at once and say whether it worked (`fsync`). A
//! rename is the new name first, then the old one goes: a cut between them
//! leaves both names.
//!
//! What is sent when, what happens when a save fails and what a sync
//! reports is decided in services/storage/writeback.rs, which the host tests
//! run against the real Storage server; this file is its link to the runtime.
use crate::log;
use crate::protocol::*;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use super::fs::Fs;

#[path = "../../services/storage/writeback.rs"]
mod writeback;
use writeback::proto::{answer_raw, encode_raw, Req, REPLY, VFS};
use writeback::{Limiter, Port, Reply, Saver, View, Why};

pub const DIR: &str = "/data";
/// The 64 KiB window shared with Storage (also appstore.rs's; one at a time).
const WINDOW: *mut u8 = 0x0913_0000 as *mut u8;

// The runtime is one thread; these are touched only from its loop.
static mut SAVER: Saver = Saver::new();
static mut LIMIT: Limiter = Limiter::new();
fn saver() -> &'static mut Saver {
    unsafe { &mut *core::ptr::addr_of_mut!(SAVER) }
}

/// Storage's filesystem server, through Runtime and the window.
struct Storage(usize);
impl Port for Storage {
    fn call(&mut self, r: &Req, path: &[u8], data: &[u8]) -> Reply {
        self.0 = 0;
        let n = unsafe { encode_raw(WINDOW, r, path, data) };
        let mut p = Message::new(EXECUTE, APPSTORE, VFS).with_arg(n as u32).call(super::sys::SLOT_RUNTIME);
        if p.op == OK {
            let ticket = p.value;
            loop {
                p = Message::new(POLL, 0, ticket).call(super::sys::SLOT_RUNTIME);
                if p.op != BUSY {
                    break;
                }
                super::sys::pause();
            }
        }
        let mut a = unsafe { answer_raw(WINDOW) };
        match p.op {
            OK => self.0 = a.len,
            // Storage has no disk at all (a disk with no room for a data
            // volume, or whose volume is not where it was made, is ENODEV).
            OFFLINE => a.status = writeback::ENXIO,
            _ => a.status = super::syscall::EIO,
        }
        Reply { status: a.status, epoch: a.epoch, value: a.value, more: a.more }
    }
    fn data(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(WINDOW.add(REPLY), self.0) }
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
    fn is_dir(&self, path: &str) -> bool {
        Fs::is_dir(self, path)
    }
    fn mtime(&self, path: &str) -> u32 {
        self.stat(path).map_or(0, |s| s.mtime)
    }
    fn stored(&self, path: &str) -> Option<&[u8]> {
        self.stored.get(path).map(|d| &d[..])
    }
    fn renamed(&self, path: &str) -> bool {
        self.moved.contains(path)
    }
    fn removed(&mut self, path: &str) {
        self.moved.remove(path);
    }
    fn set_stored(&mut self, path: &str, data: Option<Vec<u8>>) {
        match data {
            Some(d) => self.stored.insert(String::from(path), d),
            None => self.stored.remove(path),
        };
    }
    fn loaded(&mut self, path: &str, file: Option<Vec<u8>>, mtime: u32) {
        match file {
            None => drop(self.mkdir(path)),
            Some(data) => {
                if let Some(f) = self.writable(path, true, true, mtime) {
                    *f = data;
                }
            }
        }
    }
}

/// Put everything Storage keeps into /data. True if all of it was read (or
/// Storage has no data volume at all): only then does a missing file mean
/// it is not stored, which the passcode depends on (trusted_ui.rs `boot`).
/// False: the volume is there and could not be read whole. /data then
/// holds what was read, and nothing is written back this run, so that the
/// volume is not written over with less than it has.
pub fn load(fs: &mut Fs) -> bool {
    let _ = fs.mkdir(DIR);
    let mut found = Err(0);
    // An error is tried again (Storage may be starting), unless it has no volume.
    for _ in 0..3 {
        found = saver().load(fs, &mut Storage(0));
        if !matches!(found, Err(e) if e != writeback::ENXIO) {
            break;
        }
        super::sys::pause();
    }
    fs.changed.clear();
    // Status -6: Storage has no data volume and nothing is kept. Any other
    // but 0: /data could not be read whole and nothing is written back.
    let status = found.err().unwrap_or(0);
    log!("/data: {} saved files, status {}", found.unwrap_or(0), status);
    status == 0 || status == writeback::ENXIO
}

/// Write back every changed file now, however long it takes (the runtime's
/// own saves: settings, grants). True if all of them are on the volume and
/// committed when it returns.
pub fn sync(fs: &mut Fs) -> bool {
    loop {
        match saver().sync(fs, &mut Storage(0), None).0 {
            Err(Why::Later) => {}
            r => return r.is_ok(),
        }
    }
}

/// fsync and fdatasync of the file or directory at `scope`, or sync (None),
/// for app `app`: write back what is in scope and changed, and commit, now.
/// Returns the call's result and for how long to hold the answer back.
///   0        every file in scope is on the volume as it is now, durably
///   EIO      Storage failed, or /data could not be read at startup
///   ENOSPC   the volume is full; EDQUOT: the file's owner is at its quota.
///            The file is not stored (an older version may be), and a
///            failed save is tried again.
///   EINTR    not finished: one call sends a bounded number of requests, and
///            an app that has used up its allowance is not served until it is
///            back within it. Call again.
/// The wait is what keeps one app's syncs from holding the runtime for
/// more than a part of the time (writeback::Limiter).
pub fn fsync(fs: &mut Fs, scope: Option<&str>, app: usize, now_ns: u64) -> (i32, u64) {
    if scope.is_some_and(|s| s != DIR && !s.starts_with("/data/")) {
        return (0, 0); // nothing outside /data is ever stored
    }
    let limit = unsafe { &mut *core::ptr::addr_of_mut!(LIMIT) };
    if !limit.ready(app, now_ns) {
        return (Why::Later as i32, 0);
    }
    let (r, used) = saver().sync(fs, &mut Storage(0), scope);
    let wait = limit.charge(app, used);
    // Why's numbers are the errors: EIO, ENOSPC, EDQUOT and EINTR.
    match r {
        Ok(()) => (0, wait),
        Err(why) => (why as i32, 0),
    }
}

/// Is something on its way to Storage? The loop should come round again soon.
pub fn busy() -> bool {
    saver().busy()
}

/// One pass of the runtime's loop: one piece of the file being written
/// back (or a folder, or a removal), so a save never holds the runtime for
/// long, and the commit when the last has gone.
pub fn flush(fs: &mut Fs) {
    saver().pass(fs, &mut Storage(0));
}
