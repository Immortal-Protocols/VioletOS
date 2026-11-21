//! Run on the Mac with rustc --test. Exercise the same mount guard as the phone.
#[path = "../../services/storage/journal.rs"]
mod journal;
#[path = "storage_volume.rs"]
mod volume;
use journal::{crc32, Sector};
use volume::{verify, BOOT_B, MARKER, SECTORS};
const CAP: u32 = 64_000_000;
struct Media {
    header: Sector,
    entries: [Sector; 32],
    marker: Sector,
}
impl Media {
    fn new() -> Self {
        let mut disk = Self {
            header: [0; 512],
            entries: [[0; 512]; 32],
            marker: [0; 512],
        };
        let h = &mut disk.header;
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        for (off, val) in [
            (24, 1u64),
            (32, (CAP - 1) as u64),
            (40, 64),
            (48, (CAP - 34) as u64),
            (72, 2),
        ] {
            h[off..off + 8].copy_from_slice(&val.to_le_bytes());
        }
        for off in [80, 84] {
            h[off..off + 4].copy_from_slice(&128u32.to_le_bytes());
        }
        let e = &mut disk.entries[0];
        e[0] = 1;
        e[32..40].copy_from_slice(&(BOOT_B as u64).to_le_bytes());
        e[40..48].copy_from_slice(&(BOOT_B as u64 + 65535).to_le_bytes());
        e[56..68].copy_from_slice(b"b\0o\0o\0t\0_\0b\0");
        let tag = b"VioletOS XT2163DL boot_b storage v1\n";
        disk.marker[..tag.len()].copy_from_slice(tag);
        disk.sign();
        disk
    }
    fn sign(&mut self) {
        let sum = crc32(&self.entries.concat());
        self.header[88..92].copy_from_slice(&sum.to_le_bytes());
        self.header[16..20].fill(0);
        let sum = crc32(&self.header[..92]);
        self.header[16..20].copy_from_slice(&sum.to_le_bytes());
    }
    fn mount(&self) -> Result<(), &'static [u8]> {
        verify(
            |lba| match lba {
                1 => Ok(self.header),
                2..=33 => Ok(self.entries[(lba - 2) as usize]),
                BOOT_B => {
                    let mut b = [0; 512];
                    b[..8].copy_from_slice(b"ANDROID!");
                    b[40] = 2;
                    Ok(b)
                }
                MARKER => Ok(self.marker),
                _ => Err(()),
            },
            CAP,
        )
    }
}
#[test]
fn exact_volume_only() {
    assert!(Media::new().mount().is_ok());
    assert_eq!(volume::sector(0), Ok(MARKER + 1));
    assert_eq!(volume::sector(SECTORS - 1), Ok(MARKER + SECTORS as u32));
    assert!(volume::sector(SECTORS).is_err());
    assert!(volume::sector(usize::MAX).is_err());
}
#[test]
fn corrupt_header_or_entries() {
    let mut d = Media::new();
    d.header[63] ^= 1;
    assert!(d.mount().is_err());
    let mut d = Media::new();
    d.entries[31][511] ^= 1;
    assert!(d.mount().is_err());
}
#[test]
fn wrong_slot_or_overlapping_partition() {
    let mut d = Media::new();
    d.entries[0][66] = b'a';
    d.sign();
    assert!(d.mount().is_err());
    let mut d = Media::new();
    d.entries[1] = d.entries[0];
    d.sign();
    assert!(d.mount().is_err());
}
#[test]
fn missing_marker() {
    let mut d = Media::new();
    d.marker[0] = 0;
    assert!(d.mount().is_err());
}
