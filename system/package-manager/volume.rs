//! Boot-accessible package catalogs on the dedicated QEMU virtio volume.
//! Each bank has a transaction descriptor followed by up to one MiB of data.
//! The prior bank stays readable until the new bytes and descriptor are durable.
#![no_std]
#[path = "../../services/storage/block.rs"]
mod block;
#[allow(dead_code)]
#[path = "../../services/storage/journal.rs"]
mod journal;
use block::Disk;
mod catalog_volume;
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
