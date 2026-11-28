# seL4 as an AArch64 kernel (QEMU and the handset)

[build.py](build.py) builds the same pinned seL4 17.0.0 archive as the 32-bit kernels, as an AArch64 kernel for QEMU's `virt` machine, for the [qemu64 target](../../../targets/qemu64/README.md), and with `--target moto` for the XT2163DL handset ([targets/moto-g-pure](../../../targets/moto-g-pure/README.md#aarch64-boot-path), `make moto64`). The 32-bit build ([../build.py](../build.py)) is the default and is not changed by anything here: its platform file, patch, options and [delta.json](../delta.json) are untouched, and its script only forwards the `qemu64` target name.

Like the 32-bit kernels, this is seL4's code and **not a verified seL4 configuration**: the `ellis` platform is a VioletOS board port, the compiler is the NDK's clang, and the source is changed as listed below. Its handset variant differs from the QEMU one in its base device tree (decompiled from the factory boot image, pinned by that image's SHA-256, as the 32-bit handset kernel's), its 13 MHz timer and the Wi-Fi secure-monitor extension's stubs: both kernels carry the 32-bit handset kernel's extension ([ellis-wifi.patch](ellis-wifi.patch), the same `table.h` and `invoke.h`: four secure-monitor calls with fixed arguments behind one root capability in the `seL4_CapSMC` slot), but only the handset's assembles the A64 stubs ([monitor.S](monitor.S)); QEMU's gets C stubs that fail, as the 32-bit QEMU kernel does. The build checks that the only SMC instructions are those four stubs on the handset and that there are none on QEMU.

## Every difference from upstream, checked by the build

[delta.json](delta.json) lists them and `build.py` enforces the list on every build with the same code as the 32-bit kernels ([../delta.py](../delta.py)): the prepared tree is compared file by file with the pinned archive, each changed file must be exactly upstream plus its patch or script, and each added file must be its source here. After CMake runs, the build's `CMakeCache.txt` (defaults included, not only the `-D` options) is compared with the nearest verified configuration, `AARCH64_imx8mm_verified`: it must differ in exactly the options `nearest_verified` lists (platform, verification build, EL1 instead of EL2, no SMC forwarding, the boot untyped array, one domain) and have the `beyond` values (virtual counter exported, physical timer not, no MCS, no debug build, a 2^16-slot root CNode). `make test-unit` ([test_delta64.py](test_delta64.py)) checks the manifest against the repository without building. The board device trees enter both kernels; their reviewed hashes are recorded once, in the 32-bit manifest.

### Upstream files changed

| File | Changed by | What changes |
| --- | --- | --- |
| `src/arch/arm/64/traps.S` | [linux-svc.patch](linux-svc.patch) | Seven instructions at the system call entry: `svc #0` is dispatched on x8 instead of x7, and an x8 outside 0..2^31-1 is reported as 0x10000. |
| `libsel4/sel4_arch_include/aarch64/sel4/sel4_arch/syscalls.h` | [linux-svc.patch](linux-svc.patch) | libsel4's seven system call stubs issue `svc #1`. |
| `src/arch/arm/machine/gic_v3.c` | [gic-single-core.py](../../../platform/sel4/board/gic-single-core.py) | As the 32-bit kernels: one redistributor frame per booted core. |
| `tools/hardware.yml` | [gic-single-core.py](../../../platform/sel4/board/gic-single-core.py) | As the 32-bit kernels: the `violet,ellis-boot-gic` device rule. |
| `include/arch/arm/arch/64/mode/object/structures.bf` | [ellis-wifi.patch](ellis-wifi.patch) | Adds `ellis_wifi_cap` (arch cap tag 29) on ellis. |
| `include/arch/arm/arch/64/mode/object/structures.h` | [ellis-wifi.patch](ellis-wifi.patch) | The cap has no size, object or region, like the ASID control cap. |
| `src/arch/arm/kernel/boot.c` | [ellis-wifi.patch](ellis-wifi.patch) | Puts it in the root task's `seL4_CapSMC` slot (`KernelAllowSMCCalls` stays OFF). |
| `src/arch/arm/64/object/objecttype.c` | [ellis-wifi.patch](ellis-wifi.patch) | Deriving it gives a null cap; invoking it runs `ellisWifiInvocation`. |

