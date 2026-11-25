//! The end-to-end layer of docs/agent-protocol.md: identities, the HELLO
//! handshake and ChaCha20-Poly1305 records. Pure computation over byte
//! slices, so `cargo test` checks it on the Mac.
use ring::{aead, agreement, digest, hkdf, hmac, rand, signature};
use ring::signature::KeyPair;

pub const HELLO: u8 = 0x01;
pub const RECORD: u8 = 0x02;
pub const PHONE: u8 = b'p';
pub const MAC: u8 = b'm';
pub const SESSION: u8 = 0;
pub const PAIRING: u8 = 1;

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut c = digest::Context::new(&digest::SHA256);
    for p in parts {
        c.update(p);
    }
    c.finish().as_ref().try_into().unwrap()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::SecureRandom::fill(&rand::SystemRandom::new(), &mut b).expect("no randomness");
    b
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn b64url(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for chunk in b.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &v)| n | (v as u32) << (16 - 8 * i));
        for i in 0..chunk.len() + 1 {
            s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

/// The 16-byte room a pairing secret opens.
pub fn pairing_room(secret: &[u8; 32]) -> [u8; 16] {
    sha256(&[b"violet-agent pair", secret])[..16].try_into().unwrap()
}

/// The 16-byte room a paired phone and Mac meet in.
pub fn session_room(phone: &[u8; 32], mac: &[u8; 32]) -> [u8; 16] {
    sha256(&[b"violet-agent room", phone, mac])[..16].try_into().unwrap()
}

pub struct Identity {
    pair: signature::Ed25519KeyPair,
}

impl Identity {
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        let pair = signature::Ed25519KeyPair::from_seed_unchecked(&seed).expect("bad seed");
        Identity { pair }
    }
    pub fn public(&self) -> [u8; 32] {
        self.pair.public_key().as_ref().try_into().unwrap()
    }
    pub fn sign(&self, m: &[u8]) -> [u8; 64] {
        self.pair.sign(m).as_ref().try_into().unwrap()
    }
}

fn signed(role: u8, mode: u8, room: &[u8; 16], ed: &[u8; 32], x: &[u8; 32], answer: &[u8; 32]) -> Vec<u8> {
    [b"violet-agent hello v1" as &[u8], &[role, mode], room, ed, x, answer].concat()
}

fn pair_proof(secret: &[u8; 32], ed: &[u8; 32], x: &[u8; 32]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::sign(&key, &[b"violet-agent pair" as &[u8], ed, x].concat()).as_ref().try_into().unwrap()
}

/// A parsed, signature-checked HELLO.
pub struct Hello {
    pub role: u8,
    pub mode: u8,
    pub ed: [u8; 32],
    pub x: [u8; 32],
    /// The phone's X25519 key a Mac's HELLO answers (zero in the phone's own).
    pub answer: [u8; 32],
    pub proof: Option<[u8; 32]>,
}

pub fn parse_hello(m: &[u8], room: &[u8; 16]) -> Option<Hello> {
    if m.len() < 163 || m[0] != HELLO {
        return None;
    }
    let (role, mode) = (m[1], m[2]);
    let ed: [u8; 32] = m[3..35].try_into().ok()?;
    let x: [u8; 32] = m[35..67].try_into().ok()?;
    let sig = &m[67..131];
    let answer: [u8; 32] = m[131..163].try_into().ok()?;
    signature::UnparsedPublicKey::new(&signature::ED25519, &ed).verify(&signed(role, mode, room, &ed, &x, &answer), sig).ok()?;
    let proof = (m.len() >= 195).then(|| m[163..195].try_into().unwrap());
    Some(Hello { role, mode, ed, x, answer, proof })
}

/// Our half of a handshake: the ephemeral key and the HELLO that carries it.
pub struct Offer {
    key: agreement::EphemeralPrivateKey,
    pub x: [u8; 32],
    pub message: Vec<u8>,
}

pub fn offer(me: &Identity, role: u8, mode: u8, room: &[u8; 16], secret: Option<&[u8; 32]>) -> Offer {
    answer(me, role, mode, room, secret, &[0; 32])
}

/// A HELLO that answers the peer's (the Mac's half of a handshake).
pub fn answer(me: &Identity, role: u8, mode: u8, room: &[u8; 16], secret: Option<&[u8; 32]>, to: &[u8; 32]) -> Offer {
    let rng = rand::SystemRandom::new();
    let key = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).expect("x25519");
    let x: [u8; 32] = key.compute_public_key().expect("x25519").as_ref().try_into().unwrap();
    let ed = me.public();
    let mut message = vec![HELLO, role, mode];
    message.extend_from_slice(&ed);
    message.extend_from_slice(&x);
    message.extend_from_slice(&me.sign(&signed(role, mode, room, &ed, &x, to)));
    message.extend_from_slice(to);
    if let Some(s) = secret {
        message.extend_from_slice(&pair_proof(s, &ed, &x));
    }
    Offer { key, x, message }
}

