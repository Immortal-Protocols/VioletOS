//! VSP1: how the phone comes to trust a companion's signing key
//! (docs/shield-pairing.md is the specification; tools/shield-trainer/pairing.py
//! is the companion's side).
//!
//! Until now the key that signs Shield commands was fixed when the image was
//! built. With VSP1 the user starts pairing on the phone, the phone shows a
//! one-time secret (a QR code on the trusted screen), and the companion that
//! saw it sends its public key over the Sentinel link, authenticated with
//! that secret and with a signature by the key itself. The phone answers with
//! a confirmation only a holder of the secret can make.
//!
//! The key is a SOFTWARE key today: it stands in for a key held in a Secure
//! Enclave with an attestation the phone can check. Nothing here can tell
//! where a private key lives, so every key enrolled now is recorded as
//! `KIND_SOFTWARE`, may only have the automatic tier, and `KIND_ATTESTED` is
//! refused until a verifier for its attestation exists.
//!
//! Pure logic: hashing and signature checks come through `Crypto`, time and
//! randomness from the caller, nothing is stored here. NOT yet called by the
//! Sentinel (see the specification's "What is not built").

pub const VERSION: u16 = 1;
pub const KIND_SOFTWARE: u8 = 1;
/// A hardware-held key with an attestation. Refused: nothing can verify one yet.
pub const KIND_ATTESTED: u8 = 2;
pub const TIER_AUTO: u8 = 1;
pub const TIER_OWNER: u8 = 2;

/// How long the secret on the screen is good for, and how many wrong tries.
pub const WINDOW_NS: u64 = 120 * 1_000_000_000;
pub const ATTEMPTS: u8 = 3;

/// "VSP1" | version | phone nonce | secret: what the phone shows.
pub const OFFER_LEN: usize = 4 + 2 + 16 + 16;
/// "VSPE" | version | kind | tier | companion nonce | key | attestation length.
const ENROLL_HEAD: usize = 4 + 2 + 1 + 1 + 16 + 65 + 2;
pub const ENROLL_MAX: usize = ENROLL_HEAD + 32 + 2 + 72;
/// "VSPC" | version | status | key fingerprint | MAC.
pub const CONFIRM_LEN: usize = 4 + 2 + 1 + 8 + 32;
/// "VSPK" | version | kind | tier | key | last sequence | check.
pub const RECORD_LEN: usize = 4 + 2 + 1 + 1 + 65 + 8 + 32;

pub trait Crypto {
    fn sha256(&self, data: &[u8]) -> [u8; 32];
    /// Is `sig` (ASN.1 DER) an ECDSA P-256 signature over `digest` by the
    /// uncompressed point `key`? Must refuse a point that is not on the curve.
    fn p256_verify(&self, key: &[u8; 65], digest: &[u8; 32], sig: &[u8]) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Pairing was not started on the phone, ran out of time or of tries.
    NotArmed,
    Malformed,
    Version,
    /// A hardware attestation was claimed; nothing here can check one.
    AttestationUnsupported,
    /// Owner authority needs an attested key.
    TierNotAllowed,
    /// The sender did not know the secret on the screen.
    BadMac,
    /// The sender does not hold the private key it wants enrolled.
    BadSignature,
}

/// A companion key the phone trusts, as it is kept across restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enrolled {
    pub key: [u8; 65],
    pub kind: u8,
    pub tier: u8,
    /// The highest VSH1 sequence number accepted from this key. Kept with the
    /// key so an old signed command cannot be replayed after a restart.
    pub last_seq: u64,
}

