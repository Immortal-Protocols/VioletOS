//! Storage's /data server (vfs.rs) on the data volume (volume.rs), driven
//! with the requests the Open Runtime sends (vfsproto.rs), on the disk that
//! can lose power. The old files it moves in are written by the real
//! files.rs on the same disk.
use crate::blockdev::BlockDev;
use crate::files_tests::{bytes, Store, OPEN};
use crate::testdisk::*;
use crate::vfs::{Legacy, Vfs, OWNERS};
use crate::vfsproto::*;
use std::collections::BTreeMap;

/// Sectors `first..first + count` of the test disk: what a disk driver's
/// data extent is to Storage.
#[derive(Clone)]
pub struct Extent {
    pub dev: TestDisk,
    pub first: u64,
    pub count: u64,
}
impl BlockDev for Extent {
    fn sectors(&self) -> u64 {
        self.count
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        if lba + (buf.len() / 512) as u64 > self.count {
            return Err(());
        }
        self.dev.read(self.first + lba, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        if lba + (buf.len() / 512) as u64 > self.count {
            return Err(());
        }
        self.dev.write(self.first + lba, buf)
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.dev.flush()
    }
}

/// The 32 slots, read with Storage's own code (legacy.rs, over what
/// files.rs wrote).
pub struct Old(pub crate::block::Disk);
impl Legacy for Old {
    fn file(&mut self, slot: u32, out: &mut [u8]) -> Result<Option<([u8; 16], usize)>, ()> {
        crate::legacy::file(&mut self.0, slot, out)
    }
}

/// The disk of these tests, as QEMU's is laid out in small: the old volume
/// in sectors 0..2048 and the data extent after it.
pub const OLD: usize = 2048;
pub const KEY: [u8; 32] = [0; 32];
pub fn disk(blocks: usize) -> TestDisk {
    TestDisk::from_image(vec![0; (OLD + 8 + blocks * 8) * 512])
}
pub fn extent(dev: &TestDisk) -> Extent {
    Extent { dev: dev.clone(), first: OLD as u64, count: dev.sectors() - OLD as u64 }
}

/// The Open Runtime's end of the window.
pub struct Client {
    pub v: Box<Vfs<Extent>>,
    pub win: Vec<u8>,
    pub epoch: u32,
    pub time: u32,
}
pub type Out = Result<(u64, Vec<u8>, bool), i32>;
impl Client {
    pub fn open(dev: &TestDisk) -> Result<Client, String> {
        let mut v = Box::new(Vfs::new());
        if !v.attach(extent(dev), &KEY, Some(&mut Old(crate::block::Disk::new(dev.clone())))) {
            return Err(String::from_utf8_lossy(v.diag()).into_owned());
        }
        let mut c = Client { v, win: vec![0; WINDOW], epoch: 0, time: 1000 };
        c.hello();
        Ok(c)
    }
    pub fn hello(&mut self) -> u32 {
        let n = encode(&mut self.win, &Req::default(), b"", b"", b"", b"").unwrap();
        self.v.serve(&mut self.win, n);
        let a = answer(&self.win);
        assert_eq!(a.status, 0);
        self.epoch = a.epoch;
        a.epoch
    }
    pub fn call(&mut self, op: u32, flags: u32, offset: u64, mode: u32, path: &str, path2: &str, who: &str, data: &[u8]) -> Out {
        let r = Req { op, flags, offset, len: 0, mode, time: self.time, epoch: self.epoch };
        let n = encode(&mut self.win, &r, path.as_bytes(), path2.as_bytes(), who.as_bytes(), data).expect("request fits");
        self.v.serve(&mut self.win, n);
        let a = answer(&self.win);
        if a.status != 0 {
            return Err(a.status);
        }
        Ok((a.value, self.win[REPLY..REPLY + a.len].to_vec(), a.more))
    }
    /// Replace a file whole, in pieces of `piece` bytes, as `who`.
    pub fn put_as(&mut self, who: &str, path: &str, data: &[u8], piece: usize) -> Result<(), i32> {
        let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(piece).collect() };
        let mut at = 0;
        for (k, c) in chunks.iter().enumerate() {
            let flags = if k == 0 { FIRST } else { 0 } | if k + 1 == chunks.len() { LAST } else { 0 };
            self.call(PUT, flags, at, 0o644, path, "", who, c)?;
            at += c.len() as u64;
        }
        Ok(())
    }
    pub fn put(&mut self, path: &str, data: &[u8]) -> Result<(), i32> {
        self.put_as("", path, data, 16384)
    }
    pub fn mkdir(&mut self, path: &str) -> Result<(), i32> {
        self.call(MKDIR, 0, 0, 0o755, path, "", "", &[]).map(|_| ())
    }
    pub fn remove(&mut self, path: &str) -> Result<(), i32> {
        self.call(REMOVE, 0, 0, 0, path, "", "", &[]).map(|_| ())
    }
    pub fn commit(&mut self) -> Result<(), i32> {
        self.call(COMMIT, 0, 0, 0, "", "", "", &[]).map(|_| ())
    }
    pub fn read_as(&mut self, who: &str, flags: u32, path: &str) -> Result<Vec<u8>, i32> {
        let mut out = Vec::new();
        loop {
            let (n, data, _) = self.call(READ, flags, out.len() as u64, DATA_MAX as u32, path, "", who, &[])?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&data);
        }
    }
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, i32> {
        self.read_as("", 0, path)
    }
    /// The names in a folder with whether each is a folder, as `who`.
    pub fn list_as(&mut self, who: &str, flags: u32, path: &str) -> Result<Vec<(String, bool, u64)>, i32> {
        let (mut out, mut cookie) = (Vec::new(), 0);
        loop {
            let (next, data, more) = self.call(LIST, flags, cookie, 0, path, "", who, &[])?;
            let mut at = 0;
            while let Some((e, to)) = entry(&data, at) {
                out.push((String::from_utf8(e.name.to_vec()).unwrap(), e.mode & 0xf000 == 0x4000, e.size));
                at = to;
            }
            if !more {
                return Ok(out);
            }
            cookie = next;
        }
    }
    /// Every file under /data with its bytes, and every folder (None).
    pub fn tree(&mut self) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut todo = vec![String::new()];
        while let Some(dir) = todo.pop() {
            for (name, is_dir, size) in self.list_as("", 0, &dir).unwrap() {
                let path = if dir.is_empty() { name } else { format!("{dir}/{name}") };
                if is_dir {
                    out.insert(path.clone(), None);
                    todo.push(path);
                } else {
                    let data = self.read(&path).unwrap();
                    assert_eq!(data.len() as u64, size, "{path}");
                    out.insert(path, Some(data));
                }
            }
        }
        out
    }
    /// The whole-filesystem check of violetfs/check.rs.
    pub fn fsck(&mut self) {
        let fs = self.v.fs();
        let s = fs.statfs().expect("statfs");
        let (mut map, mut links) = (vec![0u8; 1 << 16], vec![0u16; s.inodes as usize + 1]);
        let why = fs.check(&mut map, &mut links).map(|_| ()).map_err(|e| format!("{e:?} at violetfs line {}", fs.why));
        assert_eq!(why, Ok(()), "fsck");
    }
    fn statfs(&mut self, who: &str, path: &str) -> [u32; 8] {
        let (_, d, _) = self.call(STATFS, 0, 0, 0, path, "", who, &[]).unwrap();
        core::array::from_fn(|k| le32(&d, 4 * k))
    }
}

