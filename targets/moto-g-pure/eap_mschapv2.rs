//! MSCHAPv2 (RFC 2759) and the keys it leaves behind (RFC 3079), with the old
//! primitives it is built from: MD4, SHA-1, HMAC-SHA-1 and single DES. They
//! exist here only because PEAP's inner method needs them; nothing else may
//! use them. MSCHAPv2 is weak by itself (the response is three DES
//! encryptions of the challenge under the password hash), which is why eap.rs
//! runs it only inside a TLS tunnel whose server certificate was checked.
//!
//! Pure `core` code: no allocation, no I/O. Host tests: test_eap.rs.

fn rounds(parts: &[&[u8]], big_endian_length: bool, state: &mut [u32; 5], compress: fn(&mut [u32; 5], &[u8; 64])) {
    let mut block = [0u8; 64];
    let mut fill = 0;
    let mut total: u64 = 0;
    for part in parts {
        total += part.len() as u64;
        for &b in *part {
            block[fill] = b;
            fill += 1;
            if fill == 64 {
                compress(state, &block);
                fill = 0;
            }
        }
    }
    block[fill] = 0x80;
    fill += 1;
    if fill > 56 {
        block[fill..].fill(0);
        compress(state, &block);
        fill = 0;
    }
    block[fill..56].fill(0);
    let bits = total.wrapping_mul(8);
    block[56..].copy_from_slice(&if big_endian_length { bits.to_be_bytes() } else { bits.to_le_bytes() });
    compress(state, &block);
    block.fill(0);
}

fn md4_block(s: &mut [u32; 5], block: &[u8; 64]) {
    let mut x = [0u32; 16];
    for (n, w) in x.iter_mut().enumerate() {
        *w = u32::from_le_bytes([block[4 * n], block[4 * n + 1], block[4 * n + 2], block[4 * n + 3]]);
    }
    let (mut a, mut b, mut c, mut d) = (s[0], s[1], s[2], s[3]);
    const ORDER: [[u8; 16]; 3] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15],
        [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15],
    ];
    const SHIFT: [[u32; 4]; 3] = [[3, 7, 11, 19], [3, 5, 9, 13], [3, 9, 11, 15]];
    const ADD: [u32; 3] = [0, 0x5a82_7999, 0x6ed9_eba1];
    for round in 0..3 {
        for step in 0..16 {
            let f = match round {
                0 => (b & c) | (!b & d),
                1 => (b & c) | (b & d) | (c & d),
                _ => b ^ c ^ d,
            };
            let t = a.wrapping_add(f).wrapping_add(x[ORDER[round][step] as usize]).wrapping_add(ADD[round]).rotate_left(SHIFT[round][step % 4]);
            a = d;
            d = c;
            c = b;
            b = t;
        }
    }
    s[0] = s[0].wrapping_add(a);
    s[1] = s[1].wrapping_add(b);
    s[2] = s[2].wrapping_add(c);
    s[3] = s[3].wrapping_add(d);
}

pub fn md4(parts: &[&[u8]]) -> [u8; 16] {
    let mut s = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0];
    rounds(parts, false, &mut s, md4_block);
    let mut out = [0; 16];
    for n in 0..4 {
        out[4 * n..4 * n + 4].copy_from_slice(&s[n].to_le_bytes());
    }
    out
}

