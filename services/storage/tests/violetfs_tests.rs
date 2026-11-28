use crate::testdisk::*;
use crate::violetfs::*;
use std::collections::BTreeMap;

type F = Fs<TestDisk>;

fn open(disk: &TestDisk) -> Box<F> {
    Box::new(Fs::new(disk.clone()))
}
fn fresh(blocks: usize, inodes: u32, slots: u32) -> (Box<F>, TestDisk) {
    let disk = TestDisk::new(blocks * 8);
    let mut fs = open(&disk);
    fs.format(inodes, slots).unwrap();
    (fs, disk)
}
fn fsck(fs: &mut F) -> Result<Report, String> {
    let blocks = fs.device().0.borrow().image.len() / 4096;
    let inodes = fs.statfs().map_err(|e| format!("statfs {e:?}"))?.inodes as usize;
    let (mut map, mut links) = (vec![0u8; blocks / 8 + 1], vec![0u16; inodes + 1]);
    fs.check(&mut map, &mut links).map_err(|e| format!("fsck: {e:?} at violetfs line {}", fs.why))
}
fn pat(seed: u64, off: u64, len: usize) -> Vec<u8> {
    (off..off + len as u64).map(|i| (i.wrapping_mul(31) ^ (i >> 8) ^ seed.wrapping_mul(0x9d)) as u8).collect()
}

#[derive(Clone, PartialEq, Debug)]
enum Node {
    Dir { owner: u32, mode: u16 },
    File { owner: u32, mode: u16, nlink: u16, data: Vec<u8> },
}
type Tree = BTreeMap<String, Node>;

fn slurp(fs: &mut F, ino: u32) -> Result<Vec<u8>, Error> {
    let size = fs.stat(ino)?.size as usize;
    let mut data = vec![0x55; size];
    let n = fs.read(ino, 0, &mut data)?;
    assert_eq!(n, size);
    Ok(data)
}
fn walk(fs: &mut F, dir: u32, prefix: &str, out: &mut Tree) -> Result<(), Error> {
    let mut name = [0u8; NAME_MAX];
    let mut cookie = 0;
    while let Some(e) = fs.readdir(dir, cookie, &mut name)? {
        cookie = e.next;
        let path = format!("{prefix}/{}", String::from_utf8_lossy(&name[..e.len]));
        let n = fs.stat(e.ino)?;
        assert_eq!((n.mode >> 12) as u8, e.kind);
        let node = if n.is_dir() {
            Node::Dir { owner: n.owner, mode: n.mode }
        } else {
            Node::File { owner: n.owner, mode: n.mode, nlink: n.nlink, data: slurp(fs, e.ino)? }
        };
        assert!(out.insert(path.clone(), node).is_none(), "{path} is in its directory twice");
        if n.is_dir() {
            walk(fs, e.ino, &path, out)?;
        }
    }
    Ok(())
}
fn dump(fs: &mut F) -> Result<Tree, Error> {
    let mut out = Tree::new();
    walk(fs, ROOT, "", &mut out)?;
    Ok(out)
}
/// The directory and last name of a path.
fn at<'a>(fs: &mut F, path: &'a str) -> Result<(u32, &'a [u8]), Error> {
    let (dirs, name) = path.rsplit_once('/').unwrap();
    let mut dir = ROOT;
    for part in dirs.split('/').filter(|p| !p.is_empty()) {
        dir = fs.lookup(dir, part.as_bytes())?;
    }
    Ok((dir, name.as_bytes()))
}
fn ino(fs: &mut F, path: &str) -> Result<u32, Error> {
    let (dir, name) = at(fs, path)?;
    fs.lookup(dir, name)
}
fn mk(fs: &mut F, path: &str, mode: u16, owner: u32) -> Result<u32, Error> {
    let (dir, name) = at(fs, path)?;
    fs.create(dir, name, mode, owner)
}
fn mv(fs: &mut F, from: &str, to: &str) -> Result<(), Error> {
    let (d1, n1) = at(fs, from)?;
    let (d2, n2) = at(fs, to)?;
    fs.rename(d1, n1, d2, n2)
}
fn rm(fs: &mut F, path: &str) -> Result<(), Error> {
    let (dir, name) = at(fs, path)?;
    fs.unlink(dir, name)
}
fn rmdir(fs: &mut F, path: &str) -> Result<(), Error> {
    let (dir, name) = at(fs, path)?;
    fs.rmdir(dir, name)
}
fn put(fs: &mut F, path: &str, off: u64, data: &[u8]) -> Result<usize, Error> {
    let i = ino(fs, path)?;
    fs.write(i, off, data)
}

#[test]
fn crc_is_crc32() {
    assert_eq!(crc32(b"123456789"), 0xcbf43926);
    assert_eq!(crc32(b"123456789"), crate::journal::crc32(b"123456789"));
}

#[test]
fn format_mount_and_basic_files() {
    let (mut fs, disk) = fresh(256, 0, 0);
    let s = fs.statfs().unwrap();
    assert_eq!(s.inodes, 31);
    assert_eq!(s.free_inodes, 30);
    fs.now = 7;
    let d = fs.create(ROOT, b"docs", S_DIR | 0o750, 3).unwrap();
    let f = fs.create(d, b"note.txt", S_FILE | 0o640, 3).unwrap();
    assert_eq!(fs.write(f, 0, b"hello").unwrap(), 5);
    assert_eq!(fs.write(f, 5, b", world").unwrap(), 7);
    assert_eq!(fs.create(d, b"note.txt", S_FILE, 3), Err(Error::Exists));
    assert_eq!(fs.create(d, b"a/b", S_FILE, 3), Err(Error::Invalid));
    assert_eq!(fs.create(d, b"..", S_FILE, 3), Err(Error::Invalid));
    assert_eq!(fs.create(f, b"x", S_FILE, 3), Err(Error::NotDir));
    assert_eq!(fs.lookup(d, b"nope"), Err(Error::NotFound));
    fs.commit().unwrap();
    // Mounted again from the medium, everything is there.
    let mut fs = open(&disk);
    fs.mount().unwrap();
    let d = fs.lookup(ROOT, b"docs").unwrap();
    let f = fs.lookup(d, b"note.txt").unwrap();
    assert_eq!(slurp(&mut fs, f).unwrap(), b"hello, world");
    let n = fs.stat(f).unwrap();
    assert_eq!((n.mode, n.owner, n.nlink, n.mtime, n.size), (S_FILE | 0o640, 3, 1, 7, 12));
    assert_eq!(fs.stat(d).unwrap().parent, ROOT);
    let r = fsck(&mut fs).unwrap();
    assert_eq!((r.files, r.dirs), (1, 2));
    assert_eq!(fs.usage(3).unwrap().inodes, 2);
    assert_eq!(fs.usage(3).unwrap().blocks, 2); // one directory block, one of data
}

// ---- power cuts ----

/// Sector of the journal record (block 2).
const RECORD: u64 = 16;

/// Power can fail again while `mount` is recovering: mount the image with the
/// device logged, and hand on every image a cut during that could leave.
fn cut_during_recovery(image: Vec<u8>, check: &mut dyn FnMut(Vec<u8>)) {
    let disk = TestDisk::from_image(image);
    disk.record();
    let _ = open(&disk).mount();
    let base = disk.0.borrow().base.clone();
    let log = std::mem::take(&mut disk.0.borrow_mut().log);
    sweep(&base, &log, |_, image| check(image));
}

