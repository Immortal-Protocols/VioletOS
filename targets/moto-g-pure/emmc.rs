//! MT6765 MSDC0. Reuse the bootloader's selected, tuned, sector-addressed eMMC
//! and execute one 512-byte programmed-I/O transfer at a time. No DMA or erase.
//!
//! Not done here, and what it costs the data volume (services/storage/
//! volume.rs): no multi-block transfers (CMD18/CMD25) and no DMA, so about
//! 100 KiB a second; no TRIM (`BlockDev::discard` is not passed on, so the
//! card's wear levelling is not told which blocks are free); no reliable
//! write and no health report (EXT_CSD life-time estimates); any error
//! retires the disk until the restart. Each of these needs the handset to
//! get right: a wrong erase range or transfer length destroys data that is
//! not VioletOS's, and none can be tried in QEMU, which has no MSDC.
use crate::journal::Sector;
#[path = "storage_volume.rs"]
mod volume;
#[allow(dead_code)]
#[path = "../../services/storage/extent.rs"]
pub mod extent;
use core::ptr::{read_volatile, write_volatile};
#[cfg(not(loader_trace))]
const BASE: usize = 0x0a000000;
#[cfg(loader_trace)]
const BASE: usize = 0x11230000;
const IRQ: usize = 0x0c;
const FIFO: usize = 0x14;
const READY: u32 = 1 << 8;
const COMPLETE: u32 = 1 << 12;
const ERRORS: u32 = (1 << 9) | (1 << 10) | (1 << 14) | (1 << 15);
const RCA: u32 = 1 << 16;
extern "C" {
    fn ticks() -> u32;
    static ticks_per_ms: u32;
}
fn reg(off: usize) -> u32 {
    unsafe { read_volatile((BASE + off) as *const u32) }
}
fn set(off: usize, value: u32) {
    unsafe {
        write_volatile((BASE + off) as *mut u32, value);
        core::arch::asm!("dmb sy", options(nostack));
    }
}
fn wait(ms: u32, mut done: impl FnMut() -> bool) -> Result<(), ()> {
    let start = unsafe { ticks() };
    while !done() {
        if unsafe { ticks().wrapping_sub(start) >= ms * ticks_per_ms } {
            return Err(());
        }
    }
    Ok(())
}
fn command(op: u32, arg: u32, flags: u32) -> Result<u32, ()> {
    wait(500, || reg(0x3c) & if op == 13 { 2 } else { 3 } == 0)?;
    set(IRQ, READY | COMPLETE | ERRORS);
    set(0x38, arg);
    set(0x34, op | flags);
    wait(500, || reg(IRQ) & (READY | ERRORS) != 0)?;
    if reg(IRQ) & ERRORS != 0 {
        return Err(());
    }
    let response = reg(0x40);
    if response & 0xfdf9_a080 != 0 {
        return Err(());
    }
    Ok(response)
}
fn status() -> Result<u32, ()> {
    command(13, RCA, 1 << 7)
}
fn ready() -> Result<(), ()> {
    let start = unsafe { ticks() };
    loop {
        let r = status()?;
        if (r >> 9) & 15 == 4 && r & (1 << 8) != 0 {
            return Ok(());
        }
        if unsafe { ticks().wrapping_sub(start) > 2000 * ticks_per_ms } {
            return Err(());
        }
    }
}
fn transfer(op: u32, addr: u32, sector: &mut Sector) -> Result<(), ()> {
    ready()?;
    set(FIFO, 1 << 31);
    wait(100, || reg(FIFO) & (1 << 31) == 0)?;
    set(0x50, 1);
    let write = op == 24;
    command(
        op,
        addr,
        (512 << 16) | (1 << 11) | (1 << 7) | ((write as u32) << 13),
    )?;
    // Drain/refill bursts so the FIFO can assert backpressure while the CPU runs.
    let mut pos = 0;
    let start = unsafe { ticks() };
    loop {
        let irq = reg(IRQ);
        if irq & ERRORS != 0 || unsafe { ticks().wrapping_sub(start) > 500 * ticks_per_ms } {
            return Err(());
        }
        let fifo = reg(FIFO);
        let words = if write {
            if fifo & 0xff0000 == 0 {
                32
            } else {
                0
            }
        } else {
            (fifo & 255) as usize / 4
        };
        for _ in 0..words.min((512 - pos) / 4) {
            if write {
                set(
                    0x18,
                    u32::from_le_bytes(sector[pos..pos + 4].try_into().unwrap()),
                );
            } else {
                sector[pos..pos + 4].copy_from_slice(&reg(0x1c).to_le_bytes());
            }
            pos += 4;
        }
        if pos == 512 && irq & COMPLETE != 0 {
            break;
        }
    }
    if write {
        ready()?;
    }
    Ok(())
}
fn read_card(op: u32, lba: u32) -> Result<Sector, ()> {
    let mut s = [0; 512];
    transfer(op, lba, &mut s)?;
    Ok(s)
}
pub struct Disk {
    pub sectors: usize,
    cached: bool,
    retired: bool,
    /// Android's userdata partition (first and last sector), from the live GPT.
    userdata: Option<(u32, u32)>,
    /// The installed apps' extent: the end of boot_b, or userdata once claimed.
    apps_first: u32,
    apps_count: usize,
    /// Where a data volume would go this boot (services/storage/extent.rs):
    /// sectors of the storage volume in boot_b, or of userdata (`raw`) once
    /// claimed and read. Storage holds it to the recorded place (mount.rs)
    /// and sets `data`, the extent served, which is nothing until then.
    offer: extent::Place,
    data: extent::Place,
    /// The slot geometry once looked up (slot_geometry), for `slot_ota` builds.
    #[cfg(slot_ota)]
    slots: Option<Option<(u32, [u32; 2], u32)>>,
}
impl Disk {
    pub fn open_checked() -> Result<Self, &'static [u8]> {
        if reg(0) & 0x81 != 0x81 || reg(0x9c) & 1 != 0 {
            return Err(b"EMMC HOST STATE");
        }
        set(0x10, 0);
        set(0, reg(0) | 8);
        let response = status().map_err(|_| b"EMMC CMD13" as &[u8])?;
        if (response >> 9) & 15 == 3 {
            command(7, RCA, 1 << 7).map_err(|_| b"EMMC SELECT" as &[u8])?;
        }
        let ext = read_card(8, 0).map_err(|_| b"EMMC EXT CSD" as &[u8])?;
        let capacity = u32::from_le_bytes(ext[212..216].try_into().unwrap());
        if capacity <= 4 * 1024 * 1024 || ext[179] & 7 != 0 || ext[61] != 0 || ext[15] & 1 != 0 {
            return Err(b"EMMC ACCESS MODE");
        }
        let userdata = volume::verify(|lba| read_card(17, lba), capacity)?;
        Ok(Self {
            sectors: volume::SECTORS,
            cached: ext[33] & 1 != 0,
            retired: false,
            userdata,
            apps_first: volume::APPS,
            apps_count: volume::APP_SECTORS,
            // Until userdata is claimed: the end of the storage volume that
            // the flat files never used (extent.rs).
            offer: extent::small(volume::SECTORS),
            data: extent::NOWHERE,
            #[cfg(slot_ota)]
            slots: None,
        })
    }
    fn finish<T>(&mut self, r: Result<T, ()>) -> Result<T, ()> {
        if r.is_err() {
            self.retired = true;
        }
        r
    }
    pub fn read(&mut self, i: usize) -> Result<Sector, ()> {
        if self.retired {
            return Err(());
        }
        let lba = volume::sector(i)?;
        self.finish(read_card(17, lba))
    }
    pub fn write(&mut self, i: usize, s: &mut Sector) -> Result<(), ()> {
        if self.retired {
            return Err(());
        }
        let lba = volume::sector(i)?;
        self.finish(transfer(24, lba, s))
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        if self.retired {
            return Err(());
        }
        if self.cached {
            // EXT_CSD FLUSH_CACHE, MMC SWITCH byte-write with R1b.
            let result = command(6, (3 << 24) | (32 << 16) | (1 << 8), 7 << 7);
            self.finish(result)?;
        }
        self.finish(ready())
    }
    pub fn commit(&mut self, i: usize, s: &mut Sector) -> Result<(), ()> {
        self.write(i, s)?;
        self.flush()
    }
    /// Use Android's userdata partition for the installed apps when VioletOS has
    /// claimed it (its tag in the first sector). The first time, the apps kept at
    /// the end of boot_b are copied over, then the move is recorded. Returns
    /// what happened, for the storage diagnostic.
    #[allow(dead_code)]
    pub fn adopt_userdata(&mut self) -> &'static [u8] {
        let Some((first, last)) = self.userdata else { return b"NO USERDATA" };
        let Ok(claim) = read_card(17, first) else { return b"USERDATA READ" };
        if &claim[..volume::USERDATA_TAG.len()] != volume::USERDATA_TAG || claim[volume::USERDATA_TAG.len()..].iter().any(|&b| b != 0) {
            return b"USERDATA NOT CLAIMED";
        }
        let start = first + volume::USERDATA_HEADER;
        let Ok(state) = read_card(17, first + 1) else { return b"USERDATA READ" };
        if &state[..volume::MOVED_TAG.len()] != volume::MOVED_TAG {
            // Copy the old extent whole (a partial copy is simply redone next boot).
            for i in 0..volume::APP_SECTORS as u32 {
                let Ok(mut s) = read_card(17, volume::APPS + i) else { return b"APPS MOVE READ" };
                if transfer(24, start + i, &mut s).is_err() {
                    return b"APPS MOVE WRITE";
                }
            }
            if self.flush().is_err() {
                return b"APPS MOVE FLUSH";
            }
            let mut moved = [0u8; 512];
            moved[..volume::MOVED_TAG.len()].copy_from_slice(volume::MOVED_TAG);
            if transfer(24, first + 1, &mut moved).is_err() || self.flush().is_err() {
                return b"APPS MOVE RECORD";
            }
        }
        self.apps_first = start;
        self.apps_count = (last + 1 - start) as usize;
        // The installed apps keep the first gibibyte, which is more than the
        // app store can ever reach; the data volume takes the next one
        // (extent.rs has the arithmetic and why).
        if let Some((apps, first, count)) = extent::userdata(start, last) {
            self.apps_count = apps as usize;
            self.offer = extent::Place { first, count, raw: true };
        }
        b"APPS IN USERDATA"
    }
    #[allow(dead_code)]
    pub fn data_offer(&self) -> extent::Place {
        self.offer
    }
    #[allow(dead_code)]
    pub fn use_data(&mut self, p: extent::Place) {
        self.data = p;
    }
    /// The data volume's extent, and nothing outside it.
    #[allow(dead_code)]
    pub fn data_sectors(&self) -> usize {
        self.data.count as usize
    }
    fn data_lba(&self, i: usize) -> Result<u32, ()> {
        if self.retired || i >= self.data.count as usize {
            return Err(());
        }
        if self.data.raw {
            Ok(self.data.first + i as u32)
        } else {
            volume::sector(self.data.first as usize + i)
        }
    }
    #[allow(dead_code)]
    pub fn data_read(&mut self, i: usize) -> Result<Sector, ()> {
        let lba = self.data_lba(i)?;
        self.finish(read_card(17, lba))
    }
    #[allow(dead_code)]
    pub fn data_write(&mut self, i: usize, s: &mut Sector) -> Result<(), ()> {
        let lba = self.data_lba(i)?;
        self.finish(transfer(24, lba, s))
    }
    /// The installed apps' extent: the end of boot_b, or userdata once claimed.
    #[allow(dead_code)]
    pub fn app_sectors(&self) -> usize {
        self.apps_count
    }
    fn app_lba(&self, i: usize) -> Result<u32, ()> {
        if i >= self.apps_count {
            return Err(());
        }
        Ok(self.apps_first + i as u32)
    }
    #[allow(dead_code)]
    pub fn app_read(&mut self, i: usize) -> Result<Sector, ()> {
        if self.retired {
            return Err(());
        }
        let lba = self.app_lba(i)?;
        self.finish(read_card(17, lba))
    }
    #[allow(dead_code)]
    pub fn app_write(&mut self, i: usize, s: &mut Sector) -> Result<(), ()> {
        if self.retired {
            return Err(());
        }
        let lba = self.app_lba(i)?;
        self.finish(transfer(24, lba, s))
    }
}

