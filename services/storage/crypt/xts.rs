//! XTS-AES-256 (IEEE 1619-2007, NIST SP 800-38E) over whole 512-byte
//! sectors: 32 AES blocks, so there is no ciphertext stealing. The tweak is
//! the sector number, little-endian, encrypted under the second key and
//! multiplied by x in GF(2^128) from one block to the next, with a mask
//! instead of a branch on the bit that falls off.
//!
//! What XTS gives and does not: the same plaintext in two sectors, or in two
//! blocks of one sector, encrypts differently, and nothing about a sector is
//! readable without the key. It does not authenticate. Changing ciphertext
//! turns the 16-byte block it is in to noise when decrypted (no chosen
//! change), an old ciphertext of the same sector can be put back, and
//! someone who sees a sector twice learns which of its 16-byte blocks
//! changed in between.
use super::aes::Aes256;
use super::sha::wipe;

pub struct Xts {
    data: Aes256,
    tweak: Aes256,
}

fn times_x(t: &mut [u8; 16]) {
    let lo = u64::from_le_bytes(t[..8].try_into().unwrap());
    let hi = u64::from_le_bytes(t[8..].try_into().unwrap());
    let carry = 0u64.wrapping_sub(hi >> 63) & 0x87;
    t[..8].copy_from_slice(&((lo << 1) ^ carry).to_le_bytes());
    t[8..].copy_from_slice(&((hi << 1) | (lo >> 63)).to_le_bytes());
}

impl Xts {
    /// `key`: the data key, then the tweak key.
    pub fn new(key: &[u8; 64]) -> Xts {
        Xts { data: Aes256::new(key[..32].try_into().unwrap()), tweak: Aes256::new(key[32..].try_into().unwrap()) }
    }

    /// Encrypt or decrypt sector number `n` in place. `sector` is a whole
    /// number of 32-byte pairs (512 bytes in use).
    pub fn sector(&self, n: u64, sector: &mut [u8], decrypt: bool) {
        debug_assert!(sector.len() % 32 == 0);
        let mut first = [0u8; 32];
        first[..8].copy_from_slice(&n.to_le_bytes());
        self.tweak.pair(&mut first, false);
        let mut t = [0u8; 32];
        t[..16].copy_from_slice(&first[..16]);
        wipe(&mut first);
        for pair in sector.chunks_exact_mut(32) {
            let mut next: [u8; 16] = t[..16].try_into().unwrap();
            times_x(&mut next);
            t[16..].copy_from_slice(&next);
            for (b, t) in pair.iter_mut().zip(&t) {
                *b ^= t;
            }
            self.data.pair(pair, decrypt);
            for (b, t) in pair.iter_mut().zip(&t) {
                *b ^= t;
            }
            times_x(&mut next);
            t[..16].copy_from_slice(&next);
            wipe(&mut next);
        }
        wipe(&mut t);
    }
}
