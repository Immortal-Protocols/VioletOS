//! AES-256 in constant time: no table is indexed by key or data, and no
//! branch depends on them. The state is bitsliced, two blocks at a time in
//! eight 32-bit words, and the S-box is the Boolean circuit of Boyar and
//! Peralta ("A new combinational logic minimization technique with
//! applications to cryptology", eprint 2009/191).
//!
//! This is a port of the `aes_ct` implementation of BearSSL 0.6 (Thomas
//! Pornin, MIT licence) from C. The host tests check it against FIPS-197
//! appendix C.3 and the whole S-box against the field arithmetic.
use super::sha::wipe_words;

fn sbox(q: &mut [u32; 8]) {
    // x0 is the high bit, x7 the low one.
    let (x0, x1, x2, x3, x4, x5, x6, x7) = (q[7], q[6], q[5], q[4], q[3], q[2], q[1], q[0]);
    // Top linear transformation.
    let y14 = x3 ^ x5;
    let y13 = x0 ^ x6;
    let y9 = x0 ^ x3;
    let y8 = x0 ^ x5;
    let t0 = x1 ^ x2;
    let y1 = t0 ^ x7;
    let y4 = y1 ^ x3;
    let y12 = y13 ^ y14;
    let y2 = y1 ^ x0;
    let y5 = y1 ^ x6;
    let y3 = y5 ^ y8;
    let t1 = x4 ^ y12;
    let y15 = t1 ^ x5;
    let y20 = t1 ^ x1;
    let y6 = y15 ^ x7;
    let y10 = y15 ^ t0;
    let y11 = y20 ^ y9;
    let y7 = x7 ^ y11;
    let y17 = y10 ^ y11;
    let y19 = y10 ^ y8;
    let y16 = t0 ^ y11;
    let y21 = y13 ^ y16;
    let y18 = x0 ^ y16;
    // Non-linear section.
    let t2 = y12 & y15;
    let t3 = y3 & y6;
    let t4 = t3 ^ t2;
    let t5 = y4 & x7;
    let t6 = t5 ^ t2;
    let t7 = y13 & y16;
    let t8 = y5 & y1;
    let t9 = t8 ^ t7;
    let t10 = y2 & y7;
    let t11 = t10 ^ t7;
    let t12 = y9 & y11;
    let t13 = y14 & y17;
    let t14 = t13 ^ t12;
    let t15 = y8 & y10;
    let t16 = t15 ^ t12;
    let t17 = t4 ^ t14;
    let t18 = t6 ^ t16;
    let t19 = t9 ^ t14;
    let t20 = t11 ^ t16;
    let t21 = t17 ^ y20;
    let t22 = t18 ^ y19;
    let t23 = t19 ^ y21;
    let t24 = t20 ^ y18;
    let t25 = t21 ^ t22;
    let t26 = t21 & t23;
    let t27 = t24 ^ t26;
    let t28 = t25 & t27;
    let t29 = t28 ^ t22;
    let t30 = t23 ^ t24;
    let t31 = t22 ^ t26;
    let t32 = t31 & t30;
    let t33 = t32 ^ t24;
    let t34 = t23 ^ t33;
    let t35 = t27 ^ t33;
    let t36 = t24 & t35;
    let t37 = t36 ^ t34;
    let t38 = t27 ^ t36;
    let t39 = t29 & t38;
    let t40 = t25 ^ t39;
    let t41 = t40 ^ t37;
    let t42 = t29 ^ t33;
    let t43 = t29 ^ t40;
    let t44 = t33 ^ t37;
    let t45 = t42 ^ t41;
    let z0 = t44 & y15;
    let z1 = t37 & y6;
    let z2 = t33 & x7;
    let z3 = t43 & y16;
    let z4 = t40 & y1;
    let z5 = t29 & y7;
    let z6 = t42 & y11;
    let z7 = t45 & y17;
    let z8 = t41 & y10;
    let z9 = t44 & y12;
    let z10 = t37 & y3;
    let z11 = t33 & y4;
    let z12 = t43 & y13;
    let z13 = t40 & y5;
    let z14 = t29 & y2;
    let z15 = t42 & y9;
    let z16 = t45 & y14;
    let z17 = t41 & y8;
    // Bottom linear transformation.
    let t46 = z15 ^ z16;
    let t47 = z10 ^ z11;
    let t48 = z5 ^ z13;
    let t49 = z9 ^ z10;
    let t50 = z2 ^ z12;
    let t51 = z2 ^ z5;
    let t52 = z7 ^ z8;
    let t53 = z0 ^ z3;
    let t54 = z6 ^ z7;
    let t55 = z16 ^ z17;
    let t56 = z12 ^ t48;
    let t57 = t50 ^ t53;
    let t58 = z4 ^ t46;
    let t59 = z3 ^ t54;
    let t60 = t46 ^ t57;
    let t61 = z14 ^ t57;
    let t62 = t52 ^ t58;
    let t63 = t49 ^ t58;
    let t64 = z4 ^ t59;
    let t65 = t61 ^ t62;
    let t66 = z1 ^ t63;
    let s0 = t59 ^ t63;
    let s6 = t56 ^ !t62;
    let s7 = t48 ^ !t60;
    let t67 = t64 ^ t65;
    let s3 = t53 ^ t66;
    let s4 = t51 ^ t66;
    let s5 = t47 ^ t65;
    let s1 = t64 ^ !s3;
    let s2 = t55 ^ !t67;
    *q = [s7, s6, s5, s4, s3, s2, s1, s0];
}

