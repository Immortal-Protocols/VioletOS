//! MT6765 MSDC0. Reuse the bootloader's selected, tuned, sector-addressed eMMC
//! and execute one 512-byte programmed-I/O transfer at a time. No DMA or erase.
use crate::journal::Sector;
#[path = "storage_volume.rs"]
mod volume;
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
        volume::verify(|lba| read_card(17, lba), capacity)?;
        Ok(Self {
            sectors: volume::SECTORS,
            cached: ext[33] & 1 != 0,
            retired: false,
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
    const COUNT: usize = 4098;
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
