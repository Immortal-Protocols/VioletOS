use crate::protocol::*;
#[path = "gateway.rs"]
mod gateway;
/// A virtual network's WPA2 key: after the eight 20-byte records, eight
/// 36-byte entries of a flags word (bit 0: WPA2, bit 1: its handshake times
/// out) and the PMK (tools/run.py).
fn stalls(i: u32) -> bool {
    i < 8 && unsafe { ((0x09000200 + i * 36) as *const u32).read_volatile() } & 2 != 0
}
fn security(i: u32) -> Option<[u8; 32]> {
    let base = (0x09000200 + i * 36) as *const u32;
    if i >= 8 || unsafe { base.read_volatile() } & 1 == 0 {
        return None;
    }
    let mut pmk = [0u8; 32];
    for (j, b) in pmk.iter_mut().enumerate() {
        *b = unsafe { (base as *const u8).add(4 + j).read_volatile() };
    }
    Some(pmk)
}
pub fn run() -> ! {
    let regs = 0x09000000 as *mut u32;
    let scan = |i: u32| {
        if i >= 8 {
            return Message::result(0);
        }
        let base = (0x09000100 + i * 20) as *const u8;
        let id = unsafe { (base as *const u32).read_volatile() };
        let mut r = Message::result(id);
        if id != 0 {
            let mut name = [0; 16];
            for (j, b) in name.iter_mut().enumerate() {
                *b = unsafe { base.add(4 + j).read_volatile() };
            }
            r.put_text(&name);
            r.arg = if security(i).is_some() { SCAN_WPA2 } else { SCAN_OPEN };
        }
        r
    };
    let mut associated = 0;
    serve(|m| {
        let online = unsafe { regs.read_volatile() & 1 != 0 && regs.add(1).read_volatile() == 0 };
        if !online {
            associated = 0;
        }
        match (m.op, m.key) {
            (GET, RADIO) => Message::result(online as u32),
            (EXECUTE, RADIO) if m.value <= 1 => {
                unsafe {
                    regs.add(1).write_volatile(1 - m.value);
                }
                if m.value == 0 {
                    associated = 0;
                }
                Message::result(m.value)
            }
            (GET, SCAN) if online => scan(m.value),
            (GET, NETWORK | PROFILE) => {
                let mut r = Message::result(if m.key == NETWORK {
                    (associated != 0) as u32
                } else {
                    associated
                });
                r.arg = online as u32;
                r
            }
            (EXECUTE, NETWORK | PROFILE) if online => {
                let target = if m.key == NETWORK && m.value == 1 {
                    scan(0).value
                } else {
                    m.value
                };
                if (m.value != 0 && target == 0)
                    || target != 0 && !(0..8).any(|i| scan(i).value == target)
                {
                    let mut r = Message::error(OFFLINE);
                    r.value = JOIN_NOT_FOUND;
                    return r;
                }
                // A secured network needs its key, unless it is the one already joined.
                let slot = (0..8).find(|&i| scan(i).value == target);
                if let Some(pmk) = slot.and_then(security) {
                    if target != associated && slot.is_some_and(stalls) {
                        let mut r = Message::error(OFFLINE);
                        r.value = JOIN_HANDSHAKE_TIMEOUT;
                        return r;
                    }
                    if target != associated && m.data != pmk {
                        let mut r = Message::error(INVALID);
                        r.value = if m.data == [0; 32] { JOIN_NO_KEY } else { JOIN_AUTH };
                        return r;
                    }
                }
                associated = target;
                Message::result(if m.key == NETWORK {
                    (target != 0) as u32
                } else {
                    target
                })
            }
            (EXECUTE, RADIO_TEST) if online && associated != 0 => {
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
