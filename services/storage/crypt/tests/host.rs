//! Host tests for encryption at rest: `make test-unit` builds this file with
//! `rustc --test`. The crypt module, VioletFS and the block device trait are
//! the service's own sources, unchanged; the disk is the one that can lose
//! power (../../tests/testdisk.rs). Every test has its own disk in memory, so
//! they run in parallel. The primitives' known answers are in ../kat.rs.
#![allow(dead_code, unused_imports)]
#[path = "../../blockdev.rs"]
mod blockdev;
#[path = "../mod.rs"]
mod crypt;
#[path = "../../tests/testdisk.rs"]
mod testdisk;
#[path = "../../violetfs/mod.rs"]
mod violetfs;

use blockdev::BlockDev;
use crypt::{Error, NewPin, PinSecret, RootKey, Throttle, VolumeKey};
use testdisk::{sweep, TestDisk};
use violetfs::Fs;

const SECTORS: usize = 2048;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn digest(b: &[u8]) -> String {
    hex(&crypt::provision::test_sha256(b))
}
fn root() -> RootKey {
    let bytes: Vec<u8> = (0..32).collect();
    RootKey::new(bytes[..].try_into().unwrap())
}
fn entropy(seed: u8) -> [u8; 48] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(1).wrapping_add(seed))
}
fn pin(digit: u8) -> PinSecret {
    PinSecret::new(&[digit; 32])
}
fn new_pin(secret: &PinSecret) -> NewPin<'_> {
    NewPin { secret, salt: *b"SSSSSSSSSSSSSSSS", iterations: 1000 }
}
/// A formatted disk and its volume key.
fn formatted() -> (TestDisk, VolumeKey) {
    let mut disk = TestDisk::new(SECTORS);
    let key = crypt::format(&mut disk, &root(), &mut entropy(0)).unwrap();
    (disk, key)
}
fn unlock(disk: &TestDisk, pin: Option<&PinSecret>) -> Result<VolumeKey, Error> {
    crypt::unlock(&mut disk.clone(), &root(), pin, &mut Throttle::new(), 1 << 20)
}
fn key_of(r: Result<VolumeKey, Error>) -> Result<[u8; 32], Error> {
    r.map(|k| *k.expose())
}
fn sector(disk: &TestDisk, n: usize) -> Vec<u8> {
    disk.image()[n * 512..][..512].to_vec()
}
fn poke(disk: &TestDisk, at: usize, f: impl Fn(&mut u8)) {
    f(&mut disk.0.borrow_mut().image[at]);
}

#[test]
fn a_blank_disk_is_formatted_once_and_unlocks_with_the_root_key() {
    let mut disk = TestDisk::new(SECTORS);
    assert_eq!(key_of(unlock(&disk, None)), Err(Error::Blank));
    assert_eq!(crypt::params(&mut disk, &root()).err(), Some(Error::Blank));
    let mut random = entropy(0);
    let key = crypt::format(&mut disk, &root(), &mut random).unwrap();
    assert_eq!(random, [0; 48], "the caller's random bytes are wiped");
    assert_eq!(key.expose()[..], entropy(0)[..32]);
    assert_eq!(key_of(unlock(&disk, None)), Ok(*key.expose()));
    // The key block is exactly the reference's (tools/root_key.py key_block).
    assert_eq!(digest(&sector(&disk, 0)), "a31beae4c744d28183f4cce218db2b0a1e862f918f0ae940174bd76887dc1fcb");
    assert_eq!(digest(&sector(&disk, 1)), "bf15f18f8ca670f02fa84d31eb0e1b3f3ff7333ed902fcc795c3c2e4b7dc803e");
    assert!(disk.image()[1024..4096].iter().all(|b| *b == 0));
    // The volume key is nowhere on the disk.
    let image = disk.image();
    assert!(!image.windows(16).any(|w| w == &key.expose()[..16] || w == &key.expose()[16..]));
    let p = crypt::params(&mut disk, &root()).unwrap();
    assert!(!p.pin_set && p.failures == 0);
}

