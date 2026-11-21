use crate::protocol::*;
// Each app has a bounded completion slot. Subscribe establishes a persistent
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
    match submit(op, key, value) {
        Ok(t) => t.wait(),
        Err(e) => Message::error(e),
    }
}
