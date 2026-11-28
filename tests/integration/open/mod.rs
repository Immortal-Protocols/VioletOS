//! The Open Runtime as the host can build it.
//!
//! Every module in `shipped!` below is the phone's own source, pulled in by
//! path and compiled unchanged. What is written here is only what joins
//! them where the phone's join cannot be built for a host:
//!
//!   - runtime/open/mod.rs, syscall.rs, device.rs, sockets.rs, net.rs,
//!     trusted_ui.rs, data.rs, appstore.rs, update.rs and guard.rs reach
//!     memory at fixed 32-bit addresses (the pool at 0x2000_0000, the
//!     framebuffer, the shared pages) or seL4 directly, so they are not
//!     here. `Runtime` below is the stand-in for mod.rs's: it has the fields
//!     the shipped `impl Runtime` blocks use (manifest.rs is one, built
//!     unchanged against it).
//!   - `front.rs` is the stand-in for syscall.rs's dispatch: the same Linux
//!     system call numbers, calling the same shipped decisions in the same
//!     order. Its glue is copied from the shipped files, and it is the part
//!     a host test cannot vouch for: targets/qemu/test_system.py runs the
//!     real dispatch.
//!
//! When a file moves out of that list (its fixed addresses go behind a
//! function), add it to `shipped!` and delete its stand-in.
pub use crate::kernel::{pool, sys};
use crate::kernel::clock;
use alloc::string::String;
use alloc::vec::Vec;

/// runtime/open/mod.rs
pub const MAX_APPS: usize = 12;
pub const HOME_APP: &str = "/apps/home";
pub const SETTINGS_APP: &str = "/apps/settings";
pub const INSTALLER: &str = "/apps/agent";

macro_rules! shipped {
    ($($name:ident = $path:literal;)*) => { $( #[path = $path] pub mod $name; )* };
}
shipped! {
    abi = "../../../runtime/open/abi.rs";
    policy = "../../../runtime/open/policy.rs";
    manifest = "../../../runtime/open/manifest.rs";
    fs = "../../../runtime/open/fs.rs";
    shield = "../../../runtime/open/shield.rs";
    scorer_model = "../../../runtime/open/scorer_model.rs";
    scorer = "../../../runtime/open/scorer.rs";
    sysui = "../../../runtime/open/sysui.rs";
    lifecycle = "../../../runtime/open/lifecycle.rs";
    signal = "../../../runtime/open/signal.rs";
    vma = "../../../runtime/open/vma.rs";
    elf = "../../../runtime/open/elf.rs";
    entropy = "../../../runtime/open/entropy.rs";
    procmgr = "../../../system/supervisor/procmgr.rs";
    loopback = "../../../runtime/open/loopback.rs";
    sockaddr = "../../../runtime/open/sockaddr.rs";
    slots = "../../../runtime/open/slots.rs";
    names = "../../../runtime/open/names.rs";
    sntp = "../../../runtime/open/sntp.rs";
    inflate = "../../../runtime/open/inflate.rs";
    hcimux = "../../../runtime/open/hcimux.rs";
    frames = "../../../runtime/open/frames.rs";
    powerctl = "../../../runtime/open/powerctl.rs";
    // runtime/open/mod.rs includes it the same way: the AArch64 call translation.
    arch = "../../../runtime/arch/mod.rs";
}

pub mod front;
pub mod life;
pub mod syscall;
pub mod trusted_ui;

use crate::protocol::*;
use policy::Cap;
use shield::{ShieldPolicy, Subject};

/// A request to Runtime, waiting for its ticket (runtime/open/mod.rs).
pub fn request(op: u32, key: u32, value: u32) -> Message {
    request_with(op, key, value, 0)
}
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
    let r = Message::new(op, key, value).with_arg(arg).call(sys::SLOT_RUNTIME);
    if r.op != OK {
        return Message::error(r.op);
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        sys::pause();
    }
}

/// SHA-256 for the shipped code that takes one as a function (the phone
/// links BearSSL's; this is Storage's own, services/storage/sha256.rs).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = crate::storage::slotwrite::sha256::Sha256::new();
    h.update(data);
    h.finish()
}

