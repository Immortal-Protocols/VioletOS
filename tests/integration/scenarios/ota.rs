//! Full-system updates across restarts: what targets/qemu/test_ota.py
//! checked through Settings and a model bootloader, against the shipped
//! pieces themselves: the supervisor's manifest check (system/package-manager/
//! ota.rs) with the real P-256 verification (BearSSL, built for the host),
//! Storage's slot writer (services/storage/slotwrite.rs) and the slot
//! metadata and bootloader choice (targets/moto-g-pure/slot.rs `choose`).
//! The manifests are signed by tools/test_ota_bundle.py --fixtures
//! (tests/integration/run.sh makes them).
//!
//! The fakes are the disk (sectors in memory) and the restart. One real
//! slot switch across a real reboot stays in targets/qemu/test_system.py.
use crate::ota::{self, Mark, Refusal};
use crate::storage::slotwrite::slot::{Control, ACTIVE_TRIES};
use crate::storage::slotwrite::{Error, Geometry, Medium, Sector, Updater, HASH_STEP};
use std::collections::BTreeMap;

include!(env!("VIOLET_PACKAGE_KEYS_RS"));

extern "C" {
    fn package_p256_verify(key: *const u8, hash: *const u8, signature: *const u8) -> i32;
}
fn p256(key: &[u8; 65], hash: &[u8; 32], signature: &[u8; 64]) -> bool {
    unsafe { package_p256_verify(key.as_ptr(), hash.as_ptr(), signature.as_ptr()) == 1 }
}
fn fixture(name: &str) -> Vec<u8> {
    let dir = std::env::var("OTA_FIXTURES").expect("OTA_FIXTURES: run through tests/integration/run.sh");
    std::fs::read(format!("{dir}/{name}")).unwrap_or_else(|e| panic!("{dir}/{name}: {e}"))
}

/// The boot disk: bootloader_control and two slots of 64 sectors.
const GEO: Geometry = Geometry { control: 4, first: [16, 16 + 64], sectors: 64 };
#[derive(Clone, Default)]
struct Disk {
    sectors: BTreeMap<u32, Sector>,
    flushes: u32,
}
impl Medium for Disk {
    fn read(&mut self, lba: u32) -> Result<Sector, ()> {
        Ok(self.sectors.get(&lba).copied().unwrap_or([0; 512]))
    }
    fn write(&mut self, lba: u32, s: &mut Sector) -> Result<(), ()> {
        self.sectors.insert(lba, *s);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.flushes += 1;
        Ok(())
    }
}
impl Disk {
    /// A phone that has always run slot A.
    fn factory() -> Disk {
        let mut d = Disk::default();
        let mut s = [0u8; 512];
        let mut c = Control::initial(0);
        c.set_suffix(0);
        s[..32].copy_from_slice(&c.encode());
        d.sectors.insert(GEO.control, s);
        d
    }
    fn control(&mut self) -> Control {
        Control::decode(&self.read(GEO.control).unwrap()).unwrap()
    }
    /// The bootloader: choose a slot (slot.rs `choose`, which spends a try
    /// of an unproven slot) and record it. None: nothing is bootable.
    fn bootloader(&mut self) -> Option<usize> {
        let mut c = self.control();
        let chosen = c.choose();
        let mut s = self.read(GEO.control).unwrap();
        s[..32].copy_from_slice(&c.encode());
        self.sectors.insert(GEO.control, s);
        chosen
    }
}

/// One boot of the system: Storage's updater for the slot the bootloader started.
struct Boot {
    slot: usize,
    updater: Updater,
    /// The running system's version (the rollback floor of what it accepts).
    version: u32,
}
fn boot(disk: &mut Disk, versions: [u32; 2]) -> Option<Boot> {
    let slot = disk.bootloader()?;
    let updater = Updater::from_control(Some(GEO), disk);
    assert_eq!(updater.running(), Some(slot));
    Some(Boot { slot, updater, version: versions[slot] })
}

