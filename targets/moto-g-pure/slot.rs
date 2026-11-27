//! A/B slot metadata: Android's `bootloader_control` record.
//!
//! Where it comes from on the XT2163DL (read from the factory files only, never
//! from the phone): the S3RHS32.20-42-13-2-3-2 boot image's ramdisk carries
//! the stock boot control HAL, `system/lib/hw/android.hardware.boot@1.0-impl-
//! 1.2-mtkimpl.so`. Its strings name AOSP's
//! `hardware/interfaces/boot/1.1/default/boot_control/libboot_control.cpp`
//! and "failed to find /misc partition", and its fstab maps `/misc` to
//! `/dev/block/platform/bootdevice/by-name/misc` (a separate `para` partition
//! also exists and is not used for this). Disassembled, the HAL reads and
//! writes 32 bytes at byte 0x800 of misc (`LoadBootloaderControl`,
//! `UpdateAndSaveBootloaderControl`: lseek 0x800, O_WRONLY|O_SYNC), builds the
//! magic 0x42414342 ("BCAB"), keeps a CRC-32 at byte 28, and changes the
//! per-slot halfwords at byte 12 + 2 * slot exactly as AOSP does:
//!
//!   MarkBootSuccessful    successful_boot = 1, tries_remaining = 1
//!   SetActiveBootSlot     priority 15, tries 6 for the slot; any other slot at
//!                         priority 15 drops to 14; verity_corrupted cleared
//!                         when the slot is not the current one
//!   SetSlotAsUnbootable   tries_remaining = 0, successful_boot = 0
//!
//! The record is
//!
//!   0  slot_suffix[4]   "_a\0\0" / "_b\0\0"
//!   4  magic u32        0x42414342
//!   8  version u8       1
//!   9  nb_slot:3, recovery_tries_remaining:3, merge_status:3 (into byte 10)
//!  11  reserved
//!  12  slot_info[4]     u16 each: priority:4 tries_remaining:3
//!                       successful_boot:1 verity_corrupted:1 reserved:7
//!  20  reserved[8]
//!  28  crc32_le         CRC-32 (zlib) of bytes 0..28
//!
//! What is NOT established from the files: that Motorola's bootloader
//! (MBM-2.1, LK) reads this record rather than a private copy (the 3-bit
//! retry field matches the 7 retries fastboot reported, nothing more); the
//! LBA of misc (it is found by name in the live, CRC-checked GPT, never
//! assumed); and which slot is running (the bootloader passes
//! androidboot.slot_suffix, which VioletOS's loader does not read yet). So
//! the handset build does not enable writes through this module (Storage's
//! `slot_ota` cfg); QEMU uses the same record on its slot disk, where
//! tools/ota_boot.py plays the bootloader.
#![allow(dead_code)]

pub const MAGIC: u32 = 0x4241_4342;
pub const VERSION: u8 = 1;
/// Byte offset of the record in misc: offsetof(bootloader_message_ab, slot_suffix).
pub const MISC_OFFSET: u32 = 0x800;
/// The sector of misc that holds it.
pub const MISC_SECTOR: u32 = MISC_OFFSET / 512;
pub const LEN: usize = 32;
/// Slots VioletOS manages (boot_a and boot_b).
pub const SLOTS: usize = 2;
/// What the stock HAL writes for a slot made active (AOSP kActivePriority, kActiveTries).
pub const ACTIVE_PRIORITY: u8 = 15;
pub const ACTIVE_TRIES: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a bootloader_control record (wrong magic): nothing is written.
    Magic,
    Version,
    Crc,
    /// nb_slot is not 2: a layout VioletOS does not know.
    Slots,
    /// A slot index past nb_slot.
    Slot,
    /// The sector read back after the write is not what was written.
    Readback,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub priority: u8,
    pub tries: u8,
    pub successful: bool,
    pub verity_corrupted: bool,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0u32.wrapping_sub(crc & 1) & 0xedb8_8320);
        }
    }
    !crc
}

/// A decoded record. Bytes it does not interpret are kept as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    raw: [u8; LEN],
}

