//! Known answers for the primitives (`make test-unit`).
//!
//! Published vectors: FIPS 180-4 / NIST examples for SHA-256, RFC 4231 for
//! HMAC-SHA-256, FIPS-197 appendix C.3 for AES-256, IEEE 1619-2007 annex B
//! vectors 10 to 14 for XTS-AES-256 (512-byte data units). The XTS
//! ciphertexts here were also reproduced with LibreSSL 3.3.6
//! (`openssl enc -aes-256-xts`), and the key-derivation digests with Python's
//! hashlib (tools/root_key.py, tools/test_root_key.py check the same ones).
use super::aes::Aes256;
use super::keys::{cipher_key, APPS, DATA};
use super::sha::{hmac, sha256};
use super::xts::Xts;
use std::string::String;
use std::vec::Vec;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| std::format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

#[test]
fn sha256_nist() {
    assert_eq!(hex(&sha256(&[b"abc"])), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(hex(&sha256(&[])), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        hex(&sha256(&[b"abcdbcdecdefdefgefghfghighijhijk", b"ijkljklmklmnlmnomnopnopq"])),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    let million = std::vec![b'a'; 1_000_000];
    assert_eq!(hex(&sha256(&[&million])), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
}

#[test]
fn hmac_rfc4231() {
    assert_eq!(hex(&hmac(&[0x0b; 20], &[b"Hi There"])), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    assert_eq!(
        hex(&hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"])),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // A key longer than a block is hashed first (checked against hashlib).
    let key: Vec<u8> = (0..100).collect();
    assert_eq!(hex(&hmac(&key, &[&b"violet".repeat(50)])), "240f3debe7c6408a54ad16065651f795015212eb55cdfa2db57f96038f2cc534");
}

#[test]
fn aes256_fips197() {
    let key: Vec<u8> = (0..32).collect();
    let aes = Aes256::new(key[..].try_into().unwrap());
    let plain = unhex("00112233445566778899aabbccddeeff");
    // The two blocks of a pair are independent: put the vector in each half.
    for half in 0..2 {
        let mut pair = [0x5au8; 32];
        pair[half * 16..][..16].copy_from_slice(&plain);
        aes.pair(&mut pair, false);
        assert_eq!(hex(&pair[half * 16..][..16]), "8ea2b7ca516745bfeafc49904b496089");
        let mut other = [0x5au8; 32];
        aes.pair(&mut other, false);
        assert_eq!(pair[(1 - half) * 16..][..16], other[..16]);
        aes.pair(&mut pair, true);
        assert_eq!(pair[half * 16..][..16], plain[..]);
        assert!(pair[(1 - half) * 16..][..16].iter().all(|b| *b == 0x5a));
    }
}

/// The whole S-box circuit against its definition: the inverse in GF(2^8)
/// modulo x^8+x^4+x^3+x+1, then the affine map.
#[test]
fn sbox_is_the_aes_sbox() {
    fn mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0;
        while b != 0 {
            if b & 1 != 0 {
                p ^= a;
            }
            a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
            b >>= 1;
        }
        p
    }
    for x in 0..=255u8 {
        let inv = (0..=255u8).find(|y| mul(x, *y) == 1).unwrap_or(0);
        let want = inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4) ^ 0x63;
        assert_eq!(Aes256::sbox_of(x), want, "S({x:#x})");
    }
    assert_eq!(Aes256::sbox_of(0x53), 0xed);
}

const KEY: &str = "27182818284590452353602874713526624977572470936999595749669676273141592653589793238462643383279502884197169399375105820974944592";
/// IEEE 1619-2007 vector 10: data unit 0xff, plaintext 00..ff twice.
const V10: &[&str] = &[
    "1c3b3a102f770386e4836c99e370cf9bea00803f5e482357a4ae12d414a3e63b",
    "5d31e276f8fe4a8d66b317f9ac683f44680a86ac35adfc3345befecb4bb188fd",
    "5776926c49a3095eb108fd1098baec70aaa66999a72a82f27d848b21d4a741b0",
    "c5cd4d5fff9dac89aeba122961d03a757123e9870f8acf1000020887891429ca",
    "2a3e7a7d7df7b10355165c8b9a6d0a7de8b062c4500dc4cd120c0f7418dae3d0",
    "b5781c34803fa75421c790dfe1de1834f280d7667b327f6c8cd7557e12ac3a0f",
    "93ec05c52e0493ef31a12d3d9260f79a289d6a379bc70c50841473d1a8cc81ec",
    "583e9645e07b8d9670655ba5bbcfecc6dc3966380ad8fecb17b6ba02469a020a",
    "84e18e8f84252070c13e9f1f289be54fbc481457778f616015e1327a02b140f1",
    "505eb309326d68378f8374595c849d84f4c333ec4423885143cb47bd71c5edae",
    "9be69a2ffeceb1bec9de244fbe15992b11b77c040f12bd8f6a975a44a0f90c29",
    "a9abc3d4d893927284c58754cce294529f8614dcd2aba991925fedc4ae74ffac",
    "6e333b93eb4aff0479da9a410e4450e0dd7ae4c6e2910900575da401fc07059f",
    "645e8b7e9bfdef33943054ff84011493c27b3429eaedb4ed5376441a77ed4385",
    "1ad77f16f541dfd269d50d6a5f14fb0aab1cbb4c1550be97f7ab4066193c4caa",
    "773dad38014bd2092fa755c824bb5e54c4f36ffda9fcea70b9c6e693e148c151",
];

#[test]
fn xts_aes256_ieee1619() {
    let xts = Xts::new(unhex(KEY)[..].try_into().unwrap());
    let plain: Vec<u8> = (0..512).map(|i| i as u8).collect();
    let mut s = plain.clone();
    xts.sector(0xff, &mut s, false);
    assert_eq!(hex(&s), V10.concat());
    xts.sector(0xff, &mut s, true);
    assert_eq!(s, plain);
    // Vectors 11 to 14: the first 32 and last 16 bytes of each ciphertext.
    for (unit, head, tail) in [
        (0xffffu64, "77a31251618a15e6b92d1d66dffe7b50b50bad552305ba0217a610688eff7e11", "a4f9e27b42af8100cb9d59cef9645803"),
        (0xffffff, "e387aaa58ba483afa7e8eb469778317ecf4cf573aa9d4eac23f2cdf914e4e200", "2263a1eef52dd6888c309f5a7d712826"),
        (0xffffffff, "bf53d2dade78e822a4d949a9bc6766b01b06a8ef70d26748c6a7fc36d80ae4c5", "4c82f335abb152c4a93411373aaa8220"),
        (0xffffffffff, "64497e5a831e4a932c09be3e5393376daa599548b816031d224bbf50a818ed23", "d609d273ad5b0d59631c531f6a0a57b9"),
    ] {
        let mut s = plain.clone();
        xts.sector(unit, &mut s, false);
        assert_eq!(hex(&s[..32]), head);
        assert_eq!(hex(&s[496..]), tail);
        xts.sector(unit, &mut s, true);
        assert_eq!(s, plain);
    }
}

#[test]
fn xts_separates_sectors_and_blocks() {
    let xts = Xts::new(unhex(KEY)[..].try_into().unwrap());
    let (mut a, mut b) = ([0u8; 512], [0u8; 512]);
    xts.sector(7, &mut a, false);
    xts.sector(8, &mut b, false);
    // Equal plaintext blocks: no two ciphertext blocks alike, in or across sectors.
    let mut blocks: Vec<&[u8]> = a.chunks(16).chain(b.chunks(16)).collect();
    blocks.sort();
    blocks.dedup();
    assert_eq!(blocks.len(), 64);
    // Ciphertext moved to another sector does not decrypt.
    xts.sector(8, &mut a, true);
    assert!(a.chunks(16).all(|c| c != [0u8; 16]));
    // One changed ciphertext bit garbles its 16-byte block and no other.
    b[100] ^= 1;
    xts.sector(8, &mut b, true);
    for (i, c) in b.chunks(16).enumerate() {
        assert_eq!(c == [0u8; 16], i != 100 / 16);
    }
}

#[test]
fn region_keys_match_the_reference() {
    let key: [u8; 32] = core::array::from_fn(|i| (i * 7 + 1) as u8);
    assert_eq!(hex(&sha256(&[cipher_key(&key, DATA).expose()])), "11da99417eeb48773122bfed484af8775aecd6f5f468d3b568f5ee10427a987d");
    assert_eq!(hex(&sha256(&[cipher_key(&key, APPS).expose()])), "24a0b324be6eeb9974830feb146fed65b5cf4cd80c2d10467b3c93e9b0ed1441");
}
