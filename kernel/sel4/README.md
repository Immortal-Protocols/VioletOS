# seL4 kernel

seL4 schedules threads and enforces the memory mappings and communication rights that isolate VioletOS processes. The [build script](build.py) downloads seL4 17.0.0 by SHA-256, adds the [board configuration](../../platform/sel4/board/) and builds one kernel per target. Run `make test-isolation` to check that the UI cannot directly access the tested touch and storage regions.

## Assurance: what is and is not claimed

**VioletOS runs seL4's code, not a verified seL4.** The kernel built here is not a verified configuration, so none of seL4's proofs (functional correctness, integrity, confidentiality, binary correctness) apply to it. That would be true even with no local patch, for these reasons:

- **The platform is not one the proofs cover.** `ellis` is a VioletOS board port: an AArch32 kernel on Cortex-A53 (ARMv8-A) cores with GICv3. The AArch32 proofs are for specific ARMv7 platforms and configurations.
- **The configuration is not a verified one.** `KernelVerificationBuild` is off, user level may read the virtual counter and program the physical timer, the thread-pointer register is `tpidruro`, and MCS and multicore are build options.
- **The compiler is not the one the binary proof uses.** The kernel is built with clang from the Android NDK.
- **The source is changed.** Six upstream files are modified and eight are added (below), including a board-specific capability that makes secure-monitor calls on the handset.

What VioletOS relies on instead is seL4's design (capabilities, and a small kernel whose source is the pinned upstream release plus the listed changes), the isolation tests, and the delta check described next. Treat a kernel bug as possible, as in any unverified kernel.

## Every difference from upstream, checked by the build

[delta.json](delta.json) lists every upstream file the build changes, every file it adds, every source in this repository that enters the kernel (with its SHA-256), every option the build passes to the kernel's CMake with its value, and the only functions allowed to contain an SMC instruction. [delta.py](delta.py) enforces it in three places:

- `make test-unit` ([test_delta.py](test_delta.py)): the patch, board files, build options (names and values) and upstream pin in the repository match `delta.json`. Changing any of them without updating the manifest fails CI. A constant option is compared by its value; one computed from the environment or the target is compared by the source text of its expression.
- Every kernel build, before CMake runs: the prepared kernel tree is compared file by file with the pinned upstream archive. A changed or added file that is not listed, or a removed one, stops the build. So does a listed file with the wrong content: each changed file must be byte for byte the upstream file with its patch or script applied (the check applies them to a pristine copy and compares), and each added file must be its source in this repository. The handset's base device tree is the one exception in kind: it is decompiled from the factory boot image, which is not in the repository, so the build requires that image to have the SHA-256 `delta.json` records.
- Every kernel build, after linking: `kernel.elf` is disassembled, and an SMC instruction anywhere but the four listed stubs stops the build. The QEMU kernel contains none.

After a deliberate kernel change, edit `delta.json` (and the tables below) in the same commit and run `python3 kernel/sel4/delta.py --update` to record the new source hashes. If an old build directory fails the tree check, delete `build/sel4/<target>/upstream`.

### Upstream files changed

| File | Changed by | What changes |
| --- | --- | --- |
| `include/arch/arm/arch/32/mode/object/structures.bf` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Adds the ellis_wifi_cap capability type (arch cap tag 0x4f), compiled only for the ellis platform. |
| `include/arch/arm/arch/32/mode/object/structures.h` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | The new capability has no size, is not a physical object and has no object pointer (three switch cases). |
| `src/arch/arm/kernel/boot.c` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Places the one ellis_wifi_cap in the root task's CNode at seL4_CapSMC during boot. |
| `src/arch/arm/32/object/objecttype.c` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Deriving the capability gives a null cap (it cannot be copied or minted), two such caps compare as the same object, and invoking it calls ellisWifiInvocation. |
| `src/arch/arm/machine/gic_v3.c` | [gic-single-core.py](../../platform/sel4/board/gic-single-core.py) | On ellis the redistributor region is one 128 KiB frame per booted core instead of a fixed 1 MiB. |
| `tools/hardware.yml` | [gic-single-core.py](../../platform/sel4/board/gic-single-core.py) | Adds the violet,ellis-boot-gic device rule: the kernel maps the distributor and four redistributor frames. |

### Files added

