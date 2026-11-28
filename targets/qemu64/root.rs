//! The 64-bit bootstrap task: the first user program of the `qemu64` target.
//!
//! It is a bring-up vehicle, not the Supervisor: it proves the AArch64 kernel,
//! the architecture module (runtime/arch) and the board description on a real
//! kernel, by doing the Supervisor's and the Open Runtime's architecture-
//! specific work in miniature: carve the kernel's untyped memory, map a
//! device, build a 4-level address space, load a static ELF64 program, start
//! a thread and answer its Linux system calls and faults. Each program's
//! objects come from one arena that is revoked when it ends (space.rs), and
//! its stack, mappings and AT_RANDOM bytes are drawn from a seeded generator.
use super::arch::{self, Access, Elf, Kernel, MapError, Protection, NATIVE};
use super::board;
use super::bootstrap::{self, Untyped};
use super::entropy::{self, Drbg};
use super::linux::{self, Outcome, PAGE};
use super::space::{self, Frames, Layout, Space};
use core::fmt::Write;

core::arch::global_asm!(
    r#"
.section .entry,"ax"
.global _start
_start:
    adrp x1, root_stack_top
    add x1, x1, :lo12:root_stack_top
    mov sp, x1
    b root_main
"#
);

extern "C" {
    fn k_init(info: *mut u8);
    fn k_empty_start() -> usize;
    fn k_empty_end() -> usize;
    fn k_untyped_count() -> usize;
    fn k_untyped(index: usize, paddr: *mut u64, bits: *mut u64, device: *mut u64) -> usize;
    fn k_retype(untyped: usize, kind: usize, bits: usize, slot: usize) -> i64;
    fn k_page_map(frame: usize, vspace: usize, va: u64, protection: usize, device: usize) -> i64;
    fn k_page_unmap(frame: usize) -> i64;
    fn k_page_unify(frame: usize, bytes: usize) -> i64;
    fn k_table_map(table: usize, vspace: usize, va: u64) -> i64;
    fn k_asid_assign(vspace: usize) -> i64;
    fn k_copy(dest: usize, src: usize) -> i64;
    fn k_revoke(slot: usize) -> i64;
    fn k_mint_into(cnode: usize, bits: usize, index: usize, endpoint: usize, badge: usize) -> i64;
    fn k_tcb_configure(tcb: usize, cnode: usize, bits: usize, fault: usize, vspace: usize) -> i64;
    fn k_tcb_write(tcb: usize, context: *const u64, words: usize) -> i64;
    fn k_tcb_resume(tcb: usize) -> i64;
    fn k_receive(endpoint: usize, badge: *mut usize, mrs: *mut u64, words: usize) -> u64;
    fn k_reply(mrs: *const u64, words: usize);
    fn k_vspace() -> usize;
    fn k_yield();
}
// Object kinds of sel4.c's k_retype, with the size (in address bits) each takes from an untyped.
const UNTYPED: usize = 0;
const TCB: (usize, u8) = (1, 11);
const ENDPOINT: (usize, u8) = (2, 4);
const VSPACE: (usize, u8) = (4, 12);
const TABLE: (usize, u8) = (5, 12);
const FRAME: (usize, u8) = (6, 12);
/// The program's whole capability space: two slots, the second its fault endpoint.
const APP_CNODE_BITS: usize = 1;
const CNODE: (usize, u8) = (3, 5 + APP_CNODE_BITS as u8);
/// Every program's objects come from one untyped of this size (or the
/// largest smaller one there is), revoked when the program ends.
const ARENA_BITS: u8 = 24;
const ARENA_MIN_BITS: u8 = 22;

/// Where this task maps things in its own address space.
const UART_VA: u64 = 0x1000_0000;

struct Uart;
impl linux::Console for Uart {
    fn write(&mut self, bytes: &[u8]) {
        let base = UART_VA as *mut u32;
        for &b in bytes {
            // PL011: wait while the transmit FIFO is full (FR bit 5), then write DR.
            unsafe {
                while base.add(6).read_volatile() & (1 << 5) != 0 {}
                base.write_volatile(u32::from(b));
            }
        }
    }
}
impl Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        linux::Console::write(self, s.as_bytes());
        Ok(())
    }
}

/// The kernel's memory and this task's free capability slots.
struct Sel4 {
    regions: [Untyped; 64],
    count: usize,
    slot: usize,
    slot_end: usize,
    vspace: usize,
}

