//! The Open Runtime's side of /data (writeback.rs, the code data.rs runs)
//! against the real Storage files (files.rs) on the disk that can lose power.
use crate::files_tests::{blank, bytes, message, Store, OPEN};
use crate::protocol::*;
use crate::testdisk::*;
use crate::writeback::*;
use std::collections::{BTreeMap, BTreeSet};

/// The runtime's files, as far as saving goes (what runtime/open/fs.rs does).
#[derive(Default)]
struct Files {
    now: BTreeMap<String, Vec<u8>>,
    changed: BTreeSet<String>,
    stored: BTreeMap<String, Vec<u8>>,
}
impl Files {
    fn write(&mut self, path: &str, data: &[u8]) {
        self.now.insert(path.into(), data.to_vec());
        self.changed.insert(path.into());
    }
    fn unlink(&mut self, path: &str) {
        self.now.remove(path).unwrap();
        self.changed.insert(path.into());
    }
    fn rename(&mut self, from: &str, to: &str) {
        let data = self.now.remove(from).unwrap();
        self.now.insert(to.into(), data);
        self.changed.insert(from.into());
        self.changed.insert(to.into());
    }
    /// What a restart should find: every file /data can keep.
    fn kept(&self) -> BTreeMap<String, Vec<u8>> {
        self.now.iter().filter(|(p, d)| key(p).is_some() && d.len() <= MAX).map(|(p, d)| (p[6..].to_string(), d.clone())).collect()
    }
}
impl View for Files {
    fn changed(&self) -> &BTreeSet<String> {
        &self.changed
    }
    fn set_changed(&mut self, path: &str, on: bool) {
        if on {
            self.changed.insert(path.into());
        } else {
            self.changed.remove(path);
        }
    }
    fn contents(&self, path: &str) -> Option<&[u8]> {
        self.now.get(path).map(|d| &d[..])
    }
    fn stored(&self, path: &str) -> Option<&[u8]> {
        self.stored.get(path).map(|d| &d[..])
    }
    fn set_stored(&mut self, path: &str, data: Option<Vec<u8>>) {
        match data {
            Some(d) => self.stored.insert(path.into(), d),
            None => self.stored.remove(path),
        };
    }
}

/// Storage at the other end of the messages, with faults to order.
struct Wire {
    store: Store,
    /// Every message sent: (key, name, staged).
    sent: Vec<(u32, String, bool)>,
    /// The names stored after each message sent after `names` was last
    /// cleared with `watch()`.
    names: Vec<BTreeSet<String>>,
    watching: bool,
    /// Answer the next `n` messages with this key with OFFLINE; `lost`: after
    /// Storage has done them (only the reply is lost).
    fail: Option<(u32, usize, bool)>,
    refused: Vec<(String, Why)>,
}
impl Wire {
    fn new(dev: &TestDisk) -> Self {
        Wire { store: Store::new(dev), sent: vec![], names: vec![], watching: false, fail: None, refused: vec![] }
    }
    fn watch(&mut self) {
        self.names.clear();
        self.watching = true;
    }
    fn files(&mut self) -> BTreeMap<String, Vec<u8>> {
        self.store.list(OPEN).unwrap()
    }
    fn deletes(&self) -> usize {
        self.sent.iter().filter(|m| m.0 == FILE_DELETE).count()
    }
}
impl Port for Wire {
    fn send(&mut self, key: u32, name: &str, offset: u32, data: &[u8], flags: u32) -> u32 {
        self.sent.push((key, name.into(), flags & FILE_STAGED != 0));
        let mut lost = false;
        if let Some((k, n, after)) = &mut self.fail {
            if *k == key && *n > 0 {
                *n -= 1;
                if !*after {
                    return OFFLINE;
                }
                lost = true;
            }
        }
        let r = match self.store.call(OPEN, message(EXECUTE, key, name, offset, data, flags)) {
            Ok(_) => OK,
            Err(code) => code,
        };
        let mut names = BTreeSet::new();
        for slot in 0..if self.watching { 32 } else { 0 } {
            if let Ok(entry) = self.store.call(OPEN, message(GET, FILE_LIST, "", slot, &[], 0)) {
                let raw = entry.text();
                let end = raw[..16].iter().position(|&b| b == 0).unwrap_or(16);
                if end > 0 {
                    names.insert(String::from_utf8(raw[..end].to_vec()).unwrap());
                }
            }
        }
        if self.watching {
            self.names.push(names);
        }
        if lost { OFFLINE } else { r }
    }
    fn refused(&mut self, path: &str, why: Why) {
        self.refused.push((path.into(), why));
    }
}

