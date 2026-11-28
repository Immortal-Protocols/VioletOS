//! The key hierarchy and the key block.
//!
//!   device root key (32 bytes, from the platform, never on this disk)
//!     |-- seal key = HMAC(root, "violet keyblock seal v1")
//!     `-- key-encryption key
//!           = HMAC(root, "violet kek v1" | flags | kek salt | passcode secret)
//!           |-- wrap stream = HMAC(kek, "violet wrap enc v1")
//!           `-- wrap tag key = HMAC(kek, "violet wrap mac v1")
//!   volume key (32 random bytes, made once by `format`, stored only wrapped)
//!     `-- cipher key of a region (64 bytes, XTS) =
//!           HMAC(volume key, "violet volume v1" | purpose | 0), (.. | 1)
//!
//! The passcode secret is the trusted UI's PBKDF2-HMAC-SHA-256 value of the
//! digits (runtime/open/sysui.rs), computed with the salt and iteration
//! count kept here in the key block; with no passcode it is 32 zero bytes
//! and the flag says so. Changing the passcode wraps the same volume key
//! again: no sector of the volume is re-encrypted.
//!
//! The key block is one 512-byte sector, kept twice (sectors 0 and 1 of the
//! device, the rest of the first 4 KiB reserved). Every change is written
//! over the older copy and flushed, so a power cut leaves the old block or
//! the new one. Layout, little-endian:
//!     0  "VKB1"            4  version (1)
//!     8  generation (u64)  16 failed attempts since the last good one
//!    20  flags (bit 0: a passcode is set)
//!    24  PBKDF2 iterations 28 zero
//!    32  PBKDF2 salt[16]   48 kek salt[16]
//!    64  wrapped volume key[32]
//!    96  wrap tag[32]: HMAC under the wrap tag key of bytes 0..8, 20..28 and
//!        32..96. It verifies only with the right root key and passcode.
//!   128  seal[32]: HMAC under the seal key of bytes 0..128. It needs the
//!        root key only, so the attempt counter is counted, sealed and
//!        flushed before a passcode is tried.
//!
//! Not protected: putting back an older copy of the block (and with it an
//! older counter or passcode) by someone who can write the raw disk. That
//! needs a counter the disk cannot roll back (RPMB), see
//! docs/encryption-at-rest.md.
use super::sha::{hmac, same, wipe};
use crate::blockdev::{BlockDev, SECTOR};

/// Sectors at the start of the device that the key block owns.
pub const HEADER_SECTORS: u64 = 8;

/// Key material. Zeroed when dropped; it has no `Debug` and is never
/// formatted, compared in variable time or copied out except by `expose`.
pub struct Secret<const N: usize>([u8; N]);
impl<const N: usize> Secret<N> {
    pub fn new(bytes: &[u8; N]) -> Self {
        Secret(*bytes)
    }
    /// Take the bytes and zero the caller's copy.
    pub fn take(bytes: &mut [u8; N]) -> Self {
        let s = Secret(*bytes);
        wipe(bytes);
        s
    }
    pub fn expose(&self) -> &[u8; N] {
        &self.0
    }
}
impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
pub type RootKey = Secret<32>;
pub type PinSecret = Secret<32>;
pub type VolumeKey = Secret<32>;

/// What a region's cipher key is for: regions never share a key.
pub const DATA: u8 = 1;
pub const APPS: u8 = 2;

/// The XTS key (data key, tweak key) of one region of a volume.
pub fn cipher_key(volume_key: &[u8; 32], purpose: u8) -> Secret<64> {
    let mut out = [0u8; 64];
    for half in 0..2u8 {
        let mut h = hmac(volume_key, &[b"violet volume v1", &[purpose, half]]);
        out[half as usize * 32..][..32].copy_from_slice(&h);
        wipe(&mut h);
    }
    Secret::take(&mut out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The device failed. Nothing was unlocked.
    Io,
    /// The device is smaller than the header and one block.
    Small,
    /// The header sectors are all one byte value (a new or erased disk): the
    /// device was never formatted. Anything else there that does not verify
    /// is `Damaged`, never `Blank`.
    Blank,
    /// A key block is there, but no copy verifies under this root key
    /// (damage, or another device's disk). Never format on this by itself.
    Damaged,
    /// A passcode is set and none was given.
    PinNeeded,
    /// The passcode did not unwrap the key. `failures` wrong ones in a row.
    WrongPin { failures: u32 },
    /// Too many wrong passcodes: nothing is tried for this many seconds.
    Wait { secs: u64 },
    /// The random bytes or passcode parameters given cannot be right.
    Invalid,
}

/// What the lock screen needs before any key is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Params {
    pub pin_set: bool,
    pub failures: u32,
    pub pin_salt: [u8; 16],
    pub pin_iterations: u32,
}

