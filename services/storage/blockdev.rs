//! The block device a filesystem sits on: 512-byte sectors, a volatile write
//! cache and a flush that empties it. Nothing here touches hardware, so the
//! filesystem above it builds and is tested on the host.
pub const SECTOR: usize = 512;

pub trait BlockDev {
    /// Size of the device in sectors.
    fn sectors(&self) -> u64;
    /// Read `buf.len() / 512` sectors starting at `lba`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()>;
    /// Write `buf.len() / 512` sectors starting at `lba`. The device may hold
    /// them in a volatile cache, in any order, until `flush` returns.
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()>;
    /// Make every earlier write durable.
    fn flush(&mut self) -> Result<(), ()>;
    /// Tell the device these sectors no longer hold data (eMMC TRIM). Optional.
    fn discard(&mut self, _lba: u64, _sectors: u64) -> Result<(), ()> {
        Ok(())
    }
}
