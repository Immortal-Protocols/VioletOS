//! WPA3-Personal: Simultaneous Authentication of Equals (IEEE 802.11-2020
//! 12.4) for a station, on group 19 (NIST P-256) only, with the password
//! element from hunting-and-pecking (12.4.4.2.2) or from hash-to-element
//! (12.4.4.2.3, RFC 9380's simplified SWU map), the anti-clogging token, and
//! the commit/confirm exchange. The result is the PMK and PMKID that the
//! 4-way handshake with AKM 00-0F-AC:8 starts from. `choose` picks SAE or
//! PSK for an access point that offers both (WPA2/WPA3 transition mode).
//!
//! Self-contained: SHA-256, HMAC, the 802.11 KDF, HKDF and P-256 arithmetic
//! are here, no allocation, no hardware. Field and scalar arithmetic is
//! Montgomery multiplication on eight 32-bit limbs with no branch or index
//! that depends on a secret, points use the complete addition formulas of
//! Renes, Costello and Batina (2016, algorithm 4, a = -3), and scalar
//! multiplication is a Montgomery ladder. Exponents (inversion, square root,
//! Legendre symbol) are public constants. Hunting-and-pecking always runs at
//! least 40 rounds and picks its result with masks.
//!
//! Tested on the host (test_sae.rs) against IEEE 802.11-2020 Annex J.10 and
//! against hostap's own SAE (`make test-sae`).

pub(crate) type U = [u32; 8];

// ---------------------------------------------------------------- SHA-256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

struct Sha256 {
    h: [u32; 8],
    block: [u8; 64],
    fill: usize,
    total: u64,
}
impl Sha256 {
    fn new() -> Sha256 {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            block: [0; 64],
            fill: 0,
            total: 0,
        }
    }
    fn compress(&mut self) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(self.block[4 * i..4 * i + 4].try_into().unwrap());
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
            v = [t1.wrapping_add(s0).wrapping_add(maj), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            self.h[i] = self.h[i].wrapping_add(v[i]);
        }
    }
    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let n = (64 - self.fill).min(data.len());
            self.block[self.fill..self.fill + n].copy_from_slice(&data[..n]);
            self.fill += n;
            data = &data[n..];
            if self.fill == 64 {
                self.compress();
                self.fill = 0;
            }
        }
    }
    fn finish(mut self) -> [u8; 32] {
        let bits = self.total * 8;
        self.update(&[0x80]);
        while self.fill != 56 {
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

/// HMAC-SHA256(key, parts concatenated).
fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let mut h = Sha256::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finish());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    let mut pad = [0u8; 64];
    for i in 0..64 {
        pad[i] = k[i] ^ 0x36;
    }
    inner.update(&pad);
    for p in parts {
        inner.update(p);
    }
    let digest = inner.finish();
    let mut outer = Sha256::new();
    for i in 0..64 {
        pad[i] = k[i] ^ 0x5c;
    }
    outer.update(&pad);
    outer.update(&digest);
    wipe(&mut k);
    wipe(&mut pad);
    outer.finish()
}

/// IEEE 802.11 KDF-Hash-Length with SHA-256 (12.7.1.6.2), `out.len()` whole octets.
fn kdf(key: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    let bits = (out.len() as u16 * 8).to_le_bytes();
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let n = (i as u16 + 1).to_le_bytes();
        let h = hmac(key, &[&n, label, context, &bits]);
        chunk.copy_from_slice(&h[..chunk.len()]);
    }
}

fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        unsafe { core::ptr::write_volatile(x, 0) };
    }
}
fn wipe_u(a: &mut U) {
    for x in a.iter_mut() {
        unsafe { core::ptr::write_volatile(x, 0) };
    }
}

// ------------------------------------------------- arithmetic modulo m

/// An odd 256-bit modulus with its Montgomery constants (R = 2^256).
struct Modulus {
    m: U,
    /// -m^-1 mod 2^32.
    n0: u32,
    /// R^2 mod m.
    rr: U,
}

const fn add_raw(a: &U, b: &U) -> (U, u32) {
    let mut r = [0u32; 8];
    let mut c = 0u64;
    let mut i = 0;
    while i < 8 {
        let s = a[i] as u64 + b[i] as u64 + c;
        r[i] = s as u32;
        c = s >> 32;
        i += 1;
    }
    (r, c as u32)
}
/// a - b and the borrow (1 when b > a).
const fn sub_raw(a: &U, b: &U) -> (U, u32) {
    let mut r = [0u32; 8];
    let mut borrow = 0i64;
    let mut i = 0;
    while i < 8 {
        let d = a[i] as i64 - b[i] as i64 - borrow;
        r[i] = d as u32;
        borrow = (d < 0) as i64;
        i += 1;
    }
    (r, borrow as u32)
}
/// `a` where mask is all ones, else `b`.
const fn select(mask: u32, a: &U, b: &U) -> U {
    let mut r = [0u32; 8];
    let mut i = 0;
    while i < 8 {
        r[i] = (a[i] & mask) | (b[i] & !mask);
        i += 1;
    }
    r
}
const fn add_mod(m: &U, a: &U, b: &U) -> U {
    let (s, carry) = add_raw(a, b);
    let (d, borrow) = sub_raw(&s, m);
    // Take s - m when the sum carried out or is at least m.
    select(0u32.wrapping_sub(carry | (borrow ^ 1)), &d, &s)
}
const fn sub_mod(m: &U, a: &U, b: &U) -> U {
    let (d, borrow) = sub_raw(a, b);
    let (s, _) = add_raw(&d, m);
    select(0u32.wrapping_sub(borrow), &s, &d)
}
const fn modulus(m: U) -> Modulus {
    let mut inv = 1u32;
    let mut i = 0;
    while i < 5 {
        inv = inv.wrapping_mul(2u32.wrapping_sub(m[0].wrapping_mul(inv)));
        i += 1;
    }
    // R^2 mod m by doubling 1 512 times.
    let mut r = [1, 0, 0, 0, 0, 0, 0, 0];
    let mut k = 0;
    while k < 512 {
        r = add_mod(&m, &r, &r);
        k += 1;
    }
    Modulus { m, n0: inv.wrapping_neg(), rr: r }
}

