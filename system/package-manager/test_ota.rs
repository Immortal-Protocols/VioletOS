//! Host tests for the OTA manifest check (ota.rs) with the real BearSSL built
//! for the host and manifests signed through openssl by
//! tools/test_ota_bundle.py. Run by test_ota.sh (from `make test-unit`).
include!(env!("VIOLET_PACKAGE_KEYS_RS"));
#[path = "ota.rs"]
mod ota;
use ota::{check, matches, Refusal};

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
    std::fs::read(format!("{}/{name}", std::env::var("OTA_FIXTURES").unwrap())).unwrap()
}
const SLOT: u32 = 64;
fn verdict(bytes: &[u8], floor: u32) -> Result<ota::Manifest, Refusal> {
    check(bytes, TARGET, KEYS, floor, SLOT, sha256, p256)
}

#[test]
fn signed_manifests_are_accepted_with_their_fields() {
    let m = verdict(&fixture("good.manifest"), 1).unwrap();
    assert_eq!((m.key_id, m.version, m.layout, m.image_len), (2, 3, ota::LAYOUT_QEMU, 5124));
    assert_eq!(m.sectors(), 11);
    // The digest is SHA-256 of the image padded to whole sectors.
    assert_eq!(m.digest, sha256(&fixture("image.bin")));
    assert_eq!(matches(&m, &sha256(&fixture("image.bin"))), Ok(()));
    let r = verdict(&fixture("release.manifest"), 7).unwrap();
    assert_eq!((r.key_id, r.version, r.layout), (1, 7, ota::LAYOUT_ANDROID));
    assert_eq!(RELEASE, false);
}

#[test]
fn every_signed_byte_is_covered() {
    let good = fixture("good.manifest");
    for at in 0..ota::MANIFEST {
        for bit in [0, 3, 7] {
            let mut bad = good.clone();
            bad[at] ^= 1 << bit;
            assert!(verdict(&bad, 1).is_err(), "flip at {at} bit {bit} accepted");
        }
    }
    for at in ota::SIGNED..ota::MANIFEST {
        let mut bad = good.clone();
        bad[at] ^= 0x20;
        assert_eq!(verdict(&bad, 1), Err(Refusal::Signature), "signature byte {at}");
    }
}

#[test]
fn rollback_is_refused() {
    assert!(verdict(&fixture("good.manifest"), 3).is_ok());
    assert_eq!(verdict(&fixture("good.manifest"), 4), Err(Refusal::Version));
    assert_eq!(verdict(&fixture("version0.manifest"), 0), Err(Refusal::Version));
    // Without VIOLET_SYSTEM_VERSION the running system is version 1.
    if std::env::var("VIOLET_SYSTEM_VERSION").is_err() {
        assert_eq!(ota::system_version(), 1);
    }
}

#[test]
fn foreign_and_malformed_manifests_are_refused() {
    assert_eq!(verdict(&fixture("other-target.manifest"), 1), Err(Refusal::Target));
    assert_eq!(verdict(&fixture("unknown-key.manifest"), 1), Err(Refusal::Key));
    assert_eq!(verdict(&fixture("wrong-key.manifest"), 1), Err(Refusal::Signature));
    assert_eq!(verdict(&fixture("reserved.manifest"), 1), Err(Refusal::Format));
    assert_eq!(verdict(&fixture("layout.manifest"), 1), Err(Refusal::Format));
    assert_eq!(verdict(&fixture("empty.manifest"), 1), Err(Refusal::Size));
    assert_eq!(verdict(&fixture("huge.manifest"), 1), Err(Refusal::Size));
    let good = fixture("good.manifest");
    assert_eq!(verdict(&good[..127], 1), Err(Refusal::Format));
    assert_eq!(verdict(&[0u8; 512], 1), Err(Refusal::Format));
    // A package's magic in front of the same signed bytes.
    let mut vpk = good.clone();
    vpk[..4].copy_from_slice(b"VPK2");
    assert_eq!(verdict(&vpk, 1), Err(Refusal::Format));
}

#[test]
fn the_image_must_fit_the_slot_and_match_what_was_read_back() {
    let good = fixture("good.manifest");
    // 11 image sectors and the manifest sector.
    assert!(check(&good, TARGET, KEYS, 1, 12, sha256, p256).is_ok());
    assert_eq!(check(&good, TARGET, KEYS, 1, 11, sha256, p256), Err(Refusal::Size));
    assert_eq!(check(&good, TARGET, KEYS, 1, 0, sha256, p256), Err(Refusal::Size));
    let m = verdict(&good, 1).unwrap();
    let mut other = sha256(&fixture("image.bin"));
    other[31] ^= 1;
    assert_eq!(matches(&m, &other), Err(Refusal::Digest));
}

#[test]
fn the_slot_is_marked_only_after_a_healthy_period_and_retried_until_it_is() {
    use ota::{Mark, MARK_TRIES};
    const S: u32 = 1000; // ticks per second
    let mut m = Mark::new();
    // Not before the health period has passed, however long the boot runs.
    for now in [0, 5 * S, 19 * S, u32::MAX / 2] {
        assert!(!m.due(false, now));
    }
    assert!(m.due(true, 21 * S));
    // Storage failed (a read-back error, busy): asked again 4 s later, not before.
    m.result(false, 21 * S, S);
    assert!(!m.due(true, 22 * S));
    assert!(m.due(true, 25 * S));
    m.result(false, 25 * S, S);
    assert!(!m.due(true, 32 * S));
    assert!(m.due(true, 33 * S));
    // Done once it worked: no more requests this boot.
    m.result(true, 33 * S, S);
    assert!(m.done());
    assert!(!m.due(true, 100 * S));
    m.restart();
    assert!(!m.due(true, 200 * S), "already marked: nothing to ask after a restart");

    // A restart of the components before the mark: wait for health again.
    let mut m = Mark::new();
    assert!(m.due(true, 30 * S));
    m.result(false, 30 * S, S);
    m.restart();
    for now in [31 * S, 40 * S, 60 * S] {
        assert!(!m.due(false, now), "not healthy since the restart");
    }
    assert!(m.due(true, 61 * S), "healthy again: asked at once");

    // Bounded: a build with no slot metadata is asked MARK_TRIES times.
    let mut m = Mark::new();
    let mut asked = 0;
    let mut now = 0u32;
    for _ in 0..10_000 {
        if m.due(true, now) {
            asked += 1;
            m.result(false, now, S);
        }
        now = now.wrapping_add(S);
    }
    assert_eq!(asked, MARK_TRIES);
    // The timer wraps: a request is still due after the wait.
    let mut m = Mark::new();
    assert!(m.due(true, u32::MAX - S));
    m.result(false, u32::MAX - S, S);
    assert!(!m.due(true, u32::MAX));
    assert!(m.due(true, 3 * S));
}

#[test]
fn the_version_note_round_trips_and_rejects_lookalikes() {
    for v in [1, 2, 7, 999_999_999] {
        let n = ota::version_note(v);
        assert_eq!(&n[..12], b"VIOLETSYSVER");
        assert_eq!(ota::note_version(&n), Some(v));
    }
    let mut n = ota::version_note(3);
    n[16] ^= 1;
    assert_eq!(ota::note_version(&n), None, "the complement must match");
    assert_eq!(ota::note_version(&ota::version_note(0)), None);
    // tools/ota_bundle.py reads the same bytes.
    let tool = include_str!("../../tools/ota_bundle.py");
    assert!(tool.contains(r#"VERSION_NOTE = b"VIOLETSYSVER""#));
}