/// Passes of the runtime's loop until everything is saved, or 3000 of them
/// (a save that keeps failing is tried again for ever, ever more rarely).
fn settle(s: &mut Saver, f: &mut Files, w: &mut Wire) -> usize {
    let mut passes = 0;
    while passes < 3000 && (!f.changed.is_empty() || s.busy()) {
        assert!(s.pass(f, w) <= PASS);
        passes += 1;
    }
    passes
}
fn setup(files: usize) -> (Saver, Files, Wire, TestDisk) {
    let dev = blank();
    let (mut s, mut f, mut w) = (Saver::new(), Files::default(), Wire::new(&dev));
    for k in 0..files {
        f.write(&format!("/data/f{k}"), &bytes(k as u8, 40 + k));
    }
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.files(), f.kept());
    w.sent.clear();
    (s, f, w, dev)
}

#[test]
fn a_save_takes_a_few_messages_a_pass_and_shows_nothing_until_it_is_whole() {
    let (mut s, mut f, mut w, _dev) = setup(1);
    let (v1, v2, v3) = (bytes(1, 4096), bytes(2, 4096), bytes(3, 100));
    f.write("/data/big", &v1);
    let mut passes = 0;
    while !f.changed.is_empty() || s.busy() {
        assert!(!w.files().contains_key("big"));
        let used = s.pass(&mut f, &mut w);
        assert!(used >= 1 && used <= PASS);
        passes += 1;
    }
    // 171 pieces of 24 bytes and the commit, 16 to a pass.
    assert_eq!((w.sent.len(), passes), (172, 11));
    assert_eq!(w.files()["big"], v1);
    assert_eq!(f.stored["/data/big"], v1);
    // Nothing more to do: nothing more is sent.
    assert_eq!(s.pass(&mut f, &mut w), 0);
    // Opened for writing and not changed: nothing is sent either.
    f.write("/data/big", &v1);
    assert_eq!((s.pass(&mut f, &mut w), f.changed.len()), (0, 0));
    // Changed while it is on its way: the old file shows until the new one
    // is whole, and the last version is what ends up stored.
    f.write("/data/big", &v2);
    for _ in 0..4 {
        s.pass(&mut f, &mut w);
    }
    f.write("/data/big", &v3);
    while !f.changed.is_empty() || s.busy() {
        let now = &w.files()["big"];
        assert!(*now == v1 || *now == v2);
        s.pass(&mut f, &mut w);
    }
    assert_eq!(w.files(), f.kept());
    assert!(w.sent.iter().all(|m| m.2), "only staged messages");
    // An empty file is one commit.
    w.sent.clear();
    f.write("/data/none", &[]);
    assert_eq!(s.pass(&mut f, &mut w), 1);
    assert_eq!(w.files()["none"], b"");
}

#[test]
fn a_rename_keeps_one_of_the_names_stored_at_every_message() {
    let (mut s, mut f, mut w, _dev) = setup(5);
    // Both orders of the names: the write goes first either way.
    for (from, to) in [("/data/f1", "/data/z"), ("/data/z", "/data/a"), ("/data/f3", "/data/f2")] {
        let data = f.now[from].clone();
        w.watch();
        f.rename(from, to);
        settle(&mut s, &mut f, &mut w);
        assert!(!w.names.is_empty());
        for names in &w.names {
            assert!(names.contains(&from[6..]) || names.contains(&to[6..]), "{from} to {to}: neither name is stored");
        }
        assert_eq!(w.files(), f.kept());
        assert_eq!(w.files()[&to[6..]], data);
    }
}