impl Modulus {
    /// a * b / R mod m, for a, b < m.
    fn mul(&self, a: &U, b: &U) -> U {
        let m = &self.m;
        let mut t = [0u32; 10];
        for i in 0..8 {
            let mut c = 0u64;
            for j in 0..8 {
                let s = t[j] as u64 + a[j] as u64 * b[i] as u64 + c;
                t[j] = s as u32;
                c = s >> 32;
            }
            let s = t[8] as u64 + c;
            t[8] = s as u32;
            t[9] = (s >> 32) as u32;
            let q = t[0].wrapping_mul(self.n0);
            let mut c = (t[0] as u64 + q as u64 * m[0] as u64) >> 32;
            for j in 1..8 {
                let s = t[j] as u64 + q as u64 * m[j] as u64 + c;
                t[j - 1] = s as u32;
                c = s >> 32;
            }
            let s = t[8] as u64 + c;
            t[7] = s as u32;
            t[8] = t[9] + (s >> 32) as u32;
        }
        let r: U = t[..8].try_into().unwrap();
        let (d, borrow) = sub_raw(&r, m);
        select(0u32.wrapping_sub(t[8] | (borrow ^ 1)), &d, &r)
    }
    fn add(&self, a: &U, b: &U) -> U {
        add_mod(&self.m, a, b)
    }
    fn sub(&self, a: &U, b: &U) -> U {
        sub_mod(&self.m, a, b)
    }
    fn to(&self, a: &U) -> U {
        self.mul(a, &self.rr)
    }
    fn from(&self, a: &U) -> U {
        self.mul(a, &[1, 0, 0, 0, 0, 0, 0, 0])
    }
    fn one(&self) -> U {
        self.to(&[1, 0, 0, 0, 0, 0, 0, 0])
    }
    /// a^e for a in Montgomery form and a public exponent.
    fn pow(&self, a: &U, e: &U) -> U {
        let mut r = self.one();
        for i in (0..256).rev() {
            r = self.mul(&r, &r);
            if e[i / 32] >> (i % 32) & 1 == 1 {
                r = self.mul(&r, a);
            }
        }
        r
    }
    /// Reduces a value below 2m.
    fn reduce(&self, a: &U) -> U {
        let (d, borrow) = sub_raw(a, &self.m);
        select(0u32.wrapping_sub(borrow), a, &d)
    }
}

/// A big-endian 32-octet value as limbs.
pub(crate) fn from_be(b: &[u8]) -> U {
    let mut r = [0u32; 8];
    for i in 0..8 {
        r[i] = u32::from_be_bytes(b[28 - 4 * i..32 - 4 * i].try_into().unwrap());
    }
    r
}
fn to_be(a: &U) -> [u8; 32] {
    let mut b = [0u8; 32];
    for i in 0..8 {
        b[28 - 4 * i..32 - 4 * i].copy_from_slice(&a[i].to_be_bytes());
    }
    b
}
/// All ones when a == b.
fn eq_mask(a: &U, b: &U) -> u32 {
    let mut d = 0;
    for i in 0..8 {
        d |= a[i] ^ b[i];
    }
    ((d as u64).wrapping_sub(1) >> 32) as u32
}
/// a < b, in constant time.
fn below(a: &U, b: &U) -> bool {
    sub_raw(a, b).1 == 1
}

const P_M: U = [0xffffffff, 0xffffffff, 0xffffffff, 0, 0, 0, 1, 0xffffffff];
const N_M: U = [0xfc632551, 0xf3b9cac2, 0xa7179e84, 0xbce6faad, 0xffffffff, 0xffffffff, 0, 0xffffffff];
const B: U = [0x27d2604b, 0x3bce3c3e, 0xcc53b0f6, 0x651d06b0, 0x769886bc, 0xb3ebbd55, 0xaa3a93e7, 0x5ac635d8];
const GX: U = [0xd898c296, 0xf4a13945, 0x2deb33a0, 0x77037d81, 0x63a440f2, 0xf8bce6e5, 0xe12c4247, 0x6b17d1f2];
const GY: U = [0x37bf51f5, 0xcbb64068, 0x6b315ece, 0x2bce3357, 0x7c0f9e16, 0x8ee7eb4a, 0xfe1a7f9b, 0x4fe342e2];
static P: Modulus = modulus(P_M);
static N: Modulus = modulus(N_M);
const fn shr(a: U, k: u32) -> U {
    let mut r = [0u32; 8];
    let mut i = 0;
    while i < 8 {
        r[i] = a[i] >> k | if i < 7 { a[i + 1] << (32 - k) } else { 0 };
        i += 1;
    }
    r
}
/// p - 2 (inversion), (p - 1) / 2 (Legendre symbol), (p + 1) / 4 (square root).
const P_MINUS_2: U = sub_raw(&P_M, &[2, 0, 0, 0, 0, 0, 0, 0]).0;
const P_HALF: U = shr(sub_raw(&P_M, &[1, 0, 0, 0, 0, 0, 0, 0]).0, 1);
const P_QUARTER: U = shr(add_raw(&P_M, &[1, 0, 0, 0, 0, 0, 0, 0]).0, 2);