fn sha1_block(s: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for n in 0..16 {
        w[n] = u32::from_be_bytes([block[4 * n], block[4 * n + 1], block[4 * n + 2], block[4 * n + 3]]);
    }
    for n in 16..80 {
        w[n] = (w[n - 3] ^ w[n - 8] ^ w[n - 14] ^ w[n - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (s[0], s[1], s[2], s[3], s[4]);
    for (n, word) in w.iter().enumerate() {
        let (f, k) = match n / 20 {
            0 => ((b & c) | (!b & d), 0x5a82_7999),
            1 => (b ^ c ^ d, 0x6ed9_eba1),
            2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6u32),
        };
        let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (x, y) in s.iter_mut().zip([a, b, c, d, e]) {
        *x = x.wrapping_add(y);
    }
    w.fill(0);
}

pub fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut s = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    rounds(parts, true, &mut s, sha1_block);
    let mut out = [0; 20];
    for n in 0..5 {
        out[4 * n..4 * n + 4].copy_from_slice(&s[n].to_be_bytes());
    }
    out
}

/// HMAC-SHA-1 over up to six parts.
pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut pad = [0u8; 64];
    if key.len() > 64 {
        pad[..20].copy_from_slice(&sha1(&[key]));
    } else {
        pad[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for n in 0..64 {
        inner_pad[n] ^= pad[n];
        outer_pad[n] ^= pad[n];
    }
    let mut all: [&[u8]; 7] = [&[]; 7];
    all[0] = &inner_pad;
    let count = parts.len().min(6);
    all[1..1 + count].copy_from_slice(&parts[..count]);
    let inner = sha1(&all[..1 + count]);
    let out = sha1(&[&outer_pad, &inner]);
    pad.fill(0);
    out
}

/// Constant-time equality.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// FIPS 46-3 tables, bit positions counted from 1 at the most significant bit.
const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6, 64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51,
    43, 35, 27, 19, 11, 3, 61, 53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];
const FP: [u8; 64] = [
    40, 8, 48, 16, 56, 24, 64, 32, 39, 7, 47, 15, 55, 23, 63, 31, 38, 6, 46, 14, 54, 22, 62, 30, 37, 5, 45, 13, 53, 21, 61, 29, 36, 4, 44, 12, 52, 20, 60, 28, 35, 3,
    43, 11, 51, 19, 59, 27, 34, 2, 42, 10, 50, 18, 58, 26, 33, 1, 41, 9, 49, 17, 57, 25,
];
const EXPAND: [u8; 48] = [
    32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17, 16, 17, 18, 19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29,
    30, 31, 32, 1,
];
const PERM: [u8; 32] = [16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19, 13, 30, 6, 22, 11, 4, 25];
const PC1: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60, 52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22,
    14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 28, 20, 12, 4,
];
const PC2: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52, 31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53,
    46, 42, 50, 36, 29, 32,
];
const ROTATE: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
const SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12, 11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0,
        15, 12, 8, 2, 4, 9, 1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1, 10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15,
        13, 8, 10, 1, 3, 15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5, 14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7,
        1, 10, 13, 0, 6, 9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2, 12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4,
        3, 15, 0, 6, 10, 1, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15, 10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14,
        11, 8, 12, 7, 1, 14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13, 14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6,
        4, 3, 2, 12, 9, 5, 15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5, 12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2,
        6, 11, 13, 8, 1, 4, 10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6, 11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8,
        2, 1, 14, 7, 4, 10, 8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

/// Pick bits of `v` (which is `width` bits wide) by a table of 1-based positions.
fn permute(v: u64, width: u32, table: &[u8]) -> u64 {
    table.iter().fold(0, |out, &p| (out << 1) | ((v >> (width - u32::from(p))) & 1))
}

/// One DES encryption with an 8-byte key (parity bits ignored).
pub fn des_encrypt(key: &[u8; 8], block: &[u8; 8]) -> [u8; 8] {
    let cd = permute(u64::from_be_bytes(*key), 64, &PC1);
    let (mut c, mut d) = ((cd >> 28) as u32 & 0x0fff_ffff, cd as u32 & 0x0fff_ffff);
    let text = permute(u64::from_be_bytes(*block), 64, &IP);
    let (mut l, mut r) = ((text >> 32) as u32, text as u32);
    for turn in ROTATE {
        let n = u32::from(turn);
        c = ((c << n) | (c >> (28 - n))) & 0x0fff_ffff;
        d = ((d << n) | (d >> (28 - n))) & 0x0fff_ffff;
        let subkey = permute((u64::from(c) << 28) | u64::from(d), 56, &PC2);
        let x = permute(u64::from(r), 32, &EXPAND) ^ subkey;
        let mut s = 0u32;
        for (n, sbox) in SBOX.iter().enumerate() {
            let six = (x >> (42 - 6 * n)) as usize & 0x3f;
            let row = ((six & 0x20) >> 4) | (six & 1);
            s = (s << 4) | u32::from(sbox[row * 16 + ((six >> 1) & 0xf)]);
        }
        let f = permute(u64::from(s), 32, &PERM) as u32;
        (l, r) = (r, l ^ f);
    }
    permute((u64::from(r) << 32) | u64::from(l), 64, &FP).to_be_bytes()
}

