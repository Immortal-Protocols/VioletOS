// QEMU battery fixture: charge and the power key are host supplied, display sleep is guest controlled.
use crate::protocol::*;
pub fn read() -> Message {
    let regs = 0x09000000 as *const u32;
    unsafe {
        let mut r = Message::result(regs.read_volatile().min(100));
        r.arg = regs.add(1).read_volatile() & 1;
        r
    }
}
/// The host's power key: word 2 is set while it is held, word 3 counts presses.
pub fn key() -> Message {
    let regs = 0x09000000 as *const u32;
    unsafe {
        let mut r = Message::result(regs.add(2).read_volatile() & 1);
        r.arg = regs.add(3).read_volatile();
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