/// Run `work` on a new filesystem, logging the device. `work` calls its
/// second argument wherever it commits; the tree is noted there. Then cut
/// the power at every write and flush, in every way testdisk.rs models, and
/// require that each resulting medium mounts, passes the full check, holds
/// exactly the tree of the last commit that became durable before the cut or
/// of the one in flight, and can be written again. `fault` breaks the
/// filesystem on purpose (see `Fs::fault`). Returns the images tried.
fn cut_everywhere(blocks: usize, inodes: u32, slots: u32, fault: u8, work: impl Fn(&mut F, &mut dyn FnMut(&mut F))) -> Result<usize, String> {
    let (mut fs, disk) = fresh(blocks, inodes, slots);
    fs.fault = fault;
    disk.record();
    // (the log index of the flush that made it durable, the tree)
    let mut states = vec![(0usize, dump(&mut fs).unwrap())];
    let mut commits = fs.commits;
    let mut mark = |fs: &mut F| {
        assert_eq!(fs.commits, commits, "the journal filled between commits: the workload is too big for this test");
        let from = disk.log_len();
        fs.commit().unwrap();
        if fs.commits == commits {
            return;
        }
        commits = fs.commits;
        let s = disk.0.borrow();
        let record = from + s.log[from..].iter().position(|op| matches!(op, Op::Write { lba: RECORD, .. })).unwrap();
        let durable = record + s.log[record..].iter().position(|op| matches!(op, Op::Flush)).unwrap();
        drop(s);
        states.push((durable, dump(fs).unwrap()));
    };
    work(&mut fs, &mut mark);
    mark(&mut fs);
    let base = disk.0.borrow().base.clone();
    let log = std::mem::take(&mut disk.0.borrow_mut().log);
    let mut failure = None;
    let mut extra = 0;
    let verify = |cut: usize, image: Vec<u8>| -> Result<(), String> {
        let done = states.iter().skip(1).filter(|s| s.0 < cut).count();
        {
            let disk = TestDisk::from_image(image);
            let mut fs = open(&disk);
            fs.mount().map_err(|e| format!("mount: {e:?} at violetfs line {}", fs.why))?;
            fsck(&mut fs)?;
            let tree = dump(&mut fs).map_err(|e| format!("reading the tree: {e:?}"))?;
            if tree != states[done].1 && states.get(done + 1).map_or(true, |s| tree != s.1) {
                return Err(format!("the tree is neither that of commit {done} nor of the next"));
            }
            // It must also still work.
            let f = fs.create(ROOT, b"after-the-cut", S_FILE | 0o600, 1).map_err(|e| format!("create after: {e:?}"))?;
            let data = pat(9, 0, 6000);
            fs.write(f, 100, &data).map_err(|e| format!("write after: {e:?}"))?;
            fs.commit().map_err(|e| format!("commit after: {e:?}"))?;
            fsck(&mut fs)?;
            let mut fs = open(&disk);
            fs.mount().map_err(|e| format!("second mount: {e:?}"))?;
            let f = fs.lookup(ROOT, b"after-the-cut").map_err(|e| format!("lookup after: {e:?}"))?;
            if slurp(&mut fs, f).unwrap()[100..] != data[..] {
                return Err("the file written after recovery reads back wrong".into());
            }
            Ok(())
        }
    };
    let images = sweep(&base, &log, |cut, image| {
        if failure.is_some() {
            return;
        }
        let mut r = Ok(());
        if cut % (if seeds() > 2 { 31 } else { 61 }) == 0 {
            cut_during_recovery(image.clone(), &mut |image| {
                extra += 1;
                if r.is_ok() {
                    r = verify(cut, image).map_err(|e| format!("{e} (after a second cut, during recovery)"));
                }
            });
        }
        if let Err(e) = r.and_then(|_| verify(cut, image)) {
            failure = Some(format!("cut at device operation {cut} of {}: {e}", log.len()));
        }
    });
    match failure {
        Some(e) => Err(e),
        None => Ok(images + extra),
    }
}

/// Every kind of operation, each (or a few together) in its own commit.
fn everything(fs: &mut F, mark: &mut dyn FnMut(&mut F)) {
    fs.now = 100;
    mk(fs, "/a", S_DIR | 0o755, 1).unwrap();
    mk(fs, "/a/f", S_FILE | 0o640, 7).unwrap();
    put(fs, "/a/f", 0, &pat(1, 0, 10_000)).unwrap();
    mark(fs);
    put(fs, "/a/f", 3000, &pat(2, 0, 6000)).unwrap(); // overwrite in the middle
    mark(fs);
    put(fs, "/a/f", 60_000, &pat(3, 0, 9000)).unwrap(); // past a hole, through a pointer block
    mark(fs);
    mv(fs, "/a/f", "/g").unwrap();
    mark(fs);
    mk(fs, "/h", S_FILE | 0o600, 8).unwrap();
    put(fs, "/h", 0, &pat(4, 0, 5000)).unwrap();
    mark(fs);
    mv(fs, "/h", "/g").unwrap(); // replaces /g and frees its blocks
    mark(fs);
    let g = ino(fs, "/g").unwrap();
    fs.truncate(g, 1234).unwrap();
    mark(fs);
    put(fs, "/g", 9000, &pat(5, 0, 100)).unwrap(); // grows over the cut-off tail
    mark(fs);
    let (a, _) = at(fs, "/a/l").unwrap();
    fs.link(a, b"l", g).unwrap();
    rm(fs, "/g").unwrap();
    mark(fs);
    mk(fs, "/a/d", S_DIR | 0o700, 2).unwrap();
    mk(fs, "/a/d/e", S_DIR | 0o700, 2).unwrap();
    mk(fs, "/a/d/e/x", S_FILE | 0o600, 2).unwrap();
    put(fs, "/a/d/e/x", 0, b"x").unwrap();
    mk(fs, "/a/d/e/sym", S_LINK | 0o777, 2).unwrap();
    put(fs, "/a/d/e/sym", 0, b"../x").unwrap();
    mark(fs);
    mv(fs, "/a/d", "/d2").unwrap(); // a directory to another parent
    mark(fs);
    rm(fs, "/d2/e/x").unwrap();
    rm(fs, "/d2/e/sym").unwrap();
    rmdir(fs, "/d2/e").unwrap();
    rmdir(fs, "/d2").unwrap();
    mark(fs);
    // Enough long names that the directory needs a second block.
    let long = |k: usize| format!("/a/{:0>100}", k);
    for k in 0..42 {
        mk(fs, &long(k), S_FILE | 0o644, 1).unwrap();
        if k % 3 == 0 {
            put(fs, &long(k), 0, &pat(k as u64, 0, 700)).unwrap();
        }
        if k % 7 == 6 {
            mark(fs);
        }
    }
    for k in (0..42).rev() {
        rm(fs, &long(k)).unwrap();
        if k % 6 == 0 {
            mark(fs);
        }
    }
    fs.set_quota(9, 3, 2).unwrap();
    mk(fs, "/q", S_FILE | 0o600, 9).unwrap();
    assert_eq!(put(fs, "/q", 0, &pat(6, 0, 3 * 4096)).unwrap(), 3 * 4096);
    assert_eq!(put(fs, "/q", 3 * 4096, b"more"), Err(Error::Quota));
    mark(fs);
    mk(fs, "/q2", S_FILE | 0o600, 9).unwrap();
    assert_eq!(mk(fs, "/q3", S_FILE | 0o600, 9), Err(Error::Quota));
    mark(fs);
    let q = ino(fs, "/q").unwrap();
    fs.chown(q, 8).unwrap();
    fs.chmod(q, 0o444).unwrap();
    mk(fs, "/q3", S_FILE | 0o600, 9).unwrap();
    mark(fs);
    fs.truncate(q, 0).unwrap();
    rm(fs, "/q").unwrap();
    rm(fs, "/a/l").unwrap();
    mark(fs);
}

#[test]
fn power_cut_at_every_write_leaves_the_old_or_the_new_state() {
    let images = cut_everywhere(160, 62, 32, 0, everything).unwrap();
    assert!(images > 2500, "only {images} images");
    println!("{images} power-cut images");
}