/// The inverse S-box from the forward one: iS(x) = B(S(B(x ^ 0x63)) ^ 0x63),
/// B the inverse of the S-box's affine map.
fn inv_sbox(q: &mut [u32; 8]) {
    fn b(q: &mut [u32; 8]) {
        let (q0, q1, q2, q3, q4, q5, q6, q7) = (!q[0], !q[1], q[2], q[3], q[4], !q[5], !q[6], q[7]);
        *q = [q2 ^ q5 ^ q7, q3 ^ q6 ^ q0, q4 ^ q7 ^ q1, q5 ^ q0 ^ q2, q6 ^ q1 ^ q3, q7 ^ q2 ^ q4, q0 ^ q3 ^ q5, q1 ^ q4 ^ q6];
    }
    b(q);
    sbox(q);
    b(q);
}

fn swap(q: &mut [u32], x: usize, y: usize, low: u32, s: u32) {
    let (a, b) = (q[x], q[y]);
    q[x] = (a & low) | ((b & low) << s);
    q[y] = ((a & !low) >> s) | (b & !low);
}
/// To and from the bitsliced form (its own inverse): afterwards word `i`
/// holds bit `i` of every byte of both blocks.
fn ortho(q: &mut [u32]) {
    for i in [0, 2, 4, 6] {
        swap(q, i, i + 1, 0x5555_5555, 1);
    }
    for i in [0, 1, 4, 5] {
        swap(q, i, i + 2, 0x3333_3333, 2);
    }
    for i in 0..4 {
        swap(q, i, i + 4, 0x0f0f_0f0f, 4);
    }
}

fn sub_word(x: u32) -> u32 {
    let mut q = [x; 8];
    ortho(&mut q);
    sbox(&mut q);
    ortho(&mut q);
    let out = q[0];
    wipe_words(&mut q);
    out
}

fn add_round_key(q: &mut [u32; 8], sk: &[u32]) {
    for (q, k) in q.iter_mut().zip(sk) {
        *q ^= k;
    }
}

fn shift_rows(q: &mut [u32; 8]) {
    for x in q.iter_mut() {
        let v = *x;
        *x = (v & 0x0000_00ff)
            | ((v & 0x0000_fc00) >> 2)
            | ((v & 0x0000_0300) << 6)
            | ((v & 0x00f0_0000) >> 4)
            | ((v & 0x000f_0000) << 4)
            | ((v & 0xc000_0000) >> 6)
            | ((v & 0x3f00_0000) << 2);
    }
}
fn inv_shift_rows(q: &mut [u32; 8]) {
    for x in q.iter_mut() {
        let v = *x;
        *x = (v & 0x0000_00ff)
            | ((v & 0x0000_3f00) << 2)
            | ((v & 0x0000_c000) >> 6)
            | ((v & 0x000f_0000) << 4)
            | ((v & 0x00f0_0000) >> 4)
            | ((v & 0x0300_0000) << 6)
            | ((v & 0xfc00_0000) >> 2);
    }
}

