// QEMU battery fixture: charge is host supplied, display sleep is guest controlled.
use crate::protocol::*;
pub fn run() -> ! {
    let regs = 0x09000000 as *mut u32;
    serve(|m| {
        if m.key != POWER {
            return Message::error(INVALID);
        }
        unsafe {
            match m.op {
                GET => {
                    let mut r = Message::result(regs.read_volatile().min(100));
                    r.arg = regs.add(1).read_volatile() & 1;
                    r
                }
                EXECUTE if m.value <= 2 => {
                    regs.add(1).write_volatile(m.value);
                    Message::result(m.value)
                }
                _ => Message::error(INVALID),
            }
        }
    })
}
