//! Encryption at rest: a block device that encrypts every sector of the
//! device under it with XTS-AES-256, and the key block that holds its key.
//! Like VioletFS above it, nothing here touches hardware or IPC, so the same
//! code runs in Storage and in the host tests (tests/host.rs, `make
//! test-unit`). docs/encryption-at-rest.md has the whole picture.
//!
//!   let root = unsafe { crypt::provision::from_page(crypt::provision::QEMU_PAGE) }?;
//!   let key = crypt::unlock_or_format(&mut dev, &root, pin, &mut throttle, now, Some(&mut random48))?;
//!   let fs = Fs::new(crypt::open(dev, key.expose()));      // /data
//!   let apps = crypt::open_extent(extent, key.expose(), crypt::APPS)?;
//!
//! Keys are zeroed when dropped. No key, and nothing computed from one, is
//! ever formatted or logged: the secret types have no `Debug`, and `Error`
//! carries only counters.
use crate::blockdev::{BlockDev, SECTOR};

mod aes;
#[cfg(test)]
mod kat;
mod keys;
pub mod provision;
mod sha;
mod xts;

#[allow(unused_imports)]
pub use keys::{backoff_secs, format, params, set_pin, unlock, unlock_or_format, Error, NewPin, Params, PinSecret, RootKey, Secret, Throttle, VolumeKey, APPS, DATA, HEADER_SECTORS};
#[allow(unused_imports)]
pub use sha::wipe;

/// Sectors encrypted per call to the device below.
const BATCH: usize = 8;

/// The device above sees plaintext sectors `0..sectors()`; the device below
/// holds their ciphertext from sector `base` on. A sector's number above is
/// its XTS tweak, so moving ciphertext to another sector decrypts to noise.
pub struct CryptDev<B: BlockDev> {
    dev: B,
    xts: xts::Xts,
    base: u64,
    buf: [u8; BATCH * SECTOR],
}

/// The name the volume code uses for the device.
pub type Dev<B> = CryptDev<B>;

/// The encrypted volume that holds the key block: everything after the
/// header (the first `HEADER_SECTORS` of `dev`), under the volume's `DATA`
/// key. `volume_key` is what `unlock` or `format` returned (`.expose()`).
pub fn open<B: BlockDev>(dev: B, volume_key: &[u8; 32]) -> CryptDev<B> {
    CryptDev::new(dev, volume_key, DATA, HEADER_SECTORS)
}

/// Another device (or extent) encrypted under the same volume key, with no
/// header of its own, under that region's key. `None` for `DATA` (the
/// volume itself, `open`) and any purpose not listed in `EXTENTS`, so two
/// regions can never share a key and tweaks. Confidentiality only: XTS does
/// not stop an old ciphertext of the extent being put back (an installed
/// app rolled back to an older version); see docs/encryption-at-rest.md.
pub fn open_extent<B: BlockDev>(dev: B, volume_key: &[u8; 32], purpose: u8) -> Option<CryptDev<B>> {
    EXTENTS.contains(&purpose).then(|| CryptDev::new(dev, volume_key, purpose, 0))
}
/// The purposes an extent may have. A new region gets a new number here.
pub const EXTENTS: [u8; 1] = [APPS];

impl<B: BlockDev> CryptDev<B> {
    fn new(dev: B, key: &[u8; 32], purpose: u8, base: u64) -> Self {
        CryptDev { dev, xts: xts::Xts::new(keys::cipher_key(key, purpose).expose()), base, buf: [0; BATCH * SECTOR] }
    }
    /// The device below, for the key block (`params`, `set_pin`).
    pub fn inner(&mut self) -> &mut B {
        &mut self.dev
    }
    /// The first sector below for `len` bytes at `lba`, if all of it exists.
    fn span(&self, lba: u64, len: usize) -> Result<u64, ()> {
        let end = lba.checked_add((len / SECTOR) as u64).ok_or(())?;
        if len % SECTOR != 0 || end > self.sectors() {
            return Err(());
        }
        Ok(lba + self.base)
    }
}

impl<B: BlockDev> BlockDev for CryptDev<B> {
    fn sectors(&self) -> u64 {
        self.dev.sectors().saturating_sub(self.base)
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        let below = self.span(lba, buf.len())?;
        if let Err(()) = self.dev.read(below, buf) {
            // Never hand ciphertext, or half of each, to the caller.
            buf.fill(0);
            return Err(());
        }
        for (i, sector) in buf.chunks_exact_mut(SECTOR).enumerate() {
            self.xts.sector(lba + i as u64, sector, true);
        }
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        let below = self.span(lba, buf.len())?;
        let mut done = 0u64;
        for chunk in buf.chunks(BATCH * SECTOR) {
            let out = &mut self.buf[..chunk.len()];
            out.copy_from_slice(chunk);
            for (i, sector) in out.chunks_exact_mut(SECTOR).enumerate() {
                self.xts.sector(lba + done + i as u64, sector, false);
            }
            self.dev.write(below + done, out)?;
            done += (chunk.len() / SECTOR) as u64;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ()> {
        self.dev.flush()
    }
    fn discard(&mut self, lba: u64, sectors: u64) -> Result<(), ()> {
        let end = lba.checked_add(sectors).ok_or(())?;
        if end > self.sectors() {
            return Err(());
        }
        self.dev.discard(lba + self.base, sectors)
    }
}