fn file(data: &[u8]) -> Option<Vec<u8>> {
    Some(data.to_vec())
}

#[test]
fn files_folders_and_long_names_come_back_after_a_restart() {
    let dev = disk(512);
    let mut c = Client::open(&dev).unwrap();
    assert!(c.tree().is_empty());
    let long = "n".repeat(255);
    let big = bytes(3, 300_000);
    c.put(".settings", b"city=Oslo\n").unwrap();
    c.mkdir("notes").unwrap();
    c.mkdir("notes/deep").unwrap();
    c.put("notes/deep/a.txt", b"hello").unwrap();
    c.put(&format!("notes/{long}"), b"long").unwrap();
    c.put_as("", &long, b"long at the top", 4096).unwrap();
    c.call(PUT, FIRST | LAST | FORCE, 0, 0o644, &"m".repeat(255), "", "", b"as the runtime sends it").unwrap();
    c.put(".big", &big).unwrap();
    c.put("empty", b"").unwrap();
    assert_eq!(c.mkdir("notes"), Err(EEXIST));
    assert_eq!(c.put("notes", b"x"), Err(EISDIR));
    assert_eq!(c.put("nowhere/x", b"x"), Err(ENOENT));
    assert_eq!(c.put(&format!("notes/{long}x"), b"x"), Err(ENAMETOOLONG));
    assert_eq!(c.put("notes/../x", b"x"), Err(EINVAL));
    assert_eq!(c.read(".vfs/owners"), Err(ENOENT));
    assert_eq!(c.remove("notes"), Err(ENOTEMPTY));
    c.commit().unwrap();
    let before = c.tree();
    assert_eq!(before.len(), 9);
    assert_eq!(before[".big"], file(&big));
    assert_eq!(before["notes/deep/a.txt"], file(b"hello"));
    let first = c.epoch;
    drop(c);
    let mut c = Client::open(&dev).unwrap();
    assert!(c.epoch > first, "every mount has its own number");
    assert_eq!(c.tree(), before);
    c.fsck();
}

#[test]
fn what_was_not_committed_is_gone_after_a_cut_and_the_rest_is_whole() {
    let dev = disk(256);
    let mut c = Client::open(&dev).unwrap();
    c.put("a", b"one").unwrap();
    c.commit().unwrap();
    let durable = dev.image();
    c.put("a", b"two").unwrap();
    c.put("b", b"new").unwrap();
    drop(c);
    let mut c = Client::open(&TestDisk::from_image(durable)).unwrap();
    assert_eq!(c.tree(), BTreeMap::from([("a".to_string(), file(b"one"))]));
}