impl Control {
    /// The record at the start of `bytes`, if its magic, version, slot count
    /// and CRC are all what the stock HAL writes.
    pub fn decode(bytes: &[u8]) -> Result<Control, Error> {
        if bytes.len() < LEN {
            return Err(Error::Magic);
        }
        let mut raw = [0u8; LEN];
        raw.copy_from_slice(&bytes[..LEN]);
        if u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) != MAGIC {
            return Err(Error::Magic);
        }
        if raw[8] != VERSION {
            return Err(Error::Version);
        }
        if crc32(&raw[..28]) != u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]) {
            return Err(Error::Crc);
        }
        if raw[9] & 7 != SLOTS as u8 {
            return Err(Error::Slots);
        }
        Ok(Control { raw })
    }
    /// A fresh record as the stock HAL initialises one (both slots priority 7,
    /// 7 tries; `current` successful). For tests and the QEMU slot disk only:
    /// VioletOS never initialises a handset's record.
    pub fn initial(current: usize) -> Control {
        let mut raw = [0u8; LEN];
        raw[..2].copy_from_slice(if current == 0 { b"_a" } else { b"_b" });
        raw[4..8].copy_from_slice(&MAGIC.to_le_bytes());
        raw[8] = VERSION;
        raw[9] = SLOTS as u8 | (7 << 3);
        let mut c = Control { raw };
        for i in 0..SLOTS {
            c.put(i, Slot { priority: 7, tries: 7, successful: i == current, verity_corrupted: false });
        }
        c
    }
    /// The 32 bytes with the CRC recomputed.
    pub fn encode(&self) -> [u8; LEN] {
        let mut raw = self.raw;
        let crc = crc32(&raw[..28]);
        raw[28..].copy_from_slice(&crc.to_le_bytes());
        raw
    }
    fn half(&self, i: usize) -> u16 {
        u16::from_le_bytes([self.raw[12 + 2 * i], self.raw[13 + 2 * i]])
    }
    fn set_half(&mut self, i: usize, v: u16) {
        self.raw[12 + 2 * i..14 + 2 * i].copy_from_slice(&v.to_le_bytes());
    }
    pub fn slot(&self, i: usize) -> Slot {
        let h = self.half(i);
        Slot {
            priority: (h & 15) as u8,
            tries: ((h >> 4) & 7) as u8,
            successful: h & 0x80 != 0,
            verity_corrupted: h & 0x100 != 0,
        }
    }
    /// Write a slot's fields; the reserved bits of its halfword stay.
    pub fn put(&mut self, i: usize, s: Slot) {
        let h = self.half(i) & !0x1ff;
        let v = u16::from(s.priority & 15)
            | (u16::from(s.tries & 7) << 4)
            | ((s.successful as u16) << 7)
            | ((s.verity_corrupted as u16) << 8);
        self.set_half(i, h | v);
    }
    fn check(i: usize) -> Result<(), Error> {
        if i < SLOTS {
            Ok(())
        } else {
            Err(Error::Slot)
        }
    }
    /// The slot named by slot_suffix, when it names one.
    pub fn suffix(&self) -> Option<usize> {
        match &self.raw[..4] {
            [b'_', b'a', 0, 0] => Some(0),
            [b'_', b'b', 0, 0] => Some(1),
            _ => None,
        }
    }
    pub fn set_suffix(&mut self, i: usize) {
        self.raw[..4].copy_from_slice(if i == 0 { b"_a\0\0" } else { b"_b\0\0" });
    }
    /// A slot the bootloader would still try: a priority, and either proven
    /// or with tries left.
    pub fn bootable(&self, i: usize) -> bool {
        let s = self.slot(i);
        s.priority > 0 && (s.successful || s.tries > 0)
    }
    /// MarkBootSuccessful for the running slot. Returns false (and changes
    /// nothing) when the slot is already marked: no write per boot.
    pub fn mark_successful(&mut self, i: usize) -> Result<bool, Error> {
        Self::check(i)?;
        let mut s = self.slot(i);
        if s.successful {
            // Already proven (the stock HAL would rewrite tries = 1; a proven
            // slot's tries are not consulted, so leave the sector alone).
            return Ok(false);
        }
        s.successful = true;
        s.tries = 1;
        self.put(i, s);
        Ok(true)
    }
    /// SetSlotAsUnbootable: before a slot's image is rewritten, so a torn
    /// write is never booted.
    pub fn set_unbootable(&mut self, i: usize) -> Result<bool, Error> {
        Self::check(i)?;
        let mut s = self.slot(i);
        if s.tries == 0 && !s.successful {
            return Ok(false);
        }
        s.tries = 0;
        s.successful = false;
        self.put(i, s);
        Ok(true)
    }
    /// SetActiveBootSlot: `i` is tried next, ACTIVE_TRIES times; until it is
    /// marked successful the bootloader falls back to the other slot.
    pub fn set_active(&mut self, i: usize, current: usize) -> Result<(), Error> {
        Self::check(i)?;
        for other in 0..SLOTS {
            let mut s = self.slot(other);
            if other != i && s.priority >= ACTIVE_PRIORITY {
                s.priority = ACTIVE_PRIORITY - 1;
                self.put(other, s);
            }
        }
        let mut s = self.slot(i);
        s.priority = ACTIVE_PRIORITY;
        s.tries = ACTIVE_TRIES;
        if i != current {
            s.verity_corrupted = false;
        }
        self.put(i, s);
        Ok(())
    }
    /// What an AOSP-style bootloader does at power-on (the QEMU model in
    /// tools/ota_boot.py follows this): the bootable slot of highest
    /// priority (slot A on a tie), one try spent if it is not proven, and its
    /// suffix recorded. None: nothing is bootable.
    pub fn choose(&mut self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for i in 0..SLOTS {
            if self.bootable(i) && best.map_or(true, |b| self.slot(i).priority > self.slot(b).priority) {
                best = Some(i);
            }
        }
        let i = best?;
        let mut s = self.slot(i);
        if !s.successful {
            s.tries -= 1;
            self.put(i, s);
        }
        self.set_suffix(i);
        Some(i)
    }
}

