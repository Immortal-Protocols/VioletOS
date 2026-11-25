//! Slot B bounds from the XT2163DL factory PGPT (manifest MD5 ecab4974a9b70a9d5ad14cb27ac6bc38).
//! Validate the live table before using the offset provisioned by the image builder.
use crate::journal::{crc32, Sector};
pub const BOOT_B: u32 = 2_000_896;
const END_B: u32 = 2_066_431;
pub const MARKER: u32 = BOOT_B + 16 * 1024 * 1024 / 512;
pub const SECTORS: usize = 2048;
const TAG: &[u8] = b"VioletOS XT2163DL boot_b storage v1\n";

fn word(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn wide(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
/// Installed open apps (runtime/open/appstore.rs) keep the last 12 MiB of boot_b,
/// which the flashed image (19,926,528 bytes) never reaches.
pub const APPS: u32 = BOOT_B + 20 * 1024 * 1024 / 512;
pub const APP_SECTORS: usize = (END_B + 1 - APPS) as usize;
pub fn app_sector(index: usize) -> Result<u32, ()> {
    if index >= APP_SECTORS {
        return Err(());
    }
    Ok(APPS + index as u32)
}
pub fn sector(index: usize) -> Result<u32, ()> {
    if index >= SECTORS {
        return Err(());
    }
    Ok(MARKER + 1 + index as u32)
}
pub fn verify(
    mut read: impl FnMut(u32) -> Result<Sector, ()>,
    capacity: u32,
) -> Result<(), &'static [u8]> {
    let mut h = read(1).map_err(|_| b"GPT READ" as &[u8])?;
    let size = word(&h, 12) as usize;
    if &h[..8] != b"EFI PART"
        || !(92..=512).contains(&size)
        || word(&h, 8) != 0x10000
        || wide(&h, 24) != 1
        || wide(&h, 72) != 2
        || word(&h, 80) != 128
        || word(&h, 84) != 128
        || wide(&h, 40) > BOOT_B as u64
        || wide(&h, 48) < END_B as u64
        || END_B >= capacity
    {
        return Err(b"GPT GEOMETRY");
    }
    let sum = word(&h, 16);
    h[16..20].fill(0);
    if crc32(&h[..size]) != sum {
        return Err(b"GPT HEADER CRC");
    }
    let mut crc = !0u32;
    let mut found = false;
    for i in 0..32 {
        let block = read(2 + i).map_err(|_| b"GPT TABLE READ" as &[u8])?;
        crc = crate::journal::crc_update(crc, &block);
        for entry in block.chunks_exact(128) {
            if entry[..16] == [0; 16] {
                continue;
            }
            let (first, last) = (wide(entry, 32), wide(entry, 40));
            if first > last {
                return Err(b"GPT RANGE");
            }
            if last < BOOT_B as u64 || first > END_B as u64 {
                continue;
            }
            if found
                || first != BOOT_B as u64
                || last != END_B as u64
                || &entry[56..68] != b"b\0o\0o\0t\0_\0b\0"
                || entry[68..128].iter().any(|&b| b != 0)
            {
                return Err(b"BOOT B BOUNDS");
            }
            found = true;
        }
    }
    if !found || !crc != word(&h, 88) {
        return Err(b"GPT TABLE CRC");
    }
    let boot = read(BOOT_B).map_err(|_| b"BOOT B READ" as &[u8])?;
    if &boot[..8] != b"ANDROID!" || word(&boot, 40) != 2 {
        return Err(b"BOOT B HEADER");
    }
    let marker = read(MARKER).map_err(|_| b"VOLUME READ" as &[u8])?;
    if &marker[..TAG.len()] != TAG || marker[TAG.len()..].iter().any(|&b| b != 0) {
        return Err(b"VOLUME MARKER");
    }
    Ok(())
}