#[test]
fn a_volume_is_formatted_once_and_never_when_it_fails_to_open() {
    let dev = disk(128);
    // Old Android data, say: not zeros, no header.
    dev.0.borrow_mut().image[OLD * 512..].fill(0x5a);
    let mut c = Client::open(&dev).unwrap();
    c.put("keep", b"me").unwrap();
    c.commit().unwrap();
    drop(c);
    // Both superblocks gone (or a cipher with the wrong key under it): the
    // header says a filesystem was made, so nothing is formatted.
    let fs = (OLD + 8) * 512;
    let saved = dev.image();
    dev.0.borrow_mut().image[fs..fs + 8192].fill(0);
    let damaged = dev.image();
    assert_eq!(Client::open(&dev).err().as_deref(), Some("DATA MOUNT"));
    assert_eq!(dev.image(), damaged, "a volume that does not open is left alone");
    // A header that says more sectors than the extent has is not believed.
    let small = TestDisk::from_image(saved[..saved.len() - 64 * 512].to_vec());
    assert_eq!(Client::open(&small).err().as_deref(), Some("DATA HEADER BOUNDS"));
    // With one header copy torn the other still says it.
    let torn = TestDisk::from_image(saved.clone());
    torn.0.borrow_mut().image[OLD * 512..OLD * 512 + 100].fill(0xff);
    assert_eq!(Client::open(&torn).unwrap().read("keep"), Ok(b"me".to_vec()));
    // A header of another cipher (byte 24; the checksum made to match): refused.
    let other = TestDisk::from_image(saved.clone());
    for copy in 0..2 {
        let at = (OLD + copy) * 512;
        let mut h = other.image()[at..at + 512].to_vec();
        h[24] = 2;
        let sum = crate::violetfs::crc32(&h[..508]);
        h[508..].copy_from_slice(&sum.to_le_bytes());
        other.0.borrow_mut().image[at..at + 512].copy_from_slice(&h);
    }
    let image = other.image();
    assert_eq!(Client::open(&other).err().as_deref(), Some("DATA CIPHER"));
    assert_eq!(other.image(), image);
    // A format that was cut before the header was written is done again.
    let cut = TestDisk::from_image(saved);
    cut.0.borrow_mut().image[OLD * 512..(OLD + 2) * 512].fill(0);
    cut.0.borrow_mut().image[fs..fs + 8192].fill(0);
    assert!(Client::open(&cut).unwrap().tree().is_empty());
    assert_eq!(Client::open(&TestDisk::from_image(vec![0; (OLD + 100) * 512])).err().as_deref(), Some("DATA EXTENT SMALL"));
}

#[test]
fn a_cut_while_the_volume_is_first_made_leaves_one_that_opens() {
    let dev = disk(96);
    let images = cuts(&dev, || drop(Client::open(&dev).unwrap()), |cut, c| {
        assert!(c.tree().is_empty(), "cut {cut}");
        c.put("x", b"works").unwrap();
        c.commit().unwrap();
    });
    assert!(images > 50);
}

/// Record what `work` writes, then check every image a power cut could leave.
fn cuts(dev: &TestDisk, work: impl FnOnce(), mut check: impl FnMut(usize, &mut Client)) -> usize {
    dev.record();
    work();
    let s = dev.0.borrow();
    sweep(&s.base, &s.log, |cut, image| {
        let mut c = Client::open(&TestDisk::from_image(image)).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        check(cut, &mut c);
        c.fsck();
    })
}

#[test]
fn a_replaced_file_is_the_old_one_or_the_new_one_after_any_cut() {
    let dev = disk(256);
    let (v1, v2) = (bytes(1, 30_000), bytes(2, 40_000));
    let mut c = Client::open(&dev).unwrap();
    c.mkdir("app").unwrap();
    c.put("app/state", &v1).unwrap();
    c.commit().unwrap();
    let mut seen = [0; 2];
    let images = cuts(
        &dev,
        || {
            c.put("app/state", &v2).unwrap();
            c.commit().unwrap();
        },
        |cut, c| {
            let t = c.tree();
            assert_eq!(t.len(), 2, "cut {cut}: nothing of a replacement in progress shows: {:?}", t.keys());
            let got = t["app/state"].clone().unwrap();
            assert!(got == v1 || got == v2, "cut {cut}: a file that is neither the old nor the new one");
            seen[(got == v2) as usize] += 1;
            // The volume takes new work.
            c.put("app/state", b"after").unwrap();
            c.commit().unwrap();
        },
    );
    assert!(images > 100 && seen[0] > 0 && seen[1] > 0, "{images} images, {seen:?}");
}

#[test]
fn a_rename_as_the_runtime_saves_it_never_loses_both_names() {
    let dev = disk(256);
    let data = bytes(9, 9000);
    let mut c = Client::open(&dev).unwrap();
    c.put("notes.draft", &data).unwrap();
    c.commit().unwrap();
    cuts(
        &dev,
        || {
            // writeback.rs: the new name first, then the old one goes.
            c.put("notes.final", &data).unwrap();
            c.remove("notes.draft").unwrap();
            c.commit().unwrap();
        },
        |cut, c| {
            let t = c.tree();
            assert!(!t.is_empty() && t.values().all(|d| *d == file(&data)), "cut {cut}: {:?}", t.keys());
        },
    );
    // The server's own rename is one step: exactly one of the names.
    let mut c = Client::open(&dev).unwrap();
    cuts(
        &dev,
        || {
            c.call(RENAME, 0, 0, 0, "notes.final", "notes.last", "", &[]).unwrap();
            c.commit().unwrap();
        },
        |cut, c| assert_eq!(c.tree().len(), 1, "cut {cut}"),
    );
}

