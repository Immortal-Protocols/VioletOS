//! The whole of /data on the host: the Open Runtime's file layer
//! (runtime/open/fs.rs), its client (writeback.rs) and Storage's server
//! (vfs.rs on VioletFS), each the shipped source, with only the window
//! between them and the disk replaced. What an app's file calls end up
//! doing to `Fs` is done here directly (syscall.rs is not built on the host).
use crate::fs::{normalize, Fs, S_IFDIR, S_IFMT};
use crate::files_tests::bytes;
use crate::testdisk::*;
use crate::vfs_tests::disk;
use crate::writeback::{Saver, View};
use crate::writeback_tests::Wire;
use std::collections::BTreeSet;

/// As runtime/open/data.rs has it (that file reaches the window and is not
/// built here): every call goes straight to `Fs`.
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

/// The runtime starting on a disk: data.rs `load`.
fn boot(dev: &TestDisk) -> (Saver, Fs, Wire) {
    let (mut s, mut fs, mut w) = (Saver::new(), Fs::new(&[]), Wire::new(dev));
    let _ = fs.mkdir("/data");
    s.load(&mut fs, &mut w).unwrap();
    fs.changed.clear();
    (s, fs, w)
}
/// The runtime's loop until nothing is left to save (data.rs `flush`).
fn settle(s: &mut Saver, fs: &mut Fs, w: &mut Wire) {
    for _ in 0..2000 {
        if fs.changed.is_empty() && !s.busy() {
            return;
        }
        s.pass(fs, w);
    }
    let why: Vec<_> = fs.changed.iter().map(|p| (&p[..p.len().min(40)], s.failure(p))).collect();
    panic!("the saves do not come to an end: {why:?}");
}
fn write(fs: &mut Fs, path: &str, data: &[u8], now: u32) {
    *fs.writable(path, true, true, now).unwrap_or_else(|| panic!("{path} cannot be made")) = data.to_vec();
}
/// Everything under /data as an app would find it: path, folder?, bytes, mtime.
fn seen(fs: &Fs, dir: &str, out: &mut Vec<(String, bool, Vec<u8>, u32)>) {
    for (name, is_dir) in fs.list(dir) {
        let path = format!("{dir}/{name}");
        let st = fs.stat(&path).unwrap();
        assert_eq!(st.mode & S_IFMT == S_IFDIR, is_dir);
        out.push((path.clone(), is_dir, fs.contents(&path).unwrap_or(&[]).to_vec(), if is_dir { 0 } else { st.mtime }));
        if is_dir {
            seen(fs, &path, out);
        }
    }
}
fn all(fs: &Fs) -> Vec<(String, bool, Vec<u8>, u32)> {
    let mut out = Vec::new();
    seen(fs, "/data", &mut out);
    out
}

#[test]
fn an_app_s_folders_and_files_survive_a_restart_with_their_times() {
    let dev = disk(512);
    let (mut s, mut fs, mut w) = boot(&dev);
    assert!(all(&fs).is_empty());
    // What apps could not keep before: a folder of their own, files in it,
    // more than 4 KiB, more than 32 files, names over 16 characters.
    fs.mkdir("/data/notes").unwrap();
    fs.mkdir("/data/notes/2026").unwrap();
    write(&mut fs, "/data/notes/2026/november.md", &bytes(1, 70_000), 1_795_000_000);
    write(&mut fs, "/data/notes.index", b"index", 5);
    write(&mut fs, "/data/.settings", b"city=Oslo\n", 6);
    for k in 0..40 {
        write(&mut fs, &format!("/data/notes/a-rather-long-file-name-{k:02}.txt"), &[k; 100], 100 + k as u32);
    }
    assert_eq!(fs.mkdir("/data/notes"), Err(-17));
    settle(&mut s, &mut fs, &mut w);
    let before = all(&fs);
    assert_eq!(before.len(), 45);
    // Power off here: the next start finds all of it.
    let dev = TestDisk::from_image(dev.image());
    let (mut s, mut fs, mut w) = boot(&dev);
    assert_eq!(all(&fs), before);
    assert_eq!(fs.stat("/data/notes/2026/november.md").unwrap().mtime, 1_795_000_000);
    assert!(fs.is_dir("/data/notes/2026") && !fs.is_dir("/data/notes.index"));
    // Rename, remove, rmdir and truncate are kept too.
    fs.rename("/data/notes.index", "/data/notes/index").unwrap();
    fs.unlink("/data/notes/2026/november.md", false).unwrap();
    fs.unlink("/data/notes/2026", true).unwrap();
    fs.writable("/data/.settings", false, false, 9).unwrap().truncate(4);
    assert_eq!(fs.unlink("/data/notes", true), Err(-39), "a folder with files in it stays");
    settle(&mut s, &mut fs, &mut w);
    let before = all(&fs);
    let (_, fs2, mut w2) = boot(&TestDisk::from_image(dev.image()));
    assert_eq!(all(&fs2), before);
    assert_eq!(fs2.contents("/data/.settings"), Some(&b"city"[..]));
    assert_eq!(fs2.contents("/data/notes/index"), Some(&b"index"[..]));
    assert!(fs2.stat("/data/notes.index").is_none() && fs2.stat("/data/notes/2026").is_none());
    w2.c.fsck();
}