/// An app Violet Agent installed (appstore.rs `Entry`, the fields the policy reads).
#[derive(Clone, Default)]
pub struct Entry {
    pub name: String,
    pub package: bool,
    pub manifest: String,
}
#[derive(Default)]
pub struct Store {
    pub entries: Vec<Entry>,
}

/// An open file description (device.rs `OpenFile`, what the front needs).
#[derive(Clone)]
pub enum File {
    Regular { path: String, flags: u32, pos: usize },
    Dir { path: String, entries: Vec<(String, bool)> },
    Device { node: String, answer: Vec<u8> },
    Socket { kind: front::Sock },
}

pub struct App {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub path: String,
    pub owner: String,
    pub digest: [u8; 32],
    /// Not part of the boot image (written or pushed since boot).
    pub new_hash: bool,
    pub cwd: String,
    pub space: procmgr::Space,
    pub fds: Vec<Option<File>>,
    pub exit_code: Option<i32>,
    /// When it last left the front (or started).
    pub left_ns: u64,
    pub used_ns: u64,
    /// Audio, the microphone or Bluetooth open (lifecycle.rs `Facts::reason`).
    pub reason: bool,
    pub life: lifecycle::Life,
    pub sig: signal::Proc<()>,
    pub thread: signal::Thread<()>,
    /// Signals delivered to handlers, in order: (signal, handler address).
    pub delivered: Vec<(u32, u32)>,
    /// LIFECYCLE_LOW_MEMORY events the app was sent.
    pub low_memory: u32,
    /// Scratch memory in the app for the arguments of scripted calls.
    pub scratch: u32,
    /// AT_RANDOM, the entry point and the first stack pointer, as loaded.
    pub random: [u8; 16],
    pub entry: u32,
    pub sp: u32,
}

/// The stand-in for runtime/open/mod.rs `Runtime`.
pub struct Runtime {
    pub k: pool::Kernel,
    pub pool: pool::Pool,
    pub fs: fs::Fs,
    pub store: Store,
    pub apps: [Option<App>; MAX_APPS],
    pub book: manifest::Book,
    pub focused: bool,
    pub foreground: Option<usize>,
    pub home: Option<usize>,
    pub next_pid: u32,
    pub entropy: entropy::Drbg,
    pub shield: ShieldPolicy,
    pub last_touch_ns: u64,
    pub life: lifecycle::Book,
    /// Launch reserve in pool pages (mod.rs RESERVE_PAGES, scaled to this pool).
    pub reserve: usize,
    /// Radio's socket slots and the resolver's table (net.rs keeps them on the phone).
    pub slots: slots::Table,
    pub names: names::Names,
    pub pipes: loopback::Budget,
}

impl Runtime {
    /// Who app `i` is to the Shield (guard.rs `subject`, as written there).
    pub fn subject(&self, i: usize) -> Subject {
        let app = self.apps[i].as_ref().unwrap();
        let shown = self.focused && self.foreground == Some(i);
        let front = shield::in_front(shown, app.pid, self.shield.pushed, self.last_touch_ns);
        Subject::derive(app.pid, &app.path, &app.owner, app.digest[..4].try_into().unwrap(), !app.new_hash, front)
    }
    /// The app a program about to start from `path` belongs to (guard.rs).
    pub fn shield_owner(&self, path: &str, digest: &[u8; 32], caller: Option<usize>) -> String {
        let parent = caller.filter(|&c| self.apps[c].is_some()).map(|c| self.subject(c)).filter(|s| s.role & shield::ROLE_CORE == 0);
        shield::owner_of(path, parent.as_ref().map(|s| s.name()), digest[..4].try_into().unwrap())
    }
    pub fn index_of(&self, pid: u32) -> Option<usize> {
        self.apps.iter().position(|a| a.as_ref().is_some_and(|a| a.pid == pid))
    }
    pub fn now(&self) -> u64 {
        clock::now_ns()
    }
}