impl Boot {
    /// What Settings > Updates does with a bundle (update.rs `command`:
    /// write, manifest, hash) and then the supervisor with "update apply":
    /// check the manifest Storage holds, compare the digest Storage read
    /// back, and only then switch slots.
    fn update(&mut self, disk: &mut Disk, manifest: &[u8], image: &[u8]) -> Result<ota::Manifest, String> {
        let mut sectors: Vec<Sector> = image.chunks(512).map(|c| {
            let mut s = [0u8; 512];
            s[..c.len()].copy_from_slice(c);
            s
        }).collect();
        self.updater.write(disk, 0, &mut sectors).map_err(|e| format!("write: {e:?}"))?;
        let mut m = [[0u8; 512]];
        m[0][..manifest.len().min(512)].copy_from_slice(&manifest[..manifest.len().min(512)]);
        self.updater.write(disk, GEO.sectors - 1, &mut m).map_err(|e| format!("manifest: {e:?}"))?;
        let mut hashed = 0;
        while hashed < sectors.len() as u32 {
            hashed = self.updater.hash(disk, hashed, (sectors.len() as u32 - hashed).min(HASH_STEP)).map_err(|e| format!("hash: {e:?}"))?;
        }
        self.apply(disk)
    }
    /// The supervisor's part ("update apply").
    fn apply(&mut self, disk: &mut Disk) -> Result<ota::Manifest, String> {
        let staged = self.updater.manifest(disk).map_err(|e| format!("{e:?}"))?;
        let m = ota::check(&staged, TARGET, KEYS, self.version, GEO.sectors, crate::open::sha256, p256).map_err(|e| format!("{e:?}"))?;
        let digest = self.updater.digest(m.sectors()).map_err(|e| format!("{e:?}"))?;
        ota::matches(&m, &digest).map_err(|e| format!("{e:?}"))?;
        self.updater.switch(disk, m.sectors(), &m.digest).map_err(|e| format!("{e:?}"))?;
        Ok(m)
    }
    /// The supervisor marks the slot once the system is healthy (ota.rs `Mark`).
    fn mark_when_healthy(&mut self, disk: &mut Disk) {
        let mut mark = Mark::new();
        assert!(!mark.due(false, 0), "nothing is marked before the system is healthy");
        assert!(mark.due(true, 10));
        let ok = self.updater.mark_successful(disk).is_ok();
        mark.result(ok, 10, 1);
        assert!(mark.done());
    }
}

#[test]
fn a_signed_update_is_staged_boots_as_a_trial_and_is_marked() {
    let (good, image) = (fixture("good.manifest"), fixture("image.bin"));
    let mut disk = Disk::factory();
    let mut a = boot(&mut disk, [2, 0]).unwrap();
    assert_eq!(a.slot, 0);
    let m = a.update(&mut disk, &good, &image).expect("a signed update is accepted");
    assert_eq!((m.version, m.layout), (3, ota::LAYOUT_QEMU));
    // The image is in slot B, byte for byte, and slot A is untouched.
    for (k, chunk) in image.chunks(512).enumerate() {
        assert_eq!(&disk.read(GEO.first[1] + k as u32).unwrap()[..chunk.len()], chunk);
    }
    assert!((GEO.first[0]..GEO.first[0] + GEO.sectors).all(|lba| !disk.sectors.contains_key(&lba)));
    let c = disk.control();
    assert!(c.slot(1).priority > c.slot(0).priority && !c.slot(1).successful && c.slot(1).tries == ACTIVE_TRIES);
    // Restart: the bootloader starts B as a trial; nothing more may be staged until it is proven.
    let mut b = boot(&mut disk, [2, 3]).unwrap();
    assert_eq!(b.slot, 1);
    assert_eq!(b.updater.proven(&mut disk), Ok(false));
    assert_eq!(disk.control().slot(1).tries, ACTIVE_TRIES - 1);
    assert_eq!(b.updater.write(&mut disk, 0, &mut [[1u8; 512]]), Err(Error::Trial));
    b.mark_when_healthy(&mut disk);
    assert_eq!(b.updater.proven(&mut disk), Ok(true));
    // Proven: later restarts stay on B and spend no tries.
    for _ in 0..10 {
        assert_eq!(disk.bootloader(), Some(1));
    }
    assert!(disk.control().slot(1).successful);
}

