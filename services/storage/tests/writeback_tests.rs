//! The Open Runtime's side of /data (writeback.rs, the code data.rs runs)
//! against the real server (vfs.rs on VioletFS) on the disk that can lose
//! power: the two ends of the window, joined in one process.
use crate::files_tests::bytes;
use crate::testdisk::*;
use crate::vfs_tests::{disk, Client};
use crate::writeback::proto::*;
use crate::writeback::*;
use std::collections::{BTreeMap, BTreeSet};

/// The runtime's files, as far as saving goes (what runtime/open/fs.rs does).
/// A folder is `None`.
#[derive(Default)]
struct Files {
    now: BTreeMap<String, Option<Vec<u8>>>,
    changed: BTreeSet<String>,
    stored: BTreeMap<String, Vec<u8>>,
    moved: BTreeSet<String>,
}
impl Files {
    fn write(&mut self, path: &str, data: &[u8]) {
        self.now.insert(path.into(), Some(data.to_vec()));
        self.changed.insert(path.into());
    }
    fn mkdir(&mut self, path: &str) {
        self.now.insert(path.into(), None);
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
        self.moved.insert(from.into());
    }
    /// The files as a restart should find them (paths as the volume has them).
    fn kept(&self) -> BTreeMap<String, Option<Vec<u8>>> {
        self.now.iter().map(|(p, d)| (p[6..].to_string(), d.clone())).collect()
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
        self.now.get(path)?.as_deref()
    }
    fn is_dir(&self, path: &str) -> bool {
        matches!(self.now.get(path), Some(None))
    }
    fn mtime(&self, _path: &str) -> u32 {
        77
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
            Some(d) => self.stored.insert(path.into(), d),
            None => self.stored.remove(path),
        };
    }
    fn loaded(&mut self, path: &str, file: Option<Vec<u8>>, _mtime: u32) {
        self.now.insert(path.into(), file);
    }
}

/// Storage at the other end of the window, with faults to order.
pub struct Wire {
    pub c: Client,
    /// Every request sent: (op, path).
    sent: Vec<(u32, String)>,
    /// Answer the next `n` requests with this op with EIO; `lost`: after
    /// Storage has done them (only the reply is lost).
    fail: Option<(u32, usize, bool)>,
    /// The status those failures answer.
    code: i32,
    len: usize,
}
impl Wire {
    pub fn new(dev: &TestDisk) -> Self {
        Wire { c: Client::open(dev).unwrap(), sent: vec![], fail: None, code: EIO, len: 0 }
    }
    fn count(&self, op: u32) -> usize {
        self.sent.iter().filter(|m| m.0 == op).count()
    }
    pub fn tree(&mut self) -> BTreeMap<String, Option<Vec<u8>>> {
        self.c.hello();
        self.c.tree()
    }
}
impl Port for Wire {
    fn call(&mut self, r: &Req, path: &[u8], data: &[u8]) -> Reply {
        self.sent.push((r.op, String::from_utf8(path.to_vec()).unwrap()));
        let mut lost = false;
        if let Some((op, n, after)) = &mut self.fail {
            if *op == r.op && *n > 0 {
                *n -= 1;
                if !*after {
                    return Reply { status: self.code, epoch: 0, value: 0, more: false };
                }
                lost = true;
            }
        }
        // As data.rs writes and reads the window, checked against the other way.
        let n = unsafe { encode_raw(self.c.win.as_mut_ptr(), r, path, data) };
        let mut same = vec![0u8; n];
        assert_eq!(encode(&mut same, r, path, b"", b"", data), Some(n));
        assert_eq!(self.c.win[..n], same[..]);
        self.c.v.serve(&mut self.c.win, n);
        let (a, b) = (answer(&self.c.win), unsafe { answer_raw(self.c.win.as_ptr()) });
        assert_eq!((a.status, a.epoch, a.value, a.len, a.more), (b.status, b.epoch, b.value, b.len, b.more));
        self.len = a.len;
        if lost {
            return Reply { status: EIO, epoch: 0, value: 0, more: false };
        }
        Reply { status: a.status, epoch: a.epoch, value: a.value, more: a.more }
    }
    fn data(&self) -> &[u8] {
        &self.c.win[REPLY..REPLY + self.len]
    }
}

