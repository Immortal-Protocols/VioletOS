//! The Sentinel: Violet's policy decisions in an address space of their own.
//! See README.md beside this file.
//!
//! One thread, one endpoint. The supervisor gives this process the receive
//! side of that endpoint, read-only sight of the Open Runtime's request
//! window, a reply window only this process can write, and 64 KiB of private
//! memory for its heap, and on QEMU read-only sight of the device root key's
//! page (for its sealing key): no other device, no storage, no network, no
//! other service's endpoint. (Like every process it also holds the send side
//! of the supervisor's fault endpoint, which the kernel uses to report its
//! faults.) It answers the Open Runtime and nobody else.
//!
//! Each request is copied out of the shared window before it is looked at
//! (the runtime could change it under our feet), checked against the CRC and
//! sequence number the IPC message announced, and handed to engine.rs, which
//! is the same code the host tests run. A request that is not understood is
//! answered V_BAD; the runtime's stub takes anything but a clear yes as no.
//! If this process faults, the supervisor rebuilds the whole system (its
//! policy for every component; at the fifth fault before a healthy run it stops): the
//! runtime's call in flight never returns, so nothing is granted in between,
//! and the new Sentinel starts with no answers until the new runtime hands it
//! the sealed store. The Sentinel alone is never restarted.
#[path = "engine.rs"]
pub mod engine;
/// The names runtime/open/shield.rs and scorer.rs look for beside them.
pub mod policy {
    pub use super::engine::common::*;
}
#[path = "../../runtime/open/shield.rs"]
pub mod shield;
#[path = "../../runtime/open/scorer_model.rs"]
pub mod scorer_model;
#[path = "../../runtime/open/scorer.rs"]
pub mod scorer;
#[path = "scoring.rs"]
pub mod scoring;
#[path = "../../runtime/open/heap.rs"]
mod heap;

use crate::protocol::*;
use alloc::vec::Vec;
use engine::common::{reply_words, REQ_MAGIC, V_BAD, WINDOW};

/// The Open Runtime's badge on this endpoint (supervisor.c OPEN_RUNTIME_BADGE).
const OPEN_RUNTIME: u32 = 9;
/// The request window: the Open Runtime writes, this process only reads.
const REQUEST: usize = 0x0930_0000;
/// The reply window: this process writes, the Open Runtime only reads.
const REPLY: usize = 0x0931_0000;
/// Zeroed private memory from the supervisor (prepare_process).
const HEAP: usize = 0x0280_0000;
const HEAP_BYTES: usize = 0x1_0000;
/// QEMU: the device root key blob's page, read-only (supervisor.c SENTINEL_ROOT_KEY).
#[cfg(device_root_key)]
const ROOT_KEY: usize = 0x0932_0000;

/// The engine with its sealing key. QEMU: derived from the device root key,
/// which no package, update or Open Runtime page holds; with no valid blob
/// there, no key at all (nothing is sealed or opened). The handset has no
/// device key yet: the build secret in this package (README.md, "The key").
fn keyed_engine() -> engine::Engine {
    #[cfg(device_root_key)]
    {
        let mut blob = [0u8; 64];
        for (i, b) in blob.iter_mut().enumerate() {
            *b = unsafe { core::ptr::read_volatile((ROOT_KEY + i) as *const u8) };
        }
        let key = engine::seal::root_key(&blob);
        blob.fill(0);
        match key {
            Some(root) => engine::Engine::new(engine::seal::master(&root)),
            None => engine::Engine::unkeyed(),
        }
    }
    #[cfg(not(device_root_key))]
    engine::Engine::new(engine::seal::master(&crate::sentinel_key::SEAL_SECRET))
}

fn reply(words: [u32; 6]) {
    let mut m = Message { op: words[0], key: words[1], value: words[2], arg: words[3], ..Message::default() };
    m.data[..4].copy_from_slice(&words[4].to_le_bytes());
    m.data[4..8].copy_from_slice(&words[5].to_le_bytes());
    m.reply();
}

pub fn run() -> ! {
    unsafe { heap::HEAP.add(HEAP, HEAP_BYTES) };
    let mut engine = keyed_engine();
    let mut scoring = scoring::Scoring::new();
    let mut frame: Vec<u8> = Vec::with_capacity(WINDOW);
    let mut out: Vec<u8> = Vec::with_capacity(WINDOW);
    loop {
        let (caller, m) = Message::receive(SERVER);
        // Only the Open Runtime holds this endpoint; anything else that wakes
        // the receive (a notification, were one ever bound) is nobody to answer.
        if caller != OPEN_RUNTIME {
            continue;
        }
        let len = m.arg as usize;
        if m.op != EXECUTE || m.key != REQ_MAGIC || len > WINDOW {
            reply(reply_words(m.value, V_BAD, 0, &[]));
            continue;
        }
        frame.clear();
        frame.extend_from_slice(unsafe { core::slice::from_raw_parts(REQUEST as *const u8, len) });
        let crc = u32::from_le_bytes([m.data[0], m.data[1], m.data[2], m.data[3]]);
        let words = engine.serve(m.value, crc, &frame, &mut out, &mut scoring);
        unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), REPLY as *mut u8, out.len()) };
        reply(words);
    }
}
