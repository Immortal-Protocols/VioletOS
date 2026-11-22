use crate::protocol::*;
#[path = "gateway.rs"]
mod gateway;
#[path = "stream.rs"]
mod stream;
extern "C" {
    fn ipc_try_receive(slot: u32, m: &mut Message) -> u32;
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
        }
        r
    };
    let mut associated = 0;
    let mut music = stream::Streamer::default();
    let online = || unsafe { regs.read_volatile() & 1 != 0 && regs.add(1).read_volatile() == 0 };
    let dispatch = |m: Message, associated: &mut u32| {
        let online = online();
        if !online {
            *associated = 0;
        }
        match (m.op, m.key) {
            (GET, RADIO) => Message::result(online as u32),
            (EXECUTE, RADIO) if m.value <= 1 => {
                unsafe {
                    regs.add(1).write_volatile(1 - m.value);
                }
                if m.value == 0 {
                    *associated = 0;
                }
                Message::result(m.value)
            }
            (GET, SCAN) if online => scan(m.value),
            (GET, NETWORK | PROFILE) => {
                let mut r = Message::result(if m.key == NETWORK {
                    (*associated != 0) as u32
                } else {
                    *associated
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
                    return Message::error(OFFLINE);
                }
                *associated = target;
                Message::result(if m.key == NETWORK {
                    (target != 0) as u32
                } else {
                    target
                })
            }
            (EXECUTE, RADIO_TEST) if online && *associated != 0 => {
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
            (EXECUTE, WEATHER) if online && *associated != 0 => {
                match gateway::endpoint_weather(m.value as usize) {
                    Some((text, n)) => {
                        let mut response = Message::result(1);
                        response.put_text(&text[..n]);
                        response
                    }
                    None => Message::result(2),
                }
            }
            _ => Message::error(OFFLINE),
        }
    };
    // Serve requests between music fetches; a fetch runs when Audio wants data.
    loop {
        let mut m = Message::default();
        if unsafe { ipc_try_receive(SERVER, &mut m) } != 0 {
            dispatch(m, &mut associated).reply();
            continue;
        }
        if music.wants() {
            let up = online() && associated != 0;
            music.step(up, &mut |path, from, to, out| gateway::endpoint_range(path, from, to, out));
        }
        unsafe { delay_ms(2) };
    }
}