/// With all 32 slots taken the new name cannot be stored first. The delete
/// of the old name then goes first and the new name follows at once: the
/// file is not lost and nothing waits for the next change.
#[test]
fn a_rename_in_a_full_directory_ends_with_the_new_name_stored() {
    let (mut s, mut f, mut w, _dev) = setup(32);
    assert_eq!(w.files().len(), 32);
    let data = f.now["/data/f7"].clone();
    f.rename("/data/f7", "/data/renamed");
    let passes = settle(&mut s, &mut f, &mut w);
    assert!(passes <= 2, "{passes} passes");
    let files = w.files();
    assert_eq!((files.len(), files.get("renamed"), files.get("f7")), (32, Some(&data), None));
    assert_eq!(files, f.kept());
    // Remove one and make another.
    f.unlink("/data/f8");
    f.write("/data/another", b"new");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/another")).0, Ok(()));
    assert_eq!(w.files(), f.kept());
    // One more than fits: it is not stored, the rest is untouched, and it
    // is stored as soon as there is room.
    f.write("/data/extra", b"33");
    settle(&mut s, &mut f, &mut w);
    assert_eq!(s.failure("/data/extra"), Some(Why::Full));
    assert_eq!(w.files().len(), 32);
    assert!(f.changed.contains("/data/extra"), "it stays queued");
    f.unlink("/data/f0");
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.files(), f.kept());
    assert_eq!(s.failure("/data/extra"), None);
}

#[test]
fn a_failed_save_holds_the_delete_back_and_is_tried_again() {
    for lost in [false, true] {
        let (mut s, mut f, mut w, _dev) = setup(4);
        let data = bytes(9, 700);
        f.write("/data/f1", &data);
        settle(&mut s, &mut f, &mut w);
        f.rename("/data/f1", "/data/moved");
        // Storage fails the commit of the new name three times.
        w.fail = Some((FILE_TRUNCATE, 3, lost));
        w.watch();
        let mut passes = 0;
        while w.fail.unwrap().1 > 0 {
            s.pass(&mut f, &mut w);
            passes += 1;
            assert_eq!(w.deletes(), 0, "the old name was deleted while the new one was not stored");
            assert!(f.changed.contains("/data/moved") || s.busy(), "the failed save was dropped");
            assert!(passes < 1000);
        }
        assert!(passes > 90, "tried again without waiting: {passes} passes");
        assert_eq!(s.failure("/data/moved"), Some(Why::Io));
        settle(&mut s, &mut f, &mut w);
        for names in &w.names {
            assert!(names.contains("f1") || names.contains("moved"));
        }
        assert_eq!(w.files(), f.kept());
        assert_eq!((w.files()["moved"].clone(), w.deletes(), s.failure("/data/moved")), (data, 1, None));
    }
    // The disk itself fails for a while (every Storage call does): nothing
    // is lost, and all of it is stored once the disk is back.
    let (mut s, mut f, mut w, dev) = setup(4);
    f.rename("/data/f1", "/data/moved");
    f.write("/data/f2", &bytes(5, 3000));
    f.unlink("/data/f3");
    dev.fail_at(Some(3));
    for _ in 0..300 {
        s.pass(&mut f, &mut w);
    }
    assert!(!f.changed.is_empty());
    assert!(w.sent.len() < 300, "{} messages into a dead disk", w.sent.len());
    dev.fail_at(None);
    assert!(w.files().contains_key("f1") || w.files().contains_key("moved"));
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.files(), f.kept());
    // A save that never succeeds holds the deletes back only so long.
    let (mut s, mut f, mut w, _dev) = setup(4);
    f.rename("/data/f1", "/data/moved");
    w.fail = Some((FILE_TRUNCATE, usize::MAX, false));
    while s.failure("/data/moved").is_none() {
        s.pass(&mut f, &mut w);
    }
    // Asked about the old name meanwhile: not deleted, and it says so.
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/f1")), (Err(Why::Io), 0));
    assert_eq!(w.deletes(), 0);
    settle(&mut s, &mut f, &mut w);
    assert_eq!((w.deletes(), s.failure("/data/moved")), (1, Some(Why::Io)));
    assert!(f.changed.contains("/data/moved"));
}