// --------------------------------------------------------- P-256 points

/// A point in projective coordinates (X:Y:Z), Montgomery form; Z = 0 is the identity.
#[derive(Clone, Copy)]
struct Pt {
    x: U,
    y: U,
    z: U,
}

/// -3 and b in Montgomery form.
fn curve() -> (U, U) {
    (P.sub(&[0; 8], &P.to(&[3, 0, 0, 0, 0, 0, 0, 0])), P.to(&B))
}

fn add(p: &Pt, q: &Pt) -> Pt {
    let f = &P;
    let b = curve().1;
    let (m, a, s) = (|x: &U, y: &U| f.mul(x, y), |x: &U, y: &U| f.add(x, y), |x: &U, y: &U| f.sub(x, y));
    let mut t0 = m(&p.x, &q.x);
    let mut t1 = m(&p.y, &q.y);
    let mut t2 = m(&p.z, &q.z);
    let mut t3 = a(&p.x, &p.y);
    let mut t4 = a(&q.x, &q.y);
    t3 = m(&t3, &t4);
    t4 = a(&t0, &t1);
    t3 = s(&t3, &t4);
    t4 = a(&p.y, &p.z);
    let mut x3 = a(&q.y, &q.z);
    t4 = m(&t4, &x3);
    x3 = a(&t1, &t2);
    t4 = s(&t4, &x3);
    x3 = a(&p.x, &p.z);
    let mut y3 = a(&q.x, &q.z);
    x3 = m(&x3, &y3);
    y3 = a(&t0, &t2);
    y3 = s(&x3, &y3);
    let mut z3 = m(&b, &t2);
    x3 = s(&y3, &z3);
    z3 = a(&x3, &x3);
    x3 = a(&x3, &z3);
    z3 = s(&t1, &x3);
    x3 = a(&t1, &x3);
    y3 = m(&b, &y3);
    t1 = a(&t2, &t2);
    t2 = a(&t1, &t2);
    y3 = s(&y3, &t2);
    y3 = s(&y3, &t0);
    t1 = a(&y3, &y3);
    y3 = a(&t1, &y3);
    t1 = a(&t0, &t0);
    t0 = a(&t1, &t0);
    t0 = s(&t0, &t2);
    t1 = m(&t4, &y3);
    t2 = m(&t0, &y3);
    y3 = m(&x3, &z3);
    y3 = a(&y3, &t2);
    x3 = m(&x3, &t3);
    x3 = s(&x3, &t1);
    z3 = m(&z3, &t4);
    t1 = m(&t3, &t0);
    z3 = a(&z3, &t1);
    Pt { x: x3, y: y3, z: z3 }
}

fn swap(mask: u32, a: &mut Pt, b: &mut Pt) {
    for (u, v) in [(&mut a.x, &mut b.x), (&mut a.y, &mut b.y), (&mut a.z, &mut b.z)] {
        for i in 0..8 {
            let t = (u[i] ^ v[i]) & mask;
            u[i] ^= t;
            v[i] ^= t;
        }
    }
}

/// k * p, the same work for every k.
fn mul(k: &U, p: &Pt) -> Pt {
    let mut r0 = Pt { x: [0; 8], y: P.one(), z: [0; 8] };
    let mut r1 = *p;
    for i in (0..256).rev() {
        let bit = 0u32.wrapping_sub(k[i / 32] >> (i % 32) & 1);
        swap(bit, &mut r0, &mut r1);
        r1 = add(&r0, &r1);
        r0 = add(&r0, &r0);
        swap(bit, &mut r0, &mut r1);
    }
    r0
}

/// x^3 - 3x + b for x in Montgomery form.
fn rhs(x: &U) -> U {
    let (a, b) = curve();
    let x3 = P.mul(&P.mul(x, x), x);
    P.add(&P.add(&x3, &P.mul(&a, x)), &b)
}

/// The point (x, y) from 64 octets, if both coordinates are below p and it is on the curve.
fn point(bytes: &[u8; 64]) -> Option<Pt> {
    let (x, y) = (from_be(&bytes[..32]), from_be(&bytes[32..]));
    if !below(&x, &P.m) || !below(&y, &P.m) {
        return None;
    }
    let (x, y) = (P.to(&x), P.to(&y));
    if eq_mask(&P.mul(&y, &y), &rhs(&x)) == 0 {
        return None;
    }
    Some(Pt { x, y, z: P.one() })
}

