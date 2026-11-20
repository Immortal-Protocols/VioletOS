#![no_std]
#![no_main]

#[cfg(network)]
use core::arch::asm;
use core::ffi::CStr;
use core::panic::PanicInfo;

const NOTE: usize = 0x4000_0000;
const MARKER: u32 = 0x5649_4f4c;
extern "C" {
    fn microkit_dbg_puts(message: *const core::ffi::c_char);
}

fn log(message: &CStr) {
    unsafe { microkit_dbg_puts(message.as_ptr()) }
}

#[no_mangle]
pub extern "C" fn init() {
    #[cfg(notes)]
    {
        // This address is the designated writable note frame in both systems.
        unsafe { (NOTE as *mut u32).write_volatile(MARKER) };
        log(c"NOTES: initialized synthetic note\n");
    }
    #[cfg(network)]
    {
        let packet = 0x4100_0000 as *mut u32;
        unsafe { packet.write_volatile(42) };
        if unsafe { packet.read_volatile() } != 42 {
            log(c"NETWORK: packet check failed\n");
            return;
        }
        log(c"NETWORK: packet OK; testing note mapping\n");
        let value: u32;
        // An explicit machine load gives the CPU a well-defined faulting instruction
        // in the unmapped case, without creating an invalid Rust reference.
        unsafe { asm!("ldr {value:w}, [{addr}]", addr = in(reg) NOTE,
                      value = out(reg) value, options(nostack, readonly)) };
        if value == MARKER {
            log(c"NETWORK: observed synthetic note marker\n");
        } else {
            log(c"NETWORK: unexpected value\n");
        }
    }
}

#[no_mangle]
pub extern "C" fn notified(_: u32) {}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop { core::hint::spin_loop(); }
}