impl Sel4 {
    fn slot(&mut self) -> Result<usize, MapError> {
        if self.slot >= self.slot_end {
            return Err(MapError::Exhausted);
        }
        self.slot += 1;
        Ok(self.slot - 1)
    }
    /// A new kernel object from ordinary memory.
    fn object(&mut self, (kind, bits): (usize, u8)) -> Result<usize, MapError> {
        loop {
            let index = bootstrap::memory_for(&self.regions[..self.count], bits).ok_or(MapError::Exhausted)?;
            let slot = self.slot()?;
            let user_bits = if kind == CNODE.0 { APP_CNODE_BITS } else if kind == UNTYPED { usize::from(bits) } else { 0 };
            if unsafe { k_retype(self.regions[index].cap, kind, user_bits, slot) } == 0 {
                self.regions[index].take(bits);
                return Ok(slot);
            }
            // The kernel disagrees that it fits: this region is done.
            self.slot -= 1;
            self.regions[index].cursor = self.regions[index].end();
        }
    }
    /// The 4 KiB frame at exactly `paddr`, from device memory.
    fn device_frame(&mut self, paddr: u64) -> Result<usize, MapError> {
        let index = bootstrap::device_for(&self.regions[..self.count], paddr, 12).ok_or(MapError::Exhausted)?;
        while let Some(bits) = self.regions[index].filler(paddr) {
            let slot = self.slot()?;
            if unsafe { k_retype(self.regions[index].cap, UNTYPED, bits as usize, slot) } != 0 {
                return Err(MapError::Exhausted);
            }
            self.regions[index].take(bits);
        }
        let slot = self.slot()?;
        if unsafe { k_retype(self.regions[index].cap, FRAME.0, 0, slot) } != 0 {
            return Err(MapError::Exhausted);
        }
        self.regions[index].take(12);
        Ok(slot)
    }
}

/// The memory and capability slots of the program running now. Every
/// capability its address space, thread and capability space use is made
/// here, so `reset` (one revoke) takes all of them back.
struct Arena {
    memory: Untyped,
    slot_base: usize,
    slot: usize,
    slot_end: usize,
    own: usize,
    /// A revoke failed: nothing is handed out any more (fail closed).
    broken: bool,
}

impl Arena {
    fn slot(&mut self) -> Result<usize, MapError> {
        if self.broken || self.slot >= self.slot_end {
            return Err(MapError::Exhausted);
        }
        self.slot += 1;
        Ok(self.slot - 1)
    }
    fn object(&mut self, (kind, bits): (usize, u8)) -> Result<usize, MapError> {
        if self.memory.clone().take(bits).is_none() {
            return Err(MapError::Exhausted);
        }
        let slot = self.slot()?;
        let user_bits = if kind == CNODE.0 { APP_CNODE_BITS } else { 0 };
        if unsafe { k_retype(self.memory.cap, kind, user_bits, slot) } != 0 {
            self.slot -= 1;
            return Err(MapError::Exhausted);
        }
        self.memory.take(bits);
        Ok(slot)
    }
}

fn map_result(code: i64) -> Result<(), MapError> {
    match code {
        0 => Ok(()),
        1 => Err(MapError::MissingTable),
        2 => Err(MapError::Occupied),
        other => Err(MapError::Kernel(other as u32)),
    }
}

fn map_page(frame: usize, vspace: usize, va: u64, protection: Protection, device: bool) -> Result<(), MapError> {
    let p = match protection {
        Protection::ReadOnly => 0,
        Protection::ReadWrite => 1,
        Protection::ReadExecute => 2,
    };
    map_result(unsafe { k_page_map(frame, vspace, va, p, usize::from(device)) })
}

/// This task's own mappings (the console) come from the boot-time allocator.
impl Kernel for Sel4 {
    fn map_frame(&mut self, frame: usize, vspace: usize, va: u64, _bits: u8, protection: Protection, device: bool) -> Result<(), MapError> {
        map_page(frame, vspace, va, protection, device)
    }
    fn new_table(&mut self) -> Result<usize, MapError> {
        self.object(TABLE)
    }
    fn map_table(&mut self, table: usize, vspace: usize, va: u64) -> Result<(), MapError> {
        map_result(unsafe { k_table_map(table, vspace, va) })
    }
}

impl Kernel for Arena {
    fn map_frame(&mut self, frame: usize, vspace: usize, va: u64, _bits: u8, protection: Protection, device: bool) -> Result<(), MapError> {
        map_page(frame, vspace, va, protection, device)
    }
    fn new_table(&mut self) -> Result<usize, MapError> {
        self.object(TABLE)
    }
    fn map_table(&mut self, table: usize, vspace: usize, va: u64) -> Result<(), MapError> {
        map_result(unsafe { k_table_map(table, vspace, va) })
    }
}

