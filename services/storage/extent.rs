//! Where the data volume (volume.rs) lies on each disk. Only arithmetic, so
//! the disk drivers (block.rs, targets/moto-g-pure/emmc.rs) and the host
//! tests share it.
//!
//! Every disk has the first megabyte Storage always had: the checkpoint
//! records and the flat files of files.rs, which end at sector 1112. The
//! rest of that megabyte was never used, and is the data volume where there
//! is nothing better (`SMALL`): a handset whose userdata still belongs to
//! Android, a small test disk. It is 464 KiB, and shares the megabyte's
//! fate: `moto-flash` leaves it alone unless told to wipe the storage.
//!
//! QEMU's disk of 16 MiB or more (tools/run.py) has the installed apps at
//! sectors 4096..28672; the data volume is the 2 MiB after them, which
//! nothing else uses (the boot slots of the update tests start at 32768).
//!
//! On a handset whose userdata VioletOS has claimed, the installed apps had
//! all of it. They keep the first gibibyte: the app store places a program
//! at the lowest gap that fits, holds at most 31 of at most 8 MiB, and so
//! never reaches beyond 496 MiB (31 programs and 31 gaps too small for
//! one), whichever system image wrote them. The data volume is the next
//! gibibyte. It is not the whole 23 GiB because the driver moves about
//! 100 KiB a second (a larger volume takes longer to make and could not be
//! filled in any useful time), and VioletFS cannot be grown later: a
//! larger one means making it again.
//!
//! Where it is, once and for all. The extent the driver offers can change
//! from one boot to the next: userdata is claimed after the phone ran on
//! the first megabyte, a read of the claim fails once, a QEMU disk is made
//! smaller. Serving whichever volume is there would roll /data back (an old
//! passcode, revoked grants) or start an empty one (no passcode at all). So
//! the first place a volume is made is recorded (mount.rs keeps the record
//! in the first megabyte) and `decide` holds every later boot to it.

/// Where the data volume lies: `count` sectors from `first`, in the disk
/// driver's numbering (`raw`: the handset's eMMC itself, else the storage
/// volume's sectors).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    pub first: u32,
    pub count: u32,
    pub raw: bool,
}
pub const NOWHERE: Place = Place { first: 0, count: 0, raw: false };

/// The volume in the first megabyte, on a disk of `sectors` that has one.
pub const fn small(sectors: usize) -> Place {
    if sectors >= SMALL_END {
        Place { first: SMALL_FIRST as u32, count: (SMALL_END - SMALL_FIRST) as u32, raw: false }
    } else {
        NOWHERE
    }
}

/// Where to serve /data from, given the record (`Some((place, done))`:
/// `done` once the old files went into it), what the driver offers this
/// boot and the first megabyte's volume. `Ok((place, fresh))`: `fresh`
/// when it may still be formatted and the old files moved in. A recorded
/// place that this boot cannot reach is not replaced by another: /data is
/// then unavailable (and the phone stays locked) until it can be.
pub fn decide(record: Option<(Place, bool)>, offer: Place, small: Place) -> Result<(Place, bool), &'static [u8]> {
    match record {
        None if offer.count == 0 => Err(b"DATA NO ROOM"),
        None => Ok((offer, true)),
        // Made in the first megabyte and a larger extent offered since:
        // what was made stays (moving it is not done).
        Some((p, done)) if p.count != 0 && (p == offer || p == small) => Ok((p, !done)),
        Some(_) => Err(b"DATA PLACE GONE"),
    }
}

/// First sector after the flat files, rounded up to a 4 KiB block.
pub const SMALL_FIRST: usize = 1120;
/// The first megabyte.
pub const SMALL_END: usize = 2048;

/// QEMU's disk (block.rs uses these): the installed apps, and where the
/// update tests' misc partition starts.
pub const QEMU_APPS: usize = 4096;
pub const QEMU_APP_SECTORS: usize = 24576;
pub const QEMU_OTA_MISC: usize = 32768;
pub const QEMU_FIRST: usize = QEMU_APPS + QEMU_APP_SECTORS;
pub const QEMU_SECTORS: usize = 4096;

/// QEMU: (first sector, sectors) of the data volume on a disk of `disk`
/// sectors; no sectors if the disk is under a megabyte.
pub const fn qemu(disk: usize) -> (usize, usize) {
    if disk >= QEMU_FIRST + QEMU_SECTORS {
        (QEMU_FIRST, QEMU_SECTORS)
    } else if disk >= SMALL_END {
        (SMALL_FIRST, SMALL_END - SMALL_FIRST)
    } else {
        (0, 0)
    }
}

/// Sectors of claimed userdata the installed apps keep, and the most the
/// data volume takes after them: a gibibyte each.
pub const APPS_SECTORS: u32 = 1 << 21;
pub const DATA_SECTORS: u32 = 1 << 21;

/// Claimed userdata from sector `start` (after its header) to `last`:
/// (sectors of the installed apps, first sector and sectors of the data
/// volume). None when it is too small to split; the apps then keep all of
/// it and the data volume stays in the first megabyte.
pub const fn userdata(start: u32, last: u32) -> Option<(u32, u32, u32)> {
    if last < start {
        return None;
    }
    let total = (last - start) as u64 + 1;
    if total < (APPS_SECTORS + DATA_SECTORS / 8) as u64 {
        return None;
    }
    let rest = total - APPS_SECTORS as u64;
    let data = if rest < DATA_SECTORS as u64 { rest as u32 & !7 } else { DATA_SECTORS };
    Some((APPS_SECTORS, start + APPS_SECTORS, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_volume_never_overlaps_what_was_there_before() {
        // QEMU: after the flat files, clear of the apps' extent and the boot slots.
        assert_eq!(qemu(0), (0, 0));
        assert_eq!(qemu(2047), (0, 0));
        assert_eq!(qemu(2048), (1120, 928));
        assert_eq!(qemu(32767), (1120, 928));
        assert_eq!(qemu(32768), (28672, 4096));
        assert_eq!(qemu(usize::MAX), (28672, 4096));
        assert!(SMALL_FIRST >= 1112 && SMALL_FIRST % 8 == 0, "files.rs's last slot ends at sector 1112");
        assert!(QEMU_FIRST >= QEMU_APPS + QEMU_APP_SECTORS && QEMU_FIRST + QEMU_SECTORS <= QEMU_OTA_MISC, "after the apps, before misc");
        assert!(QEMU_APPS >= SMALL_END, "the installed apps start after the first megabyte");
        // Handset: the XT2163DL's userdata is about 23 GiB.
        let (start, last) = (15_000_008u32, 15_000_000 + 48_000_000);
        let (apps, first, count) = userdata(start, last).unwrap();
        assert_eq!((apps, first, count), (1 << 21, start + (1 << 21), 1 << 21));
        // 31 programs of 8 MiB and as many gaps just too small for one.
        assert!(apps as u64 * 512 > 2 * 31 * (8 << 20));
        assert!(first + count - 1 <= last && count % 8 == 0);
        // A small one: the volume takes what is left, in whole blocks, or nothing.
        let (_, first, count) = userdata(8, 8 + (1 << 21) + 300_003 - 1).unwrap();
        assert_eq!((first, count), (8 + (1 << 21), 300_000));
        assert_eq!(userdata(8, 8 + (1 << 21) + 1000), None);
        assert_eq!(userdata(8, 7), None);
        assert_eq!(userdata(u32::MAX - 5, u32::MAX), None);
        assert_eq!(userdata(0, u32::MAX), Some((1 << 21, 1 << 21, 1 << 21)));
    }
}
