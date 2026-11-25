//! bitchat's binary packets (version 1), as the iOS and Android apps send
//! them over Bluetooth LE. All multi-byte fields are big-endian. See
//! docs/bitchat-protocol.md for the layout and where it comes from.
use ring::signature::{self, Ed25519KeyPair, KeyPair};

pub const ANNOUNCE: u8 = 0x01;
pub const MESSAGE: u8 = 0x02;
pub const LEAVE: u8 = 0x03;
pub const NOISE_HANDSHAKE: u8 = 0x10;
pub const NOISE_ENCRYPTED: u8 = 0x11;
pub const FRAGMENT: u8 = 0x20;
pub const REQUEST_SYNC: u8 = 0x21;

pub const HAS_RECIPIENT: u8 = 0x01;
pub const HAS_SIGNATURE: u8 = 0x02;
pub const IS_COMPRESSED: u8 = 0x04;
pub const HAS_ROUTE: u8 = 0x08;
pub const IS_RSR: u8 = 0x10;

pub const BROADCAST: [u8; 8] = [0xff; 8];
/// Hops a packet may travel; announces at this TTL mark a direct neighbour.
pub const TTL: u8 = 7;
/// Signed payloads at least this long might be compressed by the iOS encoder,
/// which re-encodes before it checks a signature: stay below it.
pub const COMPRESS_FROM: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub version: u8,
    pub kind: u8,
    pub ttl: u8,
    /// Milliseconds since the Unix epoch.
    pub timestamp: u64,
    pub flags: u8,
    pub sender: [u8; 8],
    pub recipient: Option<[u8; 8]>,
    pub route: Vec<[u8; 8]>,
    /// As carried: still compressed when `flags & IS_COMPRESSED`.
    pub payload: Vec<u8>,
    pub signature: Option<[u8; 64]>,
}

impl Packet {
    pub fn new(kind: u8, sender: [u8; 8], timestamp: u64, payload: Vec<u8>) -> Packet {
        Packet { version: 1, kind, ttl: TTL, timestamp, flags: 0, sender, recipient: None, route: Vec::new(), payload, signature: None }
    }

    /// The bytes on the wire (without padding).
    pub fn encode(&self) -> Vec<u8> {
        // The other bits say which optional fields follow, so they come from those.
        let mut flags = self.flags & !(HAS_RECIPIENT | HAS_SIGNATURE | HAS_ROUTE);
        if self.recipient.is_some() {
            flags |= HAS_RECIPIENT;
        }
        if self.signature.is_some() {
            flags |= HAS_SIGNATURE;
        }
        if self.version >= 2 && !self.route.is_empty() {
            flags |= HAS_ROUTE;
        }
        let mut out = vec![self.version, self.kind, self.ttl];
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.push(flags);
        if self.version >= 2 {
            out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(&self.sender);
        if let Some(r) = self.recipient {
            out.extend_from_slice(&r);
        }
        if flags & HAS_ROUTE != 0 {
            out.push(self.route.len() as u8);
            for hop in &self.route {
                out.extend_from_slice(hop);
            }
        }
        out.extend_from_slice(&self.payload);
        if let Some(s) = self.signature {
            out.extend_from_slice(&s);
        }
        out
    }

    /// What the sender signs: the packet with TTL 0, no signature (and no
    /// request-sync flag), padded to its block size.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut p = self.clone();
        p.ttl = 0;
        p.signature = None;
        p.flags &= !IS_RSR;
        pad(p.encode())
    }

    pub fn sign(&mut self, key: &Ed25519KeyPair) {
        self.signature = None;
        let sig = key.sign(&self.signing_bytes());
        self.signature = Some(sig.as_ref().try_into().unwrap());
    }

    pub fn verify(&self, public: &[u8; 32]) -> bool {
        let Some(sig) = self.signature else { return false };
        signature::UnparsedPublicKey::new(&signature::ED25519, public).verify(&self.signing_bytes(), &sig).is_ok()
    }

    /// The payload, inflated if it was sent compressed.
    pub fn content(&self) -> Option<Vec<u8>> {
        if self.flags & IS_COMPRESSED == 0 {
            return Some(self.payload.clone());
        }
        let size_len = if self.version >= 2 { 4 } else { 2 };
        let (size, data) = (self.payload.get(..size_len)?, &self.payload[size_len..]);
        let size = size.iter().fold(0usize, |a, b| a << 8 | *b as usize);
        if size > 1 << 20 {
            return None;
        }
        let out = miniz_oxide::inflate::decompress_to_vec_with_limit(data, size).ok()?;
        (out.len() == size).then_some(out)
    }
}

