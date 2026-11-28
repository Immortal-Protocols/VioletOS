//! Host harness for Runtime, the broker between built-in apps (and the Open
//! Runtime, its client 9) and the services: runtime/process.rs, whose tests
//! pin who may ask for what. Part of `make test-unit`.
//!
//! The real process.rs and protocol.rs are compiled; the worker lanes and the
//! IPC calls are stand-ins that record where `dispatch` sent a request and
//! answer OK. Each test thread has its own record: the tests run in parallel.
#![allow(dead_code)]

#[path = "../../protocol.rs"]
mod protocol;
#[path = "../../process.rs"]
mod process;

mod harness {
    use std::cell::RefCell;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Sent {
        /// Queued on a service's worker lane.
        Lane { lane: u32, owner: u32, op: u32, key: u32 },
        /// A direct call (the supervisor's lifecycle endpoint, or Audio).
        Call { slot: u32, op: u32, key: u32, value: u32 },
    }
    thread_local!(pub static LOG: RefCell<Vec<Sent>> = RefCell::new(Vec::new()));
    /// Everything sent since the last call.
    pub fn sent() -> Vec<Sent> {
        LOG.with(|l| core::mem::take(&mut *l.borrow_mut()))
    }
    pub fn record(s: Sent) {
        LOG.with(|l| l.borrow_mut().push(s));
    }
}

/// runtime/lanes.rs, as far as process.rs uses it.
mod lanes {
    use crate::harness::{record, Sent};
    use crate::protocol::{Message, EMPTY};
    pub fn queue(lane: u32, _index: u32, owner: u32, m: Message) -> bool {
        record(Sent::Lane { lane, owner, op: m.op, key: m.key });
        true
    }
    use std::cell::Cell;
    thread_local! {
        /// Whether queued work has finished (`take` answers), and the touch
        /// sample lane 4 holds (`sample`): set by a test, per thread.
        pub static DONE: Cell<bool> = const { Cell::new(false) };
        pub static TOUCH: Cell<Option<Message>> = const { Cell::new(None) };
    }
    pub fn take(_lane: u32, _index: u32) -> Option<Message> {
        DONE.with(|d| d.get()).then(|| Message::result(0))
    }
    pub fn sample(lane: u32, _index: u32, _who: u32, _m: Message) -> Message {
        match TOUCH.with(|t| t.get()).filter(|_| lane == 4) {
            Some(m) => m,
            None => Message::error(EMPTY),
        }
    }
    pub fn initialize(_lane: u32, _owner: u32, _m: Message) -> Message {
        Message::result(u32::MAX)
    }
}

// What protocol.rs imports from transport.c.
#[no_mangle]
extern "C" fn ipc_call(slot: u32, m: &mut protocol::Message) {
    harness::record(harness::Sent::Call { slot, op: m.op, key: m.key, value: m.value });
    *m = protocol::Message::result(0);
}
#[no_mangle]
extern "C" fn ipc_receive(_slot: u32, _m: &mut protocol::Message) -> u32 {
    unreachable!("the tests call dispatch, not the receive loop")
}
#[no_mangle]
extern "C" fn ipc_reply(_m: &mut protocol::Message) {}
#[no_mangle]
extern "C" fn ticks() -> u32 {
    0
}
#[no_mangle]
extern "C" fn delay_ms(_ms: u32) {}
#[no_mangle]
extern "C" fn hold_cpu(_on: u32) {}
#[no_mangle]
static ticks_per_ms: u32 = 1;
