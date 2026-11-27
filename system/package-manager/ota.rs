//! Signed full-system updates (OTA): the manifest the supervisor checks
//! before it lets Storage switch the phone to a newly written boot slot.
//!
//! A bundle (tools/ota_bundle.py) is one 512-byte manifest sector followed by
//! the slot image, zero-padded to whole sectors. The slot image is what the
//! bootloader loads: on the handset the Android boot image (loader, seL4, the
//! supervisor with its pinned keys and factory catalog, /apps); in QEMU the
//! same files in a small container (tools/ota_boot.py). The phone writes the
//! image to the inactive slot from sector 0 and the manifest to the slot's
//! last sector. The manifest is
//!
//!    0  "VOT1"
//!    4  key_id      u32   the VPK2 signing key IDs (image/keys/README.md)
//!    8  version     u32   security version of the system, 1 or more
//!   12  target      u32   "QEMU" or "MOTO", as in VPK2
//!   16  layout      u32   1: QEMU slot container, 2: Android boot image v2
//!   20  image_len   u32   bytes of slot image
//!   24  reserved    u32   0
//!   28  reserved    u32   0
//!   32  digest[32]        SHA-256 of the image zero-padded to whole sectors
//!   64  r[32] s[32]       ECDSA P-256 over SHA-256 of bytes 0..64
//!  128  zeros to the end of the sector
//!
//! Signed by the same keys as packages; the magic keeps a package signature
//! (whose message starts "VPK2") from passing as a manifest and the other way
//! round. The digest is checked against what Storage read back from the
//! slot after writing it (services/storage/slotwrite.rs), not against what
//! was sent. The version must be at least the running system's own version
//! (VIOLET_SYSTEM_VERSION at build time, 1 by default): an older signed
//! system is refused.
#![allow(dead_code)]