#[test]
fn a_file_removed_after_a_failed_save_is_deleted_from_storage() {
    for lost in [false, true] {
        let (mut s, mut f, mut w, _dev) = setup(2);
        f.write("/data/token", b"old secret");
        settle(&mut s, &mut f, &mut w);
        // A new version fails to save; then the app removes the file.
        f.write("/data/token", b"new secret");
        w.fail = Some((FILE_TRUNCATE, 1, lost));
        assert_eq!(s.sync(&mut f, &mut w, Some("/data/token")).0, Err(Why::Io));
        assert_eq!(w.files()["token"], if lost { &b"new secret"[..] } else { b"old secret" });
        f.unlink("/data/token");
        settle(&mut s, &mut f, &mut w);
        assert_eq!(w.deletes(), 1);
        assert!(!w.files().contains_key("token"), "the token comes back after a restart");
        assert_eq!(w.files(), f.kept());
    }
    // A new file whose first save fails, removed: also not left behind.
    let (mut s, mut f, mut w, _dev) = setup(2);
    f.write("/data/token", b"secret");
    w.fail = Some((FILE_TRUNCATE, 1, true));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Err(Why::Io));
    f.unlink("/data/token");
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.files(), f.kept());
    // The reply was lost and the app puts the old contents back: what
    // Storage holds is not what was last known, so it is written again.
    let (mut s, mut f, mut w, _dev) = setup(2);
    f.write("/data/cfg", b"one");
    settle(&mut s, &mut f, &mut w);
    f.write("/data/cfg", b"two");
    w.fail = Some((FILE_TRUNCATE, 1, true));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/cfg")).0, Err(Why::Io));
    f.write("/data/cfg", b"one");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/cfg")).0, Ok(()));
    assert_eq!(w.files()["cfg"], b"one");
}

#[test]
fn sync_says_what_is_not_stored() {
    let (mut s, mut f, mut w, _dev) = setup(3);
    // Stored when it returns, and only what was asked for.
    f.write("/data/a", &bytes(1, 4096));
    f.write("/data/b", &bytes(2, 4096));
    let (r, used) = s.sync(&mut f, &mut w, Some("/data/b"));
    assert_eq!((r, used), (Ok(()), 172));
    assert_eq!(w.files()["b"], bytes(2, 4096));
    assert!(!w.files().contains_key("a") && f.changed.contains("/data/a"));
    // Nothing to do costs nothing.
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")), (Ok(()), 0));
    assert_eq!(s.sync(&mut f, &mut w, Some("/apps/x")), (Ok(()), 0));
    // A file part way there for another name gives way to the one asked for.
    for _ in 0..3 {
        s.pass(&mut f, &mut w);
    }
    assert!(s.busy());
    f.write("/data/b", b"again");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")), (Ok(()), 2));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    assert_eq!(w.files(), f.kept());
    // Too big, or a name Storage does not keep: an error for the file, and
    // what Storage held before stays.
    f.write("/data/b", &bytes(3, 4097));
    f.write("/data/a-name-too-long-to-keep", b"x");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")).0, Err(Why::TooBig));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/a-name-too-long-to-keep")).0, Err(Why::Name));
    f.write("/data/sub/file", b"x");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/sub/file")).0, Err(Why::Name));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")), (Err(Why::TooBig), 0));
    assert_eq!(w.files()["b"], b"again");
    assert_eq!(w.refused.len(), 3);
    // Asked about the directory or everything, such files do not count
    // (they are never kept; another app's must not fail this app's sync).
    assert_eq!(s.sync(&mut f, &mut w, Some("/data")).0, Ok(()));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    // Small again: stored, and no error is left.
    f.write("/data/b", b"small");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")).0, Ok(()));
    assert_eq!((w.files()["b"].clone(), s.failure("/data/b")), (b"small".to_vec(), None));
    f.unlink("/data/sub/file");
    f.unlink("/data/a-name-too-long-to-keep");
    assert_eq!(s.sync(&mut f, &mut w, None), (Ok(()), 0));
    // No room: an error for the file, for its directory and for everything.
    for k in 0..40 {
        f.write(&format!("/data/n{k}"), b"x");
    }
    assert_eq!(s.sync(&mut f, &mut w, None).0, Err(Why::Full));
    assert_eq!(w.files().len(), 32);
    let out: Vec<String> = f.changed.iter().cloned().collect();
    assert_eq!(out.len(), 40 + 5 - 32);
    assert_eq!(s.sync(&mut f, &mut w, Some(&out[0])).0, Err(Why::Full));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data")).0, Err(Why::Full));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")), (Ok(()), 0));
    // A call that fails this way is cheap: one message per file tried.
    assert!(s.sync(&mut f, &mut w, Some(&out[0])).1 <= 1);
    for p in &out[1..] {
        f.unlink(p);
    }
    f.unlink("/data/f0");
    assert_eq!(s.sync(&mut f, &mut w, Some(&out[0])).0, Ok(()));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    assert_eq!(w.files(), f.kept());
    // Storage fails: an error, the file stays queued, and the next call
    // that works says so.
    f.write("/data/b", b"important");
    w.fail = Some((FILE_WRITE, 1, false));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")).0, Err(Why::Io));
    assert!(f.changed.contains("/data/b"));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/b")).0, Ok(()));
    assert_eq!(w.files()["b"], b"important");
}

