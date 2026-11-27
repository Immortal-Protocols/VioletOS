//! Stands in for the Storage service's disk driver (block.rs, or emmc.rs on
//! the handset) with the same calls, over the test disk.
use crate::blockdev::BlockDev;
use crate::journal::Sector;
use crate::testdisk::TestDisk;

pub struct Disk {
    dev: TestDisk,
    pub sectors: usize,
}
impl Disk {
    pub fn new(dev: TestDisk) -> Self {
        let sectors = dev.sectors() as usize;
        Disk { dev, sectors }
    }
    pub fn read(&mut self, index: usize) -> Result<Sector, ()> {
        let mut s = [0; 512];
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
}