/// A new passcode: its PBKDF2 value and the parameters that produced it.
pub struct NewPin<'a> {
    pub secret: &'a PinSecret,
    pub salt: [u8; 16],
    pub iterations: u32,
}

/// Wrong passcodes before any delay, and the delay after: the trusted UI's
/// schedule (sysui.rs `backoff_secs`), 30 s doubling to an hour.
pub const FREE_ATTEMPTS: u32 = 5;
pub fn backoff_secs(failures: u32) -> u64 {
    if failures < FREE_ATTEMPTS {
        0
    } else {
        (30u64 << (failures - FREE_ATTEMPTS).min(7)).min(3600)
    }
}

/// When the next attempt is allowed, in the caller's seconds (since boot).
/// Kept by the caller across calls. A new `Throttle` (Storage restarted, or
/// the device rebooted) waits the stored counter's whole delay counted from
/// its first use, so neither a restart nor a reboot skips it: restarting
/// only ever makes the wait longer.
#[derive(Default)]
pub struct Throttle {
    primed: bool,
    not_before: u64,
}
impl Throttle {
    pub const fn new() -> Self {
        Throttle { primed: false, not_before: 0 }
    }
}

const FLAG_PIN: u32 = 1;
const SEALED: usize = 128;

#[derive(Clone, Copy)]
struct Block {
    generation: u64,
    failures: u32,
    flags: u32,
    pin_iterations: u32,
    pin_salt: [u8; 16],
    kek_salt: [u8; 16],
    wrapped: [u8; 32],
    wrap_tag: [u8; 32],
}

fn seal_key(root: &RootKey) -> Secret<32> {
    Secret::take(&mut hmac(root.expose(), &[b"violet keyblock seal v1"]))
}

fn kek(root: &RootKey, flags: u32, salt: &[u8; 16], pin: Option<&PinSecret>) -> Secret<32> {
    let none = [0u8; 32];
    let pin = match pin {
        Some(p) if flags & FLAG_PIN != 0 => p.expose(),
        _ => &none,
    };
    Secret::take(&mut hmac(root.expose(), &[b"violet kek v1", &flags.to_le_bytes(), salt, pin]))
}

/// The bytes of an encoded block that the wrap tag covers.
fn wrap_tag(kek: &Secret<32>, s: &[u8; SECTOR]) -> [u8; 32] {
    let key = Secret::take(&mut hmac(kek.expose(), &[b"violet wrap mac v1"]));
    hmac(key.expose(), &[&s[..8], &s[20..28], &s[32..96]])
}

/// XOR the wrap stream over a volume key (wrap and unwrap are the same).
fn wrap_stream(kek: &Secret<32>, key: &mut [u8; 32]) {
    let mut stream = hmac(kek.expose(), &[b"violet wrap enc v1"]);
    for (k, s) in key.iter_mut().zip(&stream) {
        *k ^= s;
    }
    wipe(&mut stream);
}