/// Passes of the runtime's loop until everything is saved and committed, or
/// 3000 of them (a save that keeps failing is tried again for ever, ever
/// more rarely).
fn settle(s: &mut Saver, f: &mut Files, w: &mut Wire) -> usize {
    let mut passes = 0;
    while passes < 3000 && (!f.changed.is_empty() || s.busy()) {
        s.pass(f, w);
        passes += 1;
    }
    passes
}
/// A runtime that has loaded /data from `dev`.
fn start(dev: &TestDisk) -> (Saver, Files, Wire) {
    let (mut s, mut f, mut w) = (Saver::new(), Files::default(), Wire::new(dev));
    s.load(&mut f, &mut w).unwrap();
    f.changed.clear();
    w.sent.clear();
    (s, f, w)
}

#[test]
fn every_folder_and_file_comes_back_after_a_restart() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    let long = format!("/data/notes/{}", "n".repeat(255));
    f.write("/data/.settings", b"city=Oslo\n");
    f.mkdir("/data/notes");
    f.mkdir("/data/notes/deep");
    f.write("/data/notes/deep/a.txt", b"hello");
    f.write(&long, b"a name of 255 bytes");
    f.write("/data/.big", &bytes(5, 300_000));
    f.write("/data/empty", b"");
    assert!(settle(&mut s, &mut f, &mut w) < 100);
    // What a power cut now would leave is all of it.
    let cut = TestDisk::from_image(dev.image());
    let (_, f2, _) = start(&cut);
    assert_eq!(f2.kept(), f.kept());
    assert_eq!(f2.stored.len(), 4, "small files are remembered as stored; the big one is not");
    // Saved once: a file opened for writing and not changed is not sent again.
    w.sent.clear();
    f.changed.insert("/data/.settings".into());
    f.changed.insert("/data/notes/deep/a.txt".into());
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.sent, [], "nothing changed, nothing sent");
}

#[test]
fn a_save_is_a_request_a_pass_and_shows_nothing_until_it_is_whole() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    let (v1, v2) = (bytes(1, 3 * PIECE + 5), bytes(2, 2 * PIECE));
    f.write("/data/.a", &v1);
    settle(&mut s, &mut f, &mut w);
    w.sent.clear();
    f.write("/data/.a", &v2);
    for pass in 0..2 {
        let used = s.pass(&mut f, &mut w);
        assert_eq!(used, if pass == 0 { 1 } else { 2 }, "one piece, and the commit after the last");
        assert_eq!(w.tree()[".a"], Some(if pass == 0 { v1.clone() } else { v2.clone() }));
    }
    assert!(!s.busy() && f.changed.is_empty());
    assert_eq!((w.count(PUT), w.count(COMMIT)), (2, 1));
}

#[test]
fn a_rename_keeps_one_of_the_names_at_every_request() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.write("/data/notes.draft", &bytes(1, 5000));
    settle(&mut s, &mut f, &mut w);
    f.rename("/data/notes.draft", "/data/notes.final");
    let mut seen = BTreeSet::new();
    while !f.changed.is_empty() || s.busy() {
        s.pass(&mut f, &mut w);
        let names: Vec<String> = w.tree().into_keys().collect();
        assert!(!names.is_empty(), "neither name is stored");
        seen.insert(names);
    }
    assert_eq!(seen.len(), 2, "both names for a moment, then the new one: {seen:?}");
    assert_eq!(w.tree(), f.kept());
}

