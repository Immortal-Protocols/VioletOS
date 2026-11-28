//! The device root key as the platform hands it over.
//!
//! QEMU: a provisioning blob, made on the Mac by tools/root_key.py and kept
//! beside the disk image, never in it. tools/run.py loads it into one page
//! of guest memory (physical 0x49003000) that the supervisor maps into
//! Storage alone, at `QEMU_PAGE`. This is a stand-in for a key that hardware
//! keeps: it shows the mechanism, and protects nothing from someone who has
//! the Mac's files.
//!
//! The handset has no such source yet (docs/encryption-at-rest.md says what
//! it takes), so nothing here runs there.
//!
//! Blob, 64 bytes: "VRK1", version (1, little-endian), the 32-byte key, and
//! the first 24 bytes of SHA-256 of those 40 bytes. The digest only tells a
//! blob from an empty or damaged page; it does not authenticate anything.
use super::keys::RootKey;
use super::sha::{same, sha256, wipe};

pub const BLOB_LEN: usize = 64;
/// Where Storage sees the blob's page on QEMU.
pub const QEMU_PAGE: usize = 0x0b00_4000;

pub fn parse(blob: &[u8]) -> Option<RootKey> {
    if blob.len() < BLOB_LEN || &blob[..8] != b"VRK1\x01\0\0\0" {
        return None;
    }
    let key: &[u8; 32] = blob[8..40].try_into().unwrap();
    // An all-zero key is a blob nobody filled in.
    if !same(&sha256(&[&blob[..40]])[..24], &blob[40..64]) || key.iter().all(|b| *b == 0) {
        return None;
    }
    Some(RootKey::new(key))
}

/// Read the blob from a mapped page. The page is left as it is: Storage may
/// be restarted and must find the key again, as it would ask hardware again.
///
/// # Safety
/// `page` must be readable for `BLOB_LEN` bytes.
pub unsafe fn from_page(page: usize) -> Option<RootKey> {
    let mut blob = [0u8; BLOB_LEN];
    for (i, b) in blob.iter_mut().enumerate() {
        *b = core::ptr::read_volatile((page + i) as *const u8);
    }
    let key = parse(&blob);
    wipe(&mut blob);
    key
}

/// SHA-256 for the host tests' comparisons with the Python reference.
#[cfg(test)]
pub fn test_sha256(data: &[u8]) -> [u8; 32] {
    sha256(&[data])
}
