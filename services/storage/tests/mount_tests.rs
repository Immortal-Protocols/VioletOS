//! Where /data's volume is (mount.rs, extent.rs `decide`), with the shipped
//! adapter between the filesystem and the disk driver: Storage's own
//! mount.rs over the stand-in driver (tests/block.rs), on QEMU-sized disks
//! and on one playing the handset, whose userdata extent comes and goes.
use crate::block::Disk;
use crate::extent::{self, Place};
use crate::files_tests::{bytes, Store, OPEN};
use crate::mount::{self, Mount, RECORD};
use crate::testdisk::*;
use crate::vfsproto::*;
use std::collections::BTreeMap;

/// QEMU's disk of 16 MiB: the volume goes after the installed apps.
const QEMU: usize = 32768;
/// A handset's claimed userdata, in the stand-in's numbering.
const USERDATA: Place = Place { first: 33000, count: 4096, raw: true };

struct Boot {
    m: Box<Mount>,
    d: Disk,
    win: Vec<u8>,
    epoch: u32,
}
impl Boot {
    /// Storage starting on `dev`, its driver offering `offer` (None: as QEMU's does).
    fn new(dev: &TestDisk, offer: Option<Place>) -> (Boot, String) {
        let mut d = Disk::new(dev.clone());
        if let Some(p) = offer {
            d.offer = p;
        }
        let mut m = Box::new(Mount::new());
        let why = String::from_utf8_lossy(m.open(&mut d)).into_owned();
        let mut b = Boot { m, d, win: vec![0; WINDOW], epoch: 0 };
        b.epoch = b.call(HELLO, 0, "", b"").map_or(0, |_| b.epoch_seen());
        (b, why)
    }
    fn epoch_seen(&self) -> u32 {
        answer(&self.win).epoch
    }
    fn call(&mut self, op: u32, flags: u32, path: &str, data: &[u8]) -> Result<Vec<u8>, i32> {
        let r = Req { op, flags, offset: 0, len: 0, mode: if op == READ { DATA_MAX as u32 } else { 0o644 }, time: 9, epoch: self.epoch };
        let n = encode(&mut self.win, &r, path.as_bytes(), b"", b"", data).unwrap();
        self.m.serve(&mut self.d, &mut self.win, n);
        let a = answer(&self.win);
        if a.status != 0 {
            return Err(a.status);
        }
        Ok(self.win[REPLY..REPLY + a.len].to_vec())
    }
    fn put(&mut self, path: &str, data: &[u8]) {
        self.call(PUT, FIRST | LAST, path, data).unwrap();
        self.call(COMMIT, 0, "", b"").unwrap();
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>, i32> {
        self.call(READ, 0, path, b"")
    }
}
fn record(dev: &TestDisk) -> Option<(Place, bool)> {
    let image = dev.image();
    let s: [u8; 512] = image[RECORD * 512..RECORD * 512 + 512].try_into().unwrap();
    mount::parse(&s)
}
fn region(dev: &TestDisk, p: Place) -> Vec<u8> {
    dev.image()[p.first as usize * 512..(p.first + p.count) as usize * 512].to_vec()
}
fn old_lock(dev: &TestDisk) {
    let mut s = Store::new(dev);
    s.replace(OPEN, ".lock", b"old passcode").unwrap();
    s.replace(OPEN, ".grants", b"camera=notes").unwrap();
}

#[test]
fn the_rule_for_where_data_is() {
    let small = extent::small(2048);
    let qemu = Place { first: 28672, count: 4096, raw: false };
    assert_eq!(extent::small(2047), extent::NOWHERE);
    assert_eq!(extent::decide(None, qemu, small), Ok((qemu, true)));
    assert_eq!(extent::decide(None, extent::NOWHERE, extent::NOWHERE), Err(&b"DATA NO ROOM"[..]));
    assert_eq!(extent::decide(Some((qemu, false)), qemu, small), Ok((qemu, true)), "made, not yet filled");
    assert_eq!(extent::decide(Some((qemu, true)), qemu, small), Ok((qemu, false)), "never formatted or filled again");
    // Made in the first megabyte; userdata claimed since: it stays.
    assert_eq!(extent::decide(Some((small, true)), USERDATA, small), Ok((small, false)));
    // Made on userdata; this boot could not read the claim: nothing, not the small one.
    assert_eq!(extent::decide(Some((USERDATA, true)), small, small), Err(&b"DATA PLACE GONE"[..]));
    assert_eq!(extent::decide(Some((USERDATA, false)), small, small), Err(&b"DATA PLACE GONE"[..]));
    // A QEMU disk made smaller.
    assert_eq!(extent::decide(Some((qemu, true)), small, small), Err(&b"DATA PLACE GONE"[..]));
    // Record and header round trip; anything else is no record.
    let s = mount::record(USERDATA, true);
    assert_eq!(mount::parse(&s), Some((USERDATA, true)));
    let mut torn = s;
    torn[30] ^= 1;
    assert_eq!(mount::parse(&torn), None);
    assert_eq!(mount::parse(&[0; 512]), None);
}

#[test]
fn the_first_boot_places_the_volume_moves_the_old_files_in_and_says_so() {
    let dev = TestDisk::from_image(vec![0; QEMU * 512]);
    old_lock(&dev);
    let (mut b, why) = Boot::new(&dev, None);
    assert_eq!(why, "");
    let qemu = Place { first: extent::QEMU_FIRST as u32, count: extent::QEMU_SECTORS as u32, raw: false };
    assert_eq!(record(&dev), Some((qemu, true)));
    assert_eq!(b.read(".lock"), Ok(b"old passcode".to_vec()));
    // The volume is where extent.rs says and nowhere else: through the shipped adapter.
    let header = &dev.image()[extent::QEMU_FIRST * 512..extent::QEMU_FIRST * 512 + 24];
    assert_eq!(header, b"VioletOS data volume v1\n");
    b.put(".lock", b"new passcode");
    drop(b);
    let (mut b, why) = Boot::new(&dev, None);
    assert_eq!((why.as_str(), b.read(".lock")), ("", Ok(b"new passcode".to_vec())));
}

#[test]
fn a_volume_on_userdata_is_never_replaced_by_the_small_one() {
    let dev = TestDisk::from_image(vec![0; (USERDATA.first + USERDATA.count) as usize * 512]);
    old_lock(&dev);
    let (mut b, why) = Boot::new(&dev, Some(USERDATA));
    assert_eq!(why, "");
    b.put(".lock", b"new passcode");
    b.call(REMOVE, 0, ".grants", b"").unwrap();
    b.call(COMMIT, 0, "", b"").unwrap();
    drop(b);
    // A boot whose read of the claim failed offers the first megabyte.
    let small = extent::small(2048);
    let before = region(&dev, small);
    let (mut b, why) = Boot::new(&dev, Some(small));
    assert_eq!(why, "DATA PLACE GONE");
    assert_eq!(b.read(".lock"), Err(ENODEV), "not served: the runtime keeps the phone locked");
    assert_eq!(b.call(PUT, FIRST | LAST, "x", b"y"), Err(ENODEV));
    assert_eq!(region(&dev, small), before, "the small extent is neither formatted nor filled");
    drop(b);
    // The next boot that reads the claim has everything as it was.
    let (mut b, why) = Boot::new(&dev, Some(USERDATA));
    assert_eq!(why, "");
    assert_eq!(b.read(".lock"), Ok(b"new passcode".to_vec()));
    assert_eq!(b.read(".grants"), Err(ENOENT), "a revoked grant stays revoked");
}

#[test]
fn a_volume_made_before_userdata_was_claimed_stays_where_it_is() {
    let dev = TestDisk::from_image(vec![0; (USERDATA.first + USERDATA.count) as usize * 512]);
    old_lock(&dev);
    let small = extent::small(2048);
    let (mut b, why) = Boot::new(&dev, Some(small));
    assert_eq!(why, "");
    b.put(".lock", b"set on the small volume");
    drop(b);
    let (mut b, why) = Boot::new(&dev, Some(USERDATA));
    assert_eq!(why, "");
    assert_eq!(b.read(".lock"), Ok(b"set on the small volume".to_vec()), "not the old slots again");
    assert!(region(&dev, USERDATA).iter().all(|&x| x == 0), "userdata is not touched");
}

#[test]
fn a_volume_that_is_gone_is_lost_not_made_again_from_the_old_slots() {
    let dev = TestDisk::from_image(vec![0; QEMU * 512]);
    old_lock(&dev);
    let (mut b, _) = Boot::new(&dev, None);
    b.put(".lock", b"new passcode");
    drop(b);
    // Header and superblocks gone (wear, or someone with the medium).
    let at = extent::QEMU_FIRST * 512;
    dev.0.borrow_mut().image[at..at + 64 * 512].fill(0);
    let damaged = dev.image();
    let (mut b, why) = Boot::new(&dev, None);
    assert_eq!(why, "DATA LOST");
    assert_eq!(b.read(".lock"), Err(ENODEV), "neither the old passcode nor none");
    assert_eq!(dev.image(), damaged, "nothing is formatted over it");
}

#[test]
fn a_disk_with_no_room_or_no_record_answers_enodev_not_offline() {
    // Room for the old slots (.lock among them) but not for a volume.
    let dev = TestDisk::from_image(vec![0; 1500 * 512]);
    old_lock(&dev);
    let (mut b, why) = Boot::new(&dev, None);
    assert_eq!(why, "DATA NO ROOM");
    assert_eq!(b.read(".lock"), Err(ENODEV));
    // The record cannot be read: nothing is placed or formatted.
    let dev = TestDisk::from_image(vec![0; QEMU * 512]);
    dev.fail_at(Some(0));
    let (_, why) = Boot::new(&dev, None);
    assert_eq!(why, "DATA PLACE READ");
    // A QEMU disk made smaller: its volume is out of reach.
    let dev = TestDisk::from_image(vec![0; QEMU * 512]);
    drop(Boot::new(&dev, None));
    let small = TestDisk::from_image(dev.image()[..4096 * 512].to_vec());
    let (_, why) = Boot::new(&small, None);
    assert_eq!(why, "DATA PLACE GONE");
}

#[test]
fn a_cut_during_the_first_boot_leaves_the_old_files_or_nothing_yet() {
    let dev = TestDisk::from_image(vec![0; QEMU * 512]);
    old_lock(&dev);
    dev.record();
    drop(Boot::new(&dev, None));
    let s = dev.0.borrow();
    let want = BTreeMap::from([(".lock", b"old passcode".to_vec()), (".grants", b"camera=notes".to_vec())]);
    let images = sweep(&s.base, &s.log, |cut, image| {
        let dev = TestDisk::from_image(image);
        let (mut b, why) = Boot::new(&dev, None);
        assert_eq!(why, "", "cut {cut}");
        for (name, data) in &want {
            assert_eq!(b.read(name).as_ref(), Ok(data), "cut {cut}");
        }
        assert!(record(&dev).is_some_and(|(_, done)| done), "cut {cut}");
    });
    assert!(images > 20, "{images}");
}

#[test]
fn the_old_slots_are_read_a_slot_at_a_time_as_files_rs_has_them() {
    let dev = TestDisk::from_image(vec![0; 2048 * 512]);
    let mut s = Store::new(&dev);
    let mut want = BTreeMap::new();
    for k in 0..32u8 {
        let name = format!("f{k}");
        let data = bytes(k, 4096 - k as usize * 100);
        // Some twice, so that the newer bank is the other one.
        if k % 3 == 0 {
            s.replace(OPEN, &name, b"first").unwrap();
        }
        s.replace(OPEN, &name, &data).unwrap();
        want.insert(name, data);
    }
    assert_eq!(s.list(OPEN).unwrap(), want, "files.rs itself");
    let mut d = Disk::new(dev.clone());
    let mut got = BTreeMap::new();
    let mut out = [0u8; 4096];
    for slot in 0..crate::legacy::SLOTS {
        let (name, len) = crate::legacy::file(&mut d, slot, &mut out).unwrap().unwrap();
        let n = name.iter().position(|&c| c == 0).unwrap_or(16);
        got.insert(String::from_utf8(name[..n].to_vec()).unwrap(), out[..len].to_vec());
    }
    assert_eq!(got, want);
    assert!(d.reads <= 32 * 10, "{} sector reads for 32 full slots", d.reads);
    // A page that does not match its checksum: the slot cannot be read.
    let image = dev.image();
    let at = (0..2048).find(|s| image[s * 512..(s + 1) * 512] == want["f5"][..512]).unwrap();
    dev.0.borrow_mut().image[at * 512] ^= 1;
    let mut d = Disk::new(dev.clone());
    let failed = (0..32).filter(|&k| crate::legacy::file(&mut d, k, &mut out).is_err()).count();
    assert_eq!(failed, 1, "the slot holding that page");
}