/// The sweep must notice a filesystem that is not crash-consistent: leave
/// out either flush the design depends on and it has to fail.
#[test]
fn the_sweep_catches_a_missing_flush() {
    let e = cut_everywhere(160, 62, 32, 1, everything).unwrap_err();
    println!("without the data flush: {e}");
    let e = cut_everywhere(160, 62, 32, 2, everything).unwrap_err();
    println!("without the flush after copying home: {e}");
    let e = cut_everywhere(160, 31, 64, 3, wrap_onto_freed).unwrap_err();
    println!("reusing blocks freed by the running transaction: {e}");
}

/// One transaction frees a file's blocks and then allocates its way round
/// the whole volume, so the search comes back to the blocks it freed. They
/// still hold the committed file and must be passed over.
fn wrap_onto_freed(fs: &mut F, mark: &mut dyn FnMut(&mut F)) {
    mk(fs, "/kept", S_FILE | 0o600, 1).unwrap();
    mk(fs, "/tmp", S_FILE | 0o600, 1).unwrap();
    put(fs, "/kept", 0, &pat(1, 0, 5 * 4096)).unwrap();
    mark(fs);
    let tmp = ino(fs, "/tmp").unwrap();
    for round in 2..5 {
        put(fs, "/kept", 0, &pat(round, 0, 5 * 4096)).unwrap();
        for _ in 0..20 {
            fs.write(tmp, 0, &[round as u8; 8 * 4096]).unwrap();
            fs.truncate(tmp, 0).unwrap();
        }
        mark(fs);
    }
}

#[test]
fn power_cut_when_allocation_comes_round_to_freed_blocks() {
    cut_everywhere(160, 31, 64, 0, wrap_onto_freed).unwrap();
}

/// Operations too big for one transaction commit part way. A cut then leaves
/// a big write as a prefix, and a big delete or truncate finished by mount.
#[test]
fn power_cut_during_operations_that_span_transactions() {
    let (mut fs, disk) = fresh(300, 31, 16);
    assert_eq!(fs.atomic_write(), 4096);
    disk.record();
    let whole = pat(7, 0, 180_000);
    let f = fs.create(ROOT, b"big", S_FILE | 0o600, 4).unwrap();
    assert_eq!(fs.write(f, 0, &whole).unwrap(), whole.len());
    fs.commit().unwrap();
    assert!(fs.commits > 40);
    let written = disk.log_len();
    fs.truncate(f, 10_000).unwrap();
    fs.commit().unwrap();
    let truncated = disk.log_len();
    assert_eq!(fs.write(f, 10_000, &whole[10_000..120_000]).unwrap(), 110_000);
    fs.commit().unwrap();
    let rewritten = disk.log_len();
    fs.unlink(ROOT, b"big").unwrap();
    fs.commit().unwrap();
    let base = disk.0.borrow().base.clone();
    let log = std::mem::take(&mut disk.0.borrow_mut().log);
    let mut sizes = std::collections::BTreeSet::new();
    let mut verify = |cut: usize, image: Vec<u8>| {
        let disk = TestDisk::from_image(image);
        let mut fs = open(&disk);
        fs.mount().unwrap_or_else(|e| panic!("cut {cut}: mount {e:?} line {}", fs.why));
        fsck(&mut fs).unwrap_or_else(|e| panic!("cut {cut}: {e}"));
        match fs.lookup(ROOT, b"big") {
            Ok(f) => {
                let data = slurp(&mut fs, f).unwrap();
                assert!(data[..] == whole[..data.len()], "cut {cut}: not a prefix of what was written");
                sizes.insert(data.len());
                // Whatever the size, no block is held past it.
                assert_eq!(fs.usage(4).unwrap().blocks as usize, data.len().div_ceil(4096) + (data.len() > 12 * 4096) as usize, "cut {cut}");
                if cut > written && cut <= truncated {
                    assert!(data.len() == 180_000 || data.len() == 10_000, "cut {cut}: truncate left {}", data.len());
                }
                if cut > truncated && cut <= rewritten {
                    assert!(data.len() >= 10_000 && data.len() <= 120_000);
                }
            }
            Err(Error::NotFound) => {
                assert!(cut < 20 || cut > rewritten, "cut {cut}: the file vanished");
                assert_eq!(fs.usage(4).unwrap(), Usage { block_limit: UNLIMITED, inode_limit: UNLIMITED, ..Usage::default() });
            }
            Err(e) => panic!("cut {cut}: {e:?}"),
        }
    };
    let mut extra = 0;
    let images = extra_images(&base, &log, &mut verify, &mut extra);
    assert!(sizes.len() > 40, "{} sizes", sizes.len());
    println!("{images} power-cut images, {} distinct sizes of the big file", sizes.len());
}

/// The sweep, and at some cuts a second cut while mount finishes the big
/// delete or truncate (which itself takes several transactions).
fn extra_images(base: &[u8], log: &[Op], verify: &mut dyn FnMut(usize, Vec<u8>), extra: &mut usize) -> usize {
    sweep(base, log, |cut, image| {
        if cut % (if seeds() > 2 { 43 } else { 97 }) == 0 {
            cut_during_recovery(image.clone(), &mut |image| {
                *extra += 1;
                verify(cut, image)
            });
        }
        verify(cut, image)
    }) + *extra
}

// ---- a model to compare against ----

#[derive(Clone, PartialEq, Debug)]
enum M {
    Dir,
    File(Vec<u8>),
}
type Model = BTreeMap<String, M>;

fn parent_ok(m: &Model, path: &str) -> bool {
    let parent = path.rsplit_once('/').unwrap().0;
    parent.is_empty() || m.get(parent) == Some(&M::Dir)
}
fn under<'a>(m: &'a Model, path: &str) -> Vec<&'a String> {
    let prefix = format!("{path}/");
    m.keys().filter(|k| k.starts_with(&prefix)).collect()
}
fn model_of(tree: &Tree) -> Model {
    tree.iter()
        .map(|(k, v)| {
            (k.clone(), match v {
                Node::Dir { .. } => M::Dir,
                Node::File { data, .. } => M::File(data.clone()),
            })
        })
        .collect()
}