#[test]
fn a_failed_save_holds_the_removal_back_and_is_tried_again() {
    for lost in [false, true] {
        let dev = disk(512);
        let (mut s, mut f, mut w) = start(&dev);
        let data = bytes(4, 2000);
        f.write("/data/old", &data);
        settle(&mut s, &mut f, &mut w);
        w.sent.clear();
        f.rename("/data/old", "/data/new");
        w.fail = Some((PUT, 3, lost));
        for _ in 0..20 {
            s.pass(&mut f, &mut w);
        }
        assert_eq!(w.count(REMOVE), 0, "the old name stays while the new one is not known to be stored");
        assert_eq!(s.failure("/data/new"), Some(Why::Io));
        assert_eq!(s.sync(&mut f, &mut w, Some("/data/new")).0, Err(Why::Io), "sync tries at once and says it failed");
        let passes = settle(&mut s, &mut f, &mut w);
        assert!(passes > 32 && passes < 400, "tried again after a wait: {passes}");
        assert_eq!(w.tree(), f.kept(), "lost reply: {lost}");
        assert_eq!(s.failure("/data/new"), None);
    }
}

#[test]
fn sync_says_what_is_not_on_the_volume() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    let share = w.c.call(STATFS, 0, 0, 0, "", "", "app", &[]).map(|r| le32(&r.1, 20)).unwrap() as usize * 4096;
    // An app's file beyond its share: EDQUOT at the sync, the rest is saved.
    f.write("/data/app", &bytes(1, share + 4096));
    f.write("/data/app.small", b"fits");
    f.write("/data/other", b"another owner");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/app")).0, Err(Why::Quota));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/app.small")).0, Ok(()));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Err(Why::Quota), "asked about everything, a file over quota counts");
    assert_eq!(w.tree().keys().collect::<Vec<_>>(), ["app.small", "other"]);
    // Smaller, it fits; and a removal makes room for what did not.
    f.write("/data/app", &bytes(2, share / 2));
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    f.unlink("/data/app");
    f.write("/data/app.next", &bytes(3, share / 2 + 8192));
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/app.next")).0, Ok(()), "the removal went first to make room");
    assert_eq!(w.tree(), f.kept());
    // Only what is in scope is sent; the commit covers it.
    f.write("/data/one", b"1");
    f.write("/data/two", b"2");
    w.sent.clear();
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/one")), (Ok(()), 2));
    assert_eq!(w.sent, [(PUT, "one".to_string()), (COMMIT, "".to_string())]);
    assert!(f.changed.contains("/data/two"));
}

#[test]
fn one_sync_sends_a_bounded_number_of_requests() {
    let dev = disk(1024);
    let (mut s, mut f, mut w) = start(&dev);
    let big = bytes(9, SYNC as usize * PIECE + 100_000);
    f.write("/data/.big", &big);
    let (r, used) = s.sync(&mut f, &mut w, None);
    assert_eq!((r, used), (Err(Why::Later), SYNC));
    assert!(w.tree().is_empty(), "nothing of it shows yet");
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    assert_eq!(w.tree()[".big"], Some(big));

    let mut l = Limiter::new();
    let mut now = 1_000_000_000;
    assert!(l.ready(3, now));
    assert_eq!(l.charge(3, 100), 0);
    assert_eq!(l.charge(3, BURST as u32 - 100), 0);
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

#[test]
fn folders_and_files_take_each_other_s_place() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.mkdir("/data/a");
    f.mkdir("/data/a/b");
    f.write("/data/a/b/c", b"deep");
    f.write("/data/d", b"file");
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.tree(), f.kept());
    // The folder goes, a file takes its name; the file goes, a folder does.
    f.unlink("/data/a/b/c");
    f.unlink("/data/a/b");
    f.unlink("/data/a");
    f.write("/data/a", b"now a file");
    f.unlink("/data/d");
    f.mkdir("/data/d");
    f.write("/data/d/e", b"inside");
    assert!(settle(&mut s, &mut f, &mut w) < 200);
    assert_eq!(w.tree(), f.kept());
    assert_eq!(s.failure("/data/a"), None);
    // fsync of a file whose folders are not on the volume yet makes them.
    f.mkdir("/data/x");
    f.mkdir("/data/x/y");
    f.write("/data/x/y/z", b"z");
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/x/y/z")).0, Ok(()));
    assert_eq!(w.tree()["x/y/z"], Some(b"z".to_vec()));
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.tree(), f.kept());
    w.c.fsck();
}

