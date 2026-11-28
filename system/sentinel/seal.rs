//! Sealing: what the Sentinel keeps on storage it does not control. The
//! consent store and the Shield's saved state are written by the Open Runtime
//! into /data, so the Sentinel hands them over only sealed with a key that
//! exists in the Sentinel's address space alone: encrypted, and authenticated
//! so that nothing but text this Sentinel's key sealed is ever taken back.
//!
//! The construction is encrypt-then-MAC from HMAC-SHA-256 alone (the one hash
//! the tree already has in Rust, services/storage/sha256.rs):
//!   keys     k_enc = HMAC(master, "enc" | label), k_mac = HMAC(master, "mac" | label)
//!   nonce    the first 16 bytes of HMAC(k_mac, "nonce" | plaintext)
//!   cipher   plaintext XOR HMAC(k_enc, nonce | block number) blocks
//!   tag      HMAC(k_mac, nonce | ciphertext)
//!   text     "vss1." and the hex of nonce | ciphertext | tag
//! The label separates uses: text sealed as one thing never opens as another.
//! The nonce is derived, not random (the Sentinel has no entropy source of
//! its own), so equal plaintexts seal to equal text and nothing else leaks.
//!
//! The master key (`master`): on QEMU it is derived from the device root key
//! (the blob tools/root_key.py makes, in a page the supervisor maps into
//! Storage and, read-only, into this process: never into the Open Runtime,
//! never in any package or update), so a sealed text opens only on the phone
//! that sealed it. The handset has no device root key yet: there the master
//! comes from a build secret compiled into this package (README.md, "The key").
//! Sealing proves where a text came from, not when: a text this phone sealed
//! earlier still opens after a restart (no monotonic counter exists).
#![allow(dead_code)]
#[path = "../../services/storage/sha256.rs"]
pub mod sha256;

use alloc::string::String;
use alloc::vec::Vec;
use sha256::Sha256;

pub const PREFIX: &str = "vss1.";
const NONCE: usize = 16;
const TAG: usize = 32;

pub fn hmac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for n in 0..32 {
        ipad[n] ^= key[n];
        opad[n] ^= key[n];
    }
    inner.update(&ipad);
    for p in parts {
        inner.update(p);
    }
    outer.update(&opad);
    outer.update(&inner.finish());
    outer.finish()
}

fn keystream(k_enc: &[u8; 32], nonce: &[u8], data: &mut [u8]) {
    for (block, chunk) in data.chunks_mut(32).enumerate() {
        let pad = hmac(k_enc, &[nonce, &(block as u32).to_le_bytes()]);
        for (b, p) in chunk.iter_mut().zip(pad) {
            *b ^= p;
        }
    }
}

fn hex(bytes: &[u8], out: &mut String) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    let t = text.as_bytes();
    if t.len() % 2 != 0 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(t.len() / 2);
    for pair in t.chunks(2) {
        out.push(digit(pair[0])? << 4 | digit(pair[1])?);
    }
    Some(out)
}

/// The 32-byte key in a device root key blob (services/storage/crypt/
/// provision.rs, tools/root_key.py): "VRK1", version 1 (u32 LE), the key, the
/// first 24 bytes of SHA-256 of those 40 bytes. None for an empty, damaged or
/// all-zero blob: then nothing is sealed or opened (fail closed).
pub fn root_key(blob: &[u8]) -> Option<[u8; 32]> {
    if blob.len() < 64 || &blob[..8] != b"VRK1\x01\0\0\0" {
        return None;
    }
    let mut h = Sha256::new();
    h.update(&blob[..40]);
    let digest = h.finish();
    let key: [u8; 32] = blob[8..40].try_into().ok()?;
    let bad = digest[..24].iter().zip(&blob[40..64]).fold(0u8, |d, (a, b)| d | (a ^ b));
    (bad == 0 && key.iter().any(|b| *b != 0)).then_some(key)
}

/// The sealing master key from a device or build secret. Its own label, so it
/// shares nothing with what Storage derives from the same root key
/// (services/storage/crypt/keys.rs uses "violet keyblock seal v1", "violet kek v1").
pub fn master(secret: &[u8; 32]) -> [u8; 32] {
    hmac(secret, &[b"violet sentinel seal v1"])
}

pub struct Sealer {
    master: [u8; 32],
}

impl Sealer {
    pub fn new(master: [u8; 32]) -> Sealer {
        Sealer { master }
    }
    fn keys(&self, label: &str) -> ([u8; 32], [u8; 32]) {
        (hmac(&self.master, &[b"enc", label.as_bytes()]), hmac(&self.master, &[b"mac", label.as_bytes()]))
    }
    pub fn seal(&self, label: &str, plain: &[u8]) -> String {
        let (k_enc, k_mac) = self.keys(label);
        let nonce = hmac(&k_mac, &[b"nonce", plain]);
        let nonce = &nonce[..NONCE];
        let mut cipher = Vec::from(plain);
        keystream(&k_enc, nonce, &mut cipher);
        let tag = hmac(&k_mac, &[nonce, &cipher]);
        let mut text = String::with_capacity(PREFIX.len() + 2 * (NONCE + cipher.len() + TAG));
        text.push_str(PREFIX);
        hex(nonce, &mut text);
        hex(&cipher, &mut text);
        hex(&tag, &mut text);
        text
    }
    /// The plaintext, or None for anything this key did not seal under `label`.
    pub fn open(&self, label: &str, text: &str) -> Option<Vec<u8>> {
        let mut raw = unhex(text.trim_end_matches('\n').strip_prefix(PREFIX)?)?;
        if raw.len() < NONCE + TAG {
            return None;
        }
        let (k_enc, k_mac) = self.keys(label);
        let tag_at = raw.len() - TAG;
        let expected = hmac(&k_mac, &[&raw[..NONCE], &raw[NONCE..tag_at]]);
        // Compare without stopping at the first difference.
        if raw[tag_at..].iter().zip(expected).fold(0u8, |d, (a, b)| d | (a ^ b)) != 0 {
            return None;
        }
        raw.truncate(tag_at);
        let mut plain = raw.split_off(NONCE);
        keystream(&k_enc, &raw, &mut plain);
        Some(plain)
    }
}