// Full-system updates into boot_a / boot_b and the A/B metadata in misc
// (services/storage/slotwrite.rs, slot.rs). NOT ENABLED: no build sets
// `slot_ota`, because what Motorola's bootloader does with the record has
// not been seen on a handset and the running slot is not passed by the
// loader yet (slot.rs lists what is unknown). When enabled, every address
// comes from the live CRC-checked GPT and only these sectors can be reached:
// misc + 4 and the first 16 MiB of boot_a and boot_b (in boot_b that ends
// exactly at the storage marker). Nothing is offered while Android may still
// be on slot A: only once `moto-flash --take-userdata` has claimed userdata.
#[cfg(slot_ota)]
#[path = "slot.rs"]
mod slot;
#[cfg(slot_ota)]
impl Disk {
    pub const SLOT_SECTORS: u32 = 16 * 1024 * 1024 / 512;
    /// (bootloader_control sector, first sector of boot_a and boot_b, sectors per slot).
    pub fn slot_geometry(&mut self) -> Option<(u32, [u32; 2], u32)> {
        if self.retired || self.apps_first == volume::APPS {
            return None;
        }
        if let Some(known) = self.slots {
            return known;
        }
        let found = self.find_slots();
        self.slots = Some(found);
        found
    }
    fn find_slots(&mut self) -> Option<(u32, [u32; 2], u32)> {
        let ext = read_card(8, 0).ok()?;
        let capacity = u32::from_le_bytes(ext[212..216].try_into().ok()?);
        let mut found = [None; 3];
        slot::gpt_find(|lba| read_card(17, lba), capacity, &[b"misc", b"boot_a", b"boot_b"], &mut found).ok()?;
        let (misc, a, b) = (found[0]?, found[1]?, found[2]?);
        if b.0 != volume::BOOT_B || volume::MARKER != b.0 + Self::SLOT_SECTORS
            || a.1 - a.0 + 1 < Self::SLOT_SECTORS || misc.1 - misc.0 < slot::MISC_SECTOR
        {
            return None;
        }
        Some((misc.0 + slot::MISC_SECTOR, [a.0, b.0], Self::SLOT_SECTORS))
    }
    fn slot_lba(&mut self, lba: u32) -> Result<u32, ()> {
        let (control, first, sectors) = self.slot_geometry().ok_or(())?;
        if lba == control || first.iter().any(|&f| (f..f + sectors).contains(&lba)) {
            Ok(lba)
        } else {
            Err(())
        }
    }
    pub fn slot_read(&mut self, lba: u32) -> Result<Sector, ()> {
        let at = self.slot_lba(lba)?;
        self.finish(read_card(17, at))
    }
    pub fn slot_write(&mut self, lba: u32, s: &mut Sector) -> Result<(), ()> {
        let at = self.slot_lba(lba)?;
        self.finish(transfer(24, at, s))
    }
}