/// x || y of a point, or None for the identity.
fn bytes(p: &Pt) -> Option<[u8; 64]> {
    if eq_mask(&p.z, &[0; 8]) != 0 {
        return None;
    }
    let zi = P.pow(&p.z, &P_MINUS_2);
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&to_be(&P.from(&P.mul(&p.x, &zi))));
    out[32..].copy_from_slice(&to_be(&P.from(&P.mul(&p.y, &zi))));
    Some(out)
}

fn max_min(a: &[u8; 6], b: &[u8; 6]) -> [u8; 12] {
    let mut k = [0u8; 12];
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    k[..6].copy_from_slice(hi);
    k[6..].copy_from_slice(lo);
    k
}

// ------------------------------------------------- the password element

/// The password element by hunting-and-pecking (12.4.4.2.2): x || y, or None
/// if no candidate in 200 rounds was on the curve (which does not happen).
/// The result is password-equivalent for this pair of addresses: the caller
/// wipes it once `Sae::new` has it (as for `pt` and `pwe_from_pt`).
pub fn hunting_and_pecking(password: &[u8], a: &[u8; 6], b: &[u8; 6]) -> Option<[u8; 64]> {
    let key = max_min(a, b);
    let p_bytes = to_be(&P.m);
    let one = P.one();
    let mut found = 0u32;
    let mut x = [0u32; 8];
    let mut odd = 0u32;
    let mut counter = 1u8;
    while counter <= 40 || found == 0 {
        if counter > 200 {
            return None;
        }
        let mut seed = hmac(&key, &[password, &[counter]]);
        let mut value = [0u8; 32];
        kdf(&seed, b"SAE Hunting and Pecking", &p_bytes, &mut value);
        let mut v = from_be(&value);
        let in_range = 0u32.wrapping_sub(below(&v, &P.m) as u32);
        let mut candidate = P.to(&P.reduce(&v));
        let mut g = rhs(&candidate);
        let qr = eq_mask(&P.pow(&g, &P_HALF), &one);
        let take = in_range & qr & !found;
        x = select(take, &candidate, &x);
        odd = (take & (seed[31] as u32 & 1)) | (!take & odd);
        found |= in_range & qr;
        wipe(&mut seed);
        wipe(&mut value);
        wipe_u(&mut v);
        wipe_u(&mut candidate);
        wipe_u(&mut g);
        counter += 1;
    }
    let mut y = P.from(&P.pow(&rhs(&x), &P_QUARTER));
    let flip = 0u32.wrapping_sub((y[0] & 1) ^ odd);
    y = select(flip, &P.sub(&[0; 8], &y), &y);
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&to_be(&P.from(&x)));
    out[32..].copy_from_slice(&to_be(&y));
    wipe_u(&mut x);
    wipe_u(&mut y);
    Some(out)
}

/// RFC 9380's simplified SWU map to P-256 with Z = -10, as 12.4.4.2.3 has it.
fn sswu(u: &U) -> Pt {
    let f = &P;
    let (a, b) = curve();
    let one = f.one();
    let z = f.sub(&[0; 8], &f.to(&[10, 0, 0, 0, 0, 0, 0, 0]));
    let u = f.to(u);
    let u2 = f.mul(&u, &u);
    let t1 = f.mul(&z, &u2);
    let m = f.add(&f.mul(&t1, &t1), &t1);
    let m_zero = eq_mask(&m, &[0; 8]);
    let t = f.pow(&m, &P_MINUS_2);
    // x1 = b / (z a) when m = 0, else (-b / a)(1 + t).
    let x1a = f.mul(&b, &f.pow(&f.mul(&z, &a), &P_MINUS_2));
    let x1b = f.mul(&f.mul(&f.sub(&[0; 8], &b), &f.pow(&a, &P_MINUS_2)), &f.add(&one, &t));
    let x1 = select(m_zero, &x1a, &x1b);
    let gx1 = rhs(&x1);
    let x2 = f.mul(&t1, &x1);
    let gx2 = rhs(&x2);
    let l = f.pow(&gx1, &P_HALF);
    let qr = eq_mask(&l, &one) | eq_mask(&l, &[0; 8]);
    let v = select(qr, &gx1, &gx2);
    let x = select(qr, &x1, &x2);
    let y = f.from(&f.pow(&v, &P_QUARTER));
    let flip = 0u32.wrapping_sub((y[0] ^ f.from(&u)[0]) & 1);
    let y = select(flip, &f.sub(&[0; 8], &y), &y);
    Pt { x, y: f.to(&y), z: one }
}

/// A 48-octet big-endian value modulo p.
fn wide_mod_p(b: &[u8; 48]) -> U {
    let mut hi = [0u8; 32];
    hi[16..].copy_from_slice(&b[..16]);
    // hi * 2^256 = mul(hi, R^2) / R.
    let high = P.mul(&from_be(&hi), &P.rr);
    P.add(&high, &P.reduce(&from_be(&b[16..])))
}