/// Check a pairing Mac's proof that it read the secret from our QR code.
pub fn proof_ok(secret: &[u8; 32], h: &Hello) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    h.proof.is_some_and(|p| hmac::verify(&key, &[b"violet-agent pair" as &[u8], &h.ed, &h.x].concat(), &p).is_ok())
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// An established session: one key per direction and the counters.
pub struct Session {
    send: aead::LessSafeKey,
    recv: aead::LessSafeKey,
    sent: u64,
    received: u64,
}

/// Finish a handshake. `phone` and `mac` are the two Ed25519 keys.
pub fn complete(ours: Offer, theirs: &Hello, role: u8, room: &[u8; 16], phone: &[u8; 32], mac: &[u8; 32]) -> Option<Session> {
    let (px, mx) = if role == PHONE { (ours.x, theirs.x) } else { (theirs.x, ours.x) };
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, theirs.x);
    let info = [b"violet-agent v1" as &[u8], phone, mac, &px, &mx].concat();
    let okm = agreement::agree_ephemeral(ours.key, &peer, |shared| {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, room).extract(shared);
        let mut out = [0u8; 64];
        prk.expand(&[&info], Len(64)).ok()?.fill(&mut out).ok()?;
        Some(out)
    })
    .ok()??;
    let key = |k: &[u8]| aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, k).unwrap());
    let (to_mac, to_phone) = (key(&okm[..32]), key(&okm[32..]));
    let (send, recv) = if role == PHONE { (to_mac, to_phone) } else { (to_phone, to_mac) };
    Some(Session { send, recv, sent: 0, received: 0 })
}

fn nonce(counter: u64) -> aead::Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    aead::Nonce::assume_unique_for_key(n)
}

impl Session {
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        self.sent += 1;
        let mut out = Vec::with_capacity(plain.len() + 25);
        out.push(RECORD);
        out.extend_from_slice(&self.sent.to_be_bytes());
        let mut body = plain.to_vec();
        self.send.seal_in_place_append_tag(nonce(self.sent), aead::Aad::empty(), &mut body).expect("seal");
        out.extend_from_slice(&body);
        out
    }
    pub fn open(&mut self, m: &[u8]) -> Option<Vec<u8>> {
        if m.len() < 25 || m[0] != RECORD {
            return None;
        }
        let counter = u64::from_be_bytes(m[1..9].try_into().unwrap());
        if counter <= self.received {
            return None;
        }
        let mut body = m[9..].to_vec();
        let n = self.recv.open_in_place(nonce(counter), aead::Aad::empty(), &mut body).ok()?.len();
        body.truncate(n);
        self.received = counter;
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_then_records() {
        let phone = Identity::from_seed([1; 32]);
        let mac = Identity::from_seed([2; 32]);
        let secret = [7u8; 32];
        let room = pairing_room(&secret);
        let p = offer(&phone, PHONE, PAIRING, &room, None);
        let hp = parse_hello(&p.message, &room).unwrap();
        let m = answer(&mac, MAC, PAIRING, &room, Some(&secret), &hp.x);
        let hm = parse_hello(&m.message, &room).unwrap();
        assert_eq!(hm.answer, p.x);
        assert!(proof_ok(&secret, &hm));
        assert!(!proof_ok(&[8; 32], &hm));
        assert!(!proof_ok(&secret, &hp));
        let (pk, mk) = (phone.public(), mac.public());
        let mut a = complete(p, &hm, PHONE, &room, &pk, &mk).unwrap();
        let mut b = complete(m, &hp, MAC, &room, &pk, &mk).unwrap();
        let r = a.seal(b"hello mac");
        assert_eq!(b.open(&r).unwrap(), b"hello mac");
        assert!(b.open(&r).is_none(), "replay is dropped");
        let r2 = b.seal(b"hi phone");
        assert_eq!(a.open(&r2).unwrap(), b"hi phone");
        let mut bad = a.seal(b"x");
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(b.open(&bad).is_none());
    }

    #[test]
    fn hello_bound_to_room() {
        let phone = Identity::from_seed([1; 32]);
        let room = [3u8; 16];
        let p = offer(&phone, PHONE, SESSION, &room, None);
        assert!(parse_hello(&p.message, &room).is_some());
        assert!(parse_hello(&p.message, &[4u8; 16]).is_none());
    }

    #[test]
    fn encodings() {
        assert_eq!(b64url(b"\xfb\xff"), "-_8");
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), vec![0, 255, 16]);
    }
}
