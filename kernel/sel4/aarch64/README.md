# seL4 as an AArch64 kernel (QEMU and the handset)

[build.py](build.py) builds the same pinned seL4 17.0.0 archive as the 32-bit kernels, as an AArch64 kernel for QEMU's `virt` machine, for the [qemu64 target](../../../targets/qemu64/README.md), and with `--target moto` for the XT2163DL handset ([targets/moto-g-pure](../../../targets/moto-g-pure/README.md#aarch64-boot-path), `make moto64`). The 32-bit build ([../build.py](../build.py)) is the default and is not changed by anything here: its platform file, patch, options and [delta.json](../delta.json) are untouched, and its script only forwards the `qemu64` target name.

Like the 32-bit kernels, this is seL4's code and **not a verified seL4 configuration**: the `ellis` platform is a VioletOS board port, the compiler is the NDK's clang, and the source is changed as listed below. Its handset variant differs from the QEMU one in its base device tree (decompiled from the factory boot image, pinned by that image's SHA-256, as the 32-bit handset kernel's), its 13 MHz timer and the Wi-Fi secure-monitor extension's stubs: both kernels carry the 32-bit handset kernel's extension ([ellis-wifi.patch](ellis-wifi.patch), the same `table.h` and `invoke.h`: four secure-monitor calls with fixed arguments behind one root capability in the `seL4_CapSMC` slot), but only the handset's assembles the A64 stubs ([monitor.S](monitor.S)); QEMU's gets C stubs that fail, as the 32-bit QEMU kernel does. The build checks that the only SMC instructions are those four stubs on the handset and that there are none on QEMU.

## Every difference from upstream, checked by the build

[delta.json](delta.json) lists them and `build.py` enforces the list on every build with the same code as the 32-bit kernels ([../delta.py](../delta.py)): the prepared tree is compared file by file with the pinned archive, each changed file must be exactly upstream plus its patch or script, and each added file must be its source here. After CMake runs, the build's `CMakeCache.txt` (defaults included, not only the `-D` options) is compared with the nearest verified configuration, `AARCH64_imx8mm_verified`: it must differ in exactly the options `nearest_verified` lists (platform, verification build, EL1 instead of EL2, no SMC forwarding, the boot untyped array, one domain) and have the `beyond` values (virtual counter exported, physical timer not exported to every thread, no MCS, no debug build, a 2^16-slot root CNode). `make test-unit` ([test_delta64.py](test_delta64.py)) checks the manifest against the repository without building. The board device trees enter both kernels; their reviewed hashes are recorded once, in the 32-bit manifest.

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
| `libsel4/include/sel4/constants.h` | [ellis-timer.patch](ellis-timer.patch) | Adds `seL4_TCBFlag_ellisTimer` (0x2), in `seL4_TCBFlag_MASK` with `CONFIG_ELLIS_TIMER_GRANT`. |
| `src/object/tcb.c` | [ellis-timer.patch](ellis-timer.patch) | TCB SetFlags refuses (IllegalOperation) to set or clear that flag unless the calling thread has it; on the running thread it takes effect at once. |
| `src/arch/arm/64/kernel/thread.c` | [ellis-timer.patch](ellis-timer.patch) | `Arch_switchToThread` sets `CNTKCTL_EL1.EL0PTEN` for a flagged thread and clears it for any other. |
| `include/arch/arm/arch/64/mode/fastpath/fastpath.h` | [ellis-timer.patch](ellis-timer.patch) | The same in the IPC fastpath's switch (`switchToThread_fp`). |
| `src/kernel/boot.c` | [ellis-timer.patch](ellis-timer.patch) | The root task (the supervisor) starts with the flag. |

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
| `include/arch/arm/arch/object/ellis_timer.h` | [ellis_timer.h](ellis_timer.h) | `ellisTimerAccess`: `CNTKCTL_EL1.EL0PTEN` from the thread's flag (two system register accesses). |

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
| `KernelArmExportPTMRUser` | OFF | The EL0 physical timer is not exported to every thread (it is not switched between threads); [ellis-timer.patch](ellis-timer.patch) gives it to flagged threads only. |
| `KernelArmHypervisorSupport` | OFF | The kernel runs at EL1 (on the handset the 64-bit loader drops from EL2 to EL1 first). |
| `VioletHandsetBoard` | ON for the handset, OFF for QEMU | Selects the handset's 13 MHz timer. |
| `KernelRootCNodeSizeBits` | 16 | The supervisor's root CNode: 65536 slots, for its device windows in 4 KiB frames (AArch64 has no 64 KiB or 1 MiB pages). |

