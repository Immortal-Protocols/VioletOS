#![cfg_attr(not(test), no_std)]

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { core::hint::spin_loop() }
}

#[cfg(target_os="none")]
#[path="../../../services/touch/spi.rs"]
mod spi;
#[cfg(target_os="none")]
#[path="../../../services/touch/touch.rs"]
mod touch;

#[cfg(any(violet_qemu,test))]
#[path="../../../services/storage/journal.rs"]
mod journal;
#[cfg(violet_qemu)]
#[path="../../../services/storage/block.rs"]
mod block;
#[cfg(violet_qemu)]
#[path="../../../services/storage/storage.rs"]
mod storage;