/// Read, change and write back the record in one sector, then read it again.
/// `change` returns whether anything changed; when nothing did, nothing is
/// written (so marking a slot that is already proven costs no flash write).
/// Any failure before the write leaves the sector untouched; a record that
/// does not decode is never rewritten.
pub fn update(
    mut read: impl FnMut() -> Result<[u8; 512], ()>,
    mut write: impl FnMut(&mut [u8; 512]) -> Result<(), ()>,
    change: impl FnOnce(&mut Control) -> Result<bool, Error>,
) -> Result<bool, Error> {
    let mut sector = read().map_err(|_| Error::Io)?;
    let mut control = Control::decode(&sector)?;
    if !change(&mut control)? {
        return Ok(false);
    }
    sector[..LEN].copy_from_slice(&control.encode());
    let expected = sector;
    write(&mut sector).map_err(|_| Error::Io)?;
    if read().map_err(|_| Error::Io)? != expected {
        return Err(Error::Readback);
    }
    Ok(true)
}

/// A partition's first and last sector, by its GPT name (UTF-16LE, ASCII
/// only), after checking the primary GPT header's signature and CRC and the
/// entry array's CRC. `read` reads one 512-byte sector by LBA. Two entries
/// with the name, or an entry past `capacity`, is an error: the caller must
/// not guess which one is meant.
pub fn gpt_find(
    mut read: impl FnMut(u32) -> Result<[u8; 512], ()>,
    capacity: u32,
    names: &[&[u8]],
    found: &mut [Option<(u32, u32)>],
) -> Result<(), Error> {
    fn word(b: &[u8], p: usize) -> u32 {
        u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
    }
    fn wide(b: &[u8], p: usize) -> u64 {
        u64::from(word(b, p)) | (u64::from(word(b, p + 4)) << 32)
    }
    let mut h = read(1).map_err(|_| Error::Io)?;
    let size = word(&h, 12) as usize;
    if &h[..8] != b"EFI PART" || !(92..=512).contains(&size) || wide(&h, 72) != 2
        || word(&h, 80) != 128 || word(&h, 84) != 128
    {
        return Err(Error::Magic);
    }
    let sum = word(&h, 16);
    h[16..20].fill(0);
    if crc32(&h[..size]) != sum {
        return Err(Error::Crc);
    }
    for f in found.iter_mut() {
        *f = None;
    }
    let mut crc = !0u32;
    for block in 0..32u32 {
        let s = read(2 + block).map_err(|_| Error::Io)?;
        for &b in s.iter() {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0u32.wrapping_sub(crc & 1) & 0xedb8_8320);
            }
        }
        for entry in s.chunks_exact(128) {
            if entry[..16] == [0; 16] {
                continue;
            }
            for (n, name) in names.iter().enumerate() {
                if name.is_empty() || name.len() > 36 {
                    return Err(Error::Slot);
                }
                let mut matches = true;
                for (k, &c) in name.iter().enumerate() {
                    matches &= entry[56 + 2 * k] == c && entry[57 + 2 * k] == 0;
                }
                matches &= entry[56 + 2 * name.len()..128].iter().all(|&b| b == 0);
                if !matches {
                    continue;
                }
                let (first, last) = (wide(entry, 32), wide(entry, 40));
                if found[n].is_some() || first > last || last >= u64::from(capacity) {
                    return Err(Error::Slot);
                }
                found[n] = Some((first as u32, last as u32));
            }
        }
    }
    if !crc != word(&h, 88) {
        return Err(Error::Crc);
    }
    Ok(())
}