/// Reads a packet from the front of `b`; trailing bytes are ignored.
pub fn decode(b: &[u8]) -> Option<Packet> {
    decode_raw(b).or_else(|| decode_raw(&unpad(b)?))
}

fn decode_raw(b: &[u8]) -> Option<Packet> {
    let version = *b.first()?;
    if version != 1 && version != 2 {
        return None;
    }
    let header = if version == 2 { 16 } else { 14 };
    let h = b.get(..header)?;
    let kind = h[1];
    let ttl = h[2];
    let timestamp = u64::from_be_bytes(h[3..11].try_into().unwrap());
    let flags = h[11];
    let len = if version == 2 {
        u32::from_be_bytes(h[12..16].try_into().unwrap()) as usize
    } else {
        u16::from_be_bytes([h[12], h[13]]) as usize
    };
    let mut at = header;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = b.get(at..at.checked_add(n)?)?;
        at += n;
        Some(s)
    };
    let sender: [u8; 8] = take(8)?.try_into().unwrap();
    let recipient = if flags & HAS_RECIPIENT != 0 { Some(take(8)?.try_into().unwrap()) } else { None };
    let mut route = Vec::new();
    if version >= 2 && flags & HAS_ROUTE != 0 {
        let n = take(1)?[0];
        for _ in 0..n {
            route.push(take(8)?.try_into().unwrap());
        }
    }
    let payload = take(len)?.to_vec();
    let signature = if flags & HAS_SIGNATURE != 0 { Some(take(64)?.try_into().unwrap()) } else { None };
    let flags = flags & !(HAS_RECIPIENT | HAS_SIGNATURE | HAS_ROUTE);
    Some(Packet { version, kind, ttl, timestamp, flags, sender, recipient, route, payload, signature })
}

/// The length a frame decoded from `b` takes, for streams of notifications.
pub fn frame_len(b: &[u8]) -> Option<usize> {
    let p = decode_raw(b)?;
    Some(p.encode().len())
}

/// PKCS#7-style padding to 256, 512, 1024 or 2048 bytes, when it fits in one byte.
pub fn pad(mut b: Vec<u8>) -> Vec<u8> {
    let Some(&bucket) = [256usize, 512, 1024, 2048].iter().find(|&&s| s >= b.len() + 16) else { return b };
    let n = bucket - b.len();
    if n == 0 || n > 255 {
        return b;
    }
    b.resize(bucket, n as u8);
    b
}
pub fn unpad(b: &[u8]) -> Option<Vec<u8>> {
    let n = *b.last()? as usize;
    if n == 0 || n > b.len() || !b[b.len() - n..].iter().all(|&x| x as usize == n) {
        return None;
    }
    Some(b[..b.len() - n].to_vec())
}

/// What an announce says about its sender.
#[derive(Clone, Debug, PartialEq)]
pub struct Announce {
    pub nickname: String,
    pub noise_key: [u8; 32],
    pub signing_key: [u8; 32],
}

impl Announce {
    pub fn encode(&self) -> Vec<u8> {
        let nick = self.nickname.as_bytes();
        let nick = &nick[..nick.len().min(255)];
        let mut out = vec![0x01, nick.len() as u8];
        out.extend_from_slice(nick);
        out.extend_from_slice(&[0x02, 32]);
        out.extend_from_slice(&self.noise_key);
        out.extend_from_slice(&[0x03, 32]);
        out.extend_from_slice(&self.signing_key);
        // Capabilities: none.
        out.extend_from_slice(&[0x05, 1, 0]);
        out
    }
    pub fn decode(b: &[u8]) -> Option<Announce> {
        let (mut nickname, mut noise_key, mut signing_key) = (None, None, None);
        let mut at = 0;
        while at + 2 <= b.len() {
            let (t, n) = (b[at], b[at + 1] as usize);
            let v = b.get(at + 2..at + 2 + n)?;
            match t {
                0x01 => nickname = Some(String::from_utf8_lossy(v).into_owned()),
                0x02 if n == 32 => noise_key = Some(v.try_into().unwrap()),
                0x03 if n == 32 => signing_key = Some(v.try_into().unwrap()),
                _ => {}
            }
            at += 2 + n;
        }
        Some(Announce { nickname: nickname?, noise_key: noise_key?, signing_key: signing_key? })
    }
}