#[test]
fn only_a_blank_disk_is_ever_formatted() {
    let root = root();
    let mut disk = TestDisk::new(SECTORS);
    let open = |disk: &mut TestDisk, pin: Option<&PinSecret>, random: Option<&mut [u8; 48]>| key_of(crypt::unlock_or_format(disk, &root, pin, &mut Throttle::new(), 0, random));
    assert_eq!(open(&mut disk, None, None), Err(Error::Blank));
    let key = open(&mut disk, None, Some(&mut entropy(0))).unwrap();
    // Formatted once: other random bytes later change nothing.
    assert_eq!(open(&mut disk, None, Some(&mut entropy(9))), Ok(key));
    crypt::set_pin(&mut disk, &root, None, Some(&new_pin(&pin(4))), &[3; 16], &mut Throttle::new(), 0).unwrap();
    let image = disk.image();
    assert_eq!(open(&mut disk, None, Some(&mut entropy(9))), Err(Error::PinNeeded));
    assert_eq!(open(&mut disk, Some(&pin(1)), Some(&mut entropy(9))), Err(Error::WrongPin { failures: 1 }));
    assert_eq!(disk.image()[64..96], image[64..96], "the wrapped key is the one made at first");
    assert_eq!(open(&mut disk, Some(&pin(4)), Some(&mut entropy(9))), Ok(key));
    // Damage, or a disk from another device: an error, and the disk as it was.
    for at in [8usize, 70, 130] {
        let mut bad = TestDisk::from_image(image.clone());
        poke(&bad, at, |b| *b ^= 1);
        poke(&bad, 512 + at, |b| *b ^= 1);
        let before = bad.image();
        assert_eq!(open(&mut bad, Some(&pin(4)), Some(&mut entropy(9))), Err(Error::Damaged));
        assert_eq!(bad.image(), before);
    }
    let mut foreign = TestDisk::from_image(image.clone());
    let other = RootKey::new(&[9; 32]);
    assert_eq!(key_of(crypt::unlock_or_format(&mut foreign, &other, None, &mut Throttle::new(), 0, Some(&mut entropy(9)))), Err(Error::Damaged));
    assert_eq!(foreign.image(), image);
}

#[test]
fn format_refuses_bytes_that_are_not_random_and_devices_too_small() {
    let mut disk = TestDisk::new(SECTORS);
    assert_eq!(crypt::format(&mut disk, &root(), &mut [0; 48]).err(), Some(Error::Invalid));
    assert_eq!(crypt::format(&mut disk, &root(), &mut [0xa5; 48]).err(), Some(Error::Invalid));
    assert_eq!(key_of(unlock(&disk, None)), Err(Error::Blank));
    let mut small = TestDisk::new(15);
    assert_eq!(crypt::format(&mut small, &root(), &mut entropy(0)).err(), Some(Error::Small));
}

#[test]
fn another_root_key_or_a_damaged_block_never_unlocks_and_is_never_blank() {
    let (disk, key) = formatted();
    let other = RootKey::new(&[9; 32]);
    assert_eq!(key_of(crypt::unlock(&mut disk.clone(), &other, None, &mut Throttle::new(), 0)), Err(Error::Damaged));
    // The newer copy damaged: the older one still unlocks.
    poke(&disk, 512 + 70, |b| *b ^= 1);
    assert_eq!(key_of(unlock(&disk, None)), Ok(*key.expose()));
    // Any bit of any sealed byte: that copy is not used.
    for at in 0..160 {
        let (disk, key) = formatted();
        poke(&disk, at, |b| *b ^= 0x10);
        poke(&disk, 512 + at, |b| *b ^= 0x10);
        let r = key_of(unlock(&disk, None));
        // The magic too: a block whose first bytes are damaged is not a new disk.
        assert_eq!(r, Err(Error::Damaged), "byte {at}");
        drop(key);
    }
}