fn hmac(c: &dyn Crypto, key: &[u8; 16], parts: &[&[u8]]) -> [u8; 32] {
    let mut inner = [0x36u8; 64 + 256];
    let mut outer = [0x5cu8; 64 + 32];
    for n in 0..16 {
        inner[n] ^= key[n];
        outer[n] ^= key[n];
    }
    let mut at = 64;
    for p in parts {
        inner[at..at + p.len()].copy_from_slice(p);
        at += p.len();
    }
    outer[64..].copy_from_slice(&c.sha256(&inner[..at]));
    let mac = c.sha256(&outer);
    inner.fill(0);
    outer.fill(0);
    mac
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Enrolled {
    /// The first eight bytes of the key's SHA-256: what both screens show.
    pub fn fingerprint(&self, c: &dyn Crypto) -> [u8; 8] {
        let mut f = [0; 8];
        f.copy_from_slice(&c.sha256(&self.key)[..8]);
        f
    }

    /// May this key sign a command of `tier`? A software key never carries
    /// owner authority, whatever its record says.
    pub fn allows(&self, tier: u8) -> bool {
        tier == TIER_AUTO || (tier == TIER_OWNER && self.tier == TIER_OWNER && self.kind == KIND_ATTESTED)
    }

    /// Accept sequence number `seq` once: true if it is newer than any before.
    pub fn advance(&mut self, seq: u64) -> bool {
        if seq <= self.last_seq {
            return false;
        }
        self.last_seq = seq;
        true
    }

    pub fn record(&self, c: &dyn Crypto) -> [u8; RECORD_LEN] {
        let mut r = [0u8; RECORD_LEN];
        r[..4].copy_from_slice(b"VSPK");
        r[4..6].copy_from_slice(&VERSION.to_le_bytes());
        r[6] = self.kind;
        r[7] = self.tier;
        r[8..73].copy_from_slice(&self.key);
        r[73..81].copy_from_slice(&self.last_seq.to_le_bytes());
        let check = c.sha256(&r[..81]);
        r[81..].copy_from_slice(&check);
        r
    }

    /// A stored record, or None if it is not exactly one (torn, edited by
    /// accident, another version): the phone is then unpaired. The check is
    /// not a seal: whoever can write the file can write a valid record.
    pub fn parse(c: &dyn Crypto, r: &[u8]) -> Option<Enrolled> {
        if r.len() != RECORD_LEN || &r[..4] != b"VSPK" || r[4..6] != VERSION.to_le_bytes() || !same(&c.sha256(&r[..81]), &r[81..]) {
            return None;
        }
        let e = Enrolled { key: r[8..73].try_into().ok()?, kind: r[6], tier: r[7], last_seq: u64::from_le_bytes(r[73..81].try_into().ok()?) };
        // Only what `enroll` can produce is a record.
        (e.kind == KIND_SOFTWARE && e.tier == TIER_AUTO && e.key[0] == 4).then_some(e)
    }
}

/// The phone's side of one pairing attempt.
pub struct Pairing {
    secret: [u8; 16],
    nonce: [u8; 16],
    until_ns: u64,
    attempts: u8,
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.disarm();
    }
}

impl Pairing {
    pub const fn new() -> Pairing {
        Pairing { secret: [0; 16], nonce: [0; 16], until_ns: 0, attempts: 0 }
    }

    /// The user started pairing. `random` is 32 fresh random bytes (the nonce
    /// and the secret). Returns what to show; None if the bytes are not random.
    pub fn arm(&mut self, now_ns: u64, random: &[u8; 32]) -> Option<[u8; OFFER_LEN]> {
        self.disarm();
        if random[..16] == [0; 16] || random[16..] == [0; 16] || random[..16] == random[16..] {
            return None;
        }
        self.nonce.copy_from_slice(&random[..16]);
        self.secret.copy_from_slice(&random[16..]);
        self.until_ns = now_ns.saturating_add(WINDOW_NS);
        self.attempts = ATTEMPTS;
        let mut offer = [0u8; OFFER_LEN];
        offer[..4].copy_from_slice(b"VSP1");
        offer[4..6].copy_from_slice(&VERSION.to_le_bytes());
        offer[6..22].copy_from_slice(&self.nonce);
        offer[22..].copy_from_slice(&self.secret);
        Some(offer)
    }