/// A peer's ID: the first 8 bytes of SHA-256 of its Noise static key.
pub fn peer_id(noise_key: &[u8; 32]) -> [u8; 8] {
    ring::digest::digest(&ring::digest::SHA256, noise_key).as_ref()[..8].try_into().unwrap()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One piece of a packet too big for a link: the bytes of the whole frame,
/// split, with an 8-byte ID, the index and count and the original type.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub id: [u8; 8],
    pub index: u16,
    pub total: u16,
    pub kind: u8,
    pub data: Vec<u8>,
}
impl Fragment {
    pub fn decode(b: &[u8]) -> Option<Fragment> {
        if b.len() < 13 {
            return None;
        }
        Some(Fragment {
            id: b[..8].try_into().unwrap(),
            index: u16::from_be_bytes([b[8], b[9]]),
            total: u16::from_be_bytes([b[10], b[11]]),
            kind: b[12],
            data: b[13..].to_vec(),
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.id.to_vec();
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        out.push(self.kind);
        out.extend_from_slice(&self.data);
        out
    }
}
/// Splits an encoded packet into fragment packets whose frames fit `limit` bytes.
pub fn fragments(p: &Packet, frame: &[u8], limit: usize, id: [u8; 8]) -> Vec<Packet> {
    let chunk = limit.saturating_sub(42).max(64);
    let total = frame.len().div_ceil(chunk);
    frame
        .chunks(chunk)
        .enumerate()
        .map(|(i, c)| {
            let f = Fragment { id, index: i as u16, total: total as u16, kind: p.kind, data: c.to_vec() };
            let mut q = Packet::new(FRAGMENT, p.sender, p.timestamp, f.encode());
            q.ttl = p.ttl;
            q.recipient = p.recipient;
            q
        })
        .collect()
}

/// Reassembles fragments, keyed by sender and fragment ID.
#[derive(Default)]
pub struct Assembler {
    open: Vec<([u8; 8], [u8; 8], u16, u8, Vec<Option<Vec<u8>>>, std::time::Instant)>,
}
impl Assembler {
    /// Adds one fragment; returns the whole packet once every piece is in.
    pub fn add(&mut self, sender: [u8; 8], f: Fragment) -> Option<Packet> {
        self.open.retain(|e| e.5.elapsed().as_secs() < 30);
        if f.total == 0 || f.total > 4096 || f.index >= f.total {
            return None;
        }
        let at = match self.open.iter().position(|e| e.0 == sender && e.1 == f.id) {
            Some(at) => at,
            None => {
                if self.open.len() >= 32 {
                    self.open.remove(0);
                }
                self.open.push((sender, f.id, f.total, f.kind, vec![None; f.total as usize], std::time::Instant::now()));
                self.open.len() - 1
            }
        };
        let e = &mut self.open[at];
        if e.2 != f.total {
            return None;
        }
        e.4[f.index as usize] = Some(f.data);
        if e.4.iter().any(|p| p.is_none()) {
            return None;
        }
        let e = self.open.remove(at);
        let whole: Vec<u8> = e.4.into_iter().flatten().flatten().collect();
        let mut p = decode(&whole)?;
        if p.kind != e.3 {
            return None;
        }
        p.ttl = 0;
        Some(p)
    }
}

/// Builds and signs this node's packets.
pub struct Identity {
    pub signing: Ed25519KeyPair,
    pub noise_public: [u8; 32],
    pub id: [u8; 8],
}
impl Identity {
    pub fn new(signing_seed: &[u8; 32], noise_secret: &[u8; 32]) -> Identity {
        let signing = Ed25519KeyPair::from_seed_unchecked(signing_seed).unwrap();
        let noise_public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*noise_secret)).to_bytes();
        Identity { signing, id: peer_id(&noise_public), noise_public }
    }
    pub fn signing_public(&self) -> [u8; 32] {
        self.signing.public_key().as_ref().try_into().unwrap()
    }
    pub fn announce(&self, nickname: &str, now_ms: u64) -> Packet {
        let a = Announce { nickname: nickname.into(), noise_key: self.noise_public, signing_key: self.signing_public() };
        let mut p = Packet::new(ANNOUNCE, self.id, now_ms, a.encode());
        p.sign(&self.signing);
        p
    }
    pub fn message(&self, text: &str, now_ms: u64) -> Packet {
        let mut p = Packet::new(MESSAGE, self.id, now_ms, text.as_bytes().to_vec());
        p.sign(&self.signing);
        p
    }
    pub fn leave(&self, now_ms: u64) -> Packet {
        let mut p = Packet::new(LEAVE, self.id, now_ms, Vec::new());
        p.sign(&self.signing);
        p
    }
}