/// The password token PT for hash-to-element (12.4.4.2.3), x || y: it
/// depends on the SSID and password only, so it can be kept instead of the
/// password. `identifier` is the optional password identifier.
pub fn pt(ssid: &[u8], password: &[u8], identifier: Option<&[u8]>) -> Option<[u8; 64]> {
    let mut seed = hmac(ssid, &[password, identifier.unwrap_or(&[])]);
    let mut sum: Option<Pt> = None;
    for info in [&b"SAE Hash to Element u1 P1"[..], b"SAE Hash to Element u2 P2"] {
        // HKDF-Expand to 48 octets.
        let mut okm = [0u8; 48];
        let t1 = hmac(&seed, &[info, &[1]]);
        let t2 = hmac(&seed, &[&t1, info, &[2]]);
        okm[..32].copy_from_slice(&t1);
        okm[32..].copy_from_slice(&t2[..16]);
        let mut u = wide_mod_p(&okm);
        let mut q = sswu(&u);
        sum = Some(match sum {
            None => q,
            Some(p) => add(&p, &q),
        });
        wipe(&mut okm);
        wipe_u(&mut u);
        wipe_pt(&mut q);
    }
    wipe(&mut seed);
    let mut sum = sum?;
    let out = bytes(&sum);
    wipe_pt(&mut sum);
    out
}
fn wipe_pt(p: &mut Pt) {
    wipe_u(&mut p.x);
    wipe_u(&mut p.y);
    wipe_u(&mut p.z);
}

/// The password element from PT for these two addresses (12.4.4.2.3).
pub fn pwe_from_pt(pt: &[u8; 64], a: &[u8; 6], b: &[u8; 6]) -> Option<[u8; 64]> {
    let mut p = point(pt)?;
    let h = from_be(&hmac(&[0; 32], &[&max_min(a, b)]));
    // val = h mod (n - 1) + 1; h < 2^256 < 2(n - 1), so one subtraction is enough.
    let n1 = sub_raw(&N.m, &[1, 0, 0, 0, 0, 0, 0, 0]).0;
    let (d, borrow) = sub_raw(&h, &n1);
    let val = add_raw(&select(0u32.wrapping_sub(borrow), &h, &d), &[1, 0, 0, 0, 0, 0, 0, 0]).0;
    let mut e = mul(&val, &p);
    let out = bytes(&e);
    wipe_pt(&mut p);
    wipe_pt(&mut e);
    out
}

// ------------------------------------------------------- the exchange

pub const ALGORITHM: u16 = 3;
pub const GROUP: u16 = 19;
pub const STATUS_ANTI_CLOGGING: u16 = 76;
pub const STATUS_GROUP: u16 = 77;
pub const STATUS_H2E: u16 = 126;
const EXTENSION: u8 = 255;
const EXT_TOKEN: u8 = 93;
const TOKEN_MAX: usize = 128;
/// Anti-clogging tokens accepted in one exchange, and retransmissions.
const TOKENS: u8 = 3;
const SYNC: u8 = 5;
/// The largest authentication frame body this station sends.
pub const BODY_MAX: usize = 6 + 2 + 32 + 64 + 3 + TOKEN_MAX;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Committed,
    Confirmed,
    Accepted,
    Failed,
}

/// What the station does after a frame or a timeout.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Step {
    /// Send `message()` (again).
    Send,
    /// Authenticated: `pmk()` holds the keys.
    Done,
    /// Nothing (a frame that is not for this exchange, or a forgery).
    Ignore,
    /// The exchange failed; the status code says why when the peer gave one.
    Fail(u16),
}

pub struct Sae {
    h2e: bool,
    pwe: Pt,
    rand: U,
    scalar: [u8; 32],
    element: [u8; 64],
    peer_scalar: [u8; 32],
    peer_element: [u8; 64],
    kck: [u8; 32],
    pmk: [u8; 32],
    pmkid: [u8; 16],
    send_confirm: u16,
    pub state: State,
    token: [u8; TOKEN_MAX],
    token_len: usize,
    tokens: u8,
    sync: u8,
}

impl Drop for Sae {
    /// The secrets: rand, the password element (password-equivalent for
    /// this pair of addresses), the keys, and the rest of the exchange too.
    fn drop(&mut self) {
        wipe_u(&mut self.rand);
        wipe_pt(&mut self.pwe);
        wipe(&mut self.kck);
        wipe(&mut self.pmk);
        wipe(&mut self.pmkid);
        wipe(&mut self.token);
        wipe(&mut self.peer_scalar);
        wipe(&mut self.peer_element);
    }
}

/// A scalar in [2, n) drawn from `random`, or None if it keeps failing.
fn draw(random: &mut dyn FnMut(&mut [u8; 32]) -> bool) -> Option<U> {
    for _ in 0..100 {
        let mut b = [0u8; 32];
        if !random(&mut b) {
            return None;
        }
        let v = from_be(&b);
        wipe(&mut b);
        if below(&v, &N.m) && !below(&v, &[2, 0, 0, 0, 0, 0, 0, 0]) {
            return Some(v);
        }
    }
    None
}

impl Sae {
    /// Starts an exchange from the password element (`hunting_and_pecking`,
    /// or `pwe_from_pt` with `h2e`), drawing rand and mask from `random`.
    pub fn new(pwe: &[u8; 64], h2e: bool, random: &mut dyn FnMut(&mut [u8; 32]) -> bool) -> Option<Sae> {
        for _ in 0..100 {
            let rand = draw(random)?;
            let mask = draw(random)?;
            if let Some(s) = Sae::with(pwe, h2e, &rand, &mask) {
                return Some(s);
            }
        }
        None
    }