#[test]
fn after_storage_lost_what_was_sent_nothing_more_is_written() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.write("/data/kept", b"committed");
    f.write("/data/gone", b"to be removed");
    settle(&mut s, &mut f, &mut w);
    // Sent, not committed: the device fails at the commit.
    f.write("/data/kept", b"changed");
    f.unlink("/data/gone");
    dev.fail_at(Some(3));
    let r = (0..40).map(|_| s.sync(&mut f, &mut w, None).0).last().unwrap();
    assert_eq!(r, Err(Why::Io));
    dev.fail_at(None);
    // Storage mounts again: the volume is as it was committed. The client
    // is told (ESTALE) and from then on writes nothing: what it believes
    // stored may not be, and sync says so until the restart.
    let durable = BTreeMap::from([("kept".to_string(), Some(b"committed".to_vec())), ("gone".to_string(), Some(b"to be removed".to_vec()))]);
    assert_eq!(w.tree(), durable);
    f.write("/data/more", b"x");
    w.sent.clear();
    assert_eq!(s.sync(&mut f, &mut w, None).0, Err(Why::Io));
    assert!(!s.ready() && w.sent.len() == 1, "the one request that was answered ESTALE: {:?}", w.sent);
    for _ in 0..100 {
        s.pass(&mut f, &mut w);
    }
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/more")), (Err(Why::Io), 0));
    assert_eq!(w.sent.len(), 1);
    assert_eq!(w.tree(), durable);
    // The restart loads that state and goes on from it.
    let (mut s, mut f, mut w) = start(&dev);
    assert_eq!(f.kept(), durable);
    f.unlink("/data/gone");
    assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()));
    assert_eq!(w.tree(), f.kept());
    w.c.fsck();
}

#[test]
fn a_volume_that_was_not_read_whole_is_never_written() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.mkdir("/data/d");
    for k in 0..5 {
        f.write(&format!("/data/d/{k}"), &bytes(k, 9000));
    }
    f.write("/data/.lock", b"passcode record");
    settle(&mut s, &mut f, &mut w);
    let before = w.tree();
    for op in [HELLO, LIST, READ] {
        let (mut s, mut f, mut w) = (Saver::new(), Files::default(), Wire::new(&dev));
        w.fail = Some((op, 1, false));
        assert_eq!(s.load(&mut f, &mut w), Err(EIO));
        assert!(!s.ready());
        // The runtime goes on with what it has, in memory only.
        f.write("/data/.lock", b"");
        f.write("/data/new", b"x");
        w.sent.clear();
        for _ in 0..50 {
            s.pass(&mut f, &mut w);
        }
        assert_eq!(s.sync(&mut f, &mut w, None), (Err(Why::Io), 0));
        assert_eq!(w.sent, [], "nothing is sent");
    }
    assert!(w.tree() == before && before.len() == 7, "and the volume is as it was");
    // No volume at all is another matter: nothing was kept, nothing can be.
    struct Absent;
    impl Port for Absent {
        fn call(&mut self, _: &Req, _: &[u8], _: &[u8]) -> Reply {
            Reply { status: ENXIO, epoch: 0, value: 0, more: false }
        }
        fn data(&self) -> &[u8] {
            &[]
        }
    }
    let (mut s, mut f) = (Saver::new(), Files::default());
    assert_eq!(s.load(&mut f, &mut Absent), Err(ENXIO));
    f.write("/data/x", b"x");
    assert_eq!(s.sync(&mut f, &mut Absent, None), (Err(Why::Io), 0));
}

