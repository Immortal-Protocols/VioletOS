// QEMU battery fixture: charge is host supplied, display sleep is guest controlled.
use crate::protocol::*;
pub fn read() -> Message {
    let regs = 0x09000000 as *const u32;
    unsafe {
        let mut r = Message::result(regs.read_volatile().min(100));
        r.arg = regs.add(1).read_volatile() & 1;
        r
    }
}
pub fn command(value: u32) -> Message {
    unsafe { (0x09000004 as *mut u32).write_volatile(value) };
    Message::result(value)
}

pub fn radio_rails(_: bool) -> Message {
    Message::error(INVALID)
}