    /// With given rand and mask (the test vectors'): None if they make a
    /// commit scalar below 2.
    pub fn with(pwe: &[u8; 64], h2e: bool, rand: &U, mask: &U) -> Option<Sae> {
        let pwe = point(pwe)?;
        let scalar = N.add(rand, mask);
        if below(&scalar, &[2, 0, 0, 0, 0, 0, 0, 0]) {
            return None;
        }
        let e = bytes(&mul(mask, &pwe))?;
        let mut element = e;
        let y = from_be(&e[32..]);
        element[32..].copy_from_slice(&to_be(&P.sub(&[0; 8], &y)));
        Some(Sae {
            h2e,
            pwe,
            rand: *rand,
            scalar: to_be(&scalar),
            element,
            peer_scalar: [0; 32],
            peer_element: [0; 64],
            kck: [0; 32],
            pmk: [0; 32],
            pmkid: [0; 16],
            send_confirm: 0,
            state: State::Committed,
            token: [0; TOKEN_MAX],
            token_len: 0,
            tokens: 0,
            sync: 0,
        })
    }

    /// KCK, PMK and PMKID as derived from the peer's commit, before its
    /// confirm (for the Annex J.10 vector, which has no confirm).
    #[cfg(test)]
    pub(crate) fn derived(&self) -> (&[u8; 32], &[u8; 32], &[u8; 16]) {
        (&self.kck, &self.pmk, &self.pmkid)
    }

    /// The PMK and PMKID once accepted.
    pub fn pmk(&self) -> Option<(&[u8; 32], &[u8; 16])> {
        (self.state == State::Accepted).then_some((&self.pmk, &self.pmkid))
    }

    /// The authentication frame body to send now: the commit, or the
    /// confirm once the peer's commit has been processed. 0 when there is none.
    pub fn message(&mut self, out: &mut [u8; BODY_MAX]) -> usize {
        out[..2].copy_from_slice(&ALGORITHM.to_le_bytes());
        match self.state {
            State::Committed => {
                let status = if self.h2e { STATUS_H2E } else { 0 };
                out[2..6].copy_from_slice(&[1, 0, status as u8, 0]);
                out[6..8].copy_from_slice(&GROUP.to_le_bytes());
                let mut at = 8;
                let token = &self.token[..self.token_len];
                if !self.h2e {
                    out[at..at + token.len()].copy_from_slice(token);
                    at += token.len();
                }
                out[at..at + 32].copy_from_slice(&self.scalar);
                out[at + 32..at + 96].copy_from_slice(&self.element);
                at += 96;
                if self.h2e && !token.is_empty() {
                    out[at..at + 3].copy_from_slice(&[EXTENSION, 1 + token.len() as u8, EXT_TOKEN]);
                    out[at + 3..at + 3 + token.len()].copy_from_slice(token);
                    at += 3 + token.len();
                }
                at
            }
            State::Confirmed => {
                self.send_confirm = self.send_confirm.saturating_add(1);
                out[2..6].copy_from_slice(&[2, 0, 0, 0]);
                let sc = self.send_confirm.to_le_bytes();
                out[6..8].copy_from_slice(&sc);
                let c = hmac(&self.kck, &[&sc, &self.scalar, &self.element, &self.peer_scalar, &self.peer_element]);
                out[8..40].copy_from_slice(&c);
                40
            }
            _ => 0,
        }
    }

    /// The retransmission timer fired.
    pub fn timeout(&mut self) -> Step {
        if !matches!(self.state, State::Committed | State::Confirmed) {
            return Step::Ignore;
        }
        self.sync += 1;
        if self.sync > SYNC {
            self.state = State::Failed;
            return Step::Fail(0);
        }
        Step::Send
    }

    fn fail(&mut self, status: u16) -> Step {
        self.state = State::Failed;
        Step::Fail(status)
    }

    /// One authentication frame body from the access point (after the
    /// 24-octet header): algorithm, transaction, status, then the rest.
    pub fn receive(&mut self, body: &[u8]) -> Step {
        if body.len() < 6 || u16::from_le_bytes([body[0], body[1]]) != ALGORITHM {
            return Step::Ignore;
        }
        let seq = u16::from_le_bytes([body[2], body[3]]);
        let status = u16::from_le_bytes([body[4], body[5]]);
        let rest = &body[6..];
        match (seq, self.state) {
            (1, State::Committed) if status == STATUS_ANTI_CLOGGING => self.anti_clogging(rest),
            (1, State::Committed | State::Confirmed) if status == if self.h2e { STATUS_H2E } else { 0 } => {
                self.peer_commit(rest)
            }
            // A refusal, or a commit of the other password element method.
            (1, State::Committed) => self.fail(status),
            (2, State::Confirmed) if status != 0 => self.fail(status),
            (2, State::Confirmed) => self.peer_confirm(rest),
            _ => Step::Ignore,
        }
    }

    fn anti_clogging(&mut self, rest: &[u8]) -> Step {
        if rest.len() < 2 || u16::from_le_bytes([rest[0], rest[1]]) != GROUP {
            return self.fail(STATUS_ANTI_CLOGGING);
        }
        let mut token = &rest[2..];
        if self.h2e {
            // In an Anti-Clogging Token Container element.
            match token {
                [EXTENSION, len, EXT_TOKEN, t @ ..] if *len as usize >= 2 && t.len() >= *len as usize - 1 => {
                    token = &t[..*len as usize - 1];
                }
                _ => return self.fail(STATUS_ANTI_CLOGGING),
            }
        }
        self.tokens += 1;
        if token.is_empty() || token.len() > TOKEN_MAX || self.tokens > TOKENS {
            return self.fail(STATUS_ANTI_CLOGGING);
        }
        self.token[..token.len()].copy_from_slice(token);
        self.token_len = token.len();
        Step::Send
    }

