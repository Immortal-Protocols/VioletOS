//! Shared request cells between Runtime and its service-specific workers.
//! Release/acquire publishes a complete message across address spaces.
use crate::protocol::*;
use crate::router::{present_allowed, storage_route, StorageRoute};
use core::{
    cell::UnsafeCell,
    sync::atomic::{
        AtomicU32,
        Ordering::{Acquire, Release},
    },
};
const BASE: usize = 0x07400000;
#[repr(C)]
struct Cell {
    state: AtomicU32,
    owner: UnsafeCell<u32>,
    message: UnsafeCell<Message>,
}
fn cell(lane: u32, index: u32) -> &'static Cell {
    assert!((4..=9).contains(&lane) && index < 32);
    unsafe { &*((BASE + (lane - 4) as usize * 4096) as *const Cell).add(index as usize) }
}
extern "C" {
    fn worker_signal(slot: u32);
    fn worker_wait(slot: u32);
    fn touch_status() -> *const u8;
    fn touch_event(x: &mut u32, y: &mut u32, down: &mut bool) -> bool;
    fn app_checkpoint(region: u32, save: bool, value: &mut u32) -> bool;
}
/// Word in the display lane's page naming the app that may present. Runtime
/// writes it on every focus or sleep change; the display worker reads it.
fn presenter_word() -> &'static AtomicU32 {
    unsafe { &*((BASE + 4096 + 1021 * 4) as *const AtomicU32) }
}
pub fn publish_presenter(app: u32) {
    presenter_word().store(app, Release);
}
pub fn queue(lane: u32, index: u32, owner: u32, m: Message) -> bool {
    let c = cell(lane, index);
    if c.state.load(Acquire) != 0 {
        return false;
    }
    unsafe {
        *c.owner.get() = owner;
        *c.message.get() = m;
    }
    c.state.store(1, Release);
    unsafe {
        worker_signal(9 + lane - 4);
    }
    true
}
pub fn take(lane: u32, index: u32) -> Option<Message> {
    let c = cell(lane, index);
    if c.state.load(Acquire) != 2 {
        return None;
    }
    let m = unsafe { *c.message.get() };
    c.state.store(0, Release);
    Some(m)
}
pub fn sample(lane: u32, index: u32, who: u32, m: Message) -> Message {
    if let Some(r) = take(lane, index) {
        return r;
    }
    queue(lane, index, who, m);
    Message::error(EMPTY)
}
fn execute(lane: u32, owner: u32, mut m: Message) -> Message {
    match lane {
        4 if m.key == STATUS => {
            let ptr = unsafe { touch_status() };
            let mut r = Message::result(0);
            if ptr.is_null() {
                r.put_text(b"TOUCH READY");
            } else {
                let mut bytes = [0; 40];
                for (i, b) in bytes.iter_mut().enumerate() {
                    *b = unsafe { *ptr.add(i) };
                    if *b == 0 {
                        break;
                    }
                }
                r.put_text(&bytes);
            }
            r
        }
        4 => {
            let (mut x, mut y, mut down) = (0, 0, false);
            if !unsafe { touch_event(&mut x, &mut y, &mut down) } {
                return Message::error(EMPTY);
            }
            let mut r = Message::result(x);
            r.arg = y;
            r.key = down as u32;
            r
        }
        6 => match storage_route(owner, &m) {
            StorageRoute::Diagnostic => Message::new(STORAGE_DIAGNOSTIC, 0, 0).call(3),
            StorageRoute::Checkpoint(region) => {
                let mut v = m.value;
                if unsafe { app_checkpoint(region, m.op == EXECUTE, &mut v) } {
                    Message::result(v)
                } else {
                    Message::error(OFFLINE)
                }
            }
            StorageRoute::File(op) => {
                m.op = op;
                m.call(3)
            }
            StorageRoute::Reject => Message::error(DENIED),
        },
        // A present queued just before a focus change is dropped here.
        5 if present_allowed(owner, &m, presenter_word().load(Acquire)) => m.call(4),
        5 => Message::error(DENIED),
        7 => {
            if matches!(m.key, RADIO | PROFILE) && m.op == EXECUTE { m.arg = 0; }
            let reply = m.call(5);
            if reply.arg == RADIO_RELEASE_POWER {
                let cleanup=Message::new(EXECUTE,RADIO_RAILS,0).call(6);
                if cleanup.op!=OK {return cleanup;}
                return Message::result(reply.value);
            }
            let needs_rails = m.op == EXECUTE &&
                ((m.key == RADIO && m.value == 1) || (m.key == PROFILE && m.value != 0));
            if reply.op != RADIO_PREPARE_POWER || !needs_rails {
                return reply;
            }
            let rails = Message::new(EXECUTE,RADIO_RAILS,1).call(6);
            m.arg = if rails.op == OK {1} else {2};
            let result = m.call(5);
            if result.arg == RADIO_RETAIN_POWER {return result;}
            let cleanup = Message::new(EXECUTE,RADIO_RAILS,0).call(6);
            if cleanup.op != OK { return cleanup; }
            result
        }
        8 => {
            if m.op == EXECUTE && m.value == 1 {
                let blank = Message::new(EXECUTE, POWER, 1).call(4);
                if blank.op != OK {
                    return blank;
                }
            }
            m.call(6)
        }
        9 => m.call(8),
        _ => Message::error(INVALID),
    }
}
pub fn work(lane: u32) -> ! {
    loop {
        unsafe {
            worker_wait(14);
        }
        for i in 0..32 {
            let c = cell(lane, i);
            if c.state.load(Acquire) != 1 {
                continue;
            }
            let mark = (BASE + (lane - 4) as usize * 4096 + 4080) as *const AtomicU32;
            unsafe {
                (*mark).store(ticks() | 1, Release);
            }
            let r = unsafe { execute(lane, *c.owner.get(), *c.message.get()) };
            unsafe {
                *c.message.get() = r;
                (*mark).store(0, Release);
            }
            c.state.store(2, Release);
        }
    }
}

pub fn initialize(lane: u32, owner: u32, m: Message) -> Message {
    assert!(queue(lane, 0, owner, m));
    loop {
        if let Some(r) = take(lane, 0) {
            return r;
        }
        unsafe {
            delay_ms(1);
        }
    }
}