/// Do one random operation on both; they must agree on whether it works.
fn random_op(fs: &mut F, m: &mut Model, rng: &mut Rng) {
    const DIRS: [&str; 5] = ["", "/d0", "/d1", "/d0/s", "/d1/t"];
    let name = |rng: &mut Rng| -> String {
        let dir = DIRS[rng.below(5) as usize];
        match rng.below(8) {
            0 => DIRS[1 + rng.below(4) as usize].to_string(),
            _ => format!("{dir}/f{}", rng.below(5)),
        }
    };
    let p = name(rng);
    let resource = |e: Error| matches!(e, Error::NoSpace | Error::Quota | Error::NoInodes);
    match rng.below(10) {
        0 => {
            let want = parent_ok(m, &p) && !m.contains_key(&p);
            match mk(fs, &p, S_DIR | 0o755, rng.below(3) as u32) {
                Ok(_) => {
                    assert!(want, "mkdir {p}");
                    m.insert(p, M::Dir);
                }
                Err(e) => assert!(!want || resource(e), "mkdir {p}: {e:?}"),
            }
        }
        1 | 2 => {
            let want = parent_ok(m, &p) && !m.contains_key(&p);
            match mk(fs, &p, S_FILE | 0o644, rng.below(3) as u32) {
                Ok(_) => {
                    assert!(want, "create {p}");
                    m.insert(p, M::File(vec![]));
                }
                Err(e) => assert!(!want || resource(e), "create {p}: {e:?}"),
            }
        }
        3 | 4 | 5 => {
            let off = if rng.below(4) == 0 { rng.below(600_000) } else { rng.below(30_000) };
            let most = if rng.below(5) == 0 { 90_000 } else { 9000 };
            let len = rng.below(most) as usize;
            let data = pat(rng.next(), off, len);
            let want = matches!(m.get(&p), Some(M::File(_))) && parent_ok(m, &p);
            match put(fs, &p, off, &data) {
                Ok(n) => {
                    assert!(want, "write {p}");
                    let Some(M::File(f)) = m.get_mut(&p) else { unreachable!() };
                    if n > 0 {
                        let end = off as usize + n;
                        if f.len() < end {
                            f.resize(end, 0);
                        }
                        f[off as usize..end].copy_from_slice(&data[..n]);
                    }
                }
                Err(e) => assert!(!want || resource(e), "write {p}: {e:?}"),
            }
        }
        6 => {
            let size = rng.below(40_000);
            let want = matches!(m.get(&p), Some(M::File(_)));
            match ino(fs, &p).and_then(|i| fs.truncate(i, size)) {
                Ok(()) => {
                    assert!(want, "truncate {p}");
                    let Some(M::File(f)) = m.get_mut(&p) else { unreachable!() };
                    f.resize(size as usize, 0);
                }
                Err(e) => assert!(!want || resource(e), "truncate {p}: {e:?}"),
            }
        }
        7 => match m.get(&p) {
            Some(M::Dir) => {
                let want = under(m, &p).is_empty();
                assert_eq!(rm(fs, &p), Err(Error::IsDir));
                assert_eq!(rmdir(fs, &p).is_ok(), want, "rmdir {p}");
                if want {
                    m.remove(&p);
                }
            }
            Some(M::File(_)) => {
                assert_eq!(rmdir(fs, &p), Err(Error::NotDir));
                rm(fs, &p).unwrap();
                m.remove(&p);
            }
            None => assert!(rm(fs, &p).is_err() && rmdir(fs, &p).is_err()),
        },
        _ => {
            let q = name(rng);
            let src = m.get(&p).cloned();
            let dst = m.get(&q).cloned();
            let want = match (&src, &dst) {
                (None, _) => false,
                _ if !parent_ok(m, &q) => false,
                _ if p == q => true,
                (Some(M::Dir), _) if q.starts_with(&format!("{p}/")) => false,
                (Some(M::Dir), Some(M::Dir)) => under(m, &q).is_empty(),
                (Some(M::Dir), Some(M::File(_))) | (Some(M::File(_)), Some(M::Dir)) => false,
                _ => true,
            };
            match mv(fs, &p, &q) {
                Ok(()) => {
                    assert!(want, "rename {p} {q}");
                    if p != q {
                        let moved: Vec<String> = under(m, &p).into_iter().cloned().collect();
                        for k in moved {
                            let v = m.remove(&k).unwrap();
                            m.insert(format!("{q}{}", &k[p.len()..]), v);
                        }
                        let v = m.remove(&p).unwrap();
                        m.insert(q, v);
                    }
                }
                Err(e) => assert!(!want || resource(e), "rename {p} {q}: {e:?}"),
            }
        }
    }
}

#[test]
fn random_operations_match_the_model() {
    let rounds: u64 = std::env::var("VIOLETFS_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    for round in 0..rounds {
        // A small journal and a small volume, so early commits, full disks
        // and quota refusals all happen.
        let (mut fs, disk) = fresh(if round % 2 == 0 { 700 } else { 220 }, 62, 16 + 20 * (round as u32 % 3));
        fs.set_quota(2, 60, 20).unwrap();
        let mut m = Model::new();
        let mut rng = Rng(0x1234_5678_9abc_def1 + round);
        for step in 0..1500 {
            random_op(&mut fs, &mut m, &mut rng);
            if step % 97 == 0 {
                assert_eq!(model_of(&dump(&mut fs).unwrap()), m, "round {round} step {step}");
                fsck(&mut fs).unwrap();
            }
            if step % 211 == 0 {
                fs.commit().unwrap();
                fs = open(&disk);
                fs.mount().unwrap();
                assert_eq!(model_of(&dump(&mut fs).unwrap()), m, "round {round} step {step} after mounting again");
            }
        }
        let u = fs.usage(2).unwrap();
        assert!(u.blocks <= 60 && u.inodes <= 20);
    }
}

#[test]
fn power_cut_during_random_operations() {
    let images = cut_everywhere(260, 62, 100, 0, |fs, mark| {
        let mut m = Model::new();
        let mut rng = Rng(0xfeed_f00d_dead_beef);
        for d in ["/d0", "/d1", "/d0/s"] {
            mk(fs, d, S_DIR | 0o755, 1).unwrap();
            m.insert(d.to_string(), M::Dir);
        }
        for _ in 0..200 {
            random_op(fs, &mut m, &mut rng);
            mark(fs);
        }
        assert!(m.len() > 5);
    })
    .unwrap();
    println!("{images} power-cut images");
}

// ---- the rest ----

#[test]
fn large_and_sparse_files() {
    let (mut fs, _disk) = fresh(3000, 31, 0);
    fs.create(ROOT, b"first", S_FILE, 1).unwrap(); // the root directory takes its block
    let free = fs.statfs().unwrap().free_blocks;
    // 6 MiB in one piece: direct, single and double indirect pointers.
    let data = pat(11, 0, 6 << 20);
    let f = fs.create(ROOT, b"six", S_FILE | 0o600, 1).unwrap();
    assert_eq!(fs.write(f, 0, &data).unwrap(), data.len());
    fs.commit().unwrap();
    assert!(slurp(&mut fs, f).unwrap() == data);
    assert_eq!(fs.stat(f).unwrap().blocks, 1536 + 1 + 1 + 1); // data + single + double + one leaf
    // Far out: 5 GiB and 3 TiB in, through triple indirect pointers.
    let g = fs.create(ROOT, b"sparse", S_FILE | 0o600, 1).unwrap();
    for off in [5u64 << 30, 3u64 << 40] {
        assert_eq!(fs.write(g, off + 123, b"far away").unwrap(), 8);
        let mut back = [0u8; 16];
        assert_eq!(fs.read(g, off + 119, &mut back).unwrap(), 12);
        assert_eq!(&back[..12], b"\0\0\0\0far away");
    }
    assert_eq!(fs.stat(g).unwrap().size, (3u64 << 40) + 131);
    assert_eq!(fs.write(g, MAX_SIZE, b"x"), Err(Error::TooBig));
    fsck(&mut fs).unwrap();
    fs.truncate(g, (5u64 << 30) + 4096).unwrap();
    fsck(&mut fs).unwrap();
    fs.unlink(ROOT, b"sparse").unwrap();
    fs.unlink(ROOT, b"six").unwrap();
    fsck(&mut fs).unwrap();
    assert_eq!(fs.statfs().unwrap().free_blocks, free);
}

#[test]
fn a_full_volume_refuses_cleanly_and_recovers() {
    let (mut fs, _disk) = fresh(200, 31, 16);
    let free = fs.statfs().unwrap().free_blocks as usize;
    let f = fs.create(ROOT, b"fill", S_FILE | 0o600, 1).unwrap();
    let data = pat(3, 0, (free + 10) * 4096);
    let n = fs.write(f, 0, &data).unwrap();
    assert!(n < data.len() && n > (free - 4) * 4096, "wrote {n}");
    assert_eq!(fs.write(f, n as u64, b"x"), Err(Error::NoSpace));
    assert_eq!(fs.create(ROOT, b"another", S_FILE, 1).map(|_| ()), Ok(())); // room in the root's first block? it has none yet
    fsck(&mut fs).unwrap();
    // Overwriting needs new blocks; with none free it fails and the old data stays.
    let before = slurp(&mut fs, f).unwrap();
    assert!(before == data[..n]);
    let g = fs.create(ROOT, b"g", S_FILE | 0o600, 1);
    let _ = g;
    while fs.statfs().unwrap().free_blocks > 0 {
        let size = fs.stat(f).unwrap().size;
        if fs.write(f, size, &[1u8; 4096]).is_err() {
            break;
        }
    }
    if fs.statfs().unwrap().free_blocks == 0 {
        assert_eq!(fs.write(f, 0, b"new"), Err(Error::NoSpace));
        assert!(slurp(&mut fs, f).unwrap()[..3] == data[..3]);
    }
    fs.unlink(ROOT, b"fill").unwrap();
    fsck(&mut fs).unwrap();
    let h = fs.create(ROOT, b"h", S_FILE | 0o600, 1).unwrap();
    assert_eq!(fs.write(h, 0, &data[..40_000]).unwrap(), 40_000);
    fsck(&mut fs).unwrap();
}

