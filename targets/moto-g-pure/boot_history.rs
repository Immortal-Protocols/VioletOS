//! The header is immutable. A replacement is flushed in the older record slot.
use crate::journal::{recover, Record, Sector};
pub const TAG: &[u8] = b"VioletOS boot history v1\n";
pub fn update(stage: u8, mut read: impl FnMut(u32) -> Result<Sector, ()>,
              mut replace: impl FnMut(u32, &mut Sector) -> Result<(), ()>) -> Result<u8, ()> {
    let marker = read(0)?;
    // Stages 1-6 (README.md); the AArch64 loader writes 21-23 for its own 1-3,
    // so a 32-bit boot after it can tell which loader got how far, and a boot
    // it refuses before its stage 21 as 40 + the handoff check (41-51, 61-71
    // when the device tree was not in x0) or 90 + the verified-boot reason (91-99).
    let refused = (41..=51).contains(&stage) || (61..=71).contains(&stage) || (91..=99).contains(&stage);
    if !(1..=6).contains(&stage) && !(21..=23).contains(&stage) && !refused || &marker[..TAG.len()] != TAG
        || marker[TAG.len()..].iter().any(|&v| v != 0) {
        return Err(());
    }
    let (slot, old) = recover(&read(1)?, &read(2)?)?;
    // A boot's first stage (1, 21, or a refusal, which is also its last)
    // keeps the last one of the boot before.
    let previous = if stage == 1 || stage == 21 || refused { old.tenths as u8 } else { (old.tenths >> 8) as u8 };
    let mut bytes = Record {
        generation: old.generation.checked_add(1).ok_or(())?,
        tenths: (u32::from(previous) << 8) | u32::from(stage),
    }.encode();
    let target = 1 + (1 - slot) as u32;
    replace(target, &mut bytes)?;
    if read(target)? != bytes { return Err(()); }
    Ok(previous)
}
