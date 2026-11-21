//! Single-owner boot logging. The loader hands the controller to the supervisor,
//! which finishes its writes before resuming the storage process.
#![no_std]
#![allow(dead_code)]
#[path = "../../services/storage/journal.rs"]
mod journal;
mod emmc;
#[cfg(package_catalog)]
use emmc::PackageDisk as Disk;
#[cfg(package_catalog)]
#[path = "../../system/package-manager/catalog_volume.rs"]
mod catalog_volume;

#[no_mangle]
pub extern "C" fn record_boot_stage(stage: u8) -> u32 {
    match emmc::Disk::open_checked().ok().and_then(|mut d| d.boot_record(stage).ok()) {
        Some(previous) => previous.into(),
        None => u32::MAX,
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { core::arch::asm!("udf #1", options(noreturn)) }
}
