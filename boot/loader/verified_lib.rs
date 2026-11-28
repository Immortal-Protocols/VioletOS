//! The loader's verified-boot object: verified.rs with the keys this image
//! pins (image/packages/sign.py) and BearSSL's SHA-256 and P-256
//! (system/package-manager/verify_crypto.c). Built by
//! platform/sel4/loader/build.py; called once from loader.c.
#![no_std]
include!(env!("VIOLET_PACKAGE_KEYS_RS"));
#[path = "verified.rs"]
mod verified;

extern "C" {
    fn package_sha256(data: *const u8, len: usize, out: *mut u8);
    fn package_p256_verify(key: *const u8, hash: *const u8, signature: *const u8) -> i32;
}
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    unsafe { package_sha256(data.as_ptr(), data.len(), out.as_mut_ptr()) };
    out
}
fn p256(key: &[u8; 65], hash: &[u8; 32], signature: &[u8; 64]) -> bool {
    unsafe { package_p256_verify(key.as_ptr(), hash.as_ptr(), signature.as_ptr()) == 1 }
}

/// What the loader passes: each image where it lies in memory.
#[repr(C)]
pub struct Images {
    manifest: *const u8,
    manifest_len: usize,
    kernel: *const u8,
    kernel_len: usize,
    root: *const u8,
    root_len: usize,
    apps: *const u8,
    apps_room: usize,
    /// The bootloader's rollback floor record (verified.rs handoff_floor).
    floor: [u32; 3],
}

/// 0 and the verified system version in `*version` when the images may be
/// started; otherwise a verified::Refusal number, and nothing may start.
///
/// # Safety
/// Every pointer must be readable for its length; `version` writable.
#[no_mangle]
pub unsafe extern "C" fn verified_boot(images: *const Images, version: *mut u32) -> u32 {
    let i = &*images;
    let slice = |p: *const u8, n: usize| core::slice::from_raw_parts(p, n);
    let verdict = verified::handoff_floor(i.floor).and_then(|floor| {
        verified::check(
            slice(i.manifest, i.manifest_len),
            TARGET,
            KEYS,
            floor,
            // A release loader starts no /apps image the release root did not sign.
            RELEASE,
            slice(i.kernel, i.kernel_len),
            slice(i.root, i.root_len),
            slice(i.apps, i.apps_room),
            sha256,
            p256,
        )
    });
    match verdict {
        Ok(v) => {
            *version = v.version;
            0
        }
        Err(why) => why as u32,
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Nothing boots after a fault in the check.
    loop {}
}