#[test]
fn a_passcode_is_needed_counted_and_matches_the_reference() {
    let (mut disk, key) = formatted();
    let before = disk.image()[4096..].to_vec();
    let secret = PinSecret::new(&hex_bytes("1aa3ccf34b61cdb14344034c187d105bbd5694c380e0867d0460950c46576bdc"));
    let salt: [u8; 16] = core::array::from_fn(|i| 0xa0 + i as u8);
    crypt::set_pin(&mut disk, &root(), None, Some(&new_pin(&secret)), &salt, &mut Throttle::new(), 0).unwrap();
    assert_eq!(digest(&sector(&disk, 0)), "6ed939bdfd30ce9803adbab1119fe176a88983c1fd56e1d45ee7214145d3c853");
    assert_eq!(digest(&sector(&disk, 1)), "2d76f529aaf8eee2bdab9bf632becaac56711dab6814a97bb8822167af6930e9");
    let p = crypt::params(&mut disk, &root()).unwrap();
    assert_eq!((p.pin_set, p.failures, p.pin_salt, p.pin_iterations), (true, 0, *b"SSSSSSSSSSSSSSSS", 1000));

    assert_eq!(key_of(unlock(&disk, None)), Err(Error::PinNeeded));
    assert_eq!(crypt::params(&mut disk, &root()).unwrap().failures, 0, "no passcode is no attempt");
    assert_eq!(key_of(unlock(&disk, Some(&pin(1)))), Err(Error::WrongPin { failures: 1 }));
    assert_eq!(key_of(unlock(&disk, Some(&pin(2)))), Err(Error::WrongPin { failures: 2 }));
    assert_eq!(crypt::params(&mut disk, &root()).unwrap().failures, 2);
    assert_eq!(key_of(unlock(&disk, Some(&secret))), Ok(*key.expose()));
    assert_eq!(crypt::params(&mut disk, &root()).unwrap().failures, 0);
    assert_eq!(disk.image()[4096..], before[..], "setting a passcode writes only the header");
}
fn hex_bytes(s: &str) -> [u8; 32] {
    core::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

fn with_pin(digit: u8) -> (TestDisk, VolumeKey) {
    let (mut disk, key) = formatted();
    crypt::set_pin(&mut disk, &root(), None, Some(&new_pin(&pin(digit))), &[3; 16], &mut Throttle::new(), 0).unwrap();
    (disk, key)
}

#[test]
fn wrong_passcodes_wait_and_a_restart_does_not_skip_the_wait() {
    let (mut disk, key) = with_pin(4);
    let mut t = Throttle::new();
    let mut now = 100;
    for n in 1..=5 {
        assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(0)), &mut t, now)), Err(Error::WrongPin { failures: n }));
    }
    // The sixth is not tried for 30 seconds, right or wrong, and writes nothing.
    disk.record();
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, now + 29)), Err(Error::Wait { secs: 1 }));
    assert_eq!(disk.log_len(), 0);
    now += 30;
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(0)), &mut t, now)), Err(Error::WrongPin { failures: 6 }));
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, now + 59)), Err(Error::Wait { secs: 1 }));
    // Restarted (a new Throttle, the clock back at 0): the stored count sets the wait.
    let mut t = Throttle::new();
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, 0)), Err(Error::Wait { secs: 60 }));
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, 60)), Ok(*key.expose()));
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(0)), &mut t, 60)), Err(Error::WrongPin { failures: 1 }));
    assert_eq!(crypt::backoff_secs(4), 0);
}

#[test]
fn restarting_storage_while_the_clock_runs_does_not_skip_the_wait() {
    // Storage alone restarted (a new Throttle) long after boot: the wait for
    // the stored count is counted from then, not from boot.
    let (mut disk, key) = with_pin(4);
    let mut t = Throttle::new();
    let mut now = 5_000;
    for _ in 0..12 {
        let _ = crypt::unlock(&mut disk, &root(), Some(&pin(0)), &mut t, now);
        now += 3_600;
    }
    assert_eq!(crypt::params(&mut disk, &root()).unwrap().failures, 12);
    for restart_at in [now, now + 10_000, 1 << 40] {
        let mut t = Throttle::new();
        disk.record();
        assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, restart_at)), Err(Error::Wait { secs: 3600 }));
        assert_eq!(disk.log_len(), 0, "nothing tried, nothing counted");
        assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, restart_at + 3599)), Err(Error::Wait { secs: 1 }));
    }
    let mut t = Throttle::new();
    assert!(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, u64::MAX - 5).is_err(), "no overflow past the end of time");
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut t, u64::MAX)), Ok(*key.expose()));
    assert_eq!(crypt::backoff_secs(5), 30);
    assert_eq!(crypt::backoff_secs(u32::MAX), 3600);
}

#[test]
fn an_attempt_is_durable_before_it_is_checked() {
    let (mut disk, _key) = with_pin(4);
    // The disk refuses the counter's write: the right passcode gets no key.
    disk.fail_at(Some(0));
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut Throttle::new(), 0)), Err(Error::Io));
    // The write lands but the flush fails: still no key.
    disk.fail_at(Some(1));
    assert_eq!(key_of(crypt::unlock(&mut disk, &root(), Some(&pin(4)), &mut Throttle::new(), 0)), Err(Error::Io));
    disk.fail_at(None);
    // In the log, the counter's write and flush come before anything else.
    disk.record();
    assert_eq!(key_of(unlock(&disk, Some(&pin(0)))), Err(Error::WrongPin { failures: 2 }));
    assert_eq!(disk.log_len(), 2);
    // A counter set back by hand breaks the seal of that copy.
    for copy in 0..2 {
        disk.0.borrow_mut().image[copy * 512 + 16..][..4].fill(0);
    }
    assert_eq!(crypt::params(&mut disk, &root()).err(), Some(Error::Damaged));
}