#[test]
fn older_forged_and_foreign_bundles_are_refused_and_nothing_changes() {
    let image = fixture("image.bin");
    let mut disk = Disk::factory();
    let mut a = boot(&mut disk, [4, 0]).unwrap();
    let before = disk.control().encode();
    // Older than the running system (version 3 against a floor of 4).
    assert_eq!(a.update(&mut disk, &fixture("good.manifest"), &image), Err(format!("{:?}", Refusal::Version)));
    a.version = 2;
    // A forged signature, a key this image does not trust, another device's bundle, an image too big for the slot.
    let mut forged = fixture("good.manifest");
    forged[ota::SIGNED + 5] ^= 1;
    for (bytes, why) in [(forged, Refusal::Signature), (fixture("wrong-key.manifest"), Refusal::Signature), (fixture("unknown-key.manifest"), Refusal::Key),
                         (fixture("other-target.manifest"), Refusal::Target), (fixture("huge.manifest"), Refusal::Size), (vec![0u8; 128], Refusal::Format)] {
        assert_eq!(a.update(&mut disk, &bytes, &image), Err(format!("{why:?}")));
    }
    // A signed manifest over a different image: the digest Storage read back does not match.
    let mut tampered = image.clone();
    tampered[100] ^= 0x80;
    assert_eq!(a.update(&mut disk, &fixture("good.manifest"), &tampered), Err(format!("{:?}", Refusal::Digest)));
    // A version field raised by hand is no longer what was signed.
    let mut raised = fixture("good.manifest");
    raised[8] = 9;
    assert_eq!(a.update(&mut disk, &raised, &image), Err(format!("{:?}", Refusal::Signature)));
    // Through all of it the running slot stays the one that boots. (The
    // inactive slot was made unbootable before its first sector was written.)
    let c = disk.control();
    assert_eq!(c.slot(0), Control::decode(&before).unwrap().slot(0));
    assert!(!c.bootable(1));
    assert_eq!(disk.bootloader(), Some(0));
}

#[test]
fn a_release_that_does_not_start_falls_back_after_six_tries() {
    let (good, image) = (fixture("good.manifest"), fixture("image.bin"));
    let mut disk = Disk::factory();
    let mut a = boot(&mut disk, [2, 0]).unwrap();
    a.update(&mut disk, &good, &image).unwrap();
    // The new system never gets far enough to mark itself: six tries, then the old slot.
    for attempt in 1..=ACTIVE_TRIES {
        assert_eq!(disk.bootloader(), Some(1), "attempt {attempt}");
    }
    assert_eq!(disk.bootloader(), Some(0), "fallback to the slot that worked");
    let c = disk.control();
    assert!(!c.bootable(1) && c.slot(0).successful);
    // Back on A, proven: the update can be tried again.
    let mut again = Updater::from_control(Some(GEO), &mut disk);
    assert_eq!(again.running(), Some(0));
    assert_eq!(again.proven(&mut disk), Ok(true));
    assert!(again.write(&mut disk, 0, &mut [[2u8; 512]]).is_ok());
}