fn mix_columns(q: &mut [u32; 8]) {
    let [q0, q1, q2, q3, q4, q5, q6, q7] = *q;
    let r = q.map(|x| x.rotate_right(8));
    let [r0, r1, r2, r3, r4, r5, r6, r7] = r;
    q[0] = q7 ^ r7 ^ r0 ^ (q0 ^ r0).rotate_right(16);
    q[1] = q0 ^ r0 ^ q7 ^ r7 ^ r1 ^ (q1 ^ r1).rotate_right(16);
    q[2] = q1 ^ r1 ^ r2 ^ (q2 ^ r2).rotate_right(16);
    q[3] = q2 ^ r2 ^ q7 ^ r7 ^ r3 ^ (q3 ^ r3).rotate_right(16);
    q[4] = q3 ^ r3 ^ q7 ^ r7 ^ r4 ^ (q4 ^ r4).rotate_right(16);
    q[5] = q4 ^ r4 ^ r5 ^ (q5 ^ r5).rotate_right(16);
    q[6] = q5 ^ r5 ^ r6 ^ (q6 ^ r6).rotate_right(16);
    q[7] = q6 ^ r6 ^ r7 ^ (q7 ^ r7).rotate_right(16);
}
fn inv_mix_columns(q: &mut [u32; 8]) {
    let [q0, q1, q2, q3, q4, q5, q6, q7] = *q;
    let r = q.map(|x| x.rotate_right(8));
    let [r0, r1, r2, r3, r4, r5, r6, r7] = r;
    q[0] = q5 ^ q6 ^ q7 ^ r0 ^ r5 ^ r7 ^ (q0 ^ q5 ^ q6 ^ r0 ^ r5).rotate_right(16);
    q[1] = q0 ^ q5 ^ r0 ^ r1 ^ r5 ^ r6 ^ r7 ^ (q1 ^ q5 ^ q7 ^ r1 ^ r5 ^ r6).rotate_right(16);
    q[2] = q0 ^ q1 ^ q6 ^ r1 ^ r2 ^ r6 ^ r7 ^ (q0 ^ q2 ^ q6 ^ r2 ^ r6 ^ r7).rotate_right(16);
    q[3] = q0 ^ q1 ^ q2 ^ q5 ^ q6 ^ r0 ^ r2 ^ r3 ^ r5 ^ (q0 ^ q1 ^ q3 ^ q5 ^ q6 ^ q7 ^ r0 ^ r3 ^ r5 ^ r7).rotate_right(16);
    q[4] = q1 ^ q2 ^ q3 ^ q5 ^ r1 ^ r3 ^ r4 ^ r5 ^ r6 ^ r7 ^ (q1 ^ q2 ^ q4 ^ q5 ^ q7 ^ r1 ^ r4 ^ r5 ^ r6).rotate_right(16);
    q[5] = q2 ^ q3 ^ q4 ^ q6 ^ r2 ^ r4 ^ r5 ^ r6 ^ r7 ^ (q2 ^ q3 ^ q5 ^ q6 ^ r2 ^ r5 ^ r6 ^ r7).rotate_right(16);
    q[6] = q3 ^ q4 ^ q5 ^ q7 ^ r3 ^ r5 ^ r6 ^ r7 ^ (q3 ^ q4 ^ q6 ^ q7 ^ r3 ^ r6 ^ r7).rotate_right(16);
    q[7] = q4 ^ q5 ^ q6 ^ r4 ^ r6 ^ r7 ^ (q4 ^ q5 ^ q7 ^ r4 ^ r7).rotate_right(16);
}

const ROUNDS: usize = 14;
const RCON: [u32; 7] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40];

/// An expanded AES-256 key, already bitsliced. Zeroed when dropped.
pub struct Aes256 {
    sk: [u32; (ROUNDS + 1) * 8],
}

impl Aes256 {
    pub fn new(key: &[u8; 32]) -> Aes256 {
        // Built in place in the value returned, not in a stack array that
        // is then copied out and left behind.
        let mut out = Aes256 { sk: [0u32; (ROUNDS + 1) * 8] };
        let sk = &mut out.sk;
        let mut tmp = 0u32;
        for i in 0..8 {
            tmp = u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
            sk[2 * i] = tmp;
            sk[2 * i + 1] = tmp;
        }
        // The schedule's words, each twice: the same key for both blocks.
        for i in 8..(ROUNDS + 1) * 4 {
            if i % 8 == 0 {
                tmp = sub_word(tmp.rotate_right(8)) ^ RCON[i / 8 - 1];
            } else if i % 8 == 4 {
                tmp = sub_word(tmp);
            }
            tmp ^= sk[(i - 8) * 2];
            sk[2 * i] = tmp;
            sk[2 * i + 1] = tmp;
        }
        wipe_words(core::slice::from_mut(&mut tmp));
        for round in sk.chunks_exact_mut(8) {
            ortho(round);
        }
        out
    }

    /// Encrypt (or decrypt) two 16-byte blocks in place.
    pub fn pair(&self, blocks: &mut [u8], decrypt: bool) {
        debug_assert!(blocks.len() == 32);
        let mut q = [0u32; 8];
        for (i, w) in blocks.chunks_exact(4).enumerate() {
            // Block one in the even words, block two in the odd ones.
            q[(i % 4) * 2 + i / 4] = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
        ortho(&mut q);
        let key = |round: usize| &self.sk[round * 8..round * 8 + 8];
        if decrypt {
            add_round_key(&mut q, key(ROUNDS));
            for round in (1..ROUNDS).rev() {
                inv_shift_rows(&mut q);
                inv_sbox(&mut q);
                add_round_key(&mut q, key(round));
                inv_mix_columns(&mut q);
            }
            inv_shift_rows(&mut q);
            inv_sbox(&mut q);
            add_round_key(&mut q, key(0));
        } else {
            add_round_key(&mut q, key(0));
            for round in 1..ROUNDS {
                sbox(&mut q);
                shift_rows(&mut q);
                mix_columns(&mut q);
                add_round_key(&mut q, key(round));
            }
            sbox(&mut q);
            shift_rows(&mut q);
            add_round_key(&mut q, key(ROUNDS));
        }
        ortho(&mut q);
        for (i, w) in blocks.chunks_exact_mut(4).enumerate() {
            w.copy_from_slice(&q[(i % 4) * 2 + i / 4].to_le_bytes());
        }
        wipe_words(&mut q);
    }

    /// The S-box of one byte, for the host test of the circuit.
    #[cfg(test)]
    pub fn sbox_of(x: u8) -> u8 {
        sub_word(x as u32) as u8
    }
}

impl Drop for Aes256 {
    fn drop(&mut self) {
        wipe_words(&mut self.sk);
    }
}
