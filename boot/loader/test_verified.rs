//! Host tests for the loader's verified-boot check (verified.rs) with the
//! real BearSSL built for the host and manifests signed through openssl by
//! tools/test_boot_manifest.py. Run by test_verified.sh (from `make test-unit`).
include!(env!("VIOLET_PACKAGE_KEYS_RS"));
#[path = "verified.rs"]
mod verified;
use verified::{check, handoff_floor, Refusal, Verified, MANIFEST, SIGNED};

extern "C" {
    fn package_sha256(data: *const u8, len: usize, out: *mut u8);
    fn package_p256_verify(key: *const u8, hash: *const u8, signature: *const u8) -> i32;
}
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    unsafe { package_sha256(data.as_ptr(), data.len(), out.as_mut_ptr()) };
    out
}
fn p256(key: &[u8; 65], hash: &[u8; 32], signature: &[u8; 64]) -> bool {
    unsafe { package_p256_verify(key.as_ptr(), hash.as_ptr(), signature.as_ptr()) == 1 }
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/{name}", std::env::var("BOOT_FIXTURES").unwrap())).unwrap()
}
struct Image {
    kernel: Vec<u8>,
    root: Vec<u8>,
    /// The /apps window: the image, then whatever else is in memory after it.
    apps: Vec<u8>,
}
fn image() -> Image {
    let mut apps = fixture("apps.bin");
    apps.extend_from_slice(&[0x5a; 300]);
    Image { kernel: fixture("kernel.bin"), root: fixture("root.bin"), apps }
}
fn verdict_with(manifest: &[u8], i: &Image, floor: u32, release: bool) -> Result<Verified, Refusal> {
    check(manifest, TARGET, KEYS, floor, release, &i.kernel, &i.root, &i.apps, sha256, p256)
}
fn verdict(name: &str, floor: u32, release: bool) -> Result<Verified, Refusal> {
    verdict_with(&fixture(name), &image(), floor, release)
}

#[test]
fn a_signed_image_boots_with_its_fields() {
    let apps = fixture("apps.bin").len() as u32;
    assert_eq!(verdict("bound.manifest", 0, false), Ok(Verified { key_id: 2, version: 3, apps: Some(apps) }));
    assert_eq!(verdict("bound.manifest", 3, true), Ok(Verified { key_id: 2, version: 3, apps: Some(apps) }));
    assert_eq!(verdict("unbound.manifest", 3, false), Ok(Verified { key_id: 2, version: 3, apps: None }));
    assert_eq!(verdict("release.manifest", 7, true), Ok(Verified { key_id: 1, version: 7, apps: Some(apps) }));
}

#[test]
fn every_manifest_byte_is_covered() {
    let (good, i) = (fixture("bound.manifest"), image());
    assert_eq!(good.len(), MANIFEST);
    for at in 0..MANIFEST {
        for bit in [0, 3, 7] {
            let mut bad = good.clone();
            bad[at] ^= 1 << bit;
            assert!(verdict_with(&bad, &i, 0, false).is_err(), "flip at {at} bit {bit} accepted");
        }
    }
    for at in SIGNED..MANIFEST {
        let mut bad = good.clone();
        bad[at] ^= 0x20;
        assert_eq!(verdict_with(&bad, &i, 0, false), Err(Refusal::Signature), "signature byte {at}");
    }
    // A manifest cut short or with bytes after it is not one.
    assert_eq!(verdict_with(&good[..MANIFEST - 1], &i, 0, false), Err(Refusal::Format));
    let mut long = good.clone();
    long.push(0);
    assert_eq!(verdict_with(&long, &i, 0, false), Err(Refusal::Format));
    assert_eq!(verdict_with(&[], &i, 0, false), Err(Refusal::Format));
}

