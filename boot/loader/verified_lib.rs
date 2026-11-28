//! The loader's verified-boot object: verified.rs with the keys this image
//! pins (image/packages/sign.py) and BearSSL's SHA-256 and P-256
//! (system/package-manager/verify_crypto.c). Built by
//! platform/sel4/loader/build.py; called once from loader.c.
#![no_std]
#[path = "verified_entry.rs"]
mod verified_entry;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Nothing boots after a fault in the check.
    loop {}
}