/// Whether the iOS encoder would compress this payload (and so sign the
/// compressed form): at least 100 bytes, with under 90% distinct bytes.
pub fn would_compress(payload: &[u8]) -> bool {
    if payload.len() < COMPRESS_FROM {
        return false;
    }
    let mut seen = [false; 256];
    let unique = payload.iter().filter(|&&b| !std::mem::replace(&mut seen[b as usize], true)).count();
    (unique as f64) / (payload.len().min(256) as f64) < 0.9
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity::new(&[7; 32], &[9; 32])
    }

    #[test]
    fn the_spec_example_message_encodes_byte_for_byte() {
        let mut p = Packet::new(MESSAGE, [1, 2, 3, 4, 5, 6, 7, 8], 0x0000_019A_B0C0_D0E0, b"hi".to_vec());
        assert_eq!(
            hex(&p.encode()),
            "010207000001 9ab0c0d0e0 00 0002 0102030405060708 6869".replace(' ', "")
        );
        p.signature = Some([0; 64]);
        assert_eq!(p.encode().len(), 88);
        let s = p.signing_bytes();
        assert_eq!(s.len(), 256);
        assert_eq!(&s[..3], &[1, 2, 0]);
        assert_eq!(s[11], 0, "flags drop the signature bit");
        assert!(s[24..].iter().all(|&b| b == 0xe8));
    }

    #[test]
    fn signed_packets_survive_a_round_trip_and_a_relay() {
        let me = identity();
        let mut p = me.message("hello mesh", 1_795_000_000_000);
        let wire = p.encode();
        let q = decode(&wire).unwrap();
        assert_eq!(q, p);
        assert!(q.verify(&me.signing_public()));
        // TTL is not signed: a relayed copy still verifies.
        p.ttl = 3;
        assert!(decode(&p.encode()).unwrap().verify(&me.signing_public()));
        // The text is.
        let mut forged = q.clone();
        forged.payload = b"hello mess".to_vec();
        assert!(!forged.verify(&me.signing_public()));
    }

    #[test]
    fn announces_name_the_sender_by_its_noise_key() {
        let me = identity();
        let p = me.announce("violet", 5);
        let a = Announce::decode(&decode(&p.encode()).unwrap().payload).unwrap();
        assert_eq!(a.nickname, "violet");
        assert_eq!(peer_id(&a.noise_key), p.sender);
        assert_eq!(a.signing_key, me.signing_public());
        assert!(p.verify(&a.signing_key));
        // Small enough that iOS would not compress it before checking the signature.
        assert!(!would_compress(&p.payload));
    }

    #[test]
    fn padding_follows_the_block_sizes() {
        assert_eq!(pad(vec![1; 24]).len(), 256);
        assert_eq!(pad(vec![1; 200]).len(), 256);
        assert_eq!(pad(vec![1; 241]).len(), 241, "241 + 16 > 256, and 271 bytes of padding do not fit in one byte");
        assert_eq!(pad(vec![1; 300]).len(), 512);
        assert_eq!(pad(vec![1; 250]).len(), 250, "262 bytes of padding do not fit in one byte");
        assert_eq!(unpad(&pad(vec![3; 40])).unwrap(), vec![3; 40]);
    }

    #[test]
    fn padded_frames_and_trailing_bytes_still_decode() {
        let p = identity().message("x", 1);
        assert_eq!(decode(&pad(p.encode())).unwrap(), p);
        let mut two = p.encode();
        two.extend_from_slice(&[0xaa; 5]);
        assert_eq!(decode(&two).unwrap(), p);
        assert_eq!(frame_len(&two), Some(p.encode().len()));
    }

    #[test]
    fn compressed_payloads_inflate() {
        let text = "bitchat ".repeat(16);
        let text = text.as_bytes();
        let raw = miniz_oxide::deflate::compress_to_vec(text, 5);
        let mut payload = (text.len() as u16).to_be_bytes().to_vec();
        payload.extend_from_slice(&raw);
        let mut p = Packet::new(MESSAGE, [0; 8], 0, payload);
        p.flags = IS_COMPRESSED;
        assert_eq!(decode(&p.encode()).unwrap().content().unwrap(), text);
        assert!(would_compress(text));
    }

    #[test]
    fn fragments_reassemble_into_the_original() {
        let me = identity();
        let p = me.message(&"long ".repeat(19), 1);
        let frame = p.encode();
        let parts = fragments(&p, &frame, 100, [5; 8]);
        assert!(parts.len() > 1);
        let mut a = Assembler::default();
        let mut out = None;
        for q in parts.iter().rev() {
            let q = decode(&q.encode()).unwrap();
            assert!(q.encode().len() <= 100 + 30);
            out = a.add(q.sender, Fragment::decode(&q.payload).unwrap());
        }
        let out = out.unwrap();
        assert_eq!(out.payload, p.payload);
        assert_eq!(out.ttl, 0);
        assert!(out.verify(&me.signing_public()));
    }
}