#[test]
fn a_tampered_kernel_root_or_apps_image_is_refused() {
    let good = fixture("bound.manifest");
    let clean = image();
    // One bit anywhere in each image (a sample of positions, first and last included).
    for at in (0..clean.kernel.len()).step_by(97).chain([clean.kernel.len() - 1]) {
        let mut i = image();
        i.kernel[at] ^= 4;
        assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Kernel), "kernel byte {at}");
    }
    for at in (0..clean.root.len()).step_by(131).chain([clean.root.len() - 1]) {
        let mut i = image();
        i.root[at] ^= 0x80;
        assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Root), "root byte {at}");
    }
    let bound = fixture("apps.bin").len();
    for at in (0..bound).step_by(61).chain([bound - 1]) {
        let mut i = image();
        i.apps[at] ^= 1;
        assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Apps), "apps byte {at}");
    }
    // Memory after the bound image is not part of it.
    let mut i = image();
    i.apps[bound] ^= 1;
    assert!(verdict_with(&good, &i, 0, false).is_ok());
    // Longer, shorter and swapped images.
    let mut i = image();
    i.kernel.push(0);
    assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Kernel));
    let mut i = image();
    i.root.pop();
    assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Root));
    let mut i = image();
    std::mem::swap(&mut i.kernel, &mut i.root);
    assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Kernel));
    // An /apps image that would run past the window it is loaded in.
    let mut i = image();
    i.apps.truncate(bound - 1);
    assert_eq!(verdict_with(&good, &i, 0, false), Err(Refusal::Apps));
}

#[test]
fn an_unbound_apps_image_is_not_checked_and_a_release_loader_refuses_it() {
    let unbound = fixture("unbound.manifest");
    let mut i = image();
    i.apps[0] ^= 1;
    // A developer loader: kernel and root are still checked.
    assert!(verdict_with(&unbound, &i, 0, false).is_ok());
    i.kernel[0] ^= 1;
    assert_eq!(verdict_with(&unbound, &i, 0, false), Err(Refusal::Kernel));
    assert_eq!(verdict("unbound.manifest", 0, true), Err(Refusal::Unbound));
    // Unbound with a length or a digest, or bound with no length: malformed, though signed.
    assert_eq!(verdict("unbound-length.manifest", 0, false), Err(Refusal::Format));
    assert_eq!(verdict("unbound-digest.manifest", 0, false), Err(Refusal::Format));
    assert_eq!(verdict("flags.manifest", 0, false), Err(Refusal::Format));
    assert_eq!(verdict("bound-empty.manifest", 0, false), Err(Refusal::Apps));
}

#[test]
fn rollback_below_the_floor_is_refused() {
    assert!(verdict("bound.manifest", 3, false).is_ok());
    assert_eq!(verdict("bound.manifest", 4, false), Err(Refusal::Version));
    assert_eq!(verdict("bound.manifest", u32::MAX, false), Err(Refusal::Version));
    assert_eq!(verdict("version0.manifest", 0, false), Err(Refusal::Version));
    // The bootloader's record: none, a good one, a damaged one.
    assert_eq!(handoff_floor([0, 0, 0]), Ok(0));
    assert_eq!(handoff_floor([verified::FLOOR_MAGIC, 5, !5]), Ok(5));
    assert_eq!(handoff_floor([verified::FLOOR_MAGIC, 0, !0]), Ok(0));
    for bad in [[verified::FLOOR_MAGIC, 5, 5], [verified::FLOOR_MAGIC, 5, 0], [1, 5, !5], [0, 5, !5], [0, 0, 1]] {
        assert_eq!(handoff_floor(bad), Err(Refusal::Version), "{bad:x?}");
    }
    // The words tools/boot_manifest.py hands QEMU are the ones read here.
    let words: Vec<u32> = fixture("floor9.words").chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(handoff_floor([words[0], words[1], words[2]]), Ok(9));
}

#[test]
fn only_pinned_keys_for_this_target_sign_a_boot() {
    assert_eq!(verdict("other-target.manifest", 0, false), Err(Refusal::Target));
    assert_eq!(verdict("unknown-key.manifest", 0, false), Err(Refusal::Key));
    assert_eq!(verdict("wrong-key.manifest", 0, false), Err(Refusal::Signature));
    // A release loader pins only the release root: the developer key is unknown to it.
    let release_only: Vec<(u32, [u8; 65])> = KEYS.iter().filter(|k| k.0 == 1).cloned().collect();
    let i = image();
    let as_release = |name: &str| check(&fixture(name), TARGET, &release_only, 0, true, &i.kernel, &i.root, &i.apps, sha256, p256);
    assert_eq!(as_release("bound.manifest"), Err(Refusal::Key));
    assert!(as_release("release.manifest").is_ok());
}

#[test]
fn other_signed_things_are_not_boot_manifests() {
    // A package and an update manifest signed by the same key, offered as the boot manifest.
    assert_eq!(verdict("package-as.manifest", 0, false), Err(Refusal::Format));
    assert_eq!(verdict("update-as.manifest", 0, false), Err(Refusal::Format));
    // The update manifest's signature under a boot manifest's fields.
    assert_eq!(verdict("update-signature.manifest", 0, false), Err(Refusal::Signature));
}
