//! The header is immutable. A replacement is flushed in the older record slot.
use crate::journal::{recover, Record, Sector};
pub const TAG: &[u8] = b"VioletOS boot history v1\n";
pub fn update(stage: u8, mut read: impl FnMut(u32) -> Result<Sector, ()>,
              mut replace: impl FnMut(u32, &mut Sector) -> Result<(), ()>) -> Result<u8, ()> {
    let marker = read(0)?;
    if !(1..=6).contains(&stage) || &marker[..TAG.len()] != TAG
        || marker[TAG.len()..].iter().any(|&v| v != 0) {
        return Err(());
    }
    let (slot, old) = recover(&read(1)?, &read(2)?)?;
    let previous = if stage == 1 { old.tenths as u8 } else { (old.tenths >> 8) as u8 };
    let mut bytes = Record {
        generation: old.generation.checked_add(1).ok_or(())?,
        tenths: (u32::from(previous) << 8) | u32::from(stage),
    }.encode();
    let target = 1 + (1 - slot) as u32;
    replace(target, &mut bytes)?;
    if read(target)? != bytes { return Err(()); }
    Ok(previous)
}
