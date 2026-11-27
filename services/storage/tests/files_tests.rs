//! The app-private files of files.rs (what /data and the SDK's file calls
//! use today), driven with the messages the Runtime forwards.
use crate::block::Disk;
use crate::files::{dispatch, Stage};
use crate::protocol::*;
use crate::testdisk::*;
use std::collections::BTreeMap;

pub const OPEN: u32 = 7;

pub fn message(op: u32, key: u32, name: &str, offset: u32, data: &[u8], flags: u32) -> Message {
    let mut m = Message::new(op, key, offset);
    m.arg = data.len() as u32 | flags;
    let mut bytes = [0u8; 40];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    bytes[16..16 + data.len()].copy_from_slice(data);
    m.put_text(&bytes);
    m
}
pub struct Store {
    pub disk: Disk,
    pub stages: [Stage; 8],
}
impl Store {
    pub fn new(dev: &TestDisk) -> Self {
        Store { disk: Disk::new(dev.clone()), stages: [Stage::new(); 8] }
    }
    pub fn call(&mut self, owner: u32, m: Message) -> Result<Message, u32> {
        dispatch(&mut self.disk, &mut self.stages, owner, m)
    }
    /// Replace a file with the messages writeback.rs sends (that code itself
    /// is driven against this service in writeback_tests.rs).
    pub fn replace(&mut self, owner: u32, name: &str, data: &[u8]) -> Result<(), u32> {
        for (i, chunk) in data.chunks(24).enumerate() {
            self.call(owner, message(EXECUTE, FILE_WRITE, name, (i * 24) as u32, chunk, FILE_STAGED))?;
        }
        self.call(owner, message(EXECUTE, FILE_TRUNCATE, name, data.len() as u32, &[], FILE_STAGED)).map(|_| ())
    }
    /// Change a file the old way: one commit per 24 bytes, then the length.
    fn piecewise(&mut self, owner: u32, name: &str, data: &[u8]) -> Result<(), u32> {
        for (i, chunk) in data.chunks(24).enumerate() {
            self.call(owner, message(EXECUTE, FILE_WRITE, name, (i * 24) as u32, chunk, 0))?;
        }
        self.call(owner, message(EXECUTE, FILE_TRUNCATE, name, data.len() as u32, &[], 0)).map(|_| ())
    }
    fn delete(&mut self, owner: u32, name: &str) -> Result<(), u32> {
        self.call(owner, message(EXECUTE, FILE_DELETE, name, 0, &[], 0)).map(|_| ())
    }
    /// Every file of an owner, read as the Open Runtime loads /data.
    pub fn list(&mut self, owner: u32) -> Result<BTreeMap<String, Vec<u8>>, u32> {
        let mut out = BTreeMap::new();
        for slot in 0..if owner == OPEN { 32 } else { 4 } {
            let entry = self.call(owner, message(GET, FILE_LIST, "", slot, &[], 0))?;
            let raw = entry.text();
            let end = raw[..16].iter().position(|&b| b == 0).unwrap_or(16);
            if end == 0 {
                continue;
            }
            let name = String::from_utf8(raw[..end].to_vec()).unwrap();
            let mut data = Vec::new();
            while data.len() < entry.value as usize {
                let r = self.call(owner, message(GET, FILE_READ, &name, data.len() as u32, &[], 0))?;
                assert!(r.value > 0);
                data.extend_from_slice(&r.text()[..r.value as usize]);
            }
            assert!(out.insert(name, data).is_none(), "a name is stored twice");
        }
        Ok(out)
    }
}
pub fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(7) ^ seed ^ (i >> 8) as u8).collect()
}
pub fn blank() -> TestDisk {
    TestDisk::from_image(vec![0; 1200 * 512])
}

