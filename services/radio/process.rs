// QEMU modem fixture: host supplies carrier availability, guest controls airplane mode.
use crate::protocol::*;
pub fn run() -> ! {
    let regs = 0x09000000 as *mut u32;
    serve(|m| {
        if m.key != RADIO {
            return Message::error(INVALID);
        }
        unsafe {
            match m.op {
                GET => Message::result(
                    (regs.read_volatile() & 1) & (1 - regs.add(1).read_volatile().min(1)),
                ),
                EXECUTE if m.value <= 1 => {
                    regs.add(1).write_volatile(1 - m.value);
                    Message::result(m.value)
                }
                _ => Message::error(INVALID),
            }
        }
    })
}