#[test]
fn quotas_are_counted_and_enforced() {
    let (mut fs, _disk) = fresh(300, 62, 0);
    fs.set_quota(5, 10, 3).unwrap();
    let d = fs.create(ROOT, b"d", S_DIR | 0o700, 5).unwrap();
    let f = fs.create(d, b"f", S_FILE | 0o600, 5).unwrap();
    assert_eq!(fs.usage(5).unwrap(), Usage { blocks: 1, block_limit: 10, inodes: 2, inode_limit: 3 });
    assert_eq!(fs.write(f, 0, &[7; 9 * 4096]).unwrap(), 9 * 4096);
    assert_eq!(fs.write(f, 9 * 4096, &[7; 10]), Err(Error::Quota));
    // Overwriting in place takes no more quota.
    assert_eq!(fs.write(f, 100, &[8; 5000]).unwrap(), 5000);
    let g = fs.create(d, b"g", S_FILE | 0o600, 5).unwrap();
    assert_eq!(fs.create(d, b"h", S_FILE | 0o600, 5), Err(Error::Quota));
    // Another owner's file in owner 5's directory is charged to its own owner.
    let o = fs.create(d, b"other", S_FILE | 0o600, 6).unwrap();
    assert_eq!(fs.write(o, 0, &[1; 8192]).unwrap(), 8192);
    assert_eq!(fs.usage(6).unwrap().blocks, 2);
    assert_eq!(fs.chown(o, 5), Err(Error::Quota));
    fs.chown(g, 6).unwrap();
    assert_eq!((fs.usage(5).unwrap().inodes, fs.usage(6).unwrap().inodes), (2, 2));
    fs.truncate(f, 4096).unwrap();
    assert_eq!(fs.usage(5).unwrap().blocks, 2);
    fs.chown(o, 5).unwrap();
    assert_eq!(fs.usage(5).unwrap().blocks, 4);
    fsck(&mut fs).unwrap();
}

#[test]
fn names_directories_and_links() {
    let (mut fs, _disk) = fresh(300, 124, 0);
    let long = [b'n'; 255];
    let f = fs.create(ROOT, &long, S_FILE | 0o600, 1).unwrap();
    assert_eq!(fs.create(ROOT, &[b'n'; 256], S_FILE, 1), Err(Error::Invalid));
    assert_eq!(fs.lookup(ROOT, &long).unwrap(), f);
    fs.link(ROOT, b"second", f).unwrap();
    assert_eq!(fs.stat(f).unwrap().nlink, 2);
    fs.write(f, 0, b"shared").unwrap();
    fs.unlink(ROOT, &long).unwrap();
    assert_eq!(slurp(&mut fs, f).unwrap(), b"shared");
    let d = fs.create(ROOT, b"d", S_DIR | 0o755, 1).unwrap();
    assert_eq!(fs.link(ROOT, b"dirlink", d), Err(Error::Invalid));
    let e = fs.create(d, b"e", S_DIR | 0o755, 1).unwrap();
    assert_eq!(fs.rmdir(ROOT, b"d"), Err(Error::NotEmpty));
    assert_eq!(fs.unlink(ROOT, b"d"), Err(Error::IsDir));
    assert_eq!(fs.rmdir(ROOT, b"second"), Err(Error::NotDir));
    // A directory cannot move into itself.
    assert_eq!(fs.rename(ROOT, b"d", e, b"inside"), Err(Error::Invalid));
    assert_eq!(fs.rename(ROOT, b"d", d, b"inside"), Err(Error::Invalid));
    assert_eq!(fs.rename(ROOT, b"second", ROOT, b"d"), Err(Error::IsDir));
    assert_eq!(fs.rename(ROOT, b"d", ROOT, b"second"), Err(Error::NotDir));
    fs.rename(d, b"e", ROOT, b"e").unwrap();
    assert_eq!(fs.stat(e).unwrap().parent, ROOT);
    fs.rename(ROOT, b"e", ROOT, b"d").unwrap(); // over the now empty d
    assert_eq!(fs.lookup(ROOT, b"d").unwrap(), e);
    assert_eq!(fs.stat(d), Err(Error::NotFound));
    // Many names: listed once each, in any order, and all found.
    for k in 0..90 {
        fs.create(e, format!("file-{k:03}").as_bytes(), S_FILE | 0o644, 1).unwrap();
    }
    for k in (0..90).step_by(3) {
        fs.unlink(e, format!("file-{k:03}").as_bytes()).unwrap();
    }
    for k in 0..20 {
        fs.create(e, format!("x{k}").as_bytes(), S_FILE | 0o644, 1).unwrap();
    }
    let tree = dump(&mut fs).unwrap();
    assert_eq!(tree.keys().filter(|k| k.starts_with("/d/")).count(), 60 + 20);
    assert!(tree.contains_key("/d/file-001") && !tree.contains_key("/d/file-003") && tree.contains_key("/d/x19"));
    fsck(&mut fs).unwrap();
}

#[test]
fn a_device_error_stops_the_filesystem_and_mount_recovers() {
    // A clean run, to know the tree after each commit.
    let work = |fs: &mut F, done: &mut dyn FnMut(&mut F)| -> Result<(), Error> {
        let d = fs.create(ROOT, b"d", S_DIR | 0o755, 1)?;
        let f = fs.create(d, b"f", S_FILE | 0o644, 1)?;
        fs.write(f, 0, &pat(1, 0, 70_000))?;
        fs.commit()?;
        done(fs);
        fs.write(f, 5000, &pat(2, 0, 9000))?;
        fs.rename(d, b"f", ROOT, b"g")?;
        fs.commit()?;
        done(fs);
        fs.truncate(f, 100)?;
        fs.rmdir(ROOT, b"d")?;
        fs.commit()?;
        done(fs);
        fs.unlink(ROOT, b"g")?;
        fs.commit()?;
        done(fs);
        Ok(())
    };
    let (mut fs, disk) = fresh(200, 31, 100);
    let start = disk.image();
    let mut trees = vec![dump(&mut fs).unwrap()];
    disk.fail_at(None);
    work(&mut fs, &mut |fs| trees.push(dump(fs).unwrap())).unwrap();
    let total = disk.0.borrow().ops;
    assert!(total > 60);
    for fail in 0..total {
        let disk = TestDisk::from_image(start.clone());
        let mut fs = open(&disk);
        fs.mount().unwrap();
        disk.fail_at(Some(fail));
        let mut done = 0;
        assert_eq!(work(&mut fs, &mut |_| done += 1), Err(Error::Io), "failing at {fail}");
        // Nothing more is accepted, and nothing more reaches the device.
        let ops = disk.0.borrow().ops;
        assert_eq!(fs.create(ROOT, b"late", S_FILE, 1), Err(Error::Failed));
        assert_eq!(fs.commit(), Err(Error::Failed));
        assert_eq!(disk.0.borrow().ops, ops);
        disk.fail_at(None);
        let mut fs = open(&disk);
        fs.mount().unwrap();
        fsck(&mut fs).unwrap_or_else(|e| panic!("failing at {fail}: {e}"));
        let tree = dump(&mut fs).unwrap();
        assert!(tree == trees[done] || tree == trees[done + 1], "failing at {fail}: neither commit {done} nor the next");
    }
}