    pub fn disarm(&mut self) {
        for b in self.secret.iter_mut().chain(self.nonce.iter_mut()) {
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        self.attempts = 0;
        self.until_ns = 0;
    }

    pub fn armed(&self, now_ns: u64) -> bool {
        self.attempts > 0 && now_ns < self.until_ns
    }

    /// An ENROLL message from the link. On success the key to trust and the
    /// CONFIRM to send back; pairing is over either way once this succeeds.
    pub fn enroll(&mut self, c: &dyn Crypto, now_ns: u64, msg: &[u8]) -> Result<(Enrolled, [u8; CONFIRM_LEN]), Refused> {
        if !self.armed(now_ns) {
            self.disarm();
            return Err(Refused::NotArmed);
        }
        let outcome = self.check(c, msg);
        match outcome {
            Ok(_) => {}
            // Guessing at the secret, or at a signature, uses up a try.
            Err(Refused::BadMac) | Err(Refused::BadSignature) => {
                self.attempts -= 1;
                if self.attempts == 0 {
                    self.disarm();
                }
                return outcome;
            }
            Err(_) => return outcome,
        }
        let (enrolled, confirm) = outcome?;
        self.disarm();
        Ok((enrolled, confirm))
    }

    fn check(&self, c: &dyn Crypto, msg: &[u8]) -> Result<(Enrolled, [u8; CONFIRM_LEN]), Refused> {
        if msg.len() < ENROLL_HEAD + 32 + 2 || &msg[..4] != b"VSPE" {
            return Err(Refused::Malformed);
        }
        if msg[4..6] != VERSION.to_le_bytes() {
            return Err(Refused::Version);
        }
        let (kind, tier) = (msg[6], msg[7]);
        let attestation = usize::from(u16::from_le_bytes([msg[ENROLL_HEAD - 2], msg[ENROLL_HEAD - 1]]));
        if kind == KIND_ATTESTED || attestation != 0 {
            return Err(Refused::AttestationUnsupported);
        }
        if kind != KIND_SOFTWARE || msg.len() > ENROLL_MAX {
            return Err(Refused::Malformed);
        }
        if tier != TIER_AUTO {
            return Err(if tier == TIER_OWNER { Refused::TierNotAllowed } else { Refused::Malformed });
        }
        let (body, rest) = msg.split_at(ENROLL_HEAD);
        let (mac, rest) = rest.split_at(32);
        let sig_len = usize::from(u16::from_le_bytes([rest[0], rest[1]]));
        let sig = &rest[2..];
        if sig.len() != sig_len || sig_len < 8 {
            return Err(Refused::Malformed);
        }
        let key: [u8; 65] = body[24..89].try_into().map_err(|_| Refused::Malformed)?;
        if key[0] != 4 {
            return Err(Refused::Malformed);
        }
        if !same(&hmac(c, &self.secret, &[b"VSP1 enroll", &self.nonce, body]), mac) {
            return Err(Refused::BadMac);
        }
        // Proof of possession, over everything before it.
        let mut signed = [0u8; 15 + 16 + ENROLL_HEAD + 32];
        signed[..15].copy_from_slice(b"VSP1 possession");
        signed[15..31].copy_from_slice(&self.nonce);
        signed[31..31 + ENROLL_HEAD].copy_from_slice(body);
        signed[31 + ENROLL_HEAD..].copy_from_slice(mac);
        if !c.p256_verify(&key, &c.sha256(&signed), sig) {
            return Err(Refused::BadSignature);
        }
        let enrolled = Enrolled { key, kind, tier, last_seq: 0 };
        let mut confirm = [0u8; CONFIRM_LEN];
        confirm[..4].copy_from_slice(b"VSPC");
        confirm[4..6].copy_from_slice(&VERSION.to_le_bytes());
        confirm[6] = 1;
        confirm[7..15].copy_from_slice(&enrolled.fingerprint(c));
        let mac = hmac(c, &self.secret, &[b"VSP1 confirm", &self.nonce, &body[8..24], &confirm[6..7], &c.sha256(&key)]);
        confirm[15..].copy_from_slice(&mac);
        Ok((enrolled, confirm))
    }
}