impl Frames for Arena {
    fn frame(&mut self) -> Result<usize, MapError> {
        self.object(FRAME)
    }
    fn copy(&mut self, frame: usize) -> Result<usize, MapError> {
        let slot = self.slot()?;
        if unsafe { k_copy(slot, frame) } != 0 {
            self.slot -= 1;
            return Err(MapError::Exhausted);
        }
        Ok(slot)
    }
    fn unmap(&mut self, frame: usize) {
        unsafe { k_page_unmap(frame) };
    }
    fn unify(&mut self, frame: usize) {
        unsafe { k_page_unify(frame, PAGE as usize) };
    }
    fn own(&self) -> usize {
        self.own
    }
    // Alias addresses are mapped here by the address space before it uses them.
    fn poke(&mut self, va: u64, data: &[u8]) {
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), va as *mut u8, data.len()) };
    }
    fn peek(&self, va: u64, out: &mut [u8]) {
        unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), out.len()) };
    }
    fn zero(&mut self, va: u64, bytes: usize) {
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, bytes) };
    }
    fn reset(&mut self) {
        if unsafe { k_revoke(self.memory.cap) } != 0 {
            self.broken = true;
        }
        self.memory.cursor = self.memory.paddr;
        self.slot = self.slot_base;
    }
}

fn now_ns() -> u64 {
    // The board says how fast the counter runs; the kernel lets user level read it.
    (u128::from(arch::counter()) * 1_000_000_000 / u128::from(board::TIMER_HZ)) as u64
}

enum End {
    Exit(i32),
    Fault(arch::VmFault),
    Other(u64),
}

/// Load `image`, run it to its end, and take back everything it was given:
/// `space::contained` revokes the arena on every way out of `program`.
fn run(arena: &mut Arena, endpoint: usize, pid: i32, image: &[u8], args: &[&[u8]], layout: &Layout) -> Result<End, &'static str> {
    let elf = Elf::parse(NATIVE, image).map_err(|_| "not a static AArch64 program")?;
    space::contained(arena, |arena| program(arena, endpoint, pid, &elf, args, layout))
}

fn program(arena: &mut Arena, endpoint: usize, pid: i32, elf: &Elf, args: &[&[u8]], layout: &Layout) -> Result<End, &'static str> {
    let vspace = arena.object(VSPACE).map_err(|_| "no memory for an address space")?;
    if unsafe { k_asid_assign(vspace) } != 0 {
        return Err("no address space identifier");
    }
    let cnode = arena.object(CNODE).map_err(|_| "no memory for a capability space")?;
    let tcb = arena.object(TCB).map_err(|_| "no memory for a thread")?;
    if unsafe { k_mint_into(cnode, APP_CNODE_BITS, 1, endpoint, pid as usize) } != 0 {
        return Err("fault endpoint");
    }
    let mut space = Space::new(arena, vspace);
    let start = space::load(&mut space, elf, args, layout)?;
    let context = NATIVE.start_context(start.entry, start.sp, 0, 0);
    unsafe {
        if k_tcb_configure(tcb, cnode, APP_CNODE_BITS, 1, vspace) != 0 || k_tcb_write(tcb, context.words.as_ptr(), NATIVE.context_words) != 0 || k_tcb_resume(tcb) != 0 {
            return Err("cannot start the thread");
        }
    }
    let mut process = linux::Process::new(pid, start.image_end, layout.mmap_base, layout.mmap_limit);
    loop {
        let mut mrs = [0u64; arch::SYSCALL_WORDS];
        let mut badge = 0;
        let label = unsafe { k_receive(endpoint, &mut badge, mrs.as_mut_ptr(), mrs.len()) };
        if badge != pid as usize {
            continue;
        }
        match label {
            arch::FAULT_UNKNOWN_SYSCALL => {
                // A number that is no Linux call (the kernel reports one at or
                // above 0x10000 for a negative x8) is ENOSYS, as on Linux.
                let value = match arch::decode_syscall(NATIVE, &mrs) {
                    Some(call) => match process.syscall(&call, &mut space, &mut Uart, now_ns()) {
                        Outcome::Return(value) => value,
                        Outcome::Exit(status) => return Ok(End::Exit(status)),
                    },
                    None => linux::ENOSYS,
                };
                arch::syscall_return(NATIVE, &mut mrs, value);
                unsafe { k_reply(mrs.as_ptr(), arch::SYSCALL_REPLY_WORDS) };
            }
            arch::FAULT_VM => return Ok(End::Fault(arch::decode_vm_fault(NATIVE, &[mrs[0], mrs[1], mrs[2], mrs[3]]))),
            other => return Ok(End::Other(other)),
        }
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    super::sha::sha256(&[data])
}

static APP: &[u8] = include_bytes!(env!("VIOLET_QEMU64_APP"));
static C_APP: &[u8] = include_bytes!(env!("VIOLET_QEMU64_C_APP"));

