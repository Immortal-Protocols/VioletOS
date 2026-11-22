use crate::protocol::*;
// Each app has one completion slot per service and one for local operations. Subscribe establishes a persistent
// stream; Get and Execute return single-use tickets. Forget discards a result.
pub struct Ticket(u32);
pub fn submit(op: u32, key: u32, value: u32) -> Result<Ticket, u32> {
    let r = Message::new(op, key, value).call(SERVER);
    if r.op == OK {
        Ok(Ticket(r.value))
    } else {
        Err(r.op)
    }
}
impl Ticket {
    pub fn poll(&self) -> Message {
        Message::new(POLL, 0, self.0).call(SERVER)
    }
    pub fn forget(self) {
        Message::new(FORGET, 0, self.0).call(SERVER);
    }
    pub fn wait(self) -> Message {
        loop {
            let r = self.poll();
            if r.op != BUSY {
                return r;
            }
        }
    }
}
pub fn request(op: u32, key: u32, value: u32) -> Message {
    request_with(op, key, value, 0)
}
/// As `request`, with the operation's argument word (for example a tone's pitch and length).
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
    let r = Message::new(op, key, value).with_arg(arg).call(SERVER);
    if r.op != OK {
        return Message::error(r.op);
    }
    Ticket(r.value).wait()
}

pub fn file(key: u32, name: &[u8], offset: u32, data: &[u8]) -> Message {
    if name.len() > 16 || data.len() > 24 {
        return Message::error(INVALID);
    }
    let op = if matches!(key, FILE_READ | FILE_LIST) {
        GET
    } else {
        EXECUTE
    };
    let mut m = Message::new(op, key, offset);
    m.arg = data.len() as u32;
    let mut bytes = [0; 40];
    bytes[..name.len()].copy_from_slice(name);
    bytes[16..16 + data.len()].copy_from_slice(data);
    m.put_text(&bytes);
    let r = m.call(SERVER);
    if r.op != OK {
        return r;
    }
    Ticket(r.value).wait()
}