#[test]
fn a_power_cut_before_the_switch_leaves_the_running_system() {
    // Everything up to `switch` leaves slot A the one that boots, whatever
    // was written; only the metadata write changes it.
    let (good, image) = (fixture("good.manifest"), fixture("image.bin"));
    let mut disk = Disk::factory();
    let mut a = boot(&mut disk, [2, 0]).unwrap();
    let mut sectors: Vec<Sector> = image.chunks(512).map(|c| {
        let mut s = [0u8; 512];
        s[..c.len()].copy_from_slice(c);
        s
    }).collect();
    a.updater.write(&mut disk, 0, &mut sectors).unwrap();
    let mut cut = disk.clone();
    assert_eq!(cut.bootloader(), Some(0), "half an image in B never boots");
    let mut m = [[0u8; 512]];
    m[0][..good.len()].copy_from_slice(&good);
    a.updater.write(&mut disk, GEO.sectors - 1, &mut m).unwrap();
    a.updater.hash(&mut disk, 0, sectors.len() as u32).unwrap();
    let mut cut = disk.clone();
    assert_eq!(cut.bootloader(), Some(0));
    // Hashing out of order, or a digest for fewer sectors, is not a digest.
    assert_eq!(a.updater.hash(&mut disk, 3, 1), Err(Error::Order));
    assert_eq!(a.updater.digest(sectors.len() as u32 - 1), Err(Error::Digest));
    a.apply(&mut disk).unwrap();
    assert_eq!(disk.bootloader(), Some(1));
    // Writes outside the inactive slot are refused.
    let mut b = Updater::from_control(Some(GEO), &mut disk);
    b.mark_successful(&mut disk).unwrap();
    assert_eq!(b.write(&mut disk, GEO.sectors - 1, &mut [[0u8; 512]; 2]), Err(Error::Bounds));
    assert_eq!(b.write(&mut disk, GEO.sectors, &mut [[0u8; 512]]), Err(Error::Bounds));
    // Without a known running slot nothing is allowed at all.
    let mut blind = Updater::new(Some(GEO), None);
    assert_eq!(blind.write(&mut disk, 0, &mut [[0u8; 512]]), Err(Error::Unavailable));
}

extern "C" {
    /// system/package-manager/verify.rs, the entry supervisor.c's
    /// `ota_request` calls on "update check" (digest null) and "update
    /// apply" (the digest Storage read back). Built by run.sh with
    /// VIOLET_SYSTEM_VERSION=4, the image's own rollback floor.
    fn ota_verify(manifest: *const u8, slot_sectors: u32, digest: *const u8, sectors: *mut u32, why: *mut u32) -> u32;
}
/// (version, sectors, why) from the shipped entry.
fn shipped_verify(manifest: &[u8], digest: Option<[u8; 32]>) -> (u32, u32, u32) {
    let mut m = vec![0u8; ota::MANIFEST.max(manifest.len())];
    m[..manifest.len()].copy_from_slice(manifest);
    let (mut sectors, mut why) = (u32::MAX, u32::MAX);
    let d = digest.as_ref().map_or(core::ptr::null(), |d| d.as_ptr());
    let version = unsafe { ota_verify(m.as_ptr(), GEO.sectors, d, &mut sectors, &mut why) };
    (version, sectors, why)
}

#[test]
fn the_supervisors_own_check_applies_the_builds_floor_and_the_read_back_digest() {
    let image = fixture("image.bin");
    let digest = crate::open::sha256(&image);
    // Older than the image it runs in (version 3 against the build's floor of 4): refused (5).
    assert_eq!(shipped_verify(&fixture("good.manifest"), None), (0, 0, 5));
    assert_eq!(shipped_verify(&fixture("good.manifest"), Some(digest)), (0, 0, 5));
    // A newer signed release passes the check, and the apply with the
    // digest Storage read back.
    let release = fixture("release.manifest");
    let (version, sectors, why) = shipped_verify(&release, None);
    assert_eq!((version, why), (7, 0));
    assert_eq!(sectors as usize, image.len().div_ceil(512));
    assert_eq!(shipped_verify(&release, Some(digest)), (7, sectors, 0));
    // The same signed manifest with another image read back: refused (7) on apply.
    let mut other = digest;
    other[0] ^= 1;
    assert_eq!(shipped_verify(&release, Some(other)), (0, 0, 7));
    // Each refusal the supervisor reports, through the same entry.
    let mut forged = release.clone();
    forged[ota::SIGNED + 5] ^= 1;
    for (bytes, why) in [(forged, 4), (fixture("unknown-key.manifest"), 3), (fixture("other-target.manifest"), 2),
                         (fixture("version0.manifest"), 5), (vec![0u8; 512], 1)] {
        assert_eq!(shipped_verify(&bytes, None).2, why);
    }
}