fn old_files(dev: &TestDisk) -> BTreeMap<String, Vec<u8>> {
    some_old_files(dev, 7)
}
fn some_old_files(dev: &TestDisk, count: usize) -> BTreeMap<String, Vec<u8>> {
    let mut s = Store::new(dev);
    let files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
        ("networks".to_string(), bytes(1, 4096)),
        (".settings".to_string(), b"city=Lagos\n".to_vec()),
        (".lock".to_string(), bytes(2, 77)),
        ("notes".to_string(), bytes(3, 1500)),
        ("notes.index".to_string(), bytes(4, 40)),
        ("relay".to_string(), Vec::new()),
        ("sixteen-chars-ab".to_string(), bytes(5, 513)),
    ])
    .into_iter()
    .rev()
    .take(count)
    .collect();
    for (name, data) in &files {
        s.replace(OPEN, name, data).unwrap();
    }
    files
}

#[test]
fn the_old_files_move_in_once_and_whole() {
    let dev = disk(128);
    let old = old_files(&dev);
    let slots = dev.image()[..OLD * 512].to_vec();
    let mut c = Client::open(&dev).unwrap();
    let want: BTreeMap<String, Option<Vec<u8>>> = old.iter().map(|(n, d)| (n.clone(), Some(d.clone()))).collect();
    assert_eq!(c.tree(), want);
    assert_eq!(dev.image()[..OLD * 512], slots[..], "the slots are only read");
    // Owners follow the names: notes and notes.index share one, dot names are the system's.
    let owner = |c: &mut Client, p: &str| le32(&c.call(STAT, 0, 0, 0, p, "", "", &[]).unwrap().1, 4);
    assert_eq!(owner(&mut c, ".settings"), 0);
    assert_eq!(owner(&mut c, ".lock"), 0);
    assert_ne!(owner(&mut c, "notes"), 0);
    assert_eq!(owner(&mut c, "notes"), owner(&mut c, "notes.index"));
    assert_ne!(owner(&mut c, "notes"), owner(&mut c, "networks"));
    // Changes made since are not undone by a second look at the slots.
    c.put("networks", b"changed").unwrap();
    c.remove("relay").unwrap();
    c.commit().unwrap();
    drop(c);
    let mut c = Client::open(&dev).unwrap();
    assert_eq!(c.read("networks"), Ok(b"changed".to_vec()));
    assert_eq!(c.read("relay"), Err(ENOENT));
    c.fsck();
}

#[test]
fn a_cut_during_the_move_leaves_every_old_file_or_no_volume_yet() {
    let dev = disk(128);
    let old = some_old_files(&dev, 3);
    let want: BTreeMap<String, Option<Vec<u8>>> = old.iter().map(|(n, d)| (n.clone(), Some(d.clone()))).collect();
    let images = cuts(&dev, || drop(Client::open(&dev).unwrap()), |cut, c| {
        // Whatever the cut left, opening finished the move before serving.
        assert_eq!(c.tree(), want, "cut {cut}");
    });
    assert!(images > 100, "{images}");
}

#[test]
fn an_old_file_that_cannot_be_read_stops_the_move_and_nothing_is_served() {
    let dev = disk(128);
    let old = old_files(&dev);
    // Damage a data sector of one stored file: its checksum fails.
    let image = dev.image();
    let at = (0..OLD).find(|s| image[s * 512..(s + 1) * 512] == old["networks"][..512]).expect("the file's first sector");
    dev.0.borrow_mut().image[at * 512] ^= 1;
    let mut v: Box<Vfs<Extent>> = Box::new(Vfs::new());
    assert!(!v.attach(extent(&dev), &KEY, Some(&mut Old(crate::block::Disk::new(dev.clone())))));
    assert_eq!(v.diag(), b"DATA MIGRATE");
    let mut win = vec![0; WINDOW];
    let n = encode(&mut win, &Req::default(), b"", b"", b"", b"").unwrap();
    v.serve(&mut win, n);
    assert_eq!(answer(&win).status, ENODEV);
    // Readable again: the move is done from the start, and whole.
    dev.0.borrow_mut().image[at * 512] ^= 1;
    let mut c = Client::open(&dev).unwrap();
    assert_eq!(c.tree().len(), old.len());
    assert_eq!(c.read("networks"), Ok(old["networks"].clone()));
}

