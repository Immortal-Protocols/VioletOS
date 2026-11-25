//! Modern virtio-mmio, queue zero, one outstanding request, eight slots.
//! Bootstrap maps one dedicated uncached DMA page and the device into this task.
//! Layout within the page: descriptors at 0, available at 128, used at 160,
//! request header at 256, status at 272 and one sector at 512.
use crate::journal::Sector;
use core::ptr::{read_volatile, write_volatile};
#[cfg(package_volume)]
const MMIO: usize = 0x0a003000;
#[cfg(not(package_volume))]
const MMIO: usize = 0x0a000000;
#[cfg(package_volume)]
const DMA: usize = 0x0b003000;
#[cfg(not(package_volume))]
const DMA: usize = 0x0b000000;
#[cfg(package_volume)]
const PHYS: u32 = 0x49203000;
#[cfg(not(package_volume))]
const PHYS: u32 = 0x49200000;
fn reg(offset: usize) -> u32 {
    unsafe { read_volatile((MMIO + offset) as *const u32) }
}
fn set(offset: usize, v: u32) {
    unsafe { write_volatile((MMIO + offset) as *mut u32, v) }
}
fn fence() {
    unsafe { core::arch::asm!("dmb sy", options(nostack, preserves_flags)) }
}
extern "C" {
    fn ticks() -> u32;
    static ticks_per_ms: u32;
}

pub struct Disk {
    index: u16,
    retired: bool,
    pub sectors: usize,
}
impl Disk {
    #[cfg(not(package_volume))]
    pub fn open_checked() -> Result<Self, &'static [u8]> {
        Self::open().ok_or(b"")
    }
    pub fn open() -> Option<Self> {
        if reg(0) != 0x74726976 || reg(4) != 2 || reg(8) != 2 {
            return None;
        }
        set(0x70, 0);
        if reg(0x70) != 0 {
            return None;
        }
        set(0x70, 1);
        set(0x70, 3);
        // Require VERSION_1, FLUSH and a writable device.
        set(0x14, 1);
        if reg(0x10) & 1 == 0 {
            return None;
        }
        set(0x14, 0);
        let features = reg(0x10);
        if features & (1 << 9) == 0 || features & (1 << 5) != 0 {
            return None;
        }
        set(0x24, 1);
        set(0x20, 1);
        set(0x24, 0);
        set(0x20, 1 << 9);
        set(0x70, 11);
        if reg(0x70) & 8 == 0 || (reg(0x104) == 0 && reg(0x100) < 2) {
            return None;
        }
        set(0x30, 0);
        if reg(0x34) < 8 || reg(0x44) != 0 {
            return None;
        }
        // At boot the queue is idle, so the service owns the full DMA page.
        unsafe {
            core::ptr::write_bytes(DMA as *mut u8, 0, 4096);
        }
        set(0x38, 8);
        for (register, offset) in [(0x80, 0), (0x90, 128), (0xa0, 160)] {
            set(register, PHYS + offset);
            set(register + 4, 0);
        }
        fence();
        set(0x44, 1);
        set(0x70, 15);
        Some(Self {
            index: 0,
            retired: false,
            sectors: if reg(0x104) != 0 {
                usize::MAX
            } else {
                reg(0x100) as usize
            },
        })
    }
    fn descriptor(slot: usize, offset: u32, len: u32, flags: u16, next: u16) {
        let p = (DMA + 16 * slot) as *mut u32;
        unsafe {
            write_volatile(p, PHYS + offset);
            write_volatile(p.add(1), 0);
            write_volatile(p.add(2), len);
            write_volatile(p.add(3), u32::from(flags) | (u32::from(next) << 16));
        }
    }
    fn request(&mut self, kind: u32, sector: usize, data: &mut Sector) -> Result<(), ()> {
        // Bound the queue to the app journals.
        if self.retired || sector >= self.sectors {
            return Err(());
        }
        let flush = kind == 4;
        unsafe {
            let header = (DMA + 256) as *mut u32;
            write_volatile(header, kind);
            write_volatile(header.add(1), 0);
            write_volatile(header.add(2), sector as u32);
            write_volatile(header.add(3), 0);
            write_volatile((DMA + 272) as *mut u8, 0xff);
            if kind == 1 {
                core::ptr::copy_nonoverlapping(data.as_ptr(), (DMA + 512) as *mut u8, 512);
            }
        }
        Self::descriptor(0, 256, 16, 1, if flush { 2 } else { 1 });
        Self::descriptor(1, 512, 512, if kind == 0 { 3 } else { 1 }, 2);
        Self::descriptor(2, 272, 1, 2, 0);
        let slot = usize::from(self.index % 8);
        unsafe {
            write_volatile((DMA + 132 + slot * 2) as *mut u16, 0);
        }
        fence();
        self.index = self.index.wrapping_add(1);
        unsafe {
            write_volatile((DMA + 130) as *mut u16, self.index);
        }
        fence();
        set(0x50, 0);
        let start = unsafe { ticks() };
        while unsafe { read_volatile((DMA + 162) as *const u16) } != self.index {
            if unsafe { ticks() }.wrapping_sub(start) > unsafe { ticks_per_ms } * 500 {
                self.retired = true;
                return Err(());
            }
        }
        fence();
        if unsafe { read_volatile((DMA + 164 + slot * 8) as *const u32) } != 0
            || unsafe { read_volatile((DMA + 272) as *const u8) } != 0
        {
            return Err(());
        }
        if kind == 0 {
            unsafe {
                core::ptr::copy_nonoverlapping((DMA + 512) as *const u8, data.as_mut_ptr(), 512);
            }
        }
        set(0x64, reg(0x60));
        Ok(())
    }
    pub fn read(&mut self, index: usize) -> Result<Sector, ()> {
        let mut bytes = [0; 512];
        self.request(0, index, &mut bytes)?;
        Ok(bytes)
    }
    pub fn write(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        self.request(1, index, bytes)
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        self.request(4, 0, &mut [0; 512])
    }
    pub fn commit(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        self.request(1, index, bytes)?;
        self.request(4, 0, bytes)
    }
    /// The installed apps' extent: sectors 4096 onward of a disk large enough
    /// (tools/run.py makes it 16 MiB), up to 12 MiB.
    #[allow(dead_code)]
    pub fn app_sectors(&self) -> usize {
        self.sectors.saturating_sub(APPS).min(APP_SECTORS)
    }
    #[allow(dead_code)]
    pub fn app_read(&mut self, index: usize) -> Result<Sector, ()> {
        if index >= self.app_sectors() {
            return Err(());
        }
        self.read(APPS + index)
    }
    #[allow(dead_code)]
    pub fn app_write(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        if index >= self.app_sectors() {
            return Err(());
        }
        self.write(APPS + index, bytes)
    }
}
#[allow(dead_code)]
const APPS: usize = 4096;
#[allow(dead_code)]
const APP_SECTORS: usize = 24576;
