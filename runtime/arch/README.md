# Architecture module

Everything that differs between the AArch32 system VioletOS ships and the AArch64 port in progress, in one place: [mod.rs](mod.rs) and the generated [linux.rs](linux.rs). It is pure `no_std` Rust with no kernel access, so `make test-unit` runs all of it on the host for both architectures ([tests/host.rs](tests/host.rs), through [test_host.sh](test_host.sh)), and it compiles for both `armv7a-none-eabi` and `aarch64-unknown-none`.

| Piece | What it holds |
| --- | --- |
| `Abi` (`AARCH32`, `AARCH64`, `NATIVE`) | Word size, ELF class and machine, top of user space, stack alignment, the frame sizes the kernel offers, the page-table levels, where the program counter, stack pointer, status register and thread pointer sit in the kernel's thread context. |
| `decode_syscall`, `syscall_return` | A thread's Linux system call out of the kernel's unknown-syscall fault message, and the reply that returns from it. AArch32: the number is r7 and the instruction is 4 or 2 bytes (Thumb). AArch64: the number is x8, which needs the [kernel patch](../../kernel/sel4/aarch64/README.md). |
| `decode_vm_fault` | Read, write or execute, from DFSR (AArch32) or ESR_EL1 (AArch64). |
| `map_frame` and the `Kernel` trait | Map one frame, creating the page tables the address still lacks: one level on AArch32, up to three on AArch64. The kernel is behind a three-method trait; the tests supply a model of seL4's page tables. |
| `Elf` | ELF32 and ELF64 static executables to the same segment list. Refuses the other architecture's programs, position-independent ones, writable-and-executable segments, segments outside user space (empty ones included, which are then never handed to a loader) and two segments that share a page, so a page-granular loader never merges protections. `Segment::page_span` is the checked page rounding a loader uses. |
| `build_stack` | The initial stack (arguments and auxiliary vector) in the ABI's word size and alignment. |
| `counter`, `counter_frequency` | The generic timer's virtual counter: CP15 on AArch32, a system register on AArch64. |
| `linux` | Linux system call numbers of both ABIs, generated from the kernel's own headers by [gen_linux.py](gen_linux.py), with the AArch32 call each AArch64 call corresponds to (AArch64's `clock_gettime` is AArch32's `clock_gettime64`, `mmap` is `mmap2`, and so on) and whether its arguments and structures have the same layout (102 calls) or differ with the word size (50). |

## Who uses it

Today only the 64-bit bring-up target ([targets/qemu64](../../targets/qemu64/README.md)), whose bootstrap task runs this code on a real AArch64 seL4 kernel. **The Supervisor, the services and the Open Runtime do not use it yet**: they remain AArch32 code and are unchanged. Moving them onto this module is the next step of the port; nothing here changes the 32-bit image.

## How far the existing code is from AArch64

Measured by compiling the current sources for AArch64 (Rust: `rustc --target=aarch64-unknown-none`, full code generation; C: clang against the AArch64 kernel's libsel4), not by running them:

| Layer | Result |
| --- | --- |
| Component crates: launcher, runtime, touch, display, storage, radio, audio, camera, package manager | Compile unchanged (9 of 11). |
| Power | 2 errors, one line: the CP15 counter read in `services/power/process.rs`. |
| Open Runtime | 3 type errors (`entropy::boot` and `entropy::collect` exist only for `target_arch = "arm"`), then the CP15 reads in `sys.rs`. Its logic is AArch32 throughout: ELF32 loader, 64 KiB pool pages, AArch32 system call numbers and structure layouts, a 19-word thread context. |
| C shims: `events.c`, `storage_ipc.c`, `watchdog.c`, `fixture.c`, `strings.c` | Compile unchanged. |
| `runtime/transport.c` | 8 errors, all CP15 instructions in its clock. |
| `runtime/open/sel4.c` | 1 error: `seL4_ARM_PageDirectory_Clean_Data` has another name on AArch64. |
| `system/supervisor/supervisor.c` with `platform/sel4/board/memory.h` | 7 errors (section and supersection frame types, r0/r1 register names) and 13 warnings. |

Compiling is the small part. What the compiler does not see: the supervisor's allocator tracks one page-table level and names capabilities with 32-bit depths; `seL4_ARM_LargePageObject` is 64 KiB on AArch32 and 2 MiB on AArch64, so every 64 KiB device and pool mapping changes meaning silently; and every `u32` that holds an address is correct only while all mappings stay below 4 GiB (the board descriptions keep that rule).
