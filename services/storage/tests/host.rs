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
#[path = "../extent.rs"]
mod extent;
#[path = "../vfsproto.rs"]
mod vfsproto;
#[path = "../volume.rs"]
mod volume;
#[path = "../legacy.rs"]
mod legacy;
#[path = "../vfs.rs"]
mod vfs;
mod vfs_tests;
#[path = "../mount.rs"]
mod mount;
mod mount_tests;
// The Open Runtime's own file layer, to run it against the server.
#[path = "../../../runtime/open/fs.rs"]
mod fs;
/// The errors fs.rs names (runtime/open/syscall.rs).
mod syscall {
    pub const ENOENT: i32 = -2;
    pub const EEXIST: i32 = -17;
    pub const ENOTDIR: i32 = -20;
    pub const EISDIR: i32 = -21;
    pub const ENOTEMPTY: i32 = -39;
}
mod fs_tests;

// protocol.rs declares the IPC calls of the target; nothing here makes them.
#[no_mangle]
extern "C" fn ipc_call(_slot: u32, _m: *mut u8) {}
#[no_mangle]
extern "C" fn ipc_receive(_slot: u32, _m: *mut u8) -> u32 {
    0
}
#[no_mangle]
extern "C" fn ipc_reply(_m: *mut u8) {}
