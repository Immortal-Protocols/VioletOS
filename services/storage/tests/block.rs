//! Stands in for the Storage service's disk driver (block.rs, or emmc.rs on
//! the handset) with the same calls, over the test disk.
use crate::blockdev::BlockDev;
use crate::journal::Sector;
use crate::testdisk::TestDisk;
pub(crate) use crate::extent;
use extent::Place;

pub struct Disk {
    dev: TestDisk,
    pub sectors: usize,
    /// What the driver offers for the data volume this boot: block.rs's
    /// (extent.rs `qemu`) unless a test plays the handset's userdata.
    pub offer: Place,
    data: Place,
    /// Sectors read so far.
    pub reads: usize,
}
impl Disk {
    pub fn new(dev: TestDisk) -> Self {
        let sectors = dev.sectors() as usize;
        let (first, count) = extent::qemu(sectors);
        Disk { dev, sectors, offer: Place { first: first as u32, count: count as u32, raw: false }, data: extent::NOWHERE, reads: 0 }
    }
    pub fn read(&mut self, index: usize) -> Result<Sector, ()> {
        let mut s = [0; 512];
        self.reads += 1;
        self.dev.read(index as u64, &mut s)?;
        Ok(s)
    }
    pub fn write(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        self.dev.write(index as u64, bytes)
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        self.dev.flush()
    }
    pub fn commit(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        self.write(index, bytes)?;
        self.flush()
    }
    // The data extent, as block.rs has it.
    pub fn data_offer(&self) -> Place {
        self.offer
    }
    pub fn use_data(&mut self, p: Place) {
        self.data = p;
    }
    pub fn data_sectors(&self) -> usize {
        self.data.count as usize
    }
    pub fn data_read(&mut self, index: usize) -> Result<Sector, ()> {
        if index >= self.data_sectors() {
            return Err(());
        }
        self.read(self.data.first as usize + index)
    }
    pub fn data_write(&mut self, index: usize, bytes: &mut Sector) -> Result<(), ()> {
        if index >= self.data_sectors() {
            return Err(());
        }
        self.write(self.data.first as usize + index, bytes)
    }
}