impl Block {
    /// The sector without its seal.
    fn fields(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..4].copy_from_slice(b"VKB1");
        s[4..8].copy_from_slice(&1u32.to_le_bytes());
        s[8..16].copy_from_slice(&self.generation.to_le_bytes());
        s[16..20].copy_from_slice(&self.failures.to_le_bytes());
        s[20..24].copy_from_slice(&self.flags.to_le_bytes());
        s[24..28].copy_from_slice(&self.pin_iterations.to_le_bytes());
        s[32..48].copy_from_slice(&self.pin_salt);
        s[48..64].copy_from_slice(&self.kek_salt);
        s[64..96].copy_from_slice(&self.wrapped);
        s[96..128].copy_from_slice(&self.wrap_tag);
        s
    }
    fn encode(&self, seal: &Secret<32>) -> [u8; SECTOR] {
        let mut s = self.fields();
        let tag = hmac(seal.expose(), &[&s[..SEALED]]);
        s[SEALED..SEALED + 32].copy_from_slice(&tag);
        s
    }
    fn decode(s: &[u8; SECTOR], seal: &Secret<32>) -> Option<Block> {
        let word = |i: usize| u32::from_le_bytes(s[i..i + 4].try_into().unwrap());
        if !marked(s) || !same(&hmac(seal.expose(), &[&s[..SEALED]]), &s[SEALED..SEALED + 32]) {
            return None;
        }
        Some(Block {
            generation: u64::from_le_bytes(s[8..16].try_into().unwrap()),
            failures: word(16),
            flags: word(20),
            pin_iterations: word(24),
            pin_salt: s[32..48].try_into().unwrap(),
            kek_salt: s[48..64].try_into().unwrap(),
            wrapped: s[64..96].try_into().unwrap(),
            wrap_tag: s[96..128].try_into().unwrap(),
        })
    }
    fn params(&self) -> Params {
        Params { pin_set: self.flags & FLAG_PIN != 0, failures: self.failures, pin_salt: self.pin_salt, pin_iterations: self.pin_iterations }
    }
    /// Wrap `key` under the root key and, if given, a passcode.
    fn wrap(&mut self, root: &RootKey, key: &VolumeKey, pin: Option<&NewPin>, kek_salt: &[u8; 16]) {
        self.flags = if pin.is_some() { FLAG_PIN } else { 0 };
        self.pin_iterations = pin.map_or(0, |p| p.iterations);
        self.pin_salt = pin.map_or([0; 16], |p| p.salt);
        self.kek_salt = *kek_salt;
        self.failures = 0;
        let kek = kek(root, self.flags, kek_salt, pin.map(|p| p.secret));
        self.wrapped = *key.expose();
        wrap_stream(&kek, &mut self.wrapped);
        self.wrap_tag = wrap_tag(&kek, &self.fields());
    }
    /// The volume key, if the root key and passcode are the ones it was
    /// wrapped under.
    fn unwrap(&self, root: &RootKey, pin: Option<&PinSecret>) -> Option<VolumeKey> {
        let kek = kek(root, self.flags, &self.kek_salt, pin);
        if !same(&wrap_tag(&kek, &self.fields()), &self.wrap_tag) {
            return None;
        }
        let mut key = self.wrapped;
        wrap_stream(&kek, &mut key);
        Some(Secret::take(&mut key))
    }
}

fn marked(s: &[u8; SECTOR]) -> bool {
    &s[..8] == b"VKB1\x01\0\0\0"
}

/// Whether two blocks wrap the key the same way (only the generation and
/// the counter differ).
fn same_wrap(a: &Block, b: &Block) -> bool {
    a.flags == b.flags && a.pin_iterations == b.pin_iterations && a.pin_salt == b.pin_salt && a.kek_salt == b.kek_salt && a.wrapped == b.wrapped && a.wrap_tag == b.wrap_tag
}