| File | From | What it is |
| --- | --- | --- |
| `src/plat/ellis/config.cmake` | [config.cmake](../../platform/sel4/board/config.cmake) | The ellis platform: AArch32 on Cortex-A53 (ARMv8-A) with GICv3 and the ARM generic timer. Not an upstream platform. |
| `src/plat/ellis/kernel-overlay.dts` | [kernel-overlay.dts](../../platform/sel4/board/kernel-overlay.dts) | Device tree overlay: the kernel's GIC node and reserved memory. |
| `src/plat/ellis/moto-radio-memory.dts` | [moto-radio-memory.dts](../../platform/sel4/board/moto-radio-memory.dts) | The handset's connectivity memory reservations, applied on both targets. |
| `src/plat/ellis/factory-base.dts` | [qemu.dts](../../platform/sel4/board/qemu.dts) | The base device tree: qemu.dts for QEMU; for the handset, the tree decompiled from the factory boot image at build time (not in this repository). |
| `src/plat/ellis/wifi/monitor.S` | [monitor.S](../../platform/sel4/board/wifi/monitor.S) | Four secure-monitor call stubs with fixed MediaTek function IDs. Compiled for the handset only. |
| `include/arch/arm/arch/object/ellis_wifi/invoke.h` | [invoke.h](../../platform/sel4/board/wifi/invoke.h) | ellisWifiInvocation: three labels (inspect windows, prepare windows, read four secure random words). On QEMU the stubs are C functions that fail. |
| `include/arch/arm/arch/object/ellis_wifi/windows.h` | [windows.h](../../platform/sel4/board/wifi/windows.h) | The fixed connectivity and DMA window policy the invocation enforces. |
| `libsel4/sel4_plat_include/ellis/sel4/plat/api/constants.h` | upstream | A copy of upstream's imx8mm-evk/sel4/plat/api/constants.h, made by kernel/sel4/build.py. |

### Build options

| Option | Value | Why |
| --- | --- | --- |
| `CMAKE_TOOLCHAIN_FILE` | upstream's llvm.cmake, in the prepared tree | The toolchain file seL4 ships for clang. |
| `TRIPLE` | arm-none-eabi | The clang target triple llvm.cmake uses. |
| `PYTHON3` | the build's own Python (build/sel4/venv) | The interpreter for the kernel's code generators. |
| `KernelPlatform` | ellis | The VioletOS board port; no upstream platform, so no upstream proof configuration. |
| `KernelSel4Arch` | aarch32 | 32-bit ARM kernel on a 64-bit capable core. |
| `KernelVerificationBuild` | OFF | Not a verification build. |
| `KernelDebugBuild` | OFF | No debug syscalls. |
| `KernelPrinting` | OFF | No kernel console. |
| `KernelIsMCS` | OFF, ON with VIOLET_MCS=1 | Optional scheduling contexts (docs/8ms-frame-pipeline-rfc.md). |
| `KernelMaxNumNodes` | VIOLET_CORES, default 1 | Optional SMP on cluster 0. |
| `KernelArmExportVCNTUser` | ON | User level reads the virtual counter. |
| `KernelArmExportPTMRUser` | ON | The Open Runtime programs the physical timer (PPI 30). |
| `KernelArmTLSReg` | tpidruro | Linux ARM programs read their thread pointer from TPIDRURO. |
| `VioletQemuBoard` | ON for QEMU, OFF for the handset | Selects the timer frequency and the secure-monitor fixture. |

These are all the options the build passes; every other kernel option keeps the default that upstream and the `ellis` platform file give it.

## The kernel extension

On the handset, the connectivity chip's memory windows and the hardware random number generator are reached through MediaTek's secure monitor, and only the kernel's privilege level may make that call. The extension is the smallest interface found for it so far:

- One capability type, `ellis_wifi_cap`. The kernel creates one at boot, in the root task's `seL4_CapSMC` slot. Deriving it gives a null capability, so it cannot be copied or minted: only the [supervisor](../../system/supervisor/README.md) ever holds it.
- Three invocations: inspect the two fixed windows, prepare them when empty, and read four random words. The arguments never reach the secure monitor as a function ID: the four stubs in `monitor.S` use fixed MediaTek function IDs, and `windows.h` checks the window bounds.
- On QEMU the stubs are C functions that report failure, so the QEMU kernel has no SMC instruction.

It is unverified code running in kernel mode, about 200 lines. A defect in it is a kernel defect.