#[test]
fn changing_the_passcode_rewraps_and_never_reencrypts() {
    let (mut disk, key) = with_pin(4);
    let mut dev = crypt::open(disk.clone(), key.expose());
    let text = [0x42u8; 4096];
    dev.write(16, &text).unwrap();
    let volume = disk.image()[4096..].to_vec();
    let root = root();
    // The old passcode must be right.
    assert_eq!(crypt::set_pin(&mut disk, &root, Some(&pin(9)), Some(&new_pin(&pin(5))), &[7; 16], &mut Throttle::new(), 0), Err(Error::WrongPin { failures: 1 }));
    assert_eq!(crypt::set_pin(&mut disk, &root, None, Some(&new_pin(&pin(5))), &[7; 16], &mut Throttle::new(), 0), Err(Error::PinNeeded));
    for bad in [999, 10_000_001] {
        let p = pin(5);
        let new = NewPin { secret: &p, salt: [1; 16], iterations: bad };
        assert_eq!(crypt::set_pin(&mut disk, &root, Some(&pin(4)), Some(&new), &[7; 16], &mut Throttle::new(), 0), Err(Error::Invalid));
    }
    crypt::set_pin(dev.inner(), &root, Some(&pin(4)), Some(&new_pin(&pin(5))), &[7; 16], &mut Throttle::new(), 0).unwrap();
    assert_eq!(disk.image()[4096..], volume[..], "not one sector of the volume changed");
    assert_eq!(key_of(unlock(&disk, Some(&pin(5)))), Ok(*key.expose()));
    assert_eq!(key_of(unlock(&disk, Some(&pin(4)))), Err(Error::WrongPin { failures: 1 }));
    // Neither copy still opens with the old passcode.
    for copy in 0..2 {
        let one = TestDisk::from_image(disk.image());
        poke(&one, copy * 512 + 140, |b| *b ^= 1);
        assert!(matches!(key_of(unlock(&one, Some(&pin(4)))), Err(Error::WrongPin { .. })));
        assert_eq!(key_of(unlock(&one, Some(&pin(5)))), Ok(*key.expose()));
    }
    // The mounted device goes on reading, and removing the passcode works.
    let mut back = [0u8; 4096];
    dev.read(16, &mut back).unwrap();
    assert_eq!(back, text);
    crypt::set_pin(&mut disk, &root, Some(&pin(5)), None, &[8; 16], &mut Throttle::new(), 0).unwrap();
    assert_eq!(key_of(unlock(&disk, None)), Ok(*key.expose()));
    assert_eq!(disk.image()[4096..], volume[..]);
}

#[test]
fn a_power_cut_while_the_key_block_changes_leaves_a_passcode_that_opens_it() {
    // Changing the passcode: the old one or the new one, never neither.
    let (mut disk, key) = with_pin(4);
    disk.record();
    crypt::set_pin(&mut disk, &root(), Some(&pin(4)), Some(&new_pin(&pin(5))), &[7; 16], &mut Throttle::new(), 0).unwrap();
    let (base, images) = {
        let s = disk.0.borrow();
        let mut images = Vec::new();
        sweep(&s.base, &s.log, |cut, image| images.push((cut, image)));
        (s.base.clone(), images)
    };
    assert!(images.len() > 20);
    let (mut old, mut new) = (0, 0);
    for (cut, image) in images {
        assert_eq!(image[4096..], base[4096..]);
        let disk = TestDisk::from_image(image);
        let a = key_of(unlock(&disk, Some(&pin(4))));
        let b = key_of(unlock(&disk, Some(&pin(5))));
        match (a, b) {
            (Ok(k), Err(Error::WrongPin { .. })) => {
                old += 1;
                assert_eq!(k, *key.expose(), "cut {cut}");
            }
            (Err(Error::WrongPin { .. }), Ok(k)) => {
                new += 1;
                assert_eq!(k, *key.expose(), "cut {cut}");
            }
            other => panic!("cut {cut}: {:?}", other.0.err().zip(other.1.err())),
        }
    }
    assert!(old > 0 && new > 0);

    // A wrong attempt: the count never goes down, and the passcode still opens it.
    let (disk, key) = with_pin(4);
    assert!(unlock(&disk, Some(&pin(0))).is_err());
    disk.record();
    assert!(unlock(&disk, Some(&pin(0))).is_err());
    let s = disk.0.borrow();
    sweep(&s.base, &s.log, |cut, image| {
        let mut disk = TestDisk::from_image(image);
        let n = crypt::params(&mut disk, &root()).unwrap().failures;
        assert!(n == 1 || n == 2, "cut {cut}: {n}");
        assert_eq!(key_of(unlock(&disk, Some(&pin(4)))), Ok(*key.expose()));
    });
}