    fn peer_commit(&mut self, rest: &[u8]) -> Step {
        if rest.len() < 98 || u16::from_le_bytes([rest[0], rest[1]]) != GROUP {
            return self.fail(STATUS_GROUP);
        }
        let scalar: [u8; 32] = rest[2..34].try_into().unwrap();
        let element: [u8; 64] = rest[34..98].try_into().unwrap();
        if self.state == State::Confirmed {
            // The access point did not hear our confirm: send it again.
            if scalar == self.peer_scalar && element == self.peer_element {
                return self.timeout();
            }
            return Step::Ignore;
        }
        // A reflection of our own commit is dropped without an answer.
        if scalar == self.scalar && element == self.element {
            return Step::Ignore;
        }
        let s = from_be(&scalar);
        if !below(&s, &N.m) || below(&s, &[2, 0, 0, 0, 0, 0, 0, 0]) {
            return self.fail(1);
        }
        let Some(e) = point(&element) else { return self.fail(1) };
        // With H2E the key seed is salted with the groups the station
        // rejected (12.4.5.4), from its own Rejected Groups element: this
        // station offers group 19 only and never rejects one, so the salt is
        // 32 zero octets. A Rejected Groups element in the access point's
        // commit is not part of it.
        let salt = [0u8; 32];
        // K = rand (scalar PWE + element); k is its x.
        let Some(mut k) = bytes(&mul(&self.rand, &add(&mul(&s, &self.pwe), &e))) else { return self.fail(1) };
        let mut keyseed = hmac(&salt, &[&k[..32]]);
        wipe(&mut k);
        let context = to_be(&N.add(&from_be(&self.scalar), &s));
        let mut keys = [0u8; 64];
        kdf(&keyseed, b"SAE KCK and PMK", &context, &mut keys);
        self.kck.copy_from_slice(&keys[..32]);
        self.pmk.copy_from_slice(&keys[32..]);
        self.pmkid.copy_from_slice(&context[..16]);
        wipe(&mut keys);
        wipe(&mut keyseed);
        self.peer_scalar = scalar;
        self.peer_element = element;
        self.state = State::Confirmed;
        self.sync = 0;
        Step::Send
    }

    fn peer_confirm(&mut self, rest: &[u8]) -> Step {
        if rest.len() < 34 {
            return Step::Ignore;
        }
        let want = hmac(&self.kck, &[&rest[..2], &self.peer_scalar, &self.peer_element, &self.scalar, &self.element]);
        let mut d = 0u8;
        for i in 0..32 {
            d |= want[i] ^ rest[2 + i];
        }
        if d != 0 {
            // Not from the peer that knows the password: a forgery is not
            // allowed to end the exchange; the timer does.
            return Step::Ignore;
        }
        self.state = State::Accepted;
        Step::Done
    }
}

// ------------------------------------------------- transition mode

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Akm {
    /// SAE (00-0F-AC:8), with the password element from hash-to-element or not.
    Sae { h2e: bool },
    /// PSK (00-0F-AC:2) or PSK-SHA256 (00-0F-AC:6).
    Psk { sha256: bool },
}

/// The key management to use with an access point. `akms`: the suite types
/// (00-0F-AC:n) of its RSN element; `mfpc`: it is capable of management frame
/// protection (WPA3 requires it); `h2e`: its RSN Extension element has SAE
/// Hash-to-Element (bit 5); `h2e_only`: it lists BSS membership selector 123
/// (only H2E stations may join). `password`: the station has the network's
/// password (or PT); `pmk`: it has the PSK.
///
/// `sae_only`: the network is known to take SAE, because the station has
/// joined it with SAE before or a 4-way handshake there carried Transition
/// Disable for WPA3-Personal (`transition_disable`); the caller keeps that
/// with the network's profile.
///
/// SAE wins whenever both sides can do it (transition mode included); PSK is
/// used only when SAE is impossible, the access point allows PSK, and the
/// network is not known to take SAE. So a rogue access point with the same
/// SSID that offers only PSK (or SAE without MFPC) gets no handshake, whose
/// message 2 would let it guess the password offline.
pub fn choose(akms: &[u8], mfpc: bool, h2e: bool, h2e_only: bool, password: bool, pmk: bool, sae_only: bool) -> Option<Akm> {
    if akms.contains(&8) && mfpc && password {
        return Some(Akm::Sae { h2e: h2e || h2e_only });
    }
    if h2e_only || sae_only || !pmk {
        return None;
    }
    if akms.contains(&6) {
        Some(Akm::Psk { sha256: true })
    } else if akms.contains(&2) {
        Some(Akm::Psk { sha256: false })
    } else {
        None
    }
}

/// Transition Disable (IEEE 802.11-2020 12.7.2, KDE 00-0F-AC:32): its bit
/// for WPA3-Personal. Once set for a network, PSK is never used there again.
pub const TRANSITION_DISABLE_SAE: u8 = 1;