#[test]
fn an_app_is_held_to_its_own_names_and_its_mode_bits() {
    let dev = disk(256);
    let mut c = Client::open(&dev).unwrap();
    c.put(".grants", b"secret").unwrap();
    c.put("networks", b"psk").unwrap();
    c.call(MKDIR, 0, 0, 0o700, "notes", "", "notes", &[]).unwrap();
    c.put_as("notes", "notes/a", b"mine", 4096).unwrap();
    c.put_as("notes", "notes.cache", b"c", 4096).unwrap();
    c.put_as("music", "music", b"theirs", 4096).unwrap();
    // Reading: its own names only; others and the system's look absent.
    assert_eq!(c.read_as("notes", 0, "notes/a"), Ok(b"mine".to_vec()));
    assert_eq!(c.read_as("notes", 0, "music"), Err(ENOENT));
    assert_eq!(c.read_as("notes", 0, ".grants"), Err(ENOENT));
    assert_eq!(c.read_as("notes", 0, "networks"), Err(ENOENT));
    assert_eq!(c.read_as("notes", 0, "notesx"), Err(ENOENT), "a longer name is not notes'");
    assert_eq!(c.read_as("notes", SHARED, ".grants"), Err(ENOENT), "a dot name is never shared");
    assert_eq!(c.read_as("notes", SHARED, "networks"), Ok(b"psk".to_vec()), "what the policy shares");
    assert_eq!(c.read_as("no.tes", 0, "no"), Err(EACCES), "not an app's name");
    // Writing, removing, renaming, listing.
    assert_eq!(c.put_as("notes", "music", b"x", 4096), Err(ENOENT));
    assert_eq!(c.put_as("notes", "music.x", b"x", 4096), Err(ENOENT));
    assert_eq!(c.put_as("notes", ".hidden", b"x", 4096), Err(ENOENT));
    assert_eq!(c.call(REMOVE, 0, 0, 0, "music", "", "notes", &[]).err(), Some(ENOENT));
    assert_eq!(c.call(RENAME, 0, 0, 0, "notes/a", "music", "notes", &[]).err(), Some(ENOENT));
    assert_eq!(c.call(RENAME, SHARED, 0, 0, "notes/a", "music", "notes", &[]).err(), Some(EXDEV), "a file does not change owner");
    assert_eq!(c.call(MKDIR, 0, 0, 0o755, "", "", "notes", &[]).err(), Some(EACCES));
    let names = |l: Vec<(String, bool, u64)>| l.into_iter().map(|e| e.0).collect::<Vec<_>>();
    assert_eq!(names(c.list_as("notes", 0, "").unwrap()), ["notes", "notes.cache"]);
    assert_eq!(names(c.list_as("", 0, "").unwrap()).len(), 5);
    assert_eq!(c.list_as("notes", 0, "music").err(), Some(ENOENT));
    // The mode bits of the owner.
    c.call(CHMOD, 0, 0, 0o400, "notes/a", "", "notes", &[]).unwrap();
    assert_eq!(c.put_as("notes", "notes/a", b"again", 4096), Err(EACCES));
    assert_eq!(c.call(WRITE, 0, 0, 0, "notes/a", "", "notes", b"z").err(), Some(EACCES));
    assert_eq!(c.call(TRUNCATE, 0, 0, 0, "notes/a", "", "notes", &[]).err(), Some(EACCES));
    assert_eq!(c.read_as("notes", 0, "notes/a"), Ok(b"mine".to_vec()));
    c.call(CHMOD, 0, 0, 0o200, "notes/a", "", "notes", &[]).unwrap();
    assert_eq!(c.read_as("notes", 0, "notes/a"), Err(EACCES));
    c.call(CHMOD, 0, 0, 0o500, "notes", "", "notes", &[]).unwrap();
    assert_eq!(c.put_as("notes", "notes/b", b"b", 4096), Err(EACCES), "a folder without write takes no names");
    assert_eq!(c.call(REMOVE, 0, 0, 0, "notes/a", "", "notes", &[]).err(), Some(EACCES));
    assert_eq!(c.call(CHMOD, SHARED, 0, 0o777, "networks", "", "notes", &[]).err(), Some(EACCES), "sharing is not owning");
    assert_eq!(c.call(QUOTA, 0, 1 << 20, 1 << 20, "notes", "", "notes", &[]).err(), Some(EACCES), "an app sets no limits");
    // The runtime itself is not held to any of it.
    assert_eq!(c.read("notes/a"), Ok(b"mine".to_vec()));
    c.put("notes/a", b"system").unwrap();
    c.fsck();
}