#[test]
fn staged_replacement_reads_back_and_costs_one_commit() {
    let dev = blank();
    let mut s = Store::new(&dev);
    dev.record();
    let v1 = bytes(1, 4096);
    s.replace(OPEN, "settings.json", &v1).unwrap();
    assert_eq!(s.list(OPEN).unwrap()["settings.json"], v1);
    // Eight data sectors, the entry, and two flushes: not one per 24 bytes.
    assert_eq!(dev.log_len(), 8 + 1 + 2);
    for (len, seed) in [(0, 2), (1, 3), (511, 4), (512, 5), (513, 6), (24, 7), (4095, 8), (4096, 9)] {
        let v = bytes(seed, len);
        s.replace(OPEN, "settings.json", &v).unwrap();
        assert_eq!(s.list(OPEN).unwrap()["settings.json"], v, "{len} bytes");
    }
    // The other owners and files are untouched, and the old calls still work.
    s.piecewise(3, "native", &bytes(9, 100)).unwrap();
    s.replace(3, "second", &bytes(10, 600)).unwrap();
    assert_eq!(s.list(3).unwrap().len(), 2);
    assert_eq!(s.list(OPEN).unwrap().len(), 1);
    // Over 4 KiB is refused and leaves the file as it was.
    assert_eq!(s.replace(OPEN, "settings.json", &bytes(1, 4097)), Err(INVALID));
    assert_eq!(s.list(OPEN).unwrap()["settings.json"], bytes(9, 4096));
    // 32 files for the open apps, then no room; a full directory still
    // lets an existing file be replaced.
    for k in 1..32 {
        s.replace(OPEN, &format!("f{k}"), &bytes(k as u8, 50 + k)).unwrap();
    }
    assert_eq!(s.replace(OPEN, "one-too-many", b"x"), Err(EMPTY));
    s.replace(OPEN, "f31", b"again").unwrap();
    assert_eq!(s.list(OPEN).unwrap()["f31"], b"again");
    assert_eq!(s.list(OPEN).unwrap().len(), 32);
}

#[test]
fn a_replacement_out_of_step_is_dropped_and_the_file_kept() {
    let dev = blank();
    let mut s = Store::new(&dev);
    let old = bytes(1, 1000);
    s.replace(OPEN, "a", &old).unwrap();
    let stage = |s: &mut Store, owner, name: &str, at: u32, data: &[u8]| s.call(owner, message(EXECUTE, FILE_WRITE, name, at, data, FILE_STAGED)).map(|_| ());
    let commit = |s: &mut Store, owner, name: &str, len: u32| s.call(owner, message(EXECUTE, FILE_TRUNCATE, name, len, &[], FILE_STAGED)).map(|_| ());
    // A commit with nothing staged, at a wrong length, or after a gap.
    assert_eq!(commit(&mut s, OPEN, "a", 500), Err(EMPTY));
    stage(&mut s, OPEN, "a", 0, &[1; 24]).unwrap();
    assert_eq!(commit(&mut s, OPEN, "a", 23), Err(INVALID));
    assert_eq!(commit(&mut s, OPEN, "a", 24), Err(EMPTY)); // it was dropped
    stage(&mut s, OPEN, "a", 0, &[1; 24]).unwrap();
    assert_eq!(stage(&mut s, OPEN, "a", 48, &[1; 24]), Err(INVALID));
    // Another owner's replacement does not disturb this one; a second file
    // of the same owner takes its place.
    stage(&mut s, OPEN, "a", 0, &[2; 24]).unwrap();
    stage(&mut s, 3, "b", 0, &[3; 24]).unwrap();
    commit(&mut s, 3, "b", 24).unwrap();
    commit(&mut s, OPEN, "a", 24).unwrap();
    assert_eq!(s.list(OPEN).unwrap()["a"], [2; 24]);
    s.replace(OPEN, "a", &old).unwrap();
    stage(&mut s, OPEN, "a", 0, &[2; 24]).unwrap();
    stage(&mut s, OPEN, "z", 0, &[3; 24]).unwrap();
    assert_eq!(commit(&mut s, OPEN, "a", 24), Err(EMPTY));
    assert_eq!(dispatch(&mut s.disk, &mut s.stages, 8, message(GET, FILE_LIST, "", 0, &[], 0)).err(), Some(DENIED));
    // A plain write by the same owner voids its replacement.
    stage(&mut s, OPEN, "a", 0, &[4; 24]).unwrap();
    s.call(OPEN, message(EXECUTE, FILE_WRITE, "c", 0, b"plain", 0)).unwrap();
    assert_eq!(commit(&mut s, OPEN, "a", 24), Err(EMPTY));
    // Starting over at 0 always works, whatever was staged before.
    stage(&mut s, OPEN, "a", 0, &[5; 24]).unwrap();
    stage(&mut s, OPEN, "a", 0, &[6; 10]).unwrap();
    assert_eq!(s.list(OPEN).unwrap()["a"], old); // nothing shows before the commit
    commit(&mut s, OPEN, "a", 10).unwrap();
    assert_eq!(s.list(OPEN).unwrap()["a"], [6; 10]);
    assert_eq!(s.list(OPEN).unwrap()["c"], b"plain");
}