// These two records sit beyond the filesystem and require their own provisioned
// marker. Only loader/supervisor code is built with this interface.
#[cfg(boot_trace)]
impl Disk {
    pub fn boot_record(&mut self, stage: u8) -> Result<u8, ()> {
        let header = volume::MARKER + volume::SECTORS as u32 + 1;
        boot_history::update(stage, |i| read_card(17, header + i), |slot, bytes| {
            transfer(24, header + slot, bytes)?;
            self.flush()
        })
    }
}

#[cfg(boot_trace)]
#[path = "boot_history.rs"]
mod boot_history;

// Package catalogs occupy their own marked extent after the history sectors.
// No package-sector index can address stopwatch files, history or another slot.
#[cfg(package_catalog)]
pub struct PackageDisk { disk: Disk, pub sectors: usize }
#[cfg(package_catalog)]
impl PackageDisk {
    const HEADER: u32 = volume::MARKER + volume::SECTORS as u32 + 4;
    /// Two banks of system/package-manager/catalog_volume.rs BANK (1 + 2560) sectors.
    const COUNT: usize = 2 * (1 + 0x140000 / 512);
    pub fn open() -> Option<Self> {
        let disk = Disk::open_checked().ok()?;
        let h = read_card(17, Self::HEADER).ok()?;
        let tag = b"VioletOS catalog banks v1\n";
        if &h[..tag.len()] != tag || h[tag.len()..].iter().any(|&v|v!=0) { return None; }
        Some(Self {disk, sectors: Self::COUNT})
    }
    fn lba(i: usize) -> Result<u32, ()> {
        if i >= Self::COUNT { return Err(()); }
        Ok(Self::HEADER + 1 + i as u32)
    }
    pub fn read(&mut self,i:usize)->Result<Sector,()> {read_card(17,Self::lba(i)?)}
    pub fn write(&mut self,i:usize,b:&mut Sector)->Result<(),()> {transfer(24,Self::lba(i)?,b)}
    pub fn flush(&mut self)->Result<(),()> {self.disk.flush()}
    pub fn commit(&mut self,i:usize,b:&mut Sector)->Result<(),()> {self.write(i,b)?;self.flush()}
}
