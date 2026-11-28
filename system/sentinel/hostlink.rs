//! Host builds only: the Open Runtime's enforcement stub joined to the real
//! Sentinel engine through an in-memory copy of the kernel edge (the two
//! windows and the call), one Sentinel per test thread. So a host test that
//! builds runtime/open/policy.rs gets the shipped stub, the shipped wire
//! format and the shipped decisions, and can break the edge on purpose
//! (`Fault`) to see the stub refuse.
#![allow(dead_code)]
extern crate std;

#[path = "engine.rs"]
pub mod engine;

use super::client::Port;
use alloc::vec::Vec;
use std::cell::RefCell;

/// What goes wrong between the stub and the Sentinel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// The call never comes back with an answer.
    Unreachable,
    /// One bit of the request changes after the stub has summed it.
    CorruptRequest,
    /// The announced length is not the frame's.
    WrongLength,
    /// The reply words are all zero, or all ones.
    ZeroReply,
    OnesReply,
    /// The reply answers another question.
    StaleReply,
    /// The reply says "allow" in a word that is not a verdict.
    BogusVerdict,
    /// The output changes after the Sentinel has summed it.
    CorruptOutput,
    /// The output is said to be longer than the window.
    HugeOutput,
}

/// The key host tests seal with. Never in an image (image/packages/build.py
/// makes the real one).
pub const TEST_KEY: [u8; 32] = [0x5a; 32];

pub struct Link {
    pub engine: engine::Engine,
    request: Vec<u8>,
    reply: Vec<u8>,
    seq: u32,
    pub fault: Fault,
    /// The fault applies to the next call only.
    pub once: bool,
    /// Calls that reached the engine.
    pub served: u32,
}

impl Link {
    fn new() -> Link {
        Link { engine: engine::Engine::new(TEST_KEY), request: alloc::vec![0; engine::common::WINDOW], reply: alloc::vec![0; engine::common::WINDOW], seq: 0, fault: Fault::None, once: false, served: 0 }
    }
}

impl Port for Link {
    fn window(&mut self) -> &mut [u8] {
        &mut self.request
    }
    fn call(&mut self, seq: u32, len: u32, crc: u32) -> Option<[u32; 6]> {
        let mut len = len as usize;
        let fault = self.fault;
        if self.once {
            (self.fault, self.once) = (Fault::None, false);
        }
        match fault {
            Fault::Unreachable => return None,
            Fault::CorruptRequest => self.request[len - 1] ^= 1,
            Fault::WrongLength => len -= 1,
            _ => {}
        }
        // As process.rs does: a private copy, then the engine.
        let frame = Vec::from(self.request.get(..len)?);
        let mut out = Vec::new();
        let mut words = self.engine.serve(seq, crc, &frame, &mut out, &mut engine::NoExtension);
        self.served += 1;
        self.reply[..out.len()].copy_from_slice(&out);
        match fault {
            Fault::ZeroReply => words = [0; 6],
            Fault::OnesReply => words = [!0; 6],
            Fault::StaleReply => words[1] = seq.wrapping_sub(1),
            Fault::BogusVerdict => words[2] = 1,
            Fault::CorruptOutput => self.reply[0] ^= 1,
            Fault::HugeOutput => words[4] = engine::common::WINDOW as u32 + 1,
            _ => {}
        }
        Some(words)
    }
    fn output(&self) -> &[u8] {
        &self.reply
    }
}

std::thread_local! {
    static LINK: RefCell<Link> = RefCell::new(Link::new());
}

/// Run `f` with this thread's link and the next sequence number.
pub fn with<R>(f: impl FnOnce(&mut Link, u32) -> R) -> R {
    LINK.with(|l| {
        let mut l = l.borrow_mut();
        l.seq = l.seq.wrapping_add(1);
        let seq = l.seq;
        f(&mut l, seq)
    })
}

pub fn set_fault(fault: Fault) {
    LINK.with(|l| l.borrow_mut().fault = fault);
}

/// `fault` on the next call only.
pub fn set_fault_once(fault: Fault) {
    LINK.with(|l| {
        let mut l = l.borrow_mut();
        (l.fault, l.once) = (fault, true);
    });
}

/// The engine's sealed store as it stands, asked directly (not through the stub).
pub fn exported() -> Vec<u8> {
    LINK.with(|l| {
        let mut l = l.borrow_mut();
        let mut window = alloc::vec![0u8; engine::common::WINDOW];
        let len = engine::common::encode(&mut window, 0, engine::common::OP_EXPORT, 0, 0, &[]).unwrap();
        let mut out = Vec::new();
        let crc = engine::common::crc32(&window[..len]);
        l.engine.serve(0, crc, &window[..len], &mut out, &mut engine::NoExtension);
        out
    })
}

/// The Sentinel starts again (same key, nothing remembered), as after a
/// fault: the supervisor rebuilds it with the rest of the system.
pub fn restart() {
    LINK.with(|l| *l.borrow_mut() = Link::new());
}

/// The Sentinel starts again with no sealing key (QEMU with no valid root key blob).
pub fn restart_unkeyed() {
    LINK.with(|l| {
        let mut l = l.borrow_mut();
        *l = Link::new();
        l.engine = engine::Engine::unkeyed();
    });
}

/// Calls the engine has served on this thread.
pub fn served() -> u32 {
    LINK.with(|l| l.borrow().served)
}