/// Every header sector holds one byte value throughout: a new or erased
/// disk. A key block with damaged bytes, or another filesystem's first
/// sectors, is not blank.
fn blank<B: BlockDev>(dev: &mut B) -> Result<bool, Error> {
    let mut s = [0u8; SECTOR];
    let mut fill = None;
    for at in 0..HEADER_SECTORS {
        dev.read(at, &mut s).map_err(|_| Error::Io)?;
        let f = *fill.get_or_insert(s[0]);
        if s.iter().any(|b| *b != f) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The newest copy that verifies, the sector the next one goes to, and
/// whether that other sector already holds the same wrap. A copy that
/// cannot be read counts as no copy; `Io` only when no copy verifies and
/// one could not be read.
fn load<B: BlockDev>(dev: &mut B, seal: &Secret<32>) -> Result<(Block, u64, bool), Error> {
    if dev.sectors() < HEADER_SECTORS + 8 {
        return Err(Error::Small);
    }
    let mut copies: [Option<Block>; 2] = [None, None];
    let mut unreadable = false;
    for at in 0..2usize {
        let mut s = [0u8; SECTOR];
        match dev.read(at as u64, &mut s) {
            Ok(()) => copies[at] = Block::decode(&s, seal),
            Err(()) => unreadable = true,
        }
    }
    let newest = match (&copies[0], &copies[1]) {
        (Some(a), Some(b)) => (b.generation > a.generation) as usize,
        (Some(_), None) => 0,
        (None, Some(_)) => 1,
        (None, None) if unreadable => return Err(Error::Io),
        (None, None) => return Err(if blank(dev)? { Error::Blank } else { Error::Damaged }),
    };
    let best = copies[newest].unwrap();
    let current = copies[1 - newest].as_ref().is_some_and(|o| same_wrap(o, &best));
    Ok((best, 1 - newest as u64, current))
}

/// Write the block as the next generation over the older copy and flush.
fn store<B: BlockDev>(dev: &mut B, seal: &Secret<32>, block: &mut Block, at: &mut u64) -> Result<(), Error> {
    block.generation = block.generation.checked_add(1).ok_or(Error::Damaged)?;
    dev.write(*at, &block.encode(seal)).map_err(|_| Error::Io)?;
    dev.flush().map_err(|_| Error::Io)?;
    *at = 1 - *at;
    Ok(())
}

/// The passcode parameters and the attempt counter. Needs no passcode.
pub fn params<B: BlockDev>(dev: &mut B, root: &RootKey) -> Result<Params, Error> {
    load(dev, &seal_key(root)).map(|(b, _, _)| b.params())
}

/// Make a new volume key from 48 random bytes (32 of key, 16 of salt),
/// wrapped under the root key with no passcode, and write both copies of
/// the key block. The caller's `entropy` is zeroed.
///
/// This is also the erase, as far as this layer can make it one: the old
/// key block is discarded and overwritten with zeros, and that is made
/// durable before the new block is written, so a power cut leaves the old
/// key, a blank header or the new key, and never brings the old key back
/// once the new one was returned. Whether the medium still keeps the old
/// sector internally (an eMMC's flash translation layer does, until it
/// erases the page) is up to the medium; see docs/encryption-at-rest.md.
pub fn format<B: BlockDev>(dev: &mut B, root: &RootKey, entropy: &mut [u8; 48]) -> Result<VolumeKey, Error> {
    let salt: [u8; 16] = entropy[32..].try_into().unwrap();
    let key = VolumeKey::new(entropy[..32].try_into().unwrap());
    wipe(entropy);
    // An unfilled buffer, not a proof of randomness.
    if key.expose().iter().all(|b| *b == key.expose()[0]) {
        return Err(Error::Invalid);
    }
    if dev.sectors() < HEADER_SECTORS + 8 {
        return Err(Error::Small);
    }
    let seal = seal_key(root);
    let mut block = Block { generation: 0, failures: 0, flags: 0, pin_iterations: 0, pin_salt: [0; 16], kek_salt: salt, wrapped: [0; 32], wrap_tag: [0; 32] };
    block.wrap(root, &key, None, &salt);
    // The old copies (and the reserved sectors) gone and durable before the
    // new block exists. The discard is advice to the medium; its failure
    // changes nothing here.
    let _ = dev.discard(0, HEADER_SECTORS);
    let zero = [0u8; SECTOR];
    for at in 0..HEADER_SECTORS {
        dev.write(at, &zero).map_err(|_| Error::Io)?;
    }
    dev.flush().map_err(|_| Error::Io)?;
    let mut at = 0;
    store(dev, &seal, &mut block, &mut at)?;
    store(dev, &seal, &mut block, &mut at)?;
    Ok(key)
}

fn attempt<B: BlockDev>(dev: &mut B, root: &RootKey, pin: Option<&PinSecret>, throttle: &mut Throttle, now: u64) -> Result<(VolumeKey, Block, u64), Error> {
    let seal = seal_key(root);
    let (mut block, mut at, current) = load(dev, &seal)?;
    if block.flags & FLAG_PIN == 0 {
        // Nothing to guess: the root key alone decides.
        let key = block.unwrap(root, None).ok_or(Error::Damaged)?;
        if !current {
            // The other copy is damaged, unreadable or an older wrap (a
            // passcode change that wrote only one copy): replace it, so an
            // older passcode's block does not stay behind.
            let _ = store(dev, &seal, &mut block, &mut at);
        }
        return Ok((key, block, at));
    }
    let Some(pin) = pin else { return Err(Error::PinNeeded) };
    if !throttle.primed {
        // From now, not from boot: Storage restarted while the clock ran on
        // still waits the whole delay.
        throttle.primed = true;
        throttle.not_before = now.saturating_add(backoff_secs(block.failures));
    }
    if now < throttle.not_before {
        return Err(Error::Wait { secs: throttle.not_before - now });
    }
    // Counted and durable before it is checked.
    block.failures = block.failures.saturating_add(1);
    store(dev, &seal, &mut block, &mut at)?;
    let Some(key) = block.unwrap(root, Some(pin)) else {
        throttle.not_before = now.saturating_add(backoff_secs(block.failures));
        return Err(Error::WrongPin { failures: block.failures });
    };
    block.failures = 0;
    throttle.not_before = 0;
    // The key is right whether or not the counter could be cleared; a
    // counter left high only makes the next wrong guess wait longer. The
    // two stores of an attempt write both copies, which also replaces a
    // copy left behind by a passcode change that wrote only one.
    let _ = store(dev, &seal, &mut block, &mut at);
    Ok((key, block, at))
}

/// The volume key, given the root key and (if one is set) the passcode
/// secret. `now` is seconds since boot.
pub fn unlock<B: BlockDev>(dev: &mut B, root: &RootKey, pin: Option<&PinSecret>, throttle: &mut Throttle, now: u64) -> Result<VolumeKey, Error> {
    attempt(dev, root, pin, throttle, now).map(|(key, _, _)| key)
}

/// `unlock`, and on a device that was never formatted (`Blank`, nothing
/// else) `format` with the random bytes, if any were given. A damaged key
/// block, another device's disk or a wrong passcode never formats.
pub fn unlock_or_format<B: BlockDev>(dev: &mut B, root: &RootKey, pin: Option<&PinSecret>, throttle: &mut Throttle, now: u64, entropy: Option<&mut [u8; 48]>) -> Result<VolumeKey, Error> {
    match (unlock(dev, root, pin, throttle, now), entropy) {
        (Err(Error::Blank), Some(random)) => format(dev, root, random),
        (other, _) => other,
    }
}

/// Set, change (`new` given) or remove (`new` None) the passcode. `old` must
/// be the current one if one is set, and counts as an attempt. The volume
/// key is wrapped again under `kek_salt`, 16 fresh random bytes; the volume
/// itself is not touched.
///
/// `Ok` means the new passcode is the one in effect: the first copy of the
/// new block is written and flushed, and it is the newest. Both copies are
/// replaced; if the second could not be, `Ok` is still returned and the old
/// passcode's copy is replaced at the next unlock (every unlock writes it).
/// `Err` before that write means the old passcode is still in effect.
/// `Err(Io)` from the write itself means the device failed mid-write: the
/// caller cannot know which passcode is in effect and must try both.
pub fn set_pin<B: BlockDev>(dev: &mut B, root: &RootKey, old: Option<&PinSecret>, new: Option<&NewPin>, kek_salt: &[u8; 16], throttle: &mut Throttle, now: u64) -> Result<(), Error> {
    if new.is_some_and(|p| !(1000..=10_000_000).contains(&p.iterations)) {
        return Err(Error::Invalid);
    }
    let (key, mut block, mut at) = attempt(dev, root, old, throttle, now)?;
    let seal = seal_key(root);
    block.wrap(root, &key, new, kek_salt);
    store(dev, &seal, &mut block, &mut at)?;
    // In effect from here on; see above for a second copy that fails.
    let _ = store(dev, &seal, &mut block, &mut at);
    Ok(())
}
