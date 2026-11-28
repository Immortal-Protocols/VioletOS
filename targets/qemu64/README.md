# qemu64: the AArch64 bring-up target

VioletOS ships as an AArch32 system. This target moves it to AArch64 on QEMU `virt`, built beside the default one and changing nothing in it. It has two images: the bring-up image (a bootstrap task and two test programs, the first sections below), and the whole VioletOS system ([The VioletOS system on AArch64](#the-violetos-system-on-aarch64)). `make qemu64` builds the first and boots it in `qemu-system-aarch64`; `make qemu64 QEMU64_STACK=1` also builds, boots and checks the second. CI and `make test-local` neither build nor boot either.

```sh
make qemu64-image     # build/sel4/qemu64/qemu64.bin
make qemu64           # build, boot, and check the console lines below
```

It needs what `make qemu-image` needs, plus the Rust target `aarch64-unknown-none` (`rustup target add aarch64-unknown-none`).

## What runs in the bring-up image

| Layer | On qemu64 |
| --- | --- |
| Kernel | The same pinned seL4 17.0.0, built as an AArch64 kernel for the same `ellis` device trees ([kernel/sel4/aarch64](../../kernel/sel4/aarch64/README.md)). One patch: Linux system calls are dispatched on x8. |
| Boot | [stage.S](stage.S) and [entry.S](entry.S): QEMU starts the CPU at EL1; the stub turns the MMU on with boot page tables [image.py](image.py) generates and enters the kernel at its link address. No seL4 elfloader, no Microkit. |
| First task | [root.rs](root.rs), a 64-bit **bootstrap task written for this target. It is not the Supervisor.** It carves the kernel's untyped memory, maps the serial port, and for each test program builds a 4-level address space, loads a static ELF64, starts a thread and answers its Linux system calls and faults. The address space and loader are [space.rs](space.rs), pure code behind a narrow kernel trait. |
| Services | **None.** No Display, Touch, Storage, Radio, Power or Audio process runs. |
| Open Runtime | **Not ported.** [linux.rs](linux.rs) answers 13 Linux calls (write, writev, exit, exit_group, getpid, gettid, set_tid_address, ioctl, brk, mmap, munmap, mprotect, clock_gettime); everything else is ENOSYS, named on the console, and a number that is no Linux number at all is ENOSYS too. `mmap(PROT_NONE)` reserves its range unmapped; `mprotect(PROT_NONE)` is refused (EINVAL). No files, threads, signals, policy, Shield or display. |
| Programs | [app/hello.rs](app/hello.rs): a static program with no C library that makes raw `svc #0` calls. [app/hello.c](app/hello.c): an ordinary C program linked statically against musl 1.2.5 built for AArch64 (ARMv8.0, no LSE atomics, as the handset's Cortex-A53 needs). |

The architecture-specific work the bootstrap task does (decoding system calls and faults, thread contexts, page-table levels, ELF64, the initial stack) is the shared [architecture module](../../runtime/arch/README.md), so the Supervisor and the Open Runtime can adopt the same code. That README also measures how far the existing 32-bit sources are from compiling for AArch64.

## What a correct boot prints

[run.py](run.py) boots the image and, with `--expect`, requires these lines in order (the list is `EXPECTED` there):

```
VioletOS qemu64: seL4 aarch64 kernel up, bootstrap task running
                                                    (then "qemu64: entropy: N bits credited to M timer samples")
hello from a static AArch64 Linux program
argc and pid: ok
x7 never selects a kernel call: ok                 Linux calls made with each seL4 call number in x7
registers, FP and TPIDR_EL0 survive a call: ok     x1-x15, x20-x28, d8, d31 and the thread pointer
brk and mmap give zeroed writable memory: ok
writable and executable is refused: ok             mmap and mprotect with PROT_WRITE|PROT_EXEC
writev with 64-bit iovec: ok
monotonic clock with 64-bit timespec: ok
EFAULT, EBADF and ENOSYS: ok
qemu64: pid 1 exited with status 0
writing to the program's code                       the second run writes to its own code page
qemu64: pid 2 stopped: write fault at 0x400000, ...
hello from musl on AArch64: hello-c, 1 argument
thread-local storage: ok                            musl sets TPIDR_EL0 itself; the context keeps it
malloc and free: ok                                 brk for small blocks, mmap for 1 MiB
clock_gettime: ok
no file system: fopen fails with ENOSYS: ok
64-bit: sizeof(long) 8, sizeof(void *) 8, pid 3
qemu64: pid 3 exited with status 0
qemu64: done
```

## The VioletOS system on AArch64

[stack.py](stack.py) builds the real system for this machine, beside the bring-up image: `make qemu64-image QEMU64_STACK=1` (or `python3 tools/build_sel4.py qemu-aarch64`) writes `build/sel4/qemu-aarch64`, and `VIOLET_ARCH=aarch64 python3 tools/run.py VIEWER` boots it with the AArch32 emulator's devices, fixtures and blobs (the same addresses: both kernels use the same device trees). It is the QEMU half of `platform/sel4/loader/build.py` with `VIOLET_ARCH=aarch64`:

- the same delta-checked kernel and entry stub as above ([build.py](build.py) `build_kernel`, `boot_image`), with the supervisor as the root task, entered at [root64.S](../../platform/sel4/loader/root64.S) with x0 = BootInfo and its stack at the top of its 1.5 MiB window;
- the services, Open Runtime and Sentinel from [image/packages/build.py](../../image/packages/build.py) for `aarch64-unknown-none`, linked with 4 KiB pages, each package's signed ABI word marked AArch64 so neither supervisor loads the other's;
- C built with `-mgeneral-regs-only -mstrict-align` (the supervisor reads catalogs and keys in windows mapped as device memory);
- the open apps from [tools/build_initfs.py](../../tools/build_initfs.py) for `aarch64-unknown-linux-musl`, Go `linux/arm64` and an AArch64 musl and Python from [tools/build_python.py](../../tools/build_python.py) (`build/open-toolchain/aarch64`), all ARMv8.0 (no LSE atomics).

`make qemu64 QEMU64_STACK=1` then boots it and checks it layer by layer (`python3 targets/qemu/test_system.py --arch aarch64`, about 5 s): the supervisor starts every service, Storage serves `/data`, the Open Runtime reads the installed apps and the policy manifests, unpacks `/apps`, seeds its random generator, starts Home as a 64-bit program, Home shows, the screen is drawn and the Sentinel answers; then Home opens [test-apps/abi64](test-apps/abi64/main.rs), a 64-bit Rust program whose signal handlers run through the A64 gate (a handler by `signal`, an `SA_SIGINFO` handler's 64-bit siginfo, a SIGSEGV handler that makes the page writable and returns to the faulting write with the SIMD registers intact) and which starts itself with `std::process::Command` (posix_spawn: clone with `CLONE_VM|CLONE_VFORK`, execve, pipes, wait4). stack.py puts that test program in this development image, as the AArch32 test image has its own. It then runs `targets/qemu/test_open_apps.py --arch aarch64` (two boots, about 20 s): Home's Settings icon opens Settings, Date & Time picks Tokyo, the Wi-Fi page scans, Agent Apps opens, Music starts, and after a restart Settings still has Tokyo. `targets/qemu/test_ladybird.py --arch aarch64` (about 40 s) drives the browser: HTTP links, a linked style sheet, back, scrolling, a typed address and HTTPS. What it reaches today:

| Layer | AArch64 |
| --- | --- |
| Kernel, supervisor, services, Sentinel | run; every package carries the AArch64 ABI word and the supervisor takes only those |
| Open Runtime | starts, unpacks `/apps`, draws its screen; **polls** instead of sleeping on its timer (the AArch64 kernel does not export the EL0 physical timer: kernel/sel4/aarch64 `KernelArmExportPTMRUser=OFF`, so the timekeeper is off too) |
| Home, Settings, Music, Ladybird | built for `aarch64-unknown-linux-musl` (Rust 1.98), sealed (tools/relro_seal.rs `start64`), checked by the release gate (tools/elf_check.py reads ELF64), loaded by the Open Runtime below 4 GiB (elf.rs `narrow`) and served by its one dispatcher, policy and Shield: each AArch64 call is translated to the AArch32 call of the same meaning ([runtime/arch/enter64.rs](../../runtime/arch/enter64.rs), [runtime/open/abi64.rs](../../runtime/open/abi64.rs)), and the thread gets its own 64-bit registers back (runtime/open/sys.rs `calls`) |

Not yet on AArch64 (named in `violet_arch.APP_ABI_UNPORTED`): programs are placed by the AArch32 layout below 4 GiB (16 to 24 bits of randomization per area, not `vma::Layout64`'s 47-bit layout); fork (not vfork) answers ENOSYS; calls whose structures are not converted (recvmmsg, timer_create, itimers...) answer ENOSYS and are logged by number.

The build stops, naming the layer, while one is not ported (`violet_arch.UNPORTED`, now empty), and refuses any AArch64 program the release gate cannot read. The image has **no verified boot**: the entry stub checks no manifest.

Security differences from AArch32, written by `stack.py` to `differences.json` and printed by `test_system.py --arch aarch64` as absent layers (never as passes; the test refuses an image without the file, and checks the Open Runtime's timer line against it):

- **Verified boot: absent.** The kernel and the supervisor load unverified (`boot/loader/verified_lib.rs` is not built for AArch64); the supervisor still checks every package signature. The handset keeps its `#error` until a 64-bit loader verifies the manifest.
- **No user timer.** The kernel keeps `KernelArmExportPTMRUser=OFF` (one EL0 physical timer that it does not switch between threads would let any app move another's wake-up), so `vm.h` `VM_USER_TIMER` is 0: the supervisor's timekeeper and its stuck-WFI guard are compiled out, and the Open Runtime gets no IRQ 30 and polls at a fixed priority. A weaker denial-of-service posture than AArch32's until a per-thread timer or the kernel's timeout API replaces it. `stack.py` stops if the `gen_config.h` the supervisor compiles against disagrees with the kernel's `CMakeCache.txt` (a stale tree with the timer on would build a supervisor whose `cntp_*` writes trap). [toolchain.py](toolchain.py) `--check` says which Rust targets are missing; without `--check` it adds them to a copy of the pinned toolchain under `build/rust-aarch64` (and, with `--ladybird`, to Ladybird's).

## Limits

- Containment between programs: every capability a program uses (its frames and their aliases in the bootstrap task, all page tables, its address space, thread and capability space) is made from one arena untyped of 4 to 16 MiB, and `space::contained` revokes it on every way out of a program, failed loads included, so a program cannot leave mappings or used memory behind for the next one. Within a program, an unmapped page's frame is kept (zeroed on reuse) for its next page, so mapping and unmapping in a loop holds at most 512 frames; the arena bounds the rest (page tables). If a revoke ever fails, no further program starts.
- Randomness: before the first program, the bootstrap task seeds the Open Runtime's HMAC_DRBG ([runtime/open/entropy.rs](../../runtime/open/entropy.rs), with Storage's SHA-256) from timer jitter, credited as conservatively as the Open Runtime does. Each program then gets its own 16 AT_RANDOM bytes (musl's stack-protector canary and pointer guard), a stack top at a random page in the 128 MiB below 2 GiB, and anonymous mappings from a random page in a 1 GiB window. Unseeded: no program starts. Static programs (ET_EXEC) still load at their link address; there is no hardware entropy source on this machine model, so the seed is only as good as the jitter.
- A program's only capability is a badged endpoint through which the kernel delivers its faults; with a native `svc #1` it can send its own messages there. The bootstrap task trusts the fault label of what arrives; a real runtime must not.
- Executable pages are cache-synchronised (`seL4_ARM_Page_Unify_Instruction`) after loading, when made executable by mprotect, and when mapped executable. Plain QEMU does not model caches, so the boot test cannot show this is needed or sufficient; it is unverified on hardware.
- No device tree is passed to the kernel, and no handset image exists: the handset's loader, its secure-monitor kernel extension and its drivers are 32-bit only.

## Tests

`make test-unit` runs the host tests in seconds: the untyped-memory planning, every Linux call, and the address space and loader against a model of the kernel's capabilities and mappings ([tests/host.rs](tests/host.rs): failed loads leave nothing behind, a half-done mapping is undone, brk and mmap loops reuse frames, code pages are cache-synchronised, AT_RANDOM and AT_PHDR), and the image layout and boot page tables ([test_image.py](test_image.py)). The boot itself is `make qemu64`, run locally.