### Files added

| File | From | What it is |
| --- | --- | --- |
| `src/plat/ellis/config.cmake` | [config.cmake](config.cmake) | The ellis platform as an AArch64 kernel: QEMU's 62.5 MHz timer, or the handset's 13 MHz with `VioletHandsetBoard`. |
| `src/plat/ellis/kernel-overlay.dts` | [kernel-overlay.dts](../../../platform/sel4/board/kernel-overlay.dts) | The same overlay as the 32-bit kernels. |
| `src/plat/ellis/moto-radio-memory.dts` | [moto-radio-memory.dts](../../../platform/sel4/board/moto-radio-memory.dts) | The same memory reservations, so one memory map serves both. |
| `src/plat/ellis/factory-base.dts` | [qemu.dts](../../../platform/sel4/board/qemu.dts) | The base device tree for QEMU virt; for the handset, the factory boot image's, decompiled at build time and pinned by that image's SHA-256. |
| `src/plat/ellis/wifi/monitor.S` | [monitor.S](monitor.S) | The four MediaTek SiP call stubs in A64, assembled for the handset only. |
| `include/arch/arm/arch/object/ellis_wifi/invoke.h` | [invoke.h](../../../platform/sel4/board/wifi/invoke.h) | The 32-bit kernel's `ellisWifiInvocation`. |
| `include/arch/arm/arch/object/ellis_wifi/table.h` | [table.h](../../../platform/sel4/board/wifi/table.h) | The four permitted operations and their fixed monitor arguments. |
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
| `KernelArmHypervisorSupport` | OFF | The kernel runs at EL1 (on the handset the 64-bit loader drops from EL2 to EL1 first). |
| `VioletHandsetBoard` | ON for the handset, OFF for QEMU | Selects the handset's 13 MHz timer. |
| `KernelRootCNodeSizeBits` | 16 | The supervisor's root CNode: 65536 slots, for its device windows in 4 KiB frames (AArch64 has no 64 KiB or 1 MiB pages). |

## Why the system call entry is patched

Open apps are Linux programs and make system calls with `svc #0`. On AArch32 the Linux number is in r7 and seL4's own numbers are negative, so every Linux call reaches the app's fault handler (the Open Runtime) as an unknown-syscall fault with no kernel change. On AArch64 Linux puts the number in x8 and leaves x7 as an ordinary scratch register, while seL4 dispatches on x7. Whenever x7 happened to hold one of seL4's numbers (-1 to -8), an unpatched kernel would run its own call instead of delivering the Linux one; three of them (Yield, Reply, NBSend) return without any error, so the Linux call would be lost silently.

The patch separates the two by the instruction's immediate, which the kernel already has in ESR_EL1: `svc #0` is dispatched on x8, so any Linux number (0 to 2^31-1) is an unknown-syscall fault whose message carries x8 as the number; native seL4 calls use `svc #1` and are dispatched on x7 as before. The kernel's C entry takes the number as an `int`, and its own numbers are negative, so an x8 that is negative or has a negative low half (0x1_ffff_ffff, say) would otherwise run a native call: the patch passes every x8 of 2^31 or more as 0x10000, which is neither a Linux nor a kernel number, so `svc #0` never runs a native call whatever x7 and x8 hold. The bootstrap task answers that number (and any other it does not know) with ENOSYS, as Linux does.

It is unverified code in the kernel's trap path: seven instructions. The qemu64 test program makes a Linux call with each of the eight native numbers in x7 on the built kernel and they all return correctly; it has not been run against an unpatched kernel to show the failure.
