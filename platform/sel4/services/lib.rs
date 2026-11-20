#![cfg_attr(not(test), no_std)]
mod stopwatch;

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { core::hint::spin_loop() }
}

#[cfg(target_os="none")]
mod spi;
#[cfg(target_os="none")]
mod touch;

#[cfg(any(violet_qemu,test))]
mod journal;
#[cfg(violet_qemu)]
mod block;
#[cfg(violet_qemu)]
mod storage;
