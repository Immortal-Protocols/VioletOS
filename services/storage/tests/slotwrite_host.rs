//! Host tests for the boot slot writer (slotwrite.rs) and its SHA-256:
//! rustc --edition=2021 --test services/storage/tests/slotwrite_host.rs
#[path = "../slotwrite.rs"]
mod slotwrite;
use slotwrite::sha256::Sha256;
use slotwrite::slot::{Control, Slot, ACTIVE_TRIES};
use slotwrite::{Error, Geometry, Medium, Sector, Updater, HASH_STEP};

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}
fn sha(data: &[u8]) -> String {
    let mut s = Sha256::new();
    s.update(data);
    hex(&s.finish())
}

#[test]
fn sha256_known_answers() {
    // FIPS 180-4 examples, and two more from Python's hashlib.
    assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(sha(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(sha(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    assert_eq!(sha(&vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    let ramp: Vec<u8> = (0..768).map(|i| i as u8).collect();
    assert_eq!(sha(&ramp), "f3a25aa93aa2fbba28d79260535bbd6a5eb0fc1c24a8b0f04e12b484c1dfe363");
    // Any split of the input gives the same digest.
    for cut in [0, 1, 55, 56, 63, 64, 65, 500, 767] {
        let mut s = Sha256::new();
        s.update(&ramp[..cut]);
        s.update(&ramp[cut..]);
        assert_eq!(hex(&s.finish()), "f3a25aa93aa2fbba28d79260535bbd6a5eb0fc1c24a8b0f04e12b484c1dfe363", "cut {cut}");
    }
}

const GEO: Geometry = Geometry { control: 4, first: [16, 16 + 40], sectors: 40 };

#[derive(Debug, Clone, PartialEq)]
enum Op {
    Write(u32),
    Flush,
}

struct Disk {
    sectors: Vec<Sector>,
    log: Vec<Op>,
    fail_read: Option<u32>,
    fail_write: Option<u32>,
}
impl Disk {
    fn new(control: Option<Control>) -> Disk {
        let mut sectors = vec![[0u8; 512]; 16 + 80 + 4];
        if let Some(c) = control {
            sectors[4][..32].copy_from_slice(&c.encode());
        }
        Disk { sectors, log: Vec::new(), fail_read: None, fail_write: None }
    }
    fn control(&self) -> Control {
        Control::decode(&self.sectors[4]).unwrap()
    }
}
impl Medium for Disk {
    fn read(&mut self, lba: u32) -> Result<Sector, ()> {
        if self.fail_read == Some(lba) {
            return Err(());
        }
        self.sectors.get(lba as usize).copied().ok_or(())
    }
    fn write(&mut self, lba: u32, s: &mut Sector) -> Result<(), ()> {
        if self.fail_write == Some(lba) {
            return Err(());
        }
        self.log.push(Op::Write(lba));
        *self.sectors.get_mut(lba as usize).ok_or(())? = *s;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.log.push(Op::Flush);
        Ok(())
    }
}

fn image(n: usize, seed: u8) -> Vec<Sector> {
    (0..n).map(|i| {
        let mut s = [0u8; 512];
        for (k, b) in s.iter_mut().enumerate() {
            *b = seed ^ (i as u8).wrapping_mul(31) ^ (k as u8);
        }
        s
    }).collect()
}
fn digest_of(sectors: &[Sector]) -> [u8; 32] {
    let mut s = Sha256::new();
    for x in sectors {
        s.update(x);
    }
    s.finish()
}
fn running_a() -> Disk {
    Disk::new(Some(Control::initial(0)))
}

#[test]
fn nothing_is_allowed_without_metadata_or_a_running_slot() {
    let damaged = {
        let mut d = running_a();
        d.sectors[4][12] ^= 1; // CRC no longer matches
        d
    };
    let unnamed = {
        // A valid record whose slot_suffix names no slot.
        let mut d = running_a();
        let mut raw = d.sectors[4];
        raw[1] = b'z';
        let crc = slotwrite::slot::crc32(&raw[..28]);
        raw[28..32].copy_from_slice(&crc.to_le_bytes());
        assert!(Control::decode(&raw).is_ok_and(|c| c.suffix().is_none()));
        d.sectors[4] = raw;
        d
    };
    for mut d in [Disk::new(None), damaged, unnamed] {
        let mut u = Updater::from_control(Some(GEO), &mut d);
        assert_eq!(u.capacity(), 0);
        assert_eq!(u.write(&mut d, 0, &mut image(1, 1)), Err(Error::Unavailable));
        assert_eq!(u.mark_successful(&mut d), Err(Error::Unavailable));
        assert!(d.log.is_empty());
    }
    let mut d = running_a();
    let mut u = Updater::new(Some(GEO), None);
    assert_eq!(u.write(&mut d, 0, &mut image(1, 1)), Err(Error::Unavailable));
    let mut u = Updater::new(None, Some(0));
    assert_eq!(u.hash(&mut d, 0, 1), Err(Error::Bounds));
}

#[test]
fn the_inactive_slot_is_made_unbootable_before_any_image_byte() {
    let mut d = running_a();
    let mut u = Updater::from_control(Some(GEO), &mut d);
    assert_eq!(u.running(), Some(0));
    assert_eq!(u.capacity(), 40);
    u.write(&mut d, 0, &mut image(3, 7)).unwrap();
    // Metadata write, its flush, then the image into B (sector 56 on).
    assert_eq!(d.log, vec![Op::Write(4), Op::Flush, Op::Write(56), Op::Write(57), Op::Write(58)]);
    assert!(!d.control().bootable(1));
    assert!(d.control().bootable(0));
    // Later writes in the same boot do not touch the metadata again.
    d.log.clear();
    u.write(&mut d, 3, &mut image(1, 8)).unwrap();
    assert_eq!(d.log, vec![Op::Write(59)]);
    // Slot A, the running one, is never addressed.
    assert!(d.sectors[16..56].iter().all(|s| s == &[0u8; 512]));
}

#[test]
fn writes_stay_inside_the_inactive_slot() {
    let mut d = Disk::new(Some(Control::initial(1)));
    let mut u = Updater::from_control(Some(GEO), &mut d);
    assert_eq!(u.running(), Some(1));
    assert_eq!(u.write(&mut d, 40, &mut image(1, 0)), Err(Error::Bounds));
    assert_eq!(u.write(&mut d, 39, &mut image(2, 0)), Err(Error::Bounds));
    assert_eq!(u.write(&mut d, u32::MAX, &mut image(1, 0)), Err(Error::Bounds));
    assert_eq!(u.write(&mut d, 0, &mut []), Err(Error::Bounds));
    assert!(d.log.is_empty(), "a refused write changes nothing, metadata included");
    u.write(&mut d, 38, &mut image(1, 9)).unwrap();
    // Running B: the inactive slot is A (sector 16 + 38).
    assert_eq!(d.log.last(), Some(&Op::Write(54)));
    // An image write never covers the manifest's sector.
    assert_eq!(u.write(&mut d, 38, &mut image(2, 9)), Err(Error::Bounds));
}

fn staged(seed: u8, n: usize) -> (Disk, Updater, Vec<Sector>) {
    let mut d = running_a();
    let mut u = Updater::from_control(Some(GEO), &mut d);
    let mut img = image(n, seed);
    u.write(&mut d, 0, &mut img).unwrap();
    let mut manifest = [[0x4du8; 512]; 1];
    u.write(&mut d, 39, &mut manifest).unwrap();
    (d, u, img)
}

#[test]
fn a_verified_slot_becomes_the_trial_and_is_marked_after_health() {
    let (mut d, mut u, img) = staged(3, 10);
    assert_eq!(u.manifest(&mut d).unwrap(), [0x4d; 512]);
    assert_eq!(u.hash(&mut d, 0, 4), Ok(4));
    assert_eq!(u.hash(&mut d, 4, 6), Ok(10));
    let digest = u.digest(10).unwrap();
    assert_eq!(digest, digest_of(&img));
    assert_eq!(u.digest(9), Err(Error::Digest), "the digest covers exactly what was hashed");
    u.switch(&mut d, 10, &digest).unwrap();
    let c = d.control();
    assert_eq!(c.slot(1), Slot { priority: 15, tries: ACTIVE_TRIES, successful: false, verity_corrupted: false });
    assert!(c.slot(0).successful && c.bootable(0));
    // The manifest reached the disk with the switch, in the slot's last sector.
    assert_eq!(d.sectors[56 + 39], [0x4d; 512]);
    // The bootloader starts B; B's Storage marks it once healthy.
    let mut c = d.control();
    assert_eq!(c.choose(), Some(1));
    d.sectors[4][..32].copy_from_slice(&c.encode());
    let mut b = Updater::from_control(Some(GEO), &mut d);
    assert_eq!(b.running(), Some(1));
    assert_eq!(b.mark_successful(&mut d), Ok(true));
    assert_eq!(b.mark_successful(&mut d), Ok(false));
    assert!(d.control().slot(1).successful);
}

#[test]
fn a_wrong_or_stale_digest_never_switches() {
    let (mut d, mut u, img) = staged(5, 6);
    let before = d.sectors[4];
    u.hash(&mut d, 0, 6).unwrap();
    let mut wrong = digest_of(&img);
    wrong[0] ^= 1;
    assert_eq!(u.switch(&mut d, 6, &wrong), Err(Error::Digest));
    assert_eq!(u.switch(&mut d, 5, &digest_of(&img[..5])), Err(Error::Digest));
    // A write after the hash: the old digest no longer counts.
    let good = digest_of(&img);
    u.write(&mut d, 2, &mut image(1, 99)).unwrap();
    assert_eq!(u.switch(&mut d, 6, &good), Err(Error::Digest));
    assert_eq!(d.sectors[4], before, "metadata untouched by refused switches");
    // Nothing hashed at all.
    let (mut d2, mut u2, img2) = staged(6, 2);
    assert_eq!(u2.switch(&mut d2, 2, &digest_of(&img2)), Err(Error::Digest));
}

#[test]
fn hashing_reads_back_from_the_disk_in_order() {
    let (mut d, mut u, img) = staged(8, 8);
    // What the disk holds is hashed, not what was sent: corrupt a sector on
    // the medium after the write.
    d.sectors[56 + 3][100] ^= 0xff;
    u.hash(&mut d, 0, 8).unwrap();
    assert_ne!(u.digest(8).unwrap(), digest_of(&img));
    let (mut d, mut u, _) = staged(8, 8);
    assert_eq!(u.hash(&mut d, 1, 2), Err(Error::Order));
    assert_eq!(u.hash(&mut d, 0, 40), Err(Error::Bounds), "the manifest sector is never hashed");
    assert_eq!(u.hash(&mut d, 0, HASH_STEP + 1), Err(Error::Bounds));
    // A read error starts the hash over.
    d.fail_read = Some(56 + 2);
    assert_eq!(u.hash(&mut d, 0, 4), Err(Error::Io));
    d.fail_read = None;
    assert_eq!(u.hash(&mut d, 4, 1), Err(Error::Order));
    assert_eq!(u.hash(&mut d, 0, 8), Ok(8));
}

#[test]
fn a_write_after_a_switch_makes_the_slot_unbootable_again() {
    let (mut d, mut u, img) = staged(2, 4);
    u.hash(&mut d, 0, 4).unwrap();
    u.switch(&mut d, 4, &digest_of(&img)).unwrap();
    assert!(d.control().bootable(1));
    d.log.clear();
    u.write(&mut d, 0, &mut image(1, 1)).unwrap();
    assert_eq!(d.log[0], Op::Write(4));
    assert!(!d.control().bootable(1), "a torn rewrite of the trial slot is never booted");
}

#[test]
fn an_interrupted_update_leaves_the_running_slot_in_charge() {
    let (d, _u, _) = staged(4, 12);
    // Power is cut here: a new boot of A finds B unbootable.
    let mut c = d.control();
    assert_eq!(c.choose(), Some(0));
    assert!(!c.bootable(1));
    // A metadata write that fails leaves the image untouched too.
    let mut d = running_a();
    d.fail_write = Some(4);
    let mut u = Updater::from_control(Some(GEO), &mut d);
    assert_eq!(u.write(&mut d, 0, &mut image(2, 1)), Err(Error::Metadata(slotwrite::slot::Error::Io)));
    assert!(d.sectors[56..].iter().all(|s| s == &[0u8; 512]));
}

/// Boot B as a trial of an update from A: B unproven with tries left, A proven.
fn trial_of_b() -> Disk {
    let (mut d, mut u, img) = staged(1, 6);
    u.hash(&mut d, 0, 6).unwrap();
    u.switch(&mut d, 6, &digest_of(&img)).unwrap();
    let mut c = d.control();
    assert_eq!(c.choose(), Some(1));
    d.sectors[4][..32].copy_from_slice(&c.encode());
    d
}

#[test]
fn nothing_is_written_or_switched_while_the_running_slot_is_a_trial() {
    let mut d = trial_of_b();
    let fallback = d.control().slot(0);
    assert!(fallback.successful);
    let mut u = Updater::from_control(Some(GEO), &mut d);
    assert_eq!(u.running(), Some(1));
    assert_eq!(u.proven(&mut d), Ok(false));
    d.log.clear();
    // Any image write would make A, the only known-good system, unbootable.
    assert_eq!(u.write(&mut d, 0, &mut image(2, 3)), Err(Error::Trial));
    assert!(d.log.is_empty(), "A's metadata and image untouched");
    assert_eq!(d.control().slot(0), fallback);
    // Nor can A, which holds a proven image already, be switched to by hash and apply alone.
    u.hash(&mut d, 0, 4).unwrap();
    let digest = u.digest(4).unwrap();
    assert_eq!(u.switch(&mut d, 4, &digest), Err(Error::Trial));
    assert!(!d.log.iter().any(|op| matches!(op, Op::Write(_))));
    assert_eq!(d.control().slot(0), fallback);
    // Once B is marked successful, updating A is allowed again.
    assert_eq!(u.mark_successful(&mut d), Ok(true));
    assert_eq!(u.proven(&mut d), Ok(true));
    u.write(&mut d, 0, &mut image(2, 3)).unwrap();
    assert!(!d.control().bootable(0));
}

#[test]
fn a_switch_without_a_write_still_makes_a_counted_trial() {
    // Running A; B holds an older image that was proven when it last ran.
    let mut d = running_a();
    let img = image(5, 4);
    for (k, s) in img.iter().enumerate() {
        d.sectors[56 + k] = *s;
    }
    let mut c = d.control();
    c.put(1, Slot { priority: 14, tries: 1, successful: true, verity_corrupted: false });
    d.sectors[4][..32].copy_from_slice(&c.encode());
    let mut u = Updater::from_control(Some(GEO), &mut d);
    u.hash(&mut d, 0, 5).unwrap();
    u.switch(&mut d, 5, &digest_of(&img)).unwrap();
    let b = d.control().slot(1);
    assert_eq!(b, Slot { priority: 15, tries: ACTIVE_TRIES, successful: false, verity_corrupted: false });
    // So the bootloader spends B's tries and then falls back to A.
    let mut c = d.control();
    for _ in 0..ACTIVE_TRIES {
        assert_eq!(c.choose(), Some(1));
    }
    assert_eq!(c.choose(), Some(0));
}

#[test]
fn a_refused_manifest_changes_nothing_on_the_disk() {
    // Running B (proven), A holds the previous good system as a fallback.
    let mut d = trial_of_b();
    let mut u = Updater::from_control(Some(GEO), &mut d);
    u.mark_successful(&mut d).unwrap();
    let before = d.sectors.clone();
    d.log.clear();
    let mut manifest = [[0x6du8; 512]; 1];
    u.write(&mut d, 39, &mut manifest).unwrap();
    // The supervisor reads it to check it; the disk has not changed at all.
    assert_eq!(u.manifest(&mut d).unwrap(), [0x6d; 512]);
    assert!(d.log.is_empty());
    assert_eq!(d.sectors, before);
    assert!(d.control().bootable(0), "A is still the fallback after a refused bundle");
    // A new manifest also cancels a hash, like any write.
    u.write(&mut d, 0, &mut image(2, 1)).unwrap();
    u.hash(&mut d, 0, 2).unwrap();
    u.write(&mut d, 39, &mut manifest).unwrap();
    assert_eq!(u.digest(2), Err(Error::Digest));
}
