//! The package catalog banks (system/package-manager/catalog_volume.rs): the
//! code Storage commits an installed catalog with and the loader reads it back
//! with at the next boot, over a disk in memory.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

mod bank {
    include!("../../../system/package-manager/catalog_volume.rs");
    pub unsafe fn load(ptr: *mut u8) -> u32 {
        volume_load(ptr)
    }
}
use bank::{BANK, LIMIT};

/// The banks' sectors. One test at a time uses them (SERIAL).
static SECTORS: Mutex<Vec<[u8; 512]>> = Mutex::new(Vec::new());
static SERIAL: Mutex<()> = Mutex::new(());
/// When the data sector TEAR_AT (counted from 1) of a commit has been written,
/// flip the byte at TEAR_BYTE: memory that changes under the writer, as the
/// composed catalog did on the handset when Storage read it uncached and the
/// supervisor's dirty cache lines reached RAM during the commit.
static TEAR_AT: AtomicUsize = AtomicUsize::new(0);
static TEAR_BYTE: AtomicUsize = AtomicUsize::new(0);

pub struct Disk {
    pub sectors: usize,
}
impl Disk {
    pub fn open() -> Option<Self> {
        Some(Disk { sectors: SECTORS.lock().unwrap().len() })
    }
    pub fn read(&mut self, i: usize) -> Result<[u8; 512], ()> {
        SECTORS.lock().unwrap().get(i).copied().ok_or(())
    }
    pub fn write(&mut self, i: usize, b: &mut [u8; 512]) -> Result<(), ()> {
        *SECTORS.lock().unwrap().get_mut(i).ok_or(())? = *b;
        let tear = TEAR_AT.load(Ordering::SeqCst);
        if tear != 0 && i % BANK == tear {
            TEAR_AT.store(0, Ordering::SeqCst);
            unsafe { *(TEAR_BYTE.load(Ordering::SeqCst) as *mut u8) ^= 0xff };
        }
        Ok(())
    }
    pub fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }
    pub fn commit(&mut self, i: usize, b: &mut [u8; 512]) -> Result<(), ()> {
        self.write(i, b)
    }
}

fn fresh_disk() -> std::sync::MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *SECTORS.lock().unwrap() = vec![[0; 512]; 2 * BANK];
    TEAR_AT.store(0, Ordering::SeqCst);
    guard
}

/// A catalog-sized record, larger than one MiB's worth of sectors on the handset.
fn catalog(seed: u8, length: usize) -> Vec<u8> {
    (0..length).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect()
}

fn load() -> (u32, Vec<u8>) {
    let mut out = vec![0u8; LIMIT];
    let bank = unsafe { bank::load(out.as_mut_ptr()) };
    (bank, out)
}

#[test]
fn installed_catalog_is_found_at_the_next_boot() {
    let _disk = fresh_disk();
    let length = LIMIT - 4096;
    let first = catalog(1, length);
    assert_eq!(unsafe { bank::commit_bank(first.as_ptr(), length, 0) }, 2);
    let (bank, bytes) = load();
    assert_eq!(bank, 2);
    assert_eq!(&bytes[..length], &first[..]);
    // The next install goes to the other bank, and wins.
    let second = catalog(2, length - 512 * 7 - 100);
    assert_eq!(unsafe { bank::commit_bank(second.as_ptr(), second.len(), 2) }, 1);
    let (bank, bytes) = load();
    assert_eq!(bank, 1);
    assert_eq!(&bytes[..second.len()], &second[..]);
}

#[test]
fn catalog_that_changes_while_written_is_not_committed() {
    let _disk = fresh_disk();
    let length = LIMIT - 4096;
    let old = catalog(3, length);
    assert_eq!(unsafe { bank::commit_bank(old.as_ptr(), length, 0) }, 2);
    // The first sector reaches the disk; then its source changes.
    let mut new = catalog(4, length);
    TEAR_AT.store(1, Ordering::SeqCst);
    TEAR_BYTE.store(new.as_mut_ptr() as usize + 10, Ordering::SeqCst);
    assert_eq!(unsafe { bank::commit_bank(new.as_ptr(), length, 2) }, 0,
               "a bank whose sectors and checksum disagree must not be committed");
    // The next boot still finds the catalog committed before.
    let (bank, bytes) = load();
    assert_eq!(bank, 2);
    assert_eq!(&bytes[..length], &old[..]);
}