#[test]
fn an_app_gets_edquot_at_its_share_and_enospc_at_the_reserve() {
    let dev = disk(512);
    let mut c = Client::open(&dev).unwrap();
    let s = c.statfs("", "");
    let (blocks, share, reserve) = (s[0], s[0] / 4, s[0] / 16);
    assert_eq!(c.statfs("a", "")[5], share, "the default limit shows before anything is written");
    // One app fills its share: EDQUOT, with what it has intact.
    let mut n = 0;
    let err = loop {
        match c.put_as("a", &format!("a.{n}"), &bytes(n as u8, 16384), 8192) {
            Ok(()) => n += 1,
            Err(e) => break e,
        }
    };
    assert_eq!(err, EDQUOT);
    let used = c.statfs("a", "");
    assert!(used[4] <= share && used[4] + 5 > share, "{used:?} of {share}");
    assert_eq!(used[1], share - used[4], "statfs shows the app what is left of its share");
    assert_eq!(c.read("a.0"), Ok(bytes(0, 16384)));
    assert_eq!(c.list_as("a", 0, "").unwrap().len(), n);
    // Another app is not affected, and the system has no limit.
    c.put_as("b", "b", &bytes(7, 8192), 8192).unwrap();
    c.put(".settings", &bytes(8, 8192)).unwrap();
    // Removing gives the room back.
    c.call(REMOVE, 0, 0, 0, "a.0", "", "a", &[]).unwrap();
    c.put_as("a", "a.again", &bytes(9, 8192), 8192).unwrap();
    // A limit set by the runtime replaces the default.
    c.call(QUOTA, 0, (share * 2) as u64, 1000, "a", "", "", &[]).unwrap();
    c.put_as("a", "a.more", &bytes(1, 8192), 8192).unwrap();
    // Apps together cannot take the reserve; the system still can write.
    let mut owner = 0;
    let err = 'full: loop {
        owner += 1;
        for k in 0.. {
            match c.put_as(&format!("o{owner}"), &format!("o{owner}.{k}"), &bytes(k as u8, 16384), 16384) {
                Ok(()) => {}
                Err(EDQUOT) => break,
                Err(e) => break 'full e,
            }
        }
        assert!(owner < 40);
    };
    assert_eq!(err, ENOSPC);
    let free = c.statfs("", "")[1];
    assert!(free >= reserve && free < reserve + 8, "{free} free, reserve {reserve} of {blocks}");
    assert_eq!(c.statfs("o1", "")[1], 0);
    c.put(".settings", &bytes(3, 40_000)).unwrap();
    c.commit().unwrap();
    assert_eq!(c.read(".settings"), Ok(bytes(3, 40_000)));
    c.fsck();
}

#[test]
fn an_owner_that_holds_nothing_gives_its_number_back() {
    let dev = disk(512);
    let mut c = Client::open(&dev).unwrap();
    // More owners over time than there are numbers, never many at once.
    for k in 0..OWNERS * 2 + 5 {
        let name = format!("app{k}");
        c.put_as(&name, &name, b"x", 4096).unwrap();
        c.call(MKDIR, 0, 0, 0o700, &format!("{name}.d"), "", &name, &[]).unwrap();
        c.remove(&name).unwrap();
        c.remove(&format!("{name}.d")).unwrap();
    }
    // All at once: the table has an end, and says so.
    let mut made = 0;
    let err = loop {
        let name = format!("held{made}");
        match c.put_as(&name, &name, b"", 4096) {
            Ok(()) => made += 1,
            Err(e) => break e,
        }
    };
    assert_eq!((made, err), (OWNERS, ENOSPC));
    c.put(".system", b"still fine").unwrap();
    c.remove("held3").unwrap();
    c.put_as("late", "late", b"", 4096).unwrap();
    c.commit().unwrap();
    drop(c);
    // The numbers survive a restart with their owners.
    let mut c = Client::open(&dev).unwrap();
    let owner = |c: &mut Client, p: &str| le32(&c.call(STAT, 0, 0, 0, p, "", "", &[]).unwrap().1, 4);
    let (late, other) = (owner(&mut c, "late"), owner(&mut c, "held4"));
    c.put_as("late", "late.2", b"", 4096).unwrap();
    assert_eq!(owner(&mut c, "late.2"), late);
    assert_ne!(late, other);
    c.fsck();
}

#[test]
fn writes_in_place_truncate_stat_and_listing_in_pages() {
    let dev = disk(4096);
    let mut c = Client::open(&dev).unwrap();
    assert_eq!(c.call(WRITE, 0, 0, 0o600, ".f", "", "", b"abc").err(), Some(ENOENT));
    assert_eq!(c.call(WRITE, CREATE, 0, 0o600, ".f", "", "", b"abcdef").unwrap().0, 6);
    assert_eq!(c.call(WRITE, CREATE | EXCL, 0, 0o600, ".f", "", "", b"x").err(), Some(EEXIST));
    c.call(WRITE, 0, 4, 0, ".f", "", "", b"XYZ").unwrap();
    assert_eq!(c.read(".f"), Ok(b"abcdXYZ".to_vec()));
    c.call(TRUNCATE, 0, 2, 0, ".f", "", "", &[]).unwrap();
    c.time = 4242;
    c.call(WRITE, 0, 5, 0, ".f", "", "", b"!").unwrap();
    assert_eq!(c.read(".f"), Ok(b"ab\0\0\0!".to_vec()));
    let (size, st, _) = c.call(STAT, 0, 0, 0, ".f", "", "", &[]).unwrap();
    assert_eq!((size, le32(&st, 0), le64(&st, 16)), (6, 0o100600, 4242));
    c.call(WRITE, TRUNC, 0, 0, ".f", "", "", b"new").unwrap();
    assert_eq!(c.read(".f"), Ok(b"new".to_vec()));
    // A folder with more names than one reply holds.
    c.mkdir(".many").unwrap();
    let long = "x".repeat(200);
    for k in 0..400 {
        c.call(WRITE, CREATE, 0, 0o600, &format!(".many/{k:03}{long}"), "", "", &[]).unwrap();
    }
    let (_, first, more) = c.call(LIST, 0, 0, 0, ".many", "", "", &[]).unwrap();
    assert!(more && first.len() <= DATA_MAX);
    let all = c.list_as("", 0, ".many").unwrap();
    let mut names: Vec<_> = all.iter().map(|e| e.0.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 400);
    assert_eq!(c.call(LIST, 0, 7, 0, ".many", "", "", &[]).err(), Some(EINVAL), "a made-up cookie");
    c.fsck();
}

