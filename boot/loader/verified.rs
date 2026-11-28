//! Verified boot: the loader's check of what it is about to start.
//!
//! The image carries one signed boot manifest (tools/boot_manifest.py) beside
//! the kernel and the root task. Before the loader copies either, it checks
//! the manifest's signature against the keys pinned in the loader, refuses a
//! system older than the bootloader's stored rollback floor, and compares
//! SHA-256 of the kernel, of the root task (the supervisor with its pinned
//! package keys and, on the handset, the factory catalog) and, when the
//! manifest binds it, of the open apps' file system image, with the signed
//! digests. Anything else is refused: the loader starts nothing.
//!
//!    0  "VBM1"
//!    4  key_id      u32   the VPK2 signing key IDs (image/keys/README.md)
//!    8  version     u32   security version of the system, 1 or more
//!   12  target      u32   "QEMU" or "MOTO", as in VPK2
//!   16  kernel_len  u32
//!   20  root_len    u32
//!   24  apps_len    u32   bytes of /apps image; 0 unless flags bit 0
//!   28  flags       u32   bit 0: the /apps image is bound; other bits 0
//!   32  kernel[32]        SHA-256 of the kernel image
//!   64  root[32]          SHA-256 of the root task image
//!   96  apps[32]          SHA-256 of the /apps image; zeros unless bound
//!  128  r[32] s[32]       ECDSA P-256 over SHA-256 of bytes 0..128
//!
//! Signed by the same keys as packages (VPK2) and update manifests (VOT1);
//! the magic keeps one kind of signature from passing as another.
//!
//! Pure: the hash, the signature check and the memory to read are arguments,
//! so the host tests (test_verified.rs) run the same code as the loader.
#![allow(dead_code)]

pub const MANIFEST: usize = 192;
pub const SIGNED: usize = 128;
const VBM1: u32 = 0x314d_4256;
pub const APPS_BOUND: u32 = 1;

/// Why the loader refused. The number is what the loader prints and shows.
#[derive(Debug, PartialEq, Clone, Copy)]
#[repr(u32)]
pub enum Refusal {
    /// No manifest, or not one this loader understands.
    Format = 1,
    /// Signed for another target.
    Target = 2,
    /// Signed by a key this loader does not pin.
    Key = 3,
    Signature = 4,
    /// Older than the stored rollback floor, or the floor is unreadable.
    Version = 5,
    /// This loader requires the /apps image to be bound and it is not.
    Unbound = 6,
    Kernel = 7,
    Root = 8,
    Apps = 9,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Verified {
    pub key_id: u32,
    pub version: u32,
    /// Bytes of /apps image the signature covers; None when it is not bound.
    pub apps: Option<u32>,
}

fn word(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The rollback floor the bootloader hands over: "VBFL", the floor, its
/// complement. No record means no stored floor (0). A record that does not
/// check means the floor is unknown, and then nothing may boot.
pub const FLOOR_MAGIC: u32 = 0x4c46_4256;
pub fn handoff_floor(words: [u32; 3]) -> Result<u32, Refusal> {
    if words == [0; 3] {
        return Ok(0);
    }
    if words[0] == FLOOR_MAGIC && words[2] == !words[1] {
        Ok(words[1])
    } else {
        Err(Refusal::Version)
    }
}

/// Check `manifest` for the `kernel` and `root` images about to be started.
/// `apps_window` is the memory where the /apps image is, to its longest
/// possible length. `require_apps`: refuse a manifest that does not bind it
/// (release loaders). Only signed fields are used after the signature check.
pub fn check(
    manifest: &[u8],
    target: u32,
    keys: &[(u32, [u8; 65])],
    floor: u32,
    require_apps: bool,
    kernel: &[u8],
    root: &[u8],
    apps_window: &[u8],
    sha256: fn(&[u8]) -> [u8; 32],
    p256: fn(&[u8; 65], &[u8; 32], &[u8; 64]) -> bool,
) -> Result<Verified, Refusal> {
    if manifest.len() != MANIFEST || word(manifest, 0) != VBM1 {
        return Err(Refusal::Format);
    }
    let flags = word(manifest, 28);
    let apps_len = word(manifest, 24);
    let bound = flags & APPS_BOUND != 0;
    if flags & !APPS_BOUND != 0 || (!bound && (apps_len != 0 || manifest[96..128] != [0; 32])) {
        return Err(Refusal::Format);
    }
    if word(manifest, 12) != target {
        return Err(Refusal::Target);
    }
    let key_id = word(manifest, 4);
    let Some((_, key)) = keys.iter().find(|k| k.0 == key_id) else {
        return Err(Refusal::Key);
    };
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&manifest[SIGNED..MANIFEST]);
    if !p256(key, &sha256(&manifest[..SIGNED]), &signature) {
        return Err(Refusal::Signature);
    }
    let version = word(manifest, 8);
    if version == 0 || version < floor {
        return Err(Refusal::Version);
    }
    if require_apps && !bound {
        return Err(Refusal::Unbound);
    }
    if word(manifest, 16) as usize != kernel.len() || sha256(kernel)[..] != manifest[32..64] {
        return Err(Refusal::Kernel);
    }
    if word(manifest, 20) as usize != root.len() || sha256(root)[..] != manifest[64..96] {
        return Err(Refusal::Root);
    }
    if bound {
        let n = apps_len as usize;
        if n == 0 || n > apps_window.len() || sha256(&apps_window[..n])[..] != manifest[96..128] {
            return Err(Refusal::Apps);
        }
    }
    Ok(Verified { key_id, version, apps: bound.then_some(apps_len) })
}