#[test]
fn damaged_media_give_errors_not_panics() {
    let (mut fs, disk) = fresh(160, 62, 32);
    everything_small(&mut fs);
    fs.commit().unwrap();
    let good = disk.image();
    let good_tree = dump(&mut fs).unwrap();
    // The first superblock gone: the copy serves.
    let mut image = good.clone();
    image[..4096].fill(0);
    let d = TestDisk::from_image(image);
    let mut fs = open(&d);
    fs.mount().unwrap();
    fsck(&mut fs).unwrap();
    // Both gone, or never formatted.
    let mut image = good.clone();
    image[..8192].fill(0);
    assert_eq!(open(&TestDisk::from_image(image)).mount(), Err(Error::NotFormatted));
    assert_eq!(open(&TestDisk::new(4096)).mount(), Err(Error::NotFormatted));
    assert_eq!(open(&TestDisk::new(64)).format(0, 0), Err(Error::Invalid));
    // One flipped bit in the state block, the bitmap or the inode table
    // (blocks 35 to 38 here) is always noticed, by mount or by the check,
    // unless mount repaired it from the journal's last transaction.
    let mut rng = Rng(77);
    for round in 0..600 {
        let mut image = good.clone();
        let anywhere = round % 2 == 1;
        let at = if anywhere { 8192 + rng.below(70 * 4096) } else { 35 * 4096 + rng.below(4 * 4096) } as usize;
        image[at] ^= 1 << rng.below(8);
        if anywhere {
            // And more damage anywhere at all: every call must still return.
            for _ in 0..rng.below(4) {
                let at = rng.below(image.len() as u64) as usize;
                image[at] = rng.next() as u8;
            }
        }
        let d = TestDisk::from_image(image);
        let mut fs = open(&d);
        let mut noticed = fs.mount().is_err();
        if !noticed {
            let tree = dump(&mut fs);
            noticed = fsck(&mut fs).is_err() || tree.is_err() || (!anywhere && tree.unwrap() == good_tree);
            let _ = fs.create(ROOT, b"probe", S_FILE, 1).and_then(|f| fs.write(f, 0, &[1; 9000]));
            let _ = fs.commit();
        }
        assert!(noticed || anywhere, "a flipped bit at byte {at} went unnoticed");
    }
}
fn everything_small(fs: &mut F) {
    let d = fs.create(ROOT, b"d", S_DIR | 0o755, 1).unwrap();
    for k in 0..12 {
        let f = fs.create(d, format!("f{k}").as_bytes(), S_FILE | 0o644, k).unwrap();
        fs.write(f, k as u64 * 3000, &pat(k as u64, 0, 9000)).unwrap();
    }
    let f = fs.lookup(d, b"f3").unwrap();
    fs.write(f, 100_000, b"far").unwrap();
}

#[test]
fn allocation_goes_round_the_volume_and_free_blocks_are_discarded() {
    let (mut fs, disk) = fresh(400, 31, 32);
    let free = fs.statfs().unwrap().free_blocks as u64;
    disk.record();
    // Rewrite one small file again and again: with first-fit the same few
    // blocks would take every write.
    let f = fs.create(ROOT, b"hot", S_FILE | 0o600, 1).unwrap();
    for k in 0..600u64 {
        fs.write(f, 0, &pat(k, 0, 4096)).unwrap();
        fs.commit().unwrap();
    }
    let first_data = 400 - free;
    let mut hits = BTreeMap::new();
    for op in &disk.0.borrow().log {
        if let Op::Write { lba, .. } = op {
            if lba / 8 >= first_data {
                *hits.entry(lba / 8).or_insert(0u32) += 1;
            }
        }
    }
    assert!(hits.len() as u64 > free * 9 / 10, "only {} of {free} data blocks were used", hits.len());
    assert!(*hits.values().max().unwrap() <= 3, "a data block was written {} times", hits.values().max().unwrap());
    // All but the file's block and the root directory's.
    assert_eq!(fs.discard_free().unwrap(), free - 2);
    assert_eq!(disk.0.borrow().discarded, (free - 2) * 8);
}

// ---- found by review ----

/// A cut can leave the journal record of a transaction on the medium with
/// one of its slots missing. Mount rolls that transaction back. The next
/// transaction writes its own images into the same slots, and where one is
/// the same bytes as before, a second cut could make the old record whole
/// again: mount would replay a transaction it had rolled back. Cut during
/// `first`, mount, run `second` and cut again at every write: the tree is
/// always what the first recovery showed or what `second` made of it.
fn cut_twice(first: &dyn Fn(&mut F), second: &dyn Fn(&mut F)) -> Result<(usize, usize), String> {
    let (mut fs, disk) = fresh(48, 31, 16);
    let f = fs.create(ROOT, b"f", S_FILE | 0o600, 1).unwrap();
    fs.write(f, 0, &pat(1, 0, 4096)).unwrap();
    fs.commit().unwrap();
    disk.record();
    first(&mut fs);
    fs.commit().unwrap();
    let base = disk.0.borrow().base.clone();
    let log = std::mem::take(&mut disk.0.borrow_mut().log);
    let mut failure = None;
    let mut images2 = 0;
    let images = sweep(&base, &log, |cut, image| {
        if failure.is_some() {
            return;
        }
        let disk = TestDisk::from_image(image);
        let mut fs = open(&disk);
        fs.mount().unwrap();
        let before = dump(&mut fs).unwrap();
        disk.record();
        second(&mut fs);
        fs.commit().unwrap();
        let after = dump(&mut fs).unwrap();
        let base = disk.0.borrow().base.clone();
        let log = std::mem::take(&mut disk.0.borrow_mut().log);
        images2 += sweep(&base, &log, |cut2, image| {
            if failure.is_some() {
                return;
            }
            let disk = TestDisk::from_image(image);
            let mut fs = open(&disk);
            let r = fs.mount().map_err(|e| format!("mount: {e:?}")).and_then(|_| fsck(&mut fs)).and_then(|_| dump(&mut fs).map_err(|e| format!("{e:?}")));
            match r {
                Ok(tree) if tree == before || tree == after => {}
                Ok(_) => failure = Some(format!("cuts at {cut} then {cut2}: the tree is neither what the first recovery showed nor what the second transaction made")),
                Err(e) => failure = Some(format!("cuts at {cut} then {cut2}: {e}")),
            }
        });
    });
    match failure {
        Some(e) => Err(e),
        None => Ok((images, images2)),
    }
}

#[test]
fn a_transaction_that_was_rolled_back_is_never_replayed() {
    // The same name made again for another owner: the directory block's
    // image is the same both times, the inode's and the quota table's differ.
    let (a, b) = cut_twice(&|fs| fs.create(ROOT, b"x", S_FILE | 0o600, 1).map(|_| ()).unwrap(), &|fs| {
        if fs.lookup(ROOT, b"x").is_err() {
            fs.create(ROOT, b"x", S_FILE | 0o644, 2).unwrap();
        }
    })
    .unwrap();
    println!("{a} first cuts, {b} second cuts");
    // A save done again with other data lands in the same blocks.
    cut_twice(&|fs| put(fs, "/f", 0, &pat(2, 0, 4096)).map(|_| ()).unwrap(), &|fs| put(fs, "/f", 0, &pat(3, 0, 4096)).map(|_| ()).unwrap()).unwrap();
    // A delete rolled back, then something else in the same directory.
    cut_twice(&|fs| rm(fs, "/f").unwrap(), &|fs| fs.create(ROOT, b"y", S_FILE | 0o600, 1).map(|_| ()).unwrap()).unwrap();
    cut_twice(&|fs| mv(fs, "/f", "/g").unwrap(), &|fs| {
        // One operation only: with 16 slots a second would commit the first.
        match fs.lookup(ROOT, b"f") {
            Ok(_) => fs.rename(ROOT, b"f", ROOT, b"h").unwrap(),
            Err(_) => fs.rename(ROOT, b"g", ROOT, b"f").unwrap(),
        }
    })
    .unwrap();
}