## Why the system call entry is patched

Open apps are Linux programs and make system calls with `svc #0`. On AArch32 the Linux number is in r7 and seL4's own numbers are negative, so every Linux call reaches the app's fault handler (the Open Runtime) as an unknown-syscall fault with no kernel change. On AArch64 Linux puts the number in x8 and leaves x7 as an ordinary scratch register, while seL4 dispatches on x7. Whenever x7 happened to hold one of seL4's numbers (-1 to -8), an unpatched kernel would run its own call instead of delivering the Linux one; three of them (Yield, Reply, NBSend) return without any error, so the Linux call would be lost silently.

The patch separates the two by the instruction's immediate, which the kernel already has in ESR_EL1: `svc #0` is dispatched on x8, so any Linux number (0 to 2^31-1) is an unknown-syscall fault whose message carries x8 as the number; native seL4 calls use `svc #1` and are dispatched on x7 as before. The kernel's C entry takes the number as an `int`, and its own numbers are negative, so an x8 that is negative or has a negative low half (0x1_ffff_ffff, say) would otherwise run a native call: the patch passes every x8 of 2^31 or more as 0x10000, which is neither a Linux nor a kernel number, so `svc #0` never runs a native call whatever x7 and x8 hold. The bootstrap task answers that number (and any other it does not know) with ENOSYS, as Linux does.

It is unverified code in the kernel's trap path: seven instructions. The qemu64 test program makes a Linux call with each of the eight native numbers in x7 on the built kernel and they all return correctly; it has not been run against an unpatched kernel to show the failure.

## Why the physical timer is given per thread

The supervisor's timekeeper ([system/supervisor/timekeeper_thread.h](../../../system/supervisor/timekeeper_thread.h)) sleeps every service and the Open Runtime on one hardware timer, the EL1 physical timer (`CNTP_*`, PPI 30). The AArch32 kernels export it to all of user level (`KernelArmExportPTMRUser=ON`), so any open app can stop or move it, and the supervisor runs a guard thread to recover. Upstream's option is all or nothing, so the AArch64 kernels keep it OFF and [ellis-timer.patch](ellis-timer.patch) adds a narrower grant instead (`KernelEllisTimerGrant`, set by [config.cmake](config.cmake)):

- A TCB flag, `seL4_TCBFlag_ellisTimer`, beside upstream's `seL4_TCBFlag_fpuDisabled`, changed with upstream's `seL4_TCB_SetFlags`.
- On every switch of the running thread (`Arch_switchToThread` and the IPC fastpath's `switchToThread_fp`), [ellis_timer.h](ellis_timer.h) sets `CNTKCTL_EL1.EL0PTEN` if the new thread has the flag and clears it otherwise. With it clear, an EL0 read or write of `CNTP_CTL_EL0`, `CNTP_TVAL_EL0` or `CNTP_CVAL_EL0` traps (EC 0x18) and reaches the thread's fault handler as a user exception; the Open Runtime turns that into SIGILL for an app. The counter (`CNTVCT_EL0`, `KernelArmExportVCNTUser`) stays readable by everyone.
- The root task starts with the flag. SetFlags refuses to set or clear it unless the calling thread has it, so a holder of a TCB capability (the Open Runtime holds its app threads') cannot hand it out, and nothing below the supervisor can take it from the timekeeper.
- The kernel's own tick is the virtual timer (`CNTV_*`, PPI 27, `KERNEL_TIMER_IRQ` on these EL1 configurations), which `EL0PTEN` does not touch: no flagged thread can disturb the kernel's preemption either. The timer's interrupt still reaches user level only through an IRQ handler capability, which the supervisor keeps for the timekeeper.

So on AArch64 only the supervisor's own threads (the root thread and the timekeeper) can program the timer, and the timekeeper runs without the AArch32 guard. `make test-unit` ([test_delta64.py](test_delta64.py)) checks the patch's shape (the two switch sites, the boot grant, the refusal, nothing else written to a system register) and that the kernel's tick is `CNTV`; `test_system.py --arch aarch64` checks on the built kernel that an AArch64 app's `CNTP_CTL_EL0` read and `CNTP_TVAL_EL0` write both trap while the timekeeper runs. It is unverified kernel code: about forty lines.