#[no_mangle]
extern "C" fn root_main(info: *mut u8) -> ! {
    let mut kernel = Sel4 { regions: [Untyped::new(0, 0, 0, true); 64], count: 0, slot: 0, slot_end: 0, vspace: 0 };
    unsafe {
        k_init(info);
        kernel.slot = k_empty_start();
        kernel.slot_end = k_empty_end();
        kernel.vspace = k_vspace();
        for i in 0..k_untyped_count().min(kernel.regions.len()) {
            let (mut paddr, mut bits, mut device) = (0, 0, 0);
            let cap = k_untyped(i, &mut paddr, &mut bits, &mut device);
            kernel.regions[i] = Untyped::new(cap, paddr, bits as u8, device != 0);
            kernel.count = i + 1;
        }
    }
    // Nothing can be reported before the console is mapped; a failure here stops silently.
    let own = kernel.vspace;
    let uart = kernel.device_frame(board::UART_PA).and_then(|frame| arch::map_frame(NATIVE, &mut kernel, own, frame, UART_VA, 12, Protection::ReadWrite, true));
    if uart.is_err() {
        loop {
            unsafe { k_yield() };
        }
    }
    let mut out = Uart;
    let ram: u64 = kernel.regions[..kernel.count].iter().filter(|r| !r.device).map(|r| 1u64 << r.bits).sum();
    let _ = writeln!(out, "VioletOS {}: seL4 {} kernel up, bootstrap task running", board::NAME, NATIVE.name);
    let _ = writeln!(out, "qemu64: {} untyped regions, {} KiB of kernel-managed memory, {} free slots",
                     kernel.count, ram >> 10, kernel.slot_end - kernel.slot);
    let endpoint = match kernel.object(ENDPOINT) {
        Ok(endpoint) => endpoint,
        Err(_) => stop(&mut out, "no fault endpoint"),
    };
    // The programs' arena: one untyped, and every slot left.
    let Some((cap, bits)) = (ARENA_MIN_BITS..=ARENA_BITS).rev().find_map(|bits| kernel.object((UNTYPED, bits)).ok().map(|cap| (cap, bits))) else {
        stop(&mut out, "no memory for programs")
    };
    let mut arena = Arena { memory: Untyped::new(cap, 0, bits, false), slot_base: kernel.slot, slot: kernel.slot, slot_end: kernel.slot_end,
                            own: kernel.vspace, broken: false };
    kernel.slot = kernel.slot_end;
    // AT_RANDOM and each program's layout come from the Open Runtime's
    // generator (runtime/open/entropy.rs), seeded the same way: timer jitter,
    // credited conservatively. Not seeded: no program starts.
    let mut drbg = Drbg::new(sha256);
    let (samples, credited) = entropy::gather(&mut drbg, &mut || {
        unsafe { k_yield() };
        arch::counter() as u32
    });
    let _ = writeln!(out, "qemu64: entropy: {} bits credited to {} timer samples{}", credited, samples,
                     if drbg.seeded() { "" } else { ", too few: no program starts" });
    let runs: [(&[u8], &[&[u8]]); 3] = [(APP, &[b"hello"]), (APP, &[b"hello", b"write-to-code"]), (C_APP, &[b"hello-c", b"musl"])];
    for (i, (image, args)) in runs.iter().enumerate() {
        let mut random = [0u8; 32];
        let result = if drbg.fill(&mut random) {
            run(&mut arena, endpoint, i as i32 + 1, image, args, &Layout::from_random(&random))
        } else {
            Err("no random numbers")
        };
        if arena.broken {
            stop(&mut out, "cannot take a program's memory back");
        }
        match result {
            Ok(End::Exit(status)) => { let _ = writeln!(out, "qemu64: pid {} exited with status {}", i + 1, status); }
            Ok(End::Fault(fault)) => {
                let what = match fault.access { Access::Read => "read", Access::Write => "write", Access::Execute => "execute" };
                let _ = writeln!(out, "qemu64: pid {} stopped: {} fault at {:#x}, pc {:#x}", i + 1, what, fault.address, fault.pc);
            }
            Ok(End::Other(label)) => { let _ = writeln!(out, "qemu64: pid {} stopped: fault label {}", i + 1, label); }
            Err(why) => { let _ = writeln!(out, "qemu64: pid {} did not start: {}", i + 1, why); }
        }
    }
    stop(&mut out, "done")
}

fn stop(out: &mut Uart, why: &str) -> ! {
    let _ = writeln!(out, "qemu64: {}", why);
    loop {
        unsafe { k_yield() };
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    stop(&mut Uart, "bootstrap task panic")
}