/// What is all-or-nothing is a write that touches no more blocks than the
/// journal has room for in one transaction, wherever it starts: the bound
/// is `atomic_write_at(off)`. (`atomic_write()` bytes at an offset that is
/// not a block boundary touch one block more and are done in two pieces.)
#[test]
fn a_write_within_the_bound_is_all_or_nothing_at_any_offset() {
    // The second volume is nearly full: an overwrite has just the room it needs.
    for (blocks, slots) in [(64, 16), (60, 32)] {
        let images = cut_everywhere(blocks, 31, slots, 0, |fs, mark| {
            let bound = fs.atomic_write();
            assert_eq!(fs.atomic_write_at(8192), bound);
            assert_eq!(fs.atomic_write_at(100), bound - 100);
            assert_eq!(fs.atomic_write_at(3 * 4096 + 4095), bound - 4095);
            mk(fs, "/f", S_FILE | 0o600, 1).unwrap();
            mark(fs);
            for off in [0, bound] {
                put(fs, "/f", off as u64, &pat(1, 0, bound.min(4096 + bound - off))).unwrap();
                mark(fs);
            }
            for (k, off) in [100u64, 4095, 4096 + 2048, 0].into_iter().enumerate() {
                let len = fs.atomic_write_at(off);
                assert_eq!(put(fs, "/f", off, &pat(10 + k as u64, 0, len)).unwrap(), len);
                mark(fs); // fails if the write committed part way
            }
        })
        .unwrap();
        println!("{slots} slots: {images} power-cut images");
    }
    // The same sweep sees that a write of `atomic_write()` bytes which does
    // not start on a block boundary is not all-or-nothing: with 16 slots it
    // commits its first piece on the way.
    let split = std::panic::catch_unwind(|| {
        cut_everywhere(120, 31, 16, 0, |fs, mark| {
            mk(fs, "/f", S_FILE | 0o600, 1).unwrap();
            mark(fs);
            let len = fs.atomic_write();
            put(fs, "/f", 100, &pat(2, 0, len)).unwrap();
            mark(fs);
        })
    });
    assert!(split.is_err(), "one block more than the bound went through in one transaction");
}

#[test]
fn a_readdir_cookie_that_is_not_a_record_is_refused() {
    let (mut fs, _disk) = fresh(200, 62, 0);
    let d = fs.create(ROOT, b"d", S_DIR | 0o777, 1).unwrap();
    let secret = fs.create(ROOT, b"secret", S_FILE | 0o600, 9).unwrap();
    let mut name = [0u8; NAME_MAX];
    for k in 0..6 {
        fs.create(d, format!("victim-{k}").as_bytes(), S_FILE | 0o600, 1).unwrap();
    }
    // The cookies a listing hands out.
    let mut cookies = vec![0u64];
    while let Some(e) = fs.readdir(d, *cookies.last().unwrap(), &mut name).unwrap() {
        cookies.push(e.next);
    }
    assert_eq!(cookies.len(), 7);
    // A name whose bytes look like a directory record naming another
    // owner's inode, at every 4-byte step.
    let mut forged = Vec::new();
    // (A name has no zero byte, which limits what can be forged to inode
    // numbers from 0x01010101 and records of 260 bytes or more; `secret`
    // stands for such an inode on a volume big enough to have one.)
    let secret = secret | 0x0101_0100;
    while forged.len() + 16 <= 248 {
        forged.extend_from_slice(&secret.to_le_bytes());
        forged.extend_from_slice(&260u16.to_le_bytes());
        forged.extend_from_slice(&[8, 6]); // a file, a name of 6 bytes
        forged.extend_from_slice(b"forged\x01\x01");
    }
    assert!(!forged.contains(&0) && !forged.contains(&b'/'));
    // Remove names (their room is joined), then put the forged name there.
    for k in 1..6 {
        fs.unlink(d, format!("victim-{k}").as_bytes()).unwrap();
    }
    let attacker = fs.create(d, &forged, S_FILE | 0o600, 2).unwrap();
    // Every offset in the block: only real records are ever returned.
    let mut real = 0;
    for cookie in 0..4096u64 {
        match fs.readdir(d, cookie, &mut name) {
            Ok(Some(e)) => {
                assert!(e.ino != secret, "cookie {cookie} returned a forged entry");
                assert!(e.ino == attacker || &name[..e.len] == b"victim-0", "cookie {cookie}");
                real += 1;
            }
            Ok(None) => {}
            Err(e) => assert_eq!(e, Error::Invalid, "cookie {cookie}"),
        }
    }
    assert!(real >= 2 && real <= 4, "{real}");
    // Old cookies that now point into the forged name are refused, and the
    // filesystem goes on working (a bad cookie is the caller's error).
    assert!(cookies[2..6].iter().any(|&c| fs.readdir(d, c, &mut name) == Err(Error::Invalid)));
    assert!(fs.readdir(d, 1 << 40, &mut name).unwrap().is_none());
    assert!(fs.readdir(d, 4092, &mut name).unwrap().is_none()); // where the block's last record ends
    assert_eq!(fs.readdir(d, 4094, &mut name).err(), Some(Error::Invalid));
    fs.create(d, b"still-works", S_FILE, 1).unwrap();
    let tree = dump(&mut fs).unwrap();
    assert_eq!(tree.keys().filter(|k| k.starts_with("/d/")).count(), 3);
    fsck(&mut fs).unwrap();
}
impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        (self.ino, self.kind, self.len, self.next) == (other.ino, other.kind, other.len, other.next)
    }
}

#[test]
fn quota_rows_are_given_back() {
    let (mut fs, disk) = fresh(300, 62, 0);
    // Far more owners than the table has rows, one after the other.
    for owner in 100..100 + 3 * QUOTAS as u32 {
        let f = fs.create(ROOT, b"f", S_FILE | 0o600, owner).unwrap();
        fs.write(f, 0, &[1; 5000]).unwrap();
        assert_eq!(fs.usage(owner).unwrap().blocks, 2);
        fs.unlink(ROOT, b"f").unwrap();
        assert_eq!(fs.usage(owner).unwrap(), Usage { block_limit: UNLIMITED, inode_limit: UNLIMITED, ..Usage::default() });
    }
    // A refused operation leaves no row: fill the inodes, then probe with
    // new owners.
    let mut made = 0;
    while fs.create(ROOT, format!("i{made}").as_bytes(), S_FILE, 1).is_ok() {
        made += 1;
    }
    for owner in 1000..1000 + 2 * QUOTAS as u32 {
        assert_eq!(fs.create(ROOT, b"probe", S_FILE, owner), Err(Error::NoInodes));
    }
    for k in 0..6 {
        fs.unlink(ROOT, format!("i{k}").as_bytes()).unwrap();
    }
    fs.create(ROOT, b"new", S_FILE, 5000).unwrap();
    fs.unlink(ROOT, b"new").unwrap();
    // A limit holds its row until it is lifted; a full table refuses a new
    // owner and a new limit, and nothing else.
    for owner in 0..QUOTAS as u32 - 2 {
        fs.set_quota(2000 + owner, 10, 10).unwrap(); // owners 0 and 1 have rows
    }
    assert_eq!(fs.set_quota(3000, 1, 1), Err(Error::Quota));
    assert_eq!(fs.create(ROOT, b"x", S_FILE, 3000), Err(Error::Quota));
    fs.set_quota(3000, UNLIMITED, UNLIMITED).unwrap(); // nothing to lift: no row, no error
    let x = fs.create(ROOT, b"x", S_FILE, 2000).unwrap();
    assert_eq!(fs.chown(x, 3000), Err(Error::Quota));
    fs.set_quota(2000, UNLIMITED, UNLIMITED).unwrap(); // owner 2000 holds a file: the row stays
    assert_eq!(fs.create(ROOT, b"y", S_FILE, 3000), Err(Error::Quota));
    fs.set_quota(2001, UNLIMITED, UNLIMITED).unwrap(); // holds nothing: the row is free
    fs.chown(x, 3000).unwrap(); // and owner 2000's goes with its last file
    fs.create(ROOT, b"y", S_FILE, 3001).unwrap();
    assert_eq!(fs.usage(2002).unwrap().block_limit, 10);
    fsck(&mut fs).unwrap();
    fs.commit().unwrap();
    let mut fs = open(&disk);
    fs.mount().unwrap();
    fsck(&mut fs).unwrap();
    assert_eq!((fs.usage(3000).unwrap().inodes, fs.usage(2000).unwrap().inodes, fs.usage(2002).unwrap().inode_limit), (1, 0, 10));
}