#[test]
fn one_sync_sends_a_bounded_number_of_messages() {
    let (mut s, mut f, mut w, _dev) = setup(0);
    for k in 0..31 {
        f.write(&format!("/data/f{k}"), &bytes(k as u8, 4096));
    }
    let mut calls = 0;
    loop {
        let (r, used) = s.sync(&mut f, &mut w, None);
        assert!(used <= SYNC);
        calls += 1;
        match r {
            Ok(()) => break,
            Err(why) => assert_eq!((why, used), (Why::Later, SYNC)),
        }
    }
    assert_eq!(calls, (31 * 172usize).div_ceil(SYNC as usize));
    assert_eq!(w.files(), f.kept());
    // A file removed and asked about by its old name: the rename's other
    // half goes first, as always.
    f.rename("/data/f1", "/data/g1");
    w.watch();
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/f1")).0, Ok(()));
    assert!(w.names.iter().all(|n| n.contains("f1") || n.contains("g1")));
    assert_eq!(w.files(), f.kept());
    // The allowance per app: a burst, then a slow refill.
    let mut l = Limiter::new();
    let mut now = 1_000_000_000;
    assert!(l.ready(3, now));
    assert_eq!(l.charge(3, 172), 0);
    assert_eq!(l.charge(3, 852), 0);
    assert!(!l.ready(3, now), "the burst is used up");
    assert!(l.ready(4, now), "another app has its own");
    assert_eq!(l.charge(3, 100), 100 * REFILL_NS);
    now += 100 * REFILL_NS;
    assert!(!l.ready(3, now));
    now += REFILL_NS;
    assert!(l.ready(3, now));
    assert_eq!(l.charge(3, 0), 0);
    now += 3600 * 1_000_000_000;
    assert!(l.ready(3, now));
    assert_eq!(l.charge(3, BURST as u32), 0, "it does not save up past the burst");
    assert_eq!(l.charge(3, 1), REFILL_NS);
}

/// The runtime's saves, renames and deletes with the power cut at every
/// write and flush of the disk: each file reads back as one of its whole
/// versions, and the renamed file is there under one of its names.
#[test]
fn power_cut_during_the_runtime_s_saves_and_renames() {
    let dev = blank();
    let (mut s, mut f, mut w) = (Saver::new(), Files::default(), Wire::new(&dev));
    let (v1, v2) = (bytes(1, 4096), bytes(2, 3000));
    f.write("/data/a", &v1);
    f.write("/data/keep", &bytes(3, 100));
    settle(&mut s, &mut f, &mut w);
    dev.record();
    f.write("/data/a", &v2);
    settle(&mut s, &mut f, &mut w);
    f.rename("/data/a", "/data/b");
    settle(&mut s, &mut f, &mut w);
    f.write("/data/c", &bytes(4, 600));
    f.unlink("/data/keep");
    settle(&mut s, &mut f, &mut w);
    f.rename("/data/b", "/data/a");
    f.write("/data/a", &v1);
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.files(), f.kept());
    let base = dev.0.borrow().base.clone();
    let log = std::mem::take(&mut dev.0.borrow_mut().log);
    let images = sweep(&base, &log, |cut, image| {
        let files = Store::new(&TestDisk::from_image(image)).list(OPEN).unwrap_or_else(|e| panic!("cut {cut}: reading fails with {e}"));
        let (a, b) = (files.get("a"), files.get("b"));
        assert!(a.is_some() || b.is_some(), "cut {cut}: the file is under neither name");
        assert!(a.map_or(true, |d| *d == v1 || *d == v2), "cut {cut}: a is a mix");
        assert!(b.map_or(true, |d| *d == v2), "cut {cut}: b is a mix");
        assert!(files.get("keep").map_or(true, |d| *d == bytes(3, 100)) && files.get("c").map_or(true, |d| *d == bytes(4, 600)), "cut {cut}");
    });
    assert!(images > 200, "{images}");
    println!("{images} power-cut images");
}
