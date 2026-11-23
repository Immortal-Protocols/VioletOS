//! Open Runtime ("Runtime v2") — pure-seL4 translation layer for open apps.
//!
//! Coexists alongside `runtime/process.rs`: legacy built-in apps keep using the
//! old 14-word protocol while `/apps/*` binaries and scripts run here.

pub mod abi;
pub mod policy;
pub mod syscall;

use policy::{AppId, Policy};
use syscall::{SysReply, TrapFrame, WaitOn};

/// What an open file descriptor inside an app points to.
pub enum FdTarget {
    RegularFile { fs_handle: u32, offset: u64 },
    ScreenSurface { surface_id: u32 },
    AudioStream { stream_id: u32 },
    EventQueue,
    SystemControl,
    Socket { sock_id: u32 },
    Epoll { watch_list: u32 },
}

/// A thread currently parked on a blocking syscall (touch, socket, futex, timer).
pub struct ParkedThread {
    pub app: AppId,
    pub tid: u32,
    pub reply_cap_slot: u32, // saved seL4 reply cap; seL4_Send resumes the thread
    pub waiting_on: WaitOn,
}

/// Per-app state managed by OpenRuntime.
pub struct OpenProcess {
    pub app: AppId,
    pub brk_end: u32,
    pub foreground: bool,
    pub has_bg_audio: bool,
    // Fixed-cap tables or heap-backed maps once OpenRuntime uses a small allocator.
}

pub struct OpenRuntime<P: Policy> {
    pub policy: P,
    pub focus: Option<AppId>,
    // Service endpoints: Display, Audio, Radio, Storage, Touch, Power, Supervisor.
}

impl<P: Policy> OpenRuntime<P> {
    /// Main event loop: receives seL4 faults and service notifications without
    /// ever blocking on a slow driver call.
    pub fn serve_loop(&mut self) -> ! {
        loop {
            match self.recv_next_event() {
                KernelEvent::UnknownSyscall { app, tid, frame } => {
                    match self.dispatch_syscall(app, tid, &frame) {
                        SysReply::Resume(ret) => self.reply_syscall(ret),
                        SysReply::Block(wait) => self.stash_reply_cap(app, tid, wait),
                        SysReply::Exit => self.reap_thread(app, tid),
                    }
                }
                KernelEvent::VmFault { app, fault_addr, write } => {
                    // Ask Supervisor's VmaTable to demand-page or map shared buffer.
                    self.handle_page_fault(app, fault_addr, write);
                }
                KernelEvent::ServiceReady(ready) => {
                    // Wake any ParkedThread whose WaitOn matches (touch, socket, timer).
                    self.wake_matching_waiters(ready);
                }
            }
        }
    }

    // ---- Skeleton hooks (implemented against seL4 IPC + Supervisor) ----
    fn recv_next_event(&mut self) -> KernelEvent { todo!() }
    fn reply_syscall(&mut self, _ret: i32) { todo!() }
    fn stash_reply_cap(&mut self, _app: AppId, _tid: u32, _wait: WaitOn) { todo!() }
    fn reap_thread(&mut self, _app: AppId, _tid: u32) { todo!() }
    fn handle_page_fault(&mut self, _app: AppId, _addr: u32, _write: bool) { todo!() }
    fn wake_matching_waiters(&mut self, _ready: ServiceReady) { todo!() }

    pub(crate) fn read_user_str<'a>(&self, _app: AppId, _ptr: u32) -> &'a str {
        // Because apps are trusted, OpenRuntime maps app RAM frames linearly so
        // copy_from_user is a direct slice read with zero page-table churn.
        todo!()
    }
    pub(crate) fn sys_brk(&mut self, _app: AppId, _addr: u32) -> SysReply { todo!() }
    pub(crate) fn sys_mmap2(&mut self, _app: AppId, _a: u32, _len: u32, _p: u32, _fl: u32, _fd: i32, _off: u32) -> SysReply { todo!() }
    pub(crate) fn sys_munmap(&mut self, _app: AppId, _a: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_close(&mut self, _app: AppId, _fd: u32) -> SysReply { todo!() }
    pub(crate) fn sys_read(&mut self, _app: AppId, _fd: u32, _buf: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_write(&mut self, _app: AppId, _fd: u32, _buf: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_ioctl(&mut self, _app: AppId, _fd: u32, _cmd: u32, _arg: u32) -> SysReply { todo!() }
    pub(crate) fn sys_vfs_meta(&mut self, _app: AppId, _f: &TrapFrame) -> SysReply { todo!() }
    pub(crate) fn sys_socket(&mut self, _app: AppId, _dom: u32, _ty: u32, _proto: u32) -> SysReply { todo!() }
    pub(crate) fn sys_connect(&mut self, _app: AppId, _fd: u32, _addr: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_bind(&mut self, _app: AppId, _fd: u32, _addr: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_sock_io(&mut self, _app: AppId, _f: &TrapFrame) -> SysReply { todo!() }
    pub(crate) fn sys_wait_mux(&mut self, _app: AppId, _tid: u32, _f: &TrapFrame) -> SysReply { todo!() }
    pub(crate) fn sys_clock_gettime(&mut self, _app: AppId, _clk: u32, _tp: u32) -> SysReply { todo!() }
    pub(crate) fn sys_nanosleep(&mut self, _app: AppId, _req: u32) -> SysReply { todo!() }
    pub(crate) fn sys_getrandom(&mut self, _app: AppId, _buf: u32, _len: u32) -> SysReply { todo!() }
    pub(crate) fn sys_clone(&mut self, _app: AppId, _tid: u32, _f: &TrapFrame) -> SysReply { todo!() }
    pub(crate) fn sys_futex(&mut self, _app: AppId, _tid: u32, _f: &TrapFrame) -> SysReply { todo!() }
    pub(crate) fn sys_set_tls(&mut self, _app: AppId, _tid: u32, _tls: u32) -> SysReply { todo!() }
    pub(crate) fn sys_cacheflush(&mut self, _app: AppId, _start: u32, _end: u32) -> SysReply { todo!() }
    pub(crate) fn sys_exit(&mut self, _app: AppId, _tid: u32, _code: i32, _group: bool) -> SysReply { todo!() }
    pub(crate) fn sys_tgkill(&mut self, _app: AppId, _pid: u32, _tid: u32, _sig: u32) -> SysReply { todo!() }
}

pub enum KernelEvent {
    UnknownSyscall { app: AppId, tid: u32, frame: TrapFrame },
    VmFault { app: AppId, fault_addr: u32, write: bool },
    ServiceReady(ServiceReady),
}

pub enum ServiceReady {
    InputEvent,
    Socket { sock_id: u32 },
    TimerExpired { now_us: u64 },
    AudioSpace { stream_id: u32 },
}