#[test]
fn the_device_below_holds_only_ciphertext() {
    let (disk, key) = formatted();
    let header = disk.image()[..4096].to_vec();
    let mut dev = crypt::open(disk.clone(), key.expose());
    assert_eq!(dev.sectors(), SECTORS as u64 - crypt::HEADER_SECTORS);
    // More sectors than one batch, at an odd place.
    let plain: Vec<u8> = (0..20 * 512).map(|i| b"violet secret "[i % 14]).collect();
    dev.write(3, &plain).unwrap();
    dev.write(0, &plain[..512]).unwrap();
    dev.flush().unwrap();
    let image = disk.image();
    assert_eq!(image[..4096], header[..], "the volume's sector 0 is not the key block");
    assert!(!image.windows(14).any(|w| w == b"violet secret "));
    assert!(image[4096 + 23 * 512..].iter().all(|b| *b == 0xa5), "nothing past the write changed");
    let mut back = vec![0u8; plain.len()];
    dev.read(3, &mut back).unwrap();
    assert_eq!(back, plain);
    // Reads of single sectors agree with the batched write.
    for i in 0..20 {
        let mut one = [0u8; 512];
        dev.read(3 + i, &mut one).unwrap();
        assert_eq!(one[..], plain[i as usize * 512..][..512]);
    }
    // The same plaintext sector at two places is two ciphertexts.
    assert_ne!(image[4096..][..512], image[4096 + 3 * 512..][..512]);
    // A second device with the same key reads it; another key does not.
    let mut again = crypt::open(disk.clone(), key.expose());
    again.read(3, &mut back).unwrap();
    assert_eq!(back, plain);
    crypt::open(disk.clone(), &[0x11; 32]).read(3, &mut back).unwrap();
    assert!(!back.windows(14).any(|w| w == b"violet secret "));
}

#[test]
fn the_device_checks_its_bounds_and_fails_closed() {
    let (disk, key) = formatted();
    let mut dev = crypt::open(disk.clone(), key.expose());
    let n = dev.sectors();
    let mut buf = [0u8; 1024];
    assert!(dev.read(n - 2, &mut buf).is_ok());
    assert!(dev.read(n - 1, &mut buf).is_err());
    assert!(dev.write(n - 1, &buf).is_err());
    assert!(dev.read(u64::MAX, &mut buf).is_err());
    assert!(dev.write(u64::MAX - 1, &buf).is_err());
    assert!(dev.read(0, &mut buf[..700]).is_err());
    assert!(dev.write(0, &buf[..100]).is_err());
    assert!(dev.discard(n - 1, 2).is_err());
    assert!(dev.discard(u64::MAX, 2).is_err());
    let before = disk.0.borrow().discarded;
    dev.discard(0, 16).unwrap();
    assert_eq!(disk.0.borrow().discarded, before + 16);
    // A failed read hands back zeros, not ciphertext or stale plaintext.
    dev.write(0, &[0x77; 1024]).unwrap();
    buf.fill(0x33);
    disk.fail_at(Some(0));
    assert!(dev.read(0, &mut buf).is_err());
    assert_eq!(buf, [0; 1024]);
    assert!(dev.write(0, &buf).is_err());
    assert!(dev.flush().is_err());
}

#[test]
fn regions_under_one_volume_key_do_not_share_a_cipher_key() {
    let (_, key) = formatted();
    let (a, b) = (TestDisk::new(64), TestDisk::new(72));
    let mut apps = crypt::open_extent(a.clone(), key.expose(), crypt::APPS).unwrap();
    let mut data = crypt::open(b.clone(), key.expose());
    assert_eq!(apps.sectors(), 64, "an extent has no header");
    apps.write(5, &[0x42; 512]).unwrap();
    data.write(5, &[0x42; 512]).unwrap();
    // The same sector number in both regions: different ciphertext.
    assert_ne!(a.image()[5 * 512..][..512], b.image()[13 * 512..][..512]);
    let mut back = [0u8; 512];
    apps.read(5, &mut back).unwrap();
    assert_eq!(back, [0x42; 512]);
    // The app extent's ciphertext moved into /data is noise to /data's key.
    let mut moved = vec![0u8; 8 * 512];
    moved.extend(a.image());
    crypt::open(TestDisk::from_image(moved), key.expose()).read(5, &mut back).unwrap();
    assert_ne!(back, [0x42; 512]);
    // An extent can never be opened under /data's key, or an unknown one.
    for purpose in [crypt::DATA, 0, 3, 0xff] {
        assert!(crypt::open_extent(a.clone(), key.expose(), purpose).is_none(), "purpose {purpose}");
    }
}