#[test]
fn a_name_the_volume_cannot_hold_is_refused_when_it_is_made() {
    let dev = disk(256);
    let (mut s, mut fs, mut w) = boot(&dev);
    let (fits, long) = ("n".repeat(255), "n".repeat(256));
    write(&mut fs, &format!("/data/{fits}"), b"kept", 1);
    assert!(fs.writable(&format!("/data/{long}"), true, true, 1).is_none());
    assert_eq!(fs.mkdir(&format!("/data/{long}")), Err(-36));
    assert_eq!(fs.rename(&format!("/data/{fits}"), &format!("/data/{long}")), Err(-36));
    // A path of more than 1024 bytes below /data, each part short enough.
    let mut deep = String::from("/data");
    for _ in 0..5 {
        deep.push_str(&format!("/{}", "d".repeat(200)));
        fs.mkdir(&deep).unwrap();
    }
    assert_eq!(fs.mkdir(&format!("{deep}/{}", "e".repeat(30))), Err(-36));
    write(&mut fs, &format!("{deep}/{}", "e".repeat(19)), b"deepest", 2);
    // Outside /data nothing is kept, so nothing is refused.
    write(&mut fs, &format!("/tmp/{long}"), b"volatile", 1);
    settle(&mut s, &mut fs, &mut w);
    let before = all(&fs);
    let (_, fs2, _) = boot(&TestDisk::from_image(dev.image()));
    assert_eq!(all(&fs2), before);
    assert_eq!(before.len(), 7);
    assert!(fs2.stat(&format!("/tmp/{long}")).is_none(), "/tmp lasts until the restart");
}

#[test]
fn only_what_is_under_data_is_sent() {
    let dev = disk(256);
    let (mut s, mut fs, mut w) = boot(&dev);
    fs.mkdir("/tmp/x").unwrap();
    write(&mut fs, "/tmp/x/y", b"scratch", 1);
    write(&mut fs, "/database", b"not /data", 1);
    assert!(fs.changed.is_empty());
    settle(&mut s, &mut fs, &mut w);
    assert!(w.tree().is_empty());
    assert_eq!(normalize("/data/app", "../other/./x"), "/data/other/x");
    assert_eq!(normalize("/data/app", "/abs//path/"), "/abs/path");
    assert_eq!(normalize("/", "../.."), "/");
    assert_eq!(normalize("/data", "a/b"), "/data/a/b");
}

#[test]
fn a_file_saved_by_rename_keeps_its_time() {
    let dev = disk(256);
    let (mut s, mut fs, mut w) = boot(&dev);
    write(&mut fs, "/data/notes.tmp", b"draft", 1_795_000_123);
    fs.rename("/data/notes.tmp", "/data/notes").unwrap();
    assert_eq!(fs.stat("/data/notes").unwrap().mtime, 1_795_000_123);
    assert!(fs.moved.contains("/data/notes.tmp"));
    settle(&mut s, &mut fs, &mut w);
    assert!(fs.moved.is_empty(), "the removal is on the volume");
    let (_, fs, _) = boot(&TestDisk::from_image(dev.image()));
    assert_eq!(fs.stat("/data/notes").unwrap().mtime, 1_795_000_123);
    assert!(fs.stat("/data/notes.tmp").is_none());
}
