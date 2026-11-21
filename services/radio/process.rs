use crate::protocol::*;
#[path = "gateway.rs"]
mod gateway;
pub fn run() -> ! {
    let regs = 0x09000000 as *mut u32;
    let mut associated = false;
    serve(|m| {
        let online = unsafe { regs.read_volatile() & 1 != 0 && regs.add(1).read_volatile() == 0 };
        if !online {
            associated = false;
        }
        match (m.op, m.key) {
            (GET, RADIO) => Message::result(online as u32),
            (EXECUTE, RADIO) if m.value <= 1 => {
                unsafe {
                    regs.add(1).write_volatile(1 - m.value);
                }
                if m.value == 0 {
                    associated = false;
                }
                Message::result(m.value)
            }
            (GET, NETWORK) => {
                let mut r = Message::result(associated as u32);
                r.arg = online as u32;
                r
            }
            (EXECUTE, NETWORK) if online && m.value <= 1 => {
                associated = m.value == 1;
                Message::result(m.value)
            }
            (EXECUTE, RADIO_TEST) if online && associated => {
                if !gateway::probe() {
                    Message::result(0)
                } else if let Some(uuid) = gateway::endpoint_uuid() {
                    let mut response = Message::result(1);
                    response.put_text(&uuid);
                    response
                } else {
                    Message::result(2)
                }
            }
            _ => Message::error(OFFLINE),
        }
    })
}