/// The runtime's saves, renames, removals and new folders with the power cut
/// at every write and flush of the disk: each file reads back as one of its
/// whole versions, the renamed file is there under one of its names, and a
/// file is never there without its folder.
#[test]
fn power_cut_during_the_runtime_s_saves_and_renames() {
    let dev = disk(256);
    let (mut s, mut f, mut w) = start(&dev);
    let (v1, v2) = (bytes(1, 20_000), bytes(2, 13_000));
    f.write("/data/.a", &v1);
    f.write("/data/keep", &bytes(3, 100));
    f.write("/data/b.old", &bytes(4, 5000));
    f.write("/data/doomed", &bytes(5, 3000));
    settle(&mut s, &mut f, &mut w);
    dev.record();
    f.write("/data/.a", &v2);
    f.rename("/data/b.old", "/data/b.new");
    f.unlink("/data/doomed");
    f.mkdir("/data/dir");
    f.write("/data/dir/in", &bytes(6, 4097));
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.tree(), f.kept());
    let state = dev.0.borrow();
    let mut ends = BTreeSet::new();
    let images = sweep(&state.base, &state.log, |cut, image| {
        let mut c = Client::open(&TestDisk::from_image(image)).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        let t = c.tree();
        let a = t[".a"].as_ref().unwrap();
        assert!(*a == v1 || *a == v2, "cut {cut}: .a is neither version");
        assert_eq!(t["keep"], Some(bytes(3, 100)), "cut {cut}");
        let b: Vec<_> = ["b.old", "b.new"].into_iter().filter_map(|n| t.get(n)).collect();
        assert!(!b.is_empty() && b.iter().all(|d| **d == Some(bytes(4, 5000))), "cut {cut}: the renamed file");
        if let Some(d) = t.get("doomed") {
            assert_eq!(*d, Some(bytes(5, 3000)), "cut {cut}");
        }
        if let Some(d) = t.get("dir/in") {
            assert_eq!((d, &t["dir"]), (&Some(bytes(6, 4097)), &None), "cut {cut}");
        }
        ends.insert(t.keys().cloned().collect::<Vec<_>>());
        c.fsck();
        // A runtime starting on it loads it whole and can go on saving.
        drop(c);
    });
    assert!(images > 100 && ends.len() > 1, "{images} images, {} outcomes", ends.len());
}

#[test]
fn update_pieces_are_never_sent_to_the_volume() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.write("/data/settings.ota-staging", &bytes(9, 65536));
    f.write("/data/settings.note", b"kept");
    assert!(settle(&mut s, &mut f, &mut w) < 100);
    assert!(w.sent.iter().all(|m| !m.1.contains("ota-staging")), "{:?}", w.sent);
    assert_eq!(s.sync(&mut f, &mut w, Some("/data/settings.ota-staging")).0, Ok(()));
    f.unlink("/data/settings.ota-staging");
    settle(&mut s, &mut f, &mut w);
    assert!(w.sent.iter().all(|m| !m.1.contains("ota-staging")), "{:?}", w.sent);
    let cut = TestDisk::from_image(dev.image());
    let (_, f2, _) = start(&cut);
    assert_eq!(f2.kept().keys().collect::<Vec<_>>(), ["settings.note"]);
}