/// The 56-bit key in seven bytes, spread over eight with a spare low bit each.
fn des_key(k: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    let mut carry = 0u8;
    for n in 0..7 {
        out[n] = ((k[n] >> n) | carry) & 0xfe;
        carry = k[n] << (7 - n);
    }
    out[7] = carry & 0xfe;
    out
}

/// NtPasswordHash: MD4 of the password as UTF-16LE. `password` is UTF-8 of at
/// most 64 bytes; anything else is refused.
pub fn nt_hash(password: &[u8]) -> Option<[u8; 16]> {
    let text = core::str::from_utf8(password).ok()?;
    if password.len() > 64 {
        return None;
    }
    let mut wide = [0u8; 128];
    let mut n = 0;
    for unit in text.encode_utf16() {
        wide[n..n + 2].copy_from_slice(&unit.to_le_bytes());
        n += 2;
    }
    let hash = md4(&[&wide[..n]]);
    wide.fill(0);
    Some(hash)
}

/// The user name without a leading "DOMAIN\".
pub fn bare_name(user: &[u8]) -> &[u8] {
    match user.iter().rposition(|&b| b == b'\\') {
        Some(at) => &user[at + 1..],
        None => user,
    }
}

pub fn challenge_hash(peer: &[u8; 16], authenticator: &[u8; 16], user: &[u8]) -> [u8; 8] {
    let h = sha1(&[peer, authenticator, bare_name(user)]);
    let mut out = [0; 8];
    out.copy_from_slice(&h[..8]);
    out
}

/// ChallengeResponse: the 8-byte challenge under three DES keys cut from the hash.
pub fn challenge_response(challenge: &[u8; 8], hash: &[u8; 16]) -> [u8; 24] {
    let mut z = [0u8; 21];
    z[..16].copy_from_slice(hash);
    let mut out = [0u8; 24];
    for n in 0..3 {
        out[8 * n..8 * n + 8].copy_from_slice(&des_encrypt(&des_key(&z[7 * n..7 * n + 7]), challenge));
    }
    z.fill(0);
    out
}

pub fn nt_response(authenticator: &[u8; 16], peer: &[u8; 16], user: &[u8], hash: &[u8; 16]) -> [u8; 24] {
    challenge_response(&challenge_hash(peer, authenticator, user), hash)
}

/// What the server must send back in "S=..." to prove it knows the password too.
pub fn authenticator_response(hash: &[u8; 16], nt_response: &[u8; 24], peer: &[u8; 16], authenticator: &[u8; 16], user: &[u8]) -> [u8; 20] {
    let hash_hash = md4(&[hash]);
    let digest = sha1(&[&hash_hash, nt_response, b"Magic server to client signing constant"]);
    sha1(&[&digest, &challenge_hash(peer, authenticator, user), b"Pad to make it do more than one iteration"])
}

/// RFC 3079 GetMasterKey.
pub fn master_key(hash: &[u8; 16], nt_response: &[u8; 24]) -> [u8; 16] {
    let hash_hash = md4(&[hash]);
    let h = sha1(&[&hash_hash, nt_response, b"This is the MPPE Master Key"]);
    let mut out = [0; 16];
    out.copy_from_slice(&h[..16]);
    out
}

/// RFC 3079 GetAsymmetricStartKey for the client: its send key or its receive key.
pub fn start_key(master: &[u8; 16], send: bool) -> [u8; 16] {
    let magic: &[u8] = if send {
        b"On the client side, this is the send key; on the server side, it is the receive key."
    } else {
        b"On the client side, this is the receive key; on the server side, it is the send key."
    };
    let h = sha1(&[master, &[0u8; 40], magic, &[0xf2u8; 40]]);
    let mut out = [0; 16];
    out.copy_from_slice(&h[..16]);
    out
}
