//! Host tests for the Storage service: `make test-unit` builds this file with
//! `rustc --test`. It pulls the service's own sources in unchanged; only the
//! disk is replaced, by one that can lose power (testdisk.rs).
#![allow(dead_code)]
extern crate alloc;
#[path = "../blockdev.rs"]
mod blockdev;
#[path = "../journal.rs"]
mod journal;
#[path = "../../../runtime/protocol.rs"]
mod protocol;
#[path = "../violetfs/mod.rs"]
mod violetfs;

mod block;
#[path = "../files.rs"]
mod files;
mod files_tests;
#[path = "../writeback.rs"]
mod writeback;
mod writeback_tests;
mod testdisk;
mod violetfs_tests;

// protocol.rs declares the IPC calls of the target; nothing here makes them.
#[no_mangle]
extern "C" fn ipc_call(_slot: u32, _m: *mut u8) {}
#[no_mangle]
extern "C" fn ipc_receive(_slot: u32, _m: *mut u8) -> u32 {
    0
}
#[no_mangle]
extern "C" fn ipc_reply(_m: *mut u8) {}
