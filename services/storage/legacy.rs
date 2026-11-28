//! The open apps' files as they were kept before VioletFS: 32 slots of up to
//! 4 KiB in the flat store (files.rs). Read-only here, for the one move into
//! the filesystem (vfs.rs `migrate`). Each slot is read in one go, its two
//! bank headers and its pages (at most 10 sectors), not through files.rs's
//! 40-byte messages, each of which looks up every slot before it again: the
//! move runs before Storage answers anyone, and on the handset's eMMC
//! (about 200 sectors a second) those would have taken minutes. The layout
//! is files.rs's; the host tests check this against files.rs itself.
use crate::block::Disk;
use crate::journal::crc32;

pub const SLOTS: u32 = 32;
const PAGES: usize = 8;
const MAX: usize = PAGES * 512;

/// The first sector of the open apps' (files.rs owner 7) `slot`, `bank`:
/// files.rs `base`, with its SLOTS of 4 per owner and EXTRA at 608.
fn base(slot: usize, bank: usize) -> usize {
    if slot < 4 {
        32 + ((7 * 4 + slot) * 2 + bank) * (PAGES + 1)
    } else {
        608 + ((slot - 4) * 2 + bank) * (PAGES + 1)
    }
}
fn le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The name and length of the file in `slot`, its bytes in `out`.
/// `Ok(None)`: the slot is empty or the disk has no such slot. `Err`: it
/// could not be read, a page does not match its checksum, or it is longer
/// than `out`.
pub fn file(d: &mut Disk, slot: u32, out: &mut [u8]) -> Result<Option<([u8; 16], usize)>, ()> {
    let slot = slot as usize;
    if slot >= SLOTS as usize || base(slot, 0) + 2 * (PAGES + 1) > d.sectors {
        return Ok(None);
    }
    // The bank with the higher generation; one that is not a valid entry
    // counts as an empty one of generation 0 (files.rs `current`).
    let mut best: Option<(usize, [u8; 512])> = None;
    let mut generation = 0;
    for bank in 0..2 {
        let h = d.read(base(slot, bank))?;
        let valid = &h[..4] == b"VFS1" && le(&h, 508) == crc32(&h[..508]) && le(&h, 8) as usize <= MAX;
        if valid && (bank == 0 || le(&h, 4) > generation) {
            generation = le(&h, 4);
            best = Some((bank, h));
        }
    }
    let Some((bank, h)) = best else { return Ok(None) };
    let mut name = [0u8; 16];
    name.copy_from_slice(&h[12..28]);
    let len = le(&h, 8) as usize;
    if name[0] == 0 {
        return Ok(None);
    }
    if len > out.len() {
        return Err(());
    }
    for page in 0..(len + 511) / 512 {
        let s = d.read(base(slot, bank) + 1 + page)?;
        if crc32(&s) != le(&h, 28 + page * 4) {
            return Err(());
        }
        let n = (len - page * 512).min(512);
        out[page * 512..page * 512 + n].copy_from_slice(&s[..n]);
    }
    Ok(Some((name, len)))
}
