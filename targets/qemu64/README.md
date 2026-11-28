# qemu64: the AArch64 bring-up target

VioletOS is an AArch32 system today. This target is the first step of moving it to AArch64, built beside the default one and changing nothing in it. `make qemu64` builds the image and boots it in `qemu-system-aarch64`; CI neither builds nor boots it.

```sh
make qemu64-image     # build/sel4/qemu64/qemu64.bin
make qemu64           # build, boot, and check the console lines below
```

It needs what `make qemu-image` needs, plus the Rust target `aarch64-unknown-none` (`rustup target add aarch64-unknown-none`).

## What runs

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

## Limits

- Containment between programs: every capability a program uses (its frames and their aliases in the bootstrap task, all page tables, its address space, thread and capability space) is made from one arena untyped of 4 to 16 MiB, and `space::contained` revokes it on every way out of a program, failed loads included, so a program cannot leave mappings or used memory behind for the next one. Within a program, an unmapped page's frame is kept (zeroed on reuse) for its next page, so mapping and unmapping in a loop holds at most 512 frames; the arena bounds the rest (page tables). If a revoke ever fails, no further program starts.
- Randomness: before the first program, the bootstrap task seeds the Open Runtime's HMAC_DRBG ([runtime/open/entropy.rs](../../runtime/open/entropy.rs), with Storage's SHA-256) from timer jitter, credited as conservatively as the Open Runtime does. Each program then gets its own 16 AT_RANDOM bytes (musl's stack-protector canary and pointer guard), a stack top at a random page in the 128 MiB below 2 GiB, and anonymous mappings from a random page in a 1 GiB window. Unseeded: no program starts. Static programs (ET_EXEC) still load at their link address; there is no hardware entropy source on this machine model, so the seed is only as good as the jitter.
- A program's only capability is a badged endpoint through which the kernel delivers its faults; with a native `svc #1` it can send its own messages there. The bootstrap task trusts the fault label of what arrives; a real runtime must not.
- Executable pages are cache-synchronised (`seL4_ARM_Page_Unify_Instruction`) after loading, when made executable by mprotect, and when mapped executable. Plain QEMU does not model caches, so the boot test cannot show this is needed or sufficient; it is unverified on hardware.
- No device tree is passed to the kernel, and no handset image exists: the handset's loader, its secure-monitor kernel extension and its drivers are 32-bit only.

## Tests

`make test-unit` runs the host tests in seconds: the untyped-memory planning, every Linux call, and the address space and loader against a model of the kernel's capabilities and mappings ([tests/host.rs](tests/host.rs): failed loads leave nothing behind, a half-done mapping is undone, brk and mmap loops reuse frames, code pages are cache-synchronised, AT_RANDOM and AT_PHDR), and the image layout and boot page tables ([test_image.py](test_image.py)). The boot itself is `make qemu64`, run locally.