type Volume = Fs<crypt::CryptDev<TestDisk>>;
fn volume(disk: &TestDisk, key: &VolumeKey) -> Box<Volume> {
    Box::new(Fs::new(crypt::open(disk.clone(), key.expose())))
}
fn read_file(fs: &mut Volume, name: &[u8]) -> Result<Vec<u8>, violetfs::Error> {
    let ino = fs.lookup(violetfs::ROOT, name)?;
    let mut data = vec![0u8; fs.stat(ino)?.size as usize];
    fs.read(ino, 0, &mut data)?;
    Ok(data)
}

#[test]
fn violetfs_mounts_on_the_encrypted_device_and_leaves_no_plaintext() {
    let (disk, key) = with_pin(4);
    let mut fs = volume(&disk, &key);
    fs.format(0, 0).unwrap();
    let text: Vec<u8> = (0..9000).map(|i| b"pairing secret/"[i % 15]).collect();
    let ino = fs.create(violetfs::ROOT, b"agent-pairing.key", violetfs::S_FILE | 0o600, 7).unwrap();
    fs.write(ino, 0, &text).unwrap();
    fs.commit().unwrap();
    drop(fs);
    let image = disk.image();
    assert!(!image.windows(15).any(|w| w == b"pairing secret/"), "file data is on the disk in the clear");
    assert!(!image.windows(17).any(|w| w == b"agent-pairing.key"), "a file name is on the disk in the clear");

    // After a restart: unlock with the passcode, mount, read.
    let disk = TestDisk::from_image(image);
    let unlocked = unlock(&disk, Some(&pin(4))).unwrap();
    let mut fs = volume(&disk, &unlocked);
    fs.mount().unwrap();
    assert_eq!(read_file(&mut fs, b"agent-pairing.key").unwrap(), text);

    // Another volume key: the filesystem is not there, and nothing is read.
    let mut wrong = volume(&disk, &VolumeKey::new(&[0x21; 32]));
    assert!(wrong.mount().is_err());
    // A changed passcode changes nothing for the filesystem.
    crypt::set_pin(fs.device().inner(), &root(), Some(&pin(4)), Some(&new_pin(&pin(6))), &[5; 16], &mut Throttle::new(), 0).unwrap();
    fs.commit().unwrap();
    let again = unlock(&disk, Some(&pin(6))).unwrap();
    let mut fs = volume(&disk, &again);
    fs.mount().unwrap();
    assert_eq!(read_file(&mut fs, b"agent-pairing.key").unwrap(), text);
}

#[test]
fn violetfs_survives_a_power_cut_through_the_encrypted_device() {
    let (disk, key) = formatted();
    let mut fs = volume(&disk, &key);
    fs.format(0, 0).unwrap();
    let ino = fs.create(violetfs::ROOT, b"note", violetfs::S_FILE | 0o600, 7).unwrap();
    fs.write(ino, 0, &[b'o'; 6000]).unwrap();
    fs.commit().unwrap();
    disk.record();
    fs.write(ino, 0, &[b'n'; 6000]).unwrap();
    fs.create(violetfs::ROOT, b"second", violetfs::S_FILE | 0o600, 7).unwrap();
    fs.commit().unwrap();
    let s = disk.0.borrow();
    let (mut old, mut new) = (0, 0);
    let images = sweep(&s.base, &s.log, |cut, image| {
        let disk = TestDisk::from_image(image);
        let mut fs = volume(&disk, &unlock(&disk, None).unwrap());
        fs.mount().unwrap_or_else(|e| panic!("cut {cut}: {e:?}"));
        let note = read_file(&mut fs, b"note").unwrap();
        let second = fs.lookup(violetfs::ROOT, b"second").is_ok();
        if note == [b'n'; 6000] && second {
            new += 1;
        } else {
            assert!(note.iter().all(|b| *b == b'o' || *b == b'n') && note.len() == 6000, "cut {cut}");
            assert!(note == [b'o'; 6000] || fs.atomic_write() < 6000, "cut {cut}: a torn write");
            old += 1;
        }
    });
    assert!(images > 20 && old > 0 && new > 0);
}

