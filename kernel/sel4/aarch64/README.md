# seL4 as an AArch64 kernel (QEMU bring-up)

[build.py](build.py) builds the same pinned seL4 17.0.0 archive as the 32-bit kernels, as an AArch64 kernel for QEMU's `virt` machine, for the [qemu64 target](../../../targets/qemu64/README.md). The 32-bit build ([../build.py](../build.py)) is the default and is not changed by anything here: its platform file, patch, options and [delta.json](../delta.json) are untouched, and its script only forwards the `qemu64` target name.

Like the 32-bit kernels, this is seL4's code and **not a verified seL4 configuration**: the `ellis` platform is a VioletOS board port, the compiler is the NDK's clang, and the source is changed as listed below. It has no handset variant: the secure-monitor extension of the 32-bit handset kernel is not ported, and this kernel contains no SMC instruction (the build checks).

## Every difference from upstream, checked by the build

[delta.json](delta.json) lists them and `build.py` enforces the list on every build with the same code as the 32-bit kernels ([../delta.py](../delta.py)): the prepared tree is compared file by file with the pinned archive, each changed file must be exactly upstream plus its patch or script, and each added file must be its source here. After CMake runs, the build's `CMakeCache.txt` (defaults included, not only the `-D` options) is compared with the nearest verified configuration, `AARCH64_imx8mm_verified`: it must differ in exactly the options `nearest_verified` lists (platform, verification build, EL1 instead of EL2, no SMC forwarding, the boot untyped array, one domain) and have the `beyond` values (virtual counter exported, physical timer not, no MCS, no debug build, a 2^16-slot root CNode). `make test-unit` ([test_delta64.py](test_delta64.py)) checks the manifest against the repository without building. The board device trees enter both kernels; their reviewed hashes are recorded once, in the 32-bit manifest.

### Upstream files changed

| File | Changed by | What changes |
| --- | --- | --- |
| `src/arch/arm/64/traps.S` | [linux-svc.patch](linux-svc.patch) | Seven instructions at the system call entry: `svc #0` is dispatched on x8 instead of x7, and an x8 outside 0..2^31-1 is reported as 0x10000. |
| `libsel4/sel4_arch_include/aarch64/sel4/sel4_arch/syscalls.h` | [linux-svc.patch](linux-svc.patch) | libsel4's seven system call stubs issue `svc #1`. |
| `src/arch/arm/machine/gic_v3.c` | [gic-single-core.py](../../../platform/sel4/board/gic-single-core.py) | As the 32-bit kernels: one redistributor frame per booted core. |
| `tools/hardware.yml` | [gic-single-core.py](../../../platform/sel4/board/gic-single-core.py) | As the 32-bit kernels: the `violet,ellis-boot-gic` device rule. |

### Files added

| File | From | What it is |
| --- | --- | --- |
| `src/plat/ellis/config.cmake` | [config.cmake](config.cmake) | The ellis platform as an AArch64 kernel, QEMU timing only. |
| `src/plat/ellis/kernel-overlay.dts` | [kernel-overlay.dts](../../../platform/sel4/board/kernel-overlay.dts) | The same overlay as the 32-bit kernels. |
| `src/plat/ellis/moto-radio-memory.dts` | [moto-radio-memory.dts](../../../platform/sel4/board/moto-radio-memory.dts) | The same memory reservations, so one memory map serves both. |
| `src/plat/ellis/factory-base.dts` | [qemu.dts](../../../platform/sel4/board/qemu.dts) | The base device tree for QEMU virt. |
| `libsel4/sel4_plat_include/ellis/sel4/plat/api/constants.h` | upstream | A copy of upstream's imx8mm-evk constants. |

### Build options

| Option | Value | Why |
| --- | --- | --- |
| `CMAKE_TOOLCHAIN_FILE` | upstream's llvm.cmake, in the prepared tree | The toolchain file seL4 ships for clang. |
| `TRIPLE` | aarch64-none-elf | The clang target triple. |
| `PYTHON3` | the build's own Python (build/sel4/venv) | The interpreter for the kernel's code generators. |
| `KernelPlatform` | ellis | The VioletOS board port. |
| `KernelSel4Arch` | aarch64 | 64-bit kernel. |
| `KernelVerificationBuild` | OFF | Not a verification build. |
| `KernelDebugBuild` | OFF | No debug syscalls. |
| `KernelPrinting` | OFF | No kernel console. |
| `KernelIsMCS` | OFF | As the default 32-bit kernel. |
| `KernelMaxNumNodes` | 1 | One core. |
| `KernelArmExportVCNTUser` | ON | User level reads the virtual counter. |
| `KernelArmExportPTMRUser` | OFF | No user thread may program the EL0 physical timer (it is not switched between threads). |
| `KernelArmHypervisorSupport` | OFF | The kernel runs at EL1. |
| `KernelRootCNodeSizeBits` | 16 | The supervisor's root CNode: 65536 slots, for its device windows in 4 KiB frames (AArch64 has no 64 KiB or 1 MiB pages). |

## Why the system call entry is patched

Open apps are Linux programs and make system calls with `svc #0`. On AArch32 the Linux number is in r7 and seL4's own numbers are negative, so every Linux call reaches the app's fault handler (the Open Runtime) as an unknown-syscall fault with no kernel change. On AArch64 Linux puts the number in x8 and leaves x7 as an ordinary scratch register, while seL4 dispatches on x7. Whenever x7 happened to hold one of seL4's numbers (-1 to -8), an unpatched kernel would run its own call instead of delivering the Linux one; three of them (Yield, Reply, NBSend) return without any error, so the Linux call would be lost silently.

The patch separates the two by the instruction's immediate, which the kernel already has in ESR_EL1: `svc #0` is dispatched on x8, so any Linux number (0 to 2^31-1) is an unknown-syscall fault whose message carries x8 as the number; native seL4 calls use `svc #1` and are dispatched on x7 as before. The kernel's C entry takes the number as an `int`, and its own numbers are negative, so an x8 that is negative or has a negative low half (0x1_ffff_ffff, say) would otherwise run a native call: the patch passes every x8 of 2^31 or more as 0x10000, which is neither a Linux nor a kernel number, so `svc #0` never runs a native call whatever x7 and x8 hold. The bootstrap task answers that number (and any other it does not know) with ENOSYS, as Linux does.

It is unverified code in the kernel's trap path: seven instructions. The qemu64 test program makes a Linux call with each of the eight native numbers in x7 on the built kernel and they all return correctly; it has not been run against an unpatched kernel to show the failure.