#[test]
fn empty_directory_blocks_are_given_back() {
    let (mut fs, disk) = fresh(400, 124, 32);
    fs.set_quota(5, 12, 4).unwrap();
    let d = fs.create(ROOT, b"d", S_DIR | 0o777, 5).unwrap();
    let f = fs.create(d, b"file", S_FILE | 0o600, 6).unwrap();
    assert_eq!(fs.usage(5).unwrap().blocks, 1);
    // Another owner fills owner 5's directory with long names for its file.
    let name = |k: usize| format!("{k:0>250}");
    let mut links = 0;
    loop {
        match fs.link(d, name(links).as_bytes(), f) {
            Ok(()) => links += 1,
            Err(e) => {
                assert_eq!(e, Error::Quota);
                break;
            }
        }
    }
    assert_eq!(fs.usage(5).unwrap().blocks, 12);
    assert!(links > 150, "{links}");
    assert_eq!(fs.usage(6).unwrap().blocks, 0); // the linker pays nothing: see the README
    fs.commit().unwrap();
    // Removing the names gives the blocks back, last block first.
    for k in (0..links).rev() {
        fs.unlink(d, name(k).as_bytes()).unwrap();
        if k % 40 == 0 {
            fsck(&mut fs).unwrap();
        }
    }
    assert_eq!(fs.usage(5).unwrap().blocks, 1);
    assert_eq!(fs.stat(d).unwrap().size, 4096);
    // In any order: blocks before the last name stay until it goes.
    for k in 0..links {
        fs.link(d, name(k).as_bytes(), f).unwrap();
    }
    for k in 0..links - 1 {
        fs.unlink(d, name(k).as_bytes()).unwrap();
    }
    assert_eq!(fs.usage(5).unwrap().blocks, 12);
    fs.rename(d, name(links - 1).as_bytes(), ROOT, b"moved").unwrap(); // a rename out empties it too
    assert_eq!(fs.usage(5).unwrap().blocks, 1);
    assert_eq!(fs.lookup(d, b"file").unwrap(), f);
    fs.unlink(d, b"file").unwrap();
    assert_eq!((fs.usage(5).unwrap().blocks, fs.stat(d).unwrap().size), (0, 0));
    fs.create(d, b"again", S_FILE, 5).unwrap();
    fsck(&mut fs).unwrap();
    fs.commit().unwrap();
    let mut fs = open(&disk);
    fs.mount().unwrap();
    assert_eq!(dump(&mut fs).unwrap().len(), 3);
    fsck(&mut fs).unwrap();
}

/// A directory that grows over several blocks and shrinks again, with the
/// power cut at every write (a journal of 16 slots, so the shrinking is
/// often a transaction of its own that mount has to finish).
#[test]
fn power_cut_while_a_directory_shrinks() {
    for slots in [16, 40] {
        let images = cut_everywhere(64, 31, slots, 0, |fs, mark| {
            mk(fs, "/d", S_DIR | 0o755, 3).unwrap();
            mark(fs);
            let long = |k: usize| format!("/d/{k:0>250}");
            for k in 0..17 {
                mk(fs, &long(k), S_FILE | 0o644, 4).unwrap();
                if slots == 16 || k % 5 == 4 {
                    mark(fs);
                }
            }
            mark(fs);
            assert_eq!(fs.usage(3).unwrap().blocks, 2);
            // The second block empties first; then the first, out of order.
            for k in [16, 15, 3, 0, 1, 2, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4] {
                rm(fs, &long(k)).unwrap();
                mark(fs);
            }
            assert_eq!(fs.usage(3).unwrap().blocks, 0);
            rmdir(fs, "/d").unwrap();
            mark(fs);
        })
        .unwrap();
        println!("{slots} slots: {images} power-cut images");
    }
}

/// A device or checksum error on a call that only reads stops the
/// filesystem just as one on a write does.
#[test]
fn an_error_while_reading_stops_the_filesystem() {
    let (mut fs, disk) = fresh(200, 31, 32);
    let d = fs.create(ROOT, b"d", S_DIR | 0o755, 1).unwrap();
    let f = fs.create(d, b"f", S_FILE | 0o644, 1).unwrap();
    fs.write(f, 0, &pat(1, 0, 70_000)).unwrap();
    fs.commit().unwrap();
    // The last transaction is in the journal, and mount writes its blocks
    // again: make it one that has only the state block in it.
    fs.set_quota(77, 5, 5).unwrap();
    fs.commit().unwrap();
    let mut name = [0u8; NAME_MAX];
    let mut buf = [0u8; 100];
    type Call<'a> = &'a dyn Fn(&mut F, &mut [u8; NAME_MAX], &mut [u8]) -> Result<(), Error>;
    let calls: [(&str, Call); 6] = [
        ("read", &|fs, _, buf| fs.read(3, 60_000, buf).map(|_| ())),
        ("lookup", &|fs, _, _| fs.lookup(2, b"f").map(|_| ())),
        ("readdir", &|fs, name, _| fs.readdir(2, 0, name).map(|_| ())),
        ("stat", &|fs, _, _| fs.stat(3).map(|_| ())),
        ("usage", &|fs, _, _| fs.usage(1).map(|_| ())),
        ("statfs", &|fs, _, _| fs.statfs().map(|_| ())),
    ];
    assert_eq!((d, f), (2, 3));
    for (what, call) in &calls {
        // The device fails.
        let mut fs = open(&disk);
        fs.mount().unwrap();
        disk.fail_at(Some(0));
        let r = call(&mut fs, &mut name, &mut buf);
        disk.fail_at(None);
        if r.is_ok() {
            // Answered from the state block, which mount left in memory.
            assert!(*what == "usage" || *what == "statfs", "{what}");
            continue;
        }
        assert_eq!(r, Err(Error::Io), "{what}");
        assert_eq!(fs.create(ROOT, b"late", S_FILE, 1), Err(Error::Failed), "{what}");
        assert_eq!(fs.stat(ROOT), Err(Error::Failed), "{what}");
    }
    // A metadata block fails its checksum: found by a read, and no write
    // is accepted after it. (Blocks: 35 state, 36 bitmap, 37 inodes.)
    let inodes = 3 + 32 + 2;
    for (what, call) in &calls[..4] {
        let mut image = disk.image();
        image[inodes * 4096 + 2000] ^= 4;
        let bad = TestDisk::from_image(image);
        let mut fs = open(&bad);
        fs.mount().unwrap();
        assert_eq!(call(&mut fs, &mut name, &mut buf), Err(Error::Corrupt), "{what}");
        assert_eq!(fs.create(ROOT, b"late", S_FILE, 1), Err(Error::Failed), "{what}");
    }
    // Errors that are the caller's do not stop it.
    let mut fs = open(&disk);
    fs.mount().unwrap();
    assert_eq!(fs.lookup(ROOT, b"nope"), Err(Error::NotFound));
    assert_eq!(fs.readdir(ROOT, 6, &mut name).err(), Some(Error::Invalid));
    assert_eq!(fs.stat(30), Err(Error::NotFound));
    assert_eq!(fs.read(d, 0, &mut buf), Err(Error::IsDir));
    fs.create(ROOT, b"fine", S_FILE, 1).unwrap();
}