/// The Transition Disable bitmap in the key data of message 3 (decrypted),
/// if the access point sent one. Key data is a run of elements and KDEs
/// (0xdd, length, OUI 00-0F-AC, type, data); padding (0xdd with length 0)
/// ends it.
pub fn transition_disable(key_data: &[u8]) -> Option<u8> {
    let mut rest = key_data;
    while let [id, len, more @ ..] = rest {
        let n = *len as usize;
        if more.len() < n || (*id == 0xdd && n == 0) {
            return None;
        }
        if *id == 0xdd && n >= 5 && more[..4] == [0, 15, 172, 32] {
            return Some(more[4]);
        }
        rest = &more[n..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    fn g() -> Pt {
        Pt { x: P.to(&GX), y: P.to(&GY), z: P.one() }
    }

    #[test]
    fn sha256_and_hmac_match_their_standards() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(h.finish().to_vec(), hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        let mut h = Sha256::new();
        h.update(&[b'a'; 1000]);
        assert_eq!(h.finish().to_vec(), hex("41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"));
        // RFC 4231 test cases 1 and 6 (a key longer than a block).
        assert_eq!(hmac(&[0x0b; 20], &[b"Hi There"]).to_vec(), hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"));
        assert_eq!(
            hmac(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"]).to_vec(),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    #[test]
    fn montgomery_constants_and_exponents() {
        let one = P.one();
        let two = P.to(&[2, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(P.from(&P.mul(&P.pow(&two, &P_MINUS_2), &two)), [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(P.from(&N.to(&[5, 0, 0, 0, 0, 0, 0, 0])).len(), 8);
        assert_eq!(N.from(&N.mul(&N.to(&[3, 0, 0, 0, 0, 0, 0, 0]), &N.to(&[4, 0, 0, 0, 0, 0, 0, 0]))), [12, 0, 0, 0, 0, 0, 0, 0]);
        // 4 is a square, its root is 2 or p - 2; -1 is not a square (p = 3 mod 4).
        let r = P.from(&P.pow(&P.to(&[4, 0, 0, 0, 0, 0, 0, 0]), &P_QUARTER));
        assert!(r == [2, 0, 0, 0, 0, 0, 0, 0] || r == P.sub(&[0; 8], &[2, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(P.pow(&P.sub(&[0; 8], &one), &P_HALF), P.sub(&[0; 8], &one));
    }

    #[test]
    fn the_generator_has_order_n() {
        assert!(point(&{
            let mut b = [0u8; 64];
            b[..32].copy_from_slice(&to_be(&GX));
            b[32..].copy_from_slice(&to_be(&GY));
            b
        })
        .is_some());
        assert!(bytes(&mul(&N_M, &g())).is_none(), "n G is the identity");
        let two = bytes(&mul(&[2, 0, 0, 0, 0, 0, 0, 0], &g())).unwrap();
        assert_eq!(two.to_vec(), bytes(&add(&g(), &g())).unwrap().to_vec());
        assert_eq!(
            two.to_vec(),
            hex("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1")
        );
        // (n - 1) G = -G, and G + (-G) is the identity.
        let minus = bytes(&mul(&sub_raw(&N_M, &[1, 0, 0, 0, 0, 0, 0, 0]).0, &g())).unwrap();
        assert_eq!(from_be(&minus[..32]), GX);
        assert_eq!(from_be(&minus[32..]), P.sub(&[0; 8], &GY));
        let neg = Pt { x: P.to(&GX), y: P.to(&P.sub(&[0; 8], &GY)), z: P.one() };
        assert!(bytes(&add(&g(), &neg)).is_none());
    }

    #[test]
    fn points_off_the_curve_or_out_of_range_are_refused() {
        let mut b = [0u8; 64];
        b[..32].copy_from_slice(&to_be(&GX));
        b[32..].copy_from_slice(&to_be(&GY));
        b[63] ^= 1;
        assert!(point(&b).is_none());
        let mut b = [0xff; 64];
        assert!(point(&b).is_none());
        b = [0; 64];
        assert!(point(&b).is_none());
    }

    #[test]
    fn dropping_an_exchange_wipes_its_secrets() {
        let pwe = hunting_and_pecking(b"password", &[2, 0, 0, 0, 0, 1], &[2, 0, 0, 0, 0, 2]).unwrap();
        let mut n = 0u8;
        let s = Sae::new(&pwe, false, &mut |b: &mut [u8; 32]| {
            n += 1;
            b.fill(n);
            true
        })
        .unwrap();
        let mut s = core::mem::ManuallyDrop::new(s);
        s.token[..3].copy_from_slice(&[1, 2, 3]);
        s.pmkid = [9; 16];
        s.kck = [7; 32];
        assert!(s.pwe.x != [0; 8] && s.rand != [0; 8]);
        unsafe { core::mem::ManuallyDrop::drop(&mut s) };
        // ManuallyDrop keeps the memory: every secret in it is now zero.
        assert_eq!((s.rand, s.pwe.x, s.pwe.y, s.pwe.z), ([0; 8], [0; 8], [0; 8], [0; 8]));
        assert!(s.kck == [0; 32] && s.pmk == [0; 32] && s.pmkid == [0; 16] && s.token.iter().all(|&b| b == 0));
    }
}