#[test]
fn after_a_device_error_the_volume_is_as_last_committed_and_says_so() {
    let dev = disk(256);
    let mut c = Client::open(&dev).unwrap();
    c.put("a", b"kept").unwrap();
    c.commit().unwrap();
    c.put("a", b"lost").unwrap();
    dev.fail_at(Some(0));
    assert_eq!(c.commit(), Err(EIO));
    assert_eq!(c.read("a"), Err(EIO), "still failing: no mount");
    dev.fail_at(None);
    // Mounted again: the request is not done, and the caller is told that
    // what it believed stored may not be.
    let before = c.epoch;
    assert_eq!(c.read("a"), Err(ESTALE));
    assert!(c.hello() > before);
    assert_eq!(c.read("a"), Ok(b"kept".to_vec()));
    c.put("a", b"again").unwrap();
    c.commit().unwrap();
    c.fsck();
}

#[test]
fn a_replacement_out_of_step_is_dropped_and_shows_nothing() {
    let dev = disk(256);
    let mut c = Client::open(&dev).unwrap();
    c.put("a", b"old").unwrap();
    c.call(PUT, FIRST, 0, 0o644, "a", "", "", b"par").unwrap();
    assert_eq!(c.call(PUT, LAST, 9, 0o644, "a", "", "", b"t").err(), Some(EINVAL), "a gap");
    assert_eq!(c.call(PUT, LAST, 3, 0o644, "a", "", "", b"t").err(), Some(EINVAL), "nothing to go on with");
    c.call(PUT, FIRST, 0, 0o644, "a", "", "", b"par").unwrap();
    assert_eq!(c.call(PUT, LAST, 3, 0o644, "b", "", "", b"t").err(), Some(EINVAL), "another file's piece");
    assert_eq!(c.tree(), BTreeMap::from([("a".to_string(), file(b"old"))]));
    // Requests that are not requests.
    let mut win = vec![0u8; WINDOW];
    let n = encode(&mut win, &Req { op: READ, epoch: c.epoch, ..Req::default() }, b"a", b"", b"", b"").unwrap();
    for bad in [0, HEAD - 1, n - 1, n + 1, WINDOW + 1] {
        c.v.serve(&mut win, bad);
        assert_eq!(answer(&win).status, EINVAL);
        encode(&mut win, &Req { op: READ, epoch: c.epoch, ..Req::default() }, b"a", b"", b"", b"").unwrap();
    }
    win[32..34].copy_from_slice(&5000u16.to_le_bytes());
    c.v.serve(&mut win, n);
    assert_eq!(answer(&win).status, EINVAL);
    assert_eq!(c.call(99, 0, 0, 0, "a", "", "", &[]).err(), Some(EINVAL));
    c.fsck();
}

/// The smallest volume there is: the unused end of the first megabyte
/// (extent.rs), on a disk whose 32 slots are all full of 4 KiB files.
#[test]
fn the_first_megabyte_holds_the_old_files_and_the_volume_they_move_into() {
    let dev = TestDisk::from_image(vec![0; 2048 * 512]);
    let (first, count) = crate::extent::qemu(2048);
    assert_eq!((first, count), (crate::extent::SMALL_FIRST, 928));
    let mut s = Store::new(&dev);
    let mut old = BTreeMap::new();
    for k in 0..32u8 {
        let name = if k < 4 { format!(".sys{k}") } else { format!("app{}.{k}", k % 5) };
        let data = bytes(k, 4096);
        s.replace(OPEN, &name, &data).unwrap();
        old.insert(name, Some(data));
    }
    let slots = dev.image()[..first * 512].to_vec();
    assert!(slots[1111 * 512..].iter().any(|&b| b != 0), "the last slot ends just below the volume");
    let mut v = Box::new(Vfs::new());
    let raw = Extent { dev: dev.clone(), first: first as u64, count: count as u64 };
    assert!(v.attach(raw, &KEY, Some(&mut Old(crate::block::Disk::new(dev.clone())))), "{}", String::from_utf8_lossy(v.diag()));
    let mut c = Client { v, win: vec![0; WINDOW], epoch: 0, time: 5 };
    c.hello();
    assert_eq!(c.tree(), old);
    // It takes new work, and nothing it does reaches the slots below it.
    c.put(".sys0", &bytes(99, 9000)).unwrap();
    c.mkdir("app1").unwrap();
    c.put("app1/more", &bytes(98, 5000)).unwrap();
    c.remove("app2.7").unwrap();
    c.commit().unwrap();
    c.fsck();
    assert_eq!(dev.image()[..first * 512], slots[..]);
    let s = c.statfs("", "");
    assert!(s[0] >= 90 && s[1] >= 40, "{s:?}: blocks and free blocks of the smallest volume");
}