pub const MANIFEST: usize = 128;
pub const SIGNED: usize = 64;
const VOT1: u32 = 0x3154_4f56;
pub const LAYOUT_QEMU: u32 = 1;
pub const LAYOUT_ANDROID: u32 = 2;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Refusal {
    Format,
    Target,
    Key,
    Signature,
    /// Older than the system running now.
    Version,
    /// The image does not fit the slot, or is empty.
    Size,
    /// Storage's read-back digest is not the manifest's.
    Digest,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Manifest {
    pub key_id: u32,
    pub version: u32,
    pub layout: u32,
    pub image_len: u32,
    pub digest: [u8; 32],
}

impl Manifest {
    /// Sectors the image takes in the slot.
    pub fn sectors(&self) -> u32 {
        self.image_len.div_ceil(512)
    }
}

fn word(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The running system's security version, from the build.
pub const fn system_version() -> u32 {
    match option_env!("VIOLET_SYSTEM_VERSION") {
        None => 1,
        Some(text) => {
            let b = text.as_bytes();
            let mut v: u32 = 0;
            let mut i = 0;
            // A malformed value fails the build rather than meaning 0.
            if b.is_empty() || b.len() > 9 {
                panic!("VIOLET_SYSTEM_VERSION must be 1 to 999999999");
            }
            while i < b.len() {
                if !b[i].is_ascii_digit() {
                    panic!("VIOLET_SYSTEM_VERSION must be a number");
                }
                v = v * 10 + (b[i] - b'0') as u32;
                i += 1;
            }
            if v == 0 {
                panic!("VIOLET_SYSTEM_VERSION must be 1 or more");
            }
            v
        }
    }
}

/// The system version as the image carries it, so tools/ota_bundle.py can
/// read it from the image it signs and refuse a manifest version that is not
/// the one the image was built with (whose floor the phone enforces once it
/// runs that image): "VIOLETSYSVER", the version and its complement (u32 LE).
pub const NOTE_MAGIC: [u8; 12] = *b"VIOLETSYSVER";
pub const NOTE: usize = 20;
pub const fn version_note(v: u32) -> [u8; NOTE] {
    let mut n = [0u8; NOTE];
    let (a, b) = (v.to_le_bytes(), (!v).to_le_bytes());
    let mut i = 0;
    while i < 12 {
        n[i] = NOTE_MAGIC[i];
        i += 1;
    }
    i = 0;
    while i < 4 {
        n[12 + i] = a[i];
        n[16 + i] = b[i];
        i += 1;
    }
    n
}
pub fn note_version(n: &[u8; NOTE]) -> Option<u32> {
    let v = word(n, 12);
    (n[..12] == NOTE_MAGIC && word(n, 16) == !v && v != 0).then_some(v)
}

/// When the supervisor asks Storage to mark the running slot successful:
/// only once the system has run healthy since the components last started
/// (`healthy`), and again with a growing wait when Storage did not manage
/// it, a few times, until it has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    healthy: bool,
    done: bool,
    tries: u32,
    next: u32,
}
/// Requests per boot before giving up (a build with no slot metadata answers
/// every one with an error).
pub const MARK_TRIES: u32 = 8;
impl Mark {
    pub const fn new() -> Mark {
        Mark { healthy: false, done: false, tries: 0, next: 0 }
    }
    /// The components (re)start: not healthy again until they have run so.
    pub fn restart(&mut self) {
        self.healthy = false;
        self.tries = 0;
    }
    /// Whether to ask Storage now. `healthy`: the health period has passed
    /// since the last (re)start. `now` and `second` are timer ticks.
    pub fn due(&mut self, healthy: bool, now: u32) -> bool {
        if !healthy || self.done || self.tries >= MARK_TRIES {
            return false;
        }
        if !self.healthy {
            self.healthy = true;
            self.next = now;
        }
        now.wrapping_sub(self.next) as i32 >= 0
    }
    /// What Storage answered.
    pub fn result(&mut self, ok: bool, now: u32, second: u32) {
        if ok {
            self.done = true;
            return;
        }
        self.tries += 1;
        // 4, 8, 16 then 32 s between requests.
        self.next = now.wrapping_add(second.saturating_mul(2 << self.tries.min(4)));
    }
    pub fn done(&self) -> bool {
        self.done
    }
}

/// Check a manifest (the first bytes of the slot's last sector) for a slot
/// of `slot_sectors` sectors, the last of which holds the manifest itself.
/// `floor` is the running system's version. Only a manifest whose signature
/// verifies under one of `keys` for `target` is returned.
pub fn check(
    bytes: &[u8],
    target: u32,
    keys: &[(u32, [u8; 65])],
    floor: u32,
    slot_sectors: u32,
    sha256: fn(&[u8]) -> [u8; 32],
    p256: fn(&[u8; 65], &[u8; 32], &[u8; 64]) -> bool,
) -> Result<Manifest, Refusal> {
    if bytes.len() < MANIFEST || word(bytes, 0) != VOT1 || word(bytes, 24) != 0 || word(bytes, 28) != 0 {
        return Err(Refusal::Format);
    }
    let layout = word(bytes, 16);
    if layout != LAYOUT_QEMU && layout != LAYOUT_ANDROID {
        return Err(Refusal::Format);
    }
    if word(bytes, 12) != target {
        return Err(Refusal::Target);
    }
    let key_id = word(bytes, 4);
    let Some((_, key)) = keys.iter().find(|k| k.0 == key_id) else {
        return Err(Refusal::Key);
    };
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&bytes[SIGNED..MANIFEST]);
    if !p256(key, &sha256(&bytes[..SIGNED]), &signature) {
        return Err(Refusal::Signature);
    }
    // Only signed fields from here on.
    let version = word(bytes, 8);
    if version == 0 || version < floor {
        return Err(Refusal::Version);
    }
    let image_len = word(bytes, 20);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&bytes[32..64]);
    let m = Manifest { key_id, version, layout, image_len, digest };
    if image_len == 0 || slot_sectors < 2 || m.sectors() > slot_sectors - 1 {
        return Err(Refusal::Size);
    }
    Ok(m)
}

/// The last step: the digest Storage computed over the slot as it reads back.
pub fn matches(m: &Manifest, read_back: &[u8; 32]) -> Result<(), Refusal> {
    // Not secret: a plain comparison is enough.
    if &m.digest == read_back {
        Ok(())
    } else {
        Err(Refusal::Digest)
    }
}