#[test]
fn a_rename_s_old_name_stays_while_the_new_one_fails_for_any_reason() {
    for code in [EDQUOT, ENOSPC, EIO] {
        let dev = disk(512);
        let (mut s, mut f, mut w) = start(&dev);
        f.write("/data/notes.tmp", &bytes(4, 3 * PIECE));
        f.write("/data/other", b"to delete");
        settle(&mut s, &mut f, &mut w);
        w.sent.clear();
        f.rename("/data/notes.tmp", "/data/notes");
        w.fail = Some((PUT, usize::MAX, false));
        w.code = code;
        for _ in 0..3000 {
            s.pass(&mut f, &mut w);
        }
        assert_eq!(w.count(REMOVE), 0, "{code}: the old name stays while the new one is not stored");
        assert_eq!(w.tree().get("notes.tmp"), Some(&Some(bytes(4, 3 * PIECE))), "{code}");
        // Deleting something else to make room is not held back by it.
        f.unlink("/data/other");
        for _ in 0..3 {
            s.pass(&mut f, &mut w);
        }
        assert_eq!(w.tree().get("other"), None, "{code}: a plain removal goes");
        assert!(w.tree().contains_key("notes.tmp"), "{code}");
        // Once the new name is stored the old one goes.
        w.fail = None;
        assert_eq!(s.sync(&mut f, &mut w, None).0, Ok(()), "{code}");
        assert_eq!(w.tree(), f.kept(), "{code}");
        assert!(f.moved.is_empty(), "{code}");
    }
}

#[test]
fn a_save_that_found_no_room_is_not_sent_again_until_something_changes() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.write("/data/big", &bytes(1, 2 * PIECE));
    w.fail = Some((PUT, usize::MAX, false));
    w.code = EDQUOT;
    for _ in 0..3000 {
        s.pass(&mut f, &mut w);
    }
    assert_eq!(w.count(PUT), 1, "not re-sent on a timer");
    assert_eq!(s.failure("/data/big"), Some(Why::Quota));
    // The app makes it smaller: tried again.
    w.fail = None;
    f.write("/data/big", &bytes(1, PIECE));
    settle(&mut s, &mut f, &mut w);
    assert_eq!(w.tree(), f.kept());
    assert_eq!(s.failure("/data/big"), None);
}

#[test]
fn a_file_written_to_while_it_is_being_sent_starts_over() {
    let dev = disk(512);
    let (mut s, mut f, mut w) = start(&dev);
    f.write("/data/.a", &bytes(1, 3 * PIECE));
    s.pass(&mut f, &mut w);
    assert_eq!(w.count(PUT), 1);
    f.write("/data/.a", &bytes(2, 3 * PIECE + 9));
    let passes = settle(&mut s, &mut f, &mut w);
    assert!(passes < 10);
    assert_eq!(w.tree(), f.kept(), "the new version whole, nothing of the old");
    assert_eq!(w.count(PUT), 5, "one piece of the old, all four of the new");
}

/// Storage with a volume listing one file of `size` bytes: what `load`
/// does when the runtime has no memory for it.
struct Huge(&'static str, Vec<u8>);
impl Port for Huge {
    fn call(&mut self, r: &Req, _: &[u8], _: &[u8]) -> Reply {
        self.1.clear();
        if r.op == LIST {
            let name = self.0.as_bytes();
            let mut e = vec![0u8; ENTRY + ((name.len() + 3) & !3)];
            e[4..8].copy_from_slice(&(0x8000u32 | (name.len() as u32) << 16).to_le_bytes());
            e[8..16].copy_from_slice(&(1u64 << 62).to_le_bytes());
            e[ENTRY..ENTRY + name.len()].copy_from_slice(name);
            self.1 = e;
        }
        Reply { status: if r.op == READ { EIO } else { 0 }, epoch: 1, value: 0, more: false }
    }
    fn data(&self) -> &[u8] {
        &self.1
    }
}

#[test]
fn a_file_too_large_for_memory_is_left_unseen_or_fails_the_load() {
    let mut f = Files::default();
    assert_eq!(Saver::new().load(&mut f, &mut Huge("game.save", vec![])), Ok(0), "an app's: skipped, not an abort");
    assert!(f.now.is_empty());
    assert_eq!(Saver::new().load(&mut f, &mut Huge(".lock", vec![])), Err(ENOMEM), "the system's: fails closed");
}