#[test]
fn a_file_can_be_replaced_by_one_as_large_within_the_share() {
    let dev = disk(512);
    let mut c = Client::open(&dev).unwrap();
    let share = c.statfs("a", "")[5];
    // Most of the share in one file: well over half of it.
    let big = (share as usize - 8) * 4096;
    c.put_as("a", "a.db", &bytes(1, big), 16384).unwrap();
    let used = c.statfs("a", "")[4];
    // Rewritten with one byte changed: the old copy is not counted twice.
    let mut again = bytes(1, big);
    again[77] ^= 1;
    c.put_as("a", "a.db", &again, 16384).unwrap();
    assert_eq!(c.read("a.db"), Ok(again.clone()));
    assert_eq!(c.statfs("a", "")[4], used, "charged once, to the app");
    let owner = le32(&c.call(STAT, 0, 0, 0, "a.db", "", "", &[]).unwrap().1, 4);
    assert_ne!(owner, 0, "the replacement is the app's, not the system's");
    // What would go over the share still does not fit, and the old file stays.
    assert_eq!(c.put_as("a", "a.db", &bytes(2, big + 16 * 4096), 16384), Err(EDQUOT));
    assert_eq!(c.read("a.db"), Ok(again));
    assert_eq!(c.statfs("a", "")[4], used);
    // A new file next to it still counts in full.
    assert_eq!(c.put_as("a", "a.copy", &bytes(3, big), 16384), Err(EDQUOT));
    // A cut in the middle of a replacement leaves nothing charged for it.
    c.commit().unwrap();
    c.call(PUT, FIRST, 0, 0o644, "a.db", "", "a", &bytes(4, 40000)).unwrap();
    c.call(COMMIT, 0, 0, 0, "", "", "", &[]).unwrap();
    let mut c = Client::open(&TestDisk::from_image(dev.image())).unwrap();
    assert_eq!(c.statfs("a", "")[4], used);
    c.fsck();
}

#[test]
fn saved_wi_fi_networks_and_the_relay_choice_are_the_system_s() {
    let dev = disk(512);
    let mut c = Client::open(&dev).unwrap();
    // Apps fill the volume down to the reserve.
    let mut owner = 0;
    'full: loop {
        owner += 1;
        for k in 0.. {
            match c.put_as(&format!("o{owner}"), &format!("o{owner}.{k}"), &bytes(k as u8, 16384), 16384) {
                Ok(()) => {}
                Err(EDQUOT) => break,
                Err(_) => break 'full,
            }
        }
    }
    // Settings saves a newly joined network and the relay choice regardless.
    c.call(PUT, FIRST | LAST | SHARED, 0, 0o644, "networks", "", "settings", &bytes(5, 6000)).unwrap();
    c.call(PUT, FIRST | LAST | SHARED, 0, 0o644, "relay", "", "settings", b"on\n").unwrap();
    for name in ["networks", "relay"] {
        assert_eq!(le32(&c.call(STAT, 0, 0, 0, name, "", "", &[]).unwrap().1, 4), 0, "{name}");
    }
    // An app's name that only starts the same is still the app's.
    assert_eq!(crate::vfs::owner_key(b"networks.bak"), Some(&b"networks"[..]));
    assert_eq!(crate::vfs::owner_key(b"agent"), Some(&b"agent"[..]));
    c.fsck();
}

#[test]
fn an_old_file_the_move_cannot_copy_whole_stops_it() {
    for bad in [".", "..", ".vfs"] {
        let dev = disk(128);
        let mut s = Store::new(&dev);
        s.replace(OPEN, ".lock", b"passcode").unwrap();
        s.replace(OPEN, bad, b"x").unwrap();
        let mut v: Box<Vfs<Extent>> = Box::new(Vfs::new());
        assert!(!v.attach(extent(&dev), &KEY, Some(&mut Old(crate::block::Disk::new(dev.clone())))), "{bad}");
        assert_eq!(v.diag(), b"DATA MIGRATE", "{bad}: not served without it");
    }
}

#[test]
fn a_request_without_a_time_does_not_date_folders_1970() {
    let dev = disk(128);
    let mut c = Client::open(&dev).unwrap();
    c.time = 5000;
    c.mkdir("d").unwrap();
    c.put("d/a", b"1").unwrap();
    c.put("d/b", b"2").unwrap();
    c.time = 0;
    c.remove("d/a").unwrap();
    c.commit().unwrap();
    let mtime = |c: &mut Client, p: &str| le64(&c.call(STAT, 0, 0, 0, p, "", "", &[]).unwrap().1, 16);
    assert_eq!(mtime(&mut c, "d"), 5000);
    assert_eq!(mtime(&mut c, "d/b"), 5000);
}