/// Run `work`, noting every owner's files after each step, then cut the power
/// at every write and flush. Returns the first cut after which the files were
/// neither those before the step in flight nor those after it.
fn cut_everywhere(work: &dyn Fn(&mut Store, &mut dyn FnMut(&mut Store))) -> Result<usize, String> {
    let dev = blank();
    let mut s = Store::new(&dev);
    s.replace(OPEN, "keep", &bytes(42, 3000)).unwrap();
    s.piecewise(3, "native", &bytes(43, 700)).unwrap();
    dev.record();
    let all = |s: &mut Store| (s.list(OPEN).unwrap(), s.list(3).unwrap());
    // (log length when the step returned, the files then)
    let mut states = vec![(0, all(&mut s))];
    work(&mut s, &mut |s| states.push((dev.log_len(), all(s))));
    let base = dev.0.borrow().base.clone();
    let log = std::mem::take(&mut dev.0.borrow_mut().log);
    let mut failure = None;
    let images = sweep(&base, &log, |cut, image| {
        if failure.is_some() {
            return;
        }
        let mut s = Store::new(&TestDisk::from_image(image));
        let done = states.iter().filter(|st| st.0 <= cut).count() - 1;
        let now = match (s.list(OPEN), s.list(3)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                failure = Some(format!("cut {cut}: a file does not read back"));
                return;
            }
        };
        if now != states[done].1 && states.get(done + 1).map_or(true, |st| now != st.1) {
            failure = Some(format!("cut at device operation {cut} of {}: the files are neither those of step {done} nor of the next", log.len()));
            return;
        }
        // And the store still works.
        s.replace(OPEN, "after", b"still works").unwrap();
        assert_eq!(s.list(OPEN).unwrap()["after"], b"still works");
    });
    match failure {
        Some(e) => Err(e),
        None => Ok(images),
    }
}

#[test]
fn power_cut_during_a_staged_replacement_leaves_the_old_or_the_new_file() {
    let images = cut_everywhere(&|s, done| {
        s.replace(OPEN, "a", &bytes(1, 4096)).unwrap();
        done(s);
        s.replace(OPEN, "b", &bytes(2, 100)).unwrap();
        done(s);
        s.replace(OPEN, "a", &bytes(3, 3000)).unwrap(); // shorter
        done(s);
        s.replace(OPEN, "a", &bytes(4, 4096)).unwrap();
        done(s);
        s.replace(OPEN, "keep", &[]).unwrap();
        done(s);
        s.replace(3, "native", &bytes(5, 1500)).unwrap();
        done(s);
        s.delete(OPEN, "b").unwrap();
        done(s);
        s.replace(OPEN, "c", &bytes(6, 513)).unwrap();
        done(s);
    })
    .unwrap();
    assert!(images > 300, "{images}");
    println!("{images} power-cut images");
}

/// What the staged replacement fixes: the same change made 24 bytes at a
/// time is a row of separate commits, and the sweep finds a cut that leaves
/// a file which is neither the old one nor the new one.
#[test]
fn the_sweep_shows_piecewise_writes_are_not_atomic() {
    let e = cut_everywhere(&|s, done| {
        s.piecewise(OPEN, "keep", &bytes(1, 200)).unwrap();
        done(s);
    })
    .unwrap_err();
    println!("24 bytes at a time: {e}");
}