#[test]
fn the_provisioning_blob_is_parsed_or_refused() {
    // tools/root_key.py blob(bytes(range(32))).
    let blob: Vec<u8> = (0..64)
        .map(|i| u8::from_str_radix(&"56524b3101000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1fc2cdb6c60ad47d77ebb28dccaabdcb15e6e286cce6506275"[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    assert_eq!(crypt::provision::parse(&blob).unwrap().expose(), root().expose());
    assert_eq!(unsafe { crypt::provision::from_page(blob.as_ptr() as usize) }.unwrap().expose(), root().expose());
    // An empty page (no blob given at boot), a short one, any changed bit.
    assert!(crypt::provision::parse(&[0; 4096]).is_none());
    assert!(crypt::provision::parse(&blob[..63]).is_none());
    for at in 0..64 {
        let mut bad = blob.clone();
        bad[at] ^= 0x40;
        assert!(crypt::provision::parse(&bad).is_none(), "byte {at}");
    }
    // A well-formed blob of an all-zero key is refused too.
    let mut zero = b"VRK1\x01\0\0\0".to_vec();
    zero.extend([0u8; 32]);
    let sum = crypt::provision::test_sha256(&zero);
    zero.extend(&sum[..24]);
    assert!(crypt::provision::parse(&zero).is_none());
}

/// A disk on which some sectors cannot be read.
#[derive(Clone)]
struct BadSectors(TestDisk, Vec<u64>);
impl BlockDev for BadSectors {
    fn sectors(&self) -> u64 {
        self.0.sectors()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        let n = (buf.len() / 512) as u64;
        if self.1.iter().any(|b| (lba..lba + n).contains(b)) {
            return Err(());
        }
        self.0.read(lba, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        self.0.write(lba, buf)
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.0.flush()
    }
}

#[test]
fn one_unreadable_copy_still_unlocks_and_two_never_format() {
    for pinned in [false, true] {
        let (disk, key) = if pinned { with_pin(4) } else { formatted() };
        let p = pinned.then(|| pin(4));
        for bad in [0u64, 1] {
            let image = disk.image();
            let mut dev = BadSectors(TestDisk::from_image(image.clone()), vec![bad]);
            let r = crypt::unlock_or_format(&mut dev, &root(), p.as_ref(), &mut Throttle::new(), 0, Some(&mut entropy(9)));
            assert_eq!(key_of(r), Ok(*key.expose()), "sector {bad} unreadable, pin {pinned}");
            assert!(crypt::params(&mut dev, &root()).is_ok());
        }
        // Neither copy readable: Io, never Blank, and nothing written.
        let image = disk.image();
        let mut dev = BadSectors(TestDisk::from_image(image.clone()), vec![0, 1]);
        let r = crypt::unlock_or_format(&mut dev, &root(), p.as_ref(), &mut Throttle::new(), 0, Some(&mut entropy(9)));
        assert_eq!(key_of(r), Err(Error::Io));
        assert_eq!(dev.0.image(), image);
    }
    // One copy damaged and the other unreadable: Io, nothing written.
    let (disk, _key) = formatted();
    poke(&disk, 70, |b| *b ^= 1);
    let image = disk.image();
    let mut dev = BadSectors(TestDisk::from_image(image.clone()), vec![1]);
    assert_eq!(key_of(crypt::unlock_or_format(&mut dev, &root(), None, &mut Throttle::new(), 0, Some(&mut entropy(9)))), Err(Error::Io));
    assert_eq!(dev.0.image(), image);
    // A blank disk whose reserved sectors cannot be read is not called blank.
    let mut dev = BadSectors(TestDisk::new(SECTORS), vec![5]);
    assert_eq!(key_of(crypt::unlock_or_format(&mut dev, &root(), None, &mut Throttle::new(), 0, Some(&mut entropy(9)))), Err(Error::Io));
}

#[test]
fn only_a_uniform_header_is_blank() {
    for fill in [0x00u8, 0xff, 0xa5] {
        let mut disk = TestDisk::from_image(vec![fill; SECTORS * 512]);
        assert_eq!(key_of(unlock(&disk, None)), Err(Error::Blank), "fill {fill:#x}");
        assert!(crypt::unlock_or_format(&mut disk, &root(), None, &mut Throttle::new(), 0, Some(&mut entropy(0))).is_ok());
    }
    // One byte anywhere in the eight header sectors that differs: not blank.
    for at in [0usize, 7, 511, 512, 1500, 4095] {
        let disk = TestDisk::from_image(vec![0; SECTORS * 512]);
        poke(&disk, at, |b| *b = 1);
        assert_eq!(key_of(unlock(&disk, None)), Err(Error::Damaged), "byte {at}");
    }
}

#[test]
fn a_plaintext_volume_is_never_formatted_over() {
    // VioletFS written straight onto the device (no cipher, as today's
    // pass-through volume does): its first sectors are not a key block and
    // not blank, so even with random bytes given nothing is written.
    let disk = TestDisk::from_image(vec![0; SECTORS * 512]);
    let mut fs: Box<Fs<TestDisk>> = Box::new(Fs::new(disk.clone()));
    fs.format(0, 0).unwrap();
    let ino = fs.create(violetfs::ROOT, b"kept", violetfs::S_FILE | 0o600, 7).unwrap();
    fs.write(ino, 0, b"existing data").unwrap();
    fs.commit().unwrap();
    drop(fs);
    let image = disk.image();
    let mut dev = disk.clone();
    let r = crypt::unlock_or_format(&mut dev, &root(), None, &mut Throttle::new(), 0, Some(&mut entropy(9)));
    assert_eq!(key_of(r), Err(Error::Damaged));
    assert_eq!(disk.image(), image, "the plaintext volume is byte for byte as it was");
}

#[test]
fn a_passcode_change_that_writes_one_copy_is_in_effect_and_repaired() {
    for new in [Some(5u8), None] {
        let (mut disk, key) = with_pin(4);
        // attempt: two stores (write, flush each), then the new block's
        // first store; the second store's write fails.
        disk.fail_at(Some(6));
        let p = new.map(pin);
        let np = p.as_ref().map(new_pin);
        assert_eq!(crypt::set_pin(&mut disk, &root(), Some(&pin(4)), np.as_ref(), &[7; 16], &mut Throttle::new(), 0), Ok(()));
        disk.fail_at(None);
        // The new passcode is the one in effect, the old one no longer opens.
        assert_eq!(key_of(unlock(&disk, p.as_ref())), Ok(*key.expose()));
        // That unlock wrote the other copy: with either copy alone, the old
        // passcode opens nothing.
        for copy in 0..2 {
            let one = TestDisk::from_image(disk.image());
            poke(&one, copy * 512 + 140, |b| *b ^= 1);
            let mut d = one.clone();
            assert_eq!(crypt::params(&mut d, &root()).unwrap().pin_set, new.is_some(), "copy {copy}");
            assert_eq!(key_of(unlock(&one, p.as_ref())), Ok(*key.expose()));
            if new.is_some() {
                assert!(matches!(unlock(&one, Some(&pin(4))), Err(Error::WrongPin { .. })));
            }
        }
    }
}

#[test]
fn erasing_a_volume_never_brings_its_old_key_back_after_a_power_cut() {
    let (mut disk, old) = with_pin(4);
    // Many changes, so the old block's generation is high.
    for _ in 0..20 {
        assert!(unlock(&disk, Some(&pin(4))).is_ok());
    }
    disk.record();
    let new = crypt::format(&mut disk, &root(), &mut entropy(3)).unwrap();
    assert!(disk.0.borrow().discarded >= crypt::HEADER_SECTORS);
    let final_header = disk.image()[..1024].to_vec();
    let s = disk.0.borrow();
    let (mut olds, mut news, mut blanks) = (0, 0, 0);
    // The cut points after which the new block was written: from there on
    // the old key must never come back.
    let first_new = s.log.iter().position(|op| matches!(op, testdisk::Op::Write { lba, data } if *lba < 2 && data[..] == final_header[*lba as usize * 512..][..512])).unwrap();
    let images = sweep(&s.base, &s.log, |cut, image| {
        let disk = TestDisk::from_image(image);
        match key_of(unlock(&disk, Some(&pin(4)))) {
            Ok(k) if k == *new.expose() => news += 1,
            Ok(k) => {
                assert_eq!(k, *old.expose(), "cut {cut}");
                assert!(cut <= first_new, "cut {cut}: the old key is back after the new block was written");
                olds += 1;
            }
            Err(Error::Blank) | Err(Error::Damaged) => blanks += 1,
            Err(e) => panic!("cut {cut}: {e:?}"),
        }
    });
    assert!(images > 10 && olds > 0 && news > 0 && blanks > 0, "{olds} {news} {blanks}");
}
