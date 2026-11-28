//! SHA-256 (FIPS 180-4) and HMAC-SHA-256 (RFC 2104) for the key hierarchy,
//! and the wipes. Unlike ../sha256.rs, which hashes public boot images, the
//! state here holds key material, so it is zeroed when dropped. The round
//! constants are indexed by the round number only.
use core::ptr::write_volatile;
use core::sync::atomic::{compiler_fence, Ordering};

/// Zero memory in a way the compiler may not drop as a dead store.
pub fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        unsafe { write_volatile(x, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}
pub fn wipe_words(b: &mut [u32]) {
    for x in b.iter_mut() {
        unsafe { write_volatile(x, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Equality without an early exit: the time does not say where they differ.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
const H0: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

pub struct Sha256 {
    h: [u32; 8],
    block: [u8; 64],
    used: usize,
    bytes: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 { h: H0, block: [0; 64], used: 0, bytes: 0 }
    }
    fn compress(&mut self) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([self.block[4 * i], self.block[4 * i + 1], self.block[4 * i + 2], self.block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = self.h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            self.h[i] = self.h[i].wrapping_add(v[i]);
        }
        wipe_words(&mut w);
        wipe_words(&mut v);
        self.used = 0;
    }
    pub fn update(&mut self, mut data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let n = (64 - self.used).min(data.len());
            self.block[self.used..self.used + n].copy_from_slice(&data[..n]);
            self.used += n;
            data = &data[n..];
            if self.used == 64 {
                self.compress();
            }
        }
    }
    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.bytes.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for i in 0..8 {
            out[4 * i..4 * i + 4].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }
}
impl Drop for Sha256 {
    fn drop(&mut self) {
        wipe_words(&mut self.h);
        wipe(&mut self.block);
    }
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

/// HMAC-SHA-256 of the parts, one after another. The caller owns the result
/// and wipes it if it is a key.
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut pad = [0u8; 64];
    if key.len() > 64 {
        pad[..32].copy_from_slice(&sha256(&[key]));
    } else {
        pad[..key.len()].copy_from_slice(key);
    }
    for b in pad.iter_mut() {
        *b ^= 0x36;
    }
    let mut inner = Sha256::new();
    inner.update(&pad);
    for p in parts {
        inner.update(p);
    }
    let mut digest = inner.finish();
    for b in pad.iter_mut() {
        *b ^= 0x36 ^ 0x5c;
    }
    let mut outer = Sha256::new();
    outer.update(&pad);
    outer.update(&digest);
    wipe(&mut pad);
    wipe(&mut digest);
    outer.finish()
}
