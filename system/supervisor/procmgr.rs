//! Dynamic Process & Memory Manager skeleton (evolution of `supervisor.c`).
//!
//! Replaces the current 50 MiB RAM cap, 15 fixed slots, and 128 KiB flat VPK
//! loader with a real reclaimable page pool, demand-paged VMAs, ELF32 + `#!`
//! shebang launching, and multi-thread (`clone`) support on pure seL4.

/// Reclaimable seL4 frame pool backed by all usable DRAM (outside the DTB's
/// reserved-memory ranges — unlocking hundreds of MiB / GiBs on Moto G Pure).
pub trait PagePool {
    /// Allocate a zeroed 4 KiB seL4 SmallPage cap.
    fn alloc_4k(&mut self) -> Option<FrameCap>;
    /// Allocate a zeroed 64 KiB seL4 LargePage cap (used for 720x1600 surfaces).
    fn alloc_64k(&mut self) -> Option<FrameCap>;
    /// Return a frame cap to the free list when an app calls `munmap` or exits.
    fn free(&mut self, cap: FrameCap);
}

#[derive(Clone, Copy, Debug)]
pub struct FrameCap(pub u32);

#[derive(Clone, Copy, Debug)]
pub enum VmaBacking {
    /// Demand-zero anonymous RAM (Go reserves large arenas with PROT_NONE and
    /// only faults in pages it actually touches).
    Anonymous,
    /// Shared surface or audio PCM ring owned by Display / Audio service.
    SharedDevice { first_frame: FrameCap, pages: u32 },
    /// File-backed mapping from StorageFsService.
    File { fs_handle: u32, file_offset: u64 },
}

#[derive(Clone, Copy, Debug)]
pub struct Vma {
    pub va_start: u32,
    pub va_end: u32,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub backing: VmaBacking,
}

/// Per-process virtual address layout tracked by Supervisor / OpenRuntime.
pub trait VmaTable {
    fn map(&mut self, vma: Vma) -> Result<u32, i32>;
    fn unmap(&mut self, va_start: u32, len: u32) -> Result<(), i32>;
    /// Called on `seL4_Fault_VMFault`: allocates & maps a backing frame on demand.
    fn handle_vm_fault(&mut self, pool: &mut dyn PagePool, fault_addr: u32, is_write: bool) -> bool;
}

/// How an executable in `/apps/` is launched.
pub enum LaunchTarget<'a> {
    /// Standard static (or later dynamic) ARMv7 hard-float ELF binary (Go, Rust, C, python3).
    ElfBinary { path: &'a str },
    /// Text script starting with `#!<interpreter>` (e.g. `#!/bin/python3`).
    /// Loader transparently rewrites argv to `[interpreter, script_path, ...args]`.
    ShebangScript { interpreter: &'a str, script_path: &'a str },
}

/// Core process/thread lifecycle operations exposed by Supervisor to OpenRuntime.
pub trait ProcessManager {
    /// Inspect header bytes: `#!` -> `ShebangScript`, `\x7fELF` -> `ElfBinary`.
    fn classify_executable<'a>(&self, header: &'a [u8], path: &'a str) -> Option<LaunchTarget<'a>>;

    /// Create a fresh seL4 PageDirectory, CSpace, initial TCB, load ELF `PT_LOAD`
    /// segments, build the initial Linux stack (`argc`, `argv`, `envp`, `auxv`
    /// with `AT_PAGESZ=4096`, `AT_RANDOM`, `AT_HWCAP` for VFPv4/NEON), and point
    /// the thread's fault endpoint at OpenRuntime with badge = `app_id`.
    fn spawn_process(&mut self, path: &str, argv: &[&str], envp: &[&str]) -> Result<u32, i32>;

    /// Create a new seL4 TCB sharing the parent's PageDirectory & CSpace (`clone`).
    /// Calls `seL4_TCB_SetTLSBase(tcb, tls)` so ARM `TPIDRURO` works natively.
    fn spawn_thread(&mut self, app_id: u32, entry_pc: u32, stack_sp: u32, tls: u32) -> Result<u32, i32>;

    /// Suspend all threads, unmap every VMA, return frames to `PagePool`.
    fn destroy_process(&mut self, app_id: u32);
}

// Note on Moto G Pure hardware readiness:
// - `platform/sel4/board/moto-radio-memory.dts` expands from 50 MiB to full DRAM
//   (respecting the 22 reserved-memory nodes already verified by `boot/loader/`).
// - Userspace compiles with hard-float VFPv4/NEON so stock `armhf` Python and
//   `GOOS=linux GOARCH=arm GOARM=7` Go binaries run at full speed.
