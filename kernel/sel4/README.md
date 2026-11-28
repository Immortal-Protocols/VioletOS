# seL4 kernel

seL4 schedules threads and enforces the memory mappings and communication rights that isolate VioletOS processes. The [build script](build.py) downloads seL4 17.0.0 by SHA-256, adds the [board configuration](../../platform/sel4/board/) and builds one kernel per target. Run `make test-isolation` to check that the UI cannot directly access the tested touch and storage regions.

An AArch64 build of the same archive, for QEMU only, lives beside this one with its own configuration and delta manifest: [aarch64/README.md](aarch64/README.md). It does not change the kernels described here.

## Assurance: what is and is not claimed

**VioletOS runs seL4's code, not a verified seL4.** The kernel built here matches no verified configuration, so none of seL4's proofs (functional correctness, integrity, confidentiality, binary correctness) apply to it. That would be true even with no local patch, for these reasons:

- **The platform is not one the proofs cover.** `ellis` is a VioletOS board port: an AArch32 kernel on Cortex-A53 (ARMv8-A) cores with GICv3.
- **The configuration is not a verified one.** `KernelVerificationBuild` is off, user level may read the virtual counter and program the physical timer, the thread-pointer register is `tpidruro`, and MCS and multicore are build options.
- **The compiler is not the one the binary proof uses.** The kernel is built with clang from the Android NDK.
- **The source is changed.** Six upstream files are modified and eight are added (below), including a board-specific capability that makes secure-monitor calls on the handset.

What VioletOS relies on instead is seL4's design (capabilities, and a small kernel whose source is the pinned upstream release plus the listed changes), the isolation tests, and the delta check described below. Treat a kernel bug as possible, as in any unverified kernel.

### The nearest verified configuration

The verified configuration closest to this kernel is upstream's `configs/ARM_imx8mm_verified.cmake`: the same architecture (AArch32, no hypervisor mode), the same core (Cortex-A53), a GICv3 and floating-point context switching. Upstream's `CAVEATS.md` lists it among the platforms with a C-level functional correctness proof; the integrity and confidentiality proofs it describes are for AArch32 without floating point, which that configuration turns on. VioletOS's kernel is not that configuration. It differs from it in every row of this table, in the compiler, and in every source change in the tables after it:

| Option | ARM_imx8mm_verified.cmake | VioletOS | What the difference does to the proofs |
| --- | --- | --- | --- |
| `KernelPlatform` | imx8mm-evk | ellis | Another platform: none of the proof's platform assumptions were checked for it. |
| `KernelVerificationBuild` | ON | OFF | The build is not restricted to what the proofs cover. |
| `KernelBinaryVerificationBuild` | ON | OFF | Nothing relates the compiled kernel to its C source. |
| `KernelOptimisationCloneFunctions` | OFF | ON | A compiler optimisation the binary verification excludes. |
| `KernelIPCBufferLocation` | threadID_register | absent | The option does not exist for this core; the thread pointer is set by KernelArmTLSReg instead. |
| `KernelNumDomains` | 16 | 1 | The proofs are set up to be robust to this number (upstream CAVEATS.md); no domain scheduling is used. |
| `KernelArmExportVCNTUser` | upstream's default | ON | Not set in any verified configuration. User level reads a clock, which the confidentiality proof's exclusion of timing channels does not cover. |
| `KernelArmExportPTMRUser` | upstream's default | ON | Not set in any verified configuration. User level programs a timer whose interrupt the kernel then delivers: hardware state the machine model does not have. |
| `KernelArmTLSReg` | upstream's default | tpidruro | The verified configurations keep the default (tpidru). tpidruro also turns on KernelSetTLSBaseSelf, a system call outside the verified API. |

The first six rows are options that configuration file sets; the build stops if any option the file sets has another value than this table says, or if one of them matches after all. The last three are options VioletOS passes that the file leaves at upstream's default. `VIOLET_MCS=1` and `VIOLET_CORES` above 1 move further away: MCS is verified only as `ARM_MCS_verified` on imx6, and no multicore configuration is verified. Options that neither that file nor VioletOS's build sets take whatever default upstream gives each platform; they are not compared.

Getting any proof to apply would take a platform port accepted into the proofs, a build with the verified options and compiler, re-proving every changed function, and a model of the secure monitor that does not exist. None of that is planned here. The purpose of the tables is narrower: to keep the unverified difference small, known, and unable to grow unnoticed.

## Every difference from upstream, checked by the build

[delta.json](delta.json) lists every upstream file the build changes, every file it adds, every source in this repository that enters the kernel (with its SHA-256), every option the build passes to the kernel's CMake with its value, what each of those does to the proofs, how the configuration differs from the nearest verified one, and the only functions allowed to contain an SMC instruction. The tables on this page are generated from it (`python3 kernel/sel4/delta.py --tables`) and compared with it by the tests. [delta.py](delta.py) enforces it in four places:

- `make test-unit` ([test_delta.py](test_delta.py)): the patch, board files, build options (names and values) and upstream pin in the repository match `delta.json`, every entry says what it does to the proofs, and these tables are the generated ones. Changing any of them without updating the manifest fails CI. A constant option is compared by its value; one computed from the environment or the target is compared by the source text of its expression. When the pinned archive and a built kernel are present, the configuration comparison below runs here too.
- Every kernel build, before CMake runs: the prepared kernel tree is compared file by file with the pinned upstream archive. A changed or added file that is not listed, or a removed one, stops the build. So does a listed file with the wrong content: each changed file must be byte for byte the upstream file with its patch or script applied (the check applies them to a pristine copy and compares), and each added file must be its source in this repository. The handset's base device tree is the one exception in kind: it is decompiled from the factory boot image, which is not in the repository, so the build requires that image to have the SHA-256 `delta.json` records.
- Every kernel build, after CMake runs: the configured options are compared with the nearest verified configuration, read from the pinned archive, as described above.
- Every kernel build, after linking: `kernel.elf` is disassembled, and an SMC instruction anywhere but the four listed stubs stops the build. The QEMU kernel contains none.

After a deliberate kernel change, edit `delta.json` in the same commit, run `python3 kernel/sel4/delta.py --update` to record the new source hashes, and paste the output of `--tables` over the tables here. If an old build directory fails the tree check, delete `build/sel4/<target>/upstream`.

### Upstream files changed

| File | Changed by | What changes | What it does to the proofs |
| --- | --- | --- | --- |
| `include/arch/arm/arch/32/mode/object/structures.bf` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Adds the ellis_wifi_cap capability type (arch cap tag 0x4f), compiled only for the ellis platform. | The generated capability datatype gains a type no proof layer knows: the abstract, design and C-level specifications and every lemma by cases over capabilities would need the new case. |
| `include/arch/arm/arch/32/mode/object/structures.h` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | The new capability has no size, is not a physical object and has no object pointer (three switch cases). | Three functions the C refinement proof covers (capability size, physical, pointer) gain a case; their correspondence lemmas no longer match the code. |
| `src/arch/arm/kernel/boot.c` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Places the one ellis_wifi_cap in the root task's CNode at seL4_CapSMC during boot. | The initial state differs from the one the boot invariants and the system-initialisation proof describe: one more capability in the root CNode. |
| `src/arch/arm/32/object/objecttype.c` | [aarch32.patch](../../platform/sel4/board/wifi/aarch32.patch) | Deriving the capability gives a null cap (it cannot be copied or minted), two such caps compare as the same object, and invoking it calls ellisWifiInvocation. | Arch_deriveCap, Arch_sameObjectAs and Arch_decodeInvocation gain a case. The new invocation calls the secure monitor, which the proofs' machine model does not contain: its effect on memory protection cannot be stated in them, so integrity and confidentiality would not follow even if the rest were re-proved. |
| `src/arch/arm/machine/gic_v3.c` | [gic-single-core.py](../../platform/sel4/board/gic-single-core.py) | On ellis the redistributor region is one 128 KiB frame per booted core instead of a fixed 1 MiB. | Interrupt controller driver: machine-interface code the proofs assume correct and do not verify. Changed or not, a defect in it is not excluded by any proof. |
| `tools/hardware.yml` | [gic-single-core.py](../../platform/sel4/board/gic-single-core.py) | Adds the violet,ellis-boot-gic device rule: the kernel maps the distributor and four redistributor frames. | Build-time hardware description that decides which device memory the kernel keeps and which it offers to user level. The proofs assume the generated layout is right. |

### Files added

| File | From | What it is | What it does to the proofs |
| --- | --- | --- | --- |
| `src/plat/ellis/config.cmake` | [config.cmake](../../platform/sel4/board/config.cmake) | The ellis platform: AArch32 on Cortex-A53 (ARMv8-A) with GICv3 and the ARM generic timer. Not an upstream platform. | A platform the proofs have never been run for: its memory map, timer and interrupt numbers are assumptions no proof discharges. |
| `src/plat/ellis/kernel-overlay.dts` | [kernel-overlay.dts](../../platform/sel4/board/kernel-overlay.dts) | Device tree overlay: the kernel's GIC node and reserved memory. | Input to the generated kernel memory layout, which the proofs assume correct. |
| `src/plat/ellis/moto-radio-memory.dts` | [moto-radio-memory.dts](../../platform/sel4/board/moto-radio-memory.dts) | The handset's connectivity memory reservations, applied on both targets. | Input to the generated kernel memory layout, which the proofs assume correct. |
| `src/plat/ellis/factory-base.dts` | [qemu.dts](../../platform/sel4/board/qemu.dts) | The base device tree: qemu.dts for QEMU; for the handset, the tree decompiled from the factory boot image at build time (not in this repository). | Input to the generated kernel memory layout, which the proofs assume correct. On the handset it comes from the manufacturer. |
| `src/plat/ellis/wifi/monitor.S` | [monitor.S](../../platform/sel4/board/wifi/monitor.S) | Four secure-monitor call stubs with fixed MediaTek function IDs. Compiled for the handset only. | Assembly, which the proofs do not cover on any platform, entering closed secure firmware that can change memory protection and is outside every proof assumption. |
| `include/arch/arm/arch/object/ellis_wifi/invoke.h` | [invoke.h](../../platform/sel4/board/wifi/invoke.h) | ellisWifiInvocation: turns one of four permitted requests into one secure-monitor call with fixed arguments and returns the monitor's reply words. No policy. On QEMU the stubs are C functions that fail. | Kernel-mode C that was never verified. It runs with interrupts masked for as long as the secure monitor takes, so the kernel's worst-case execution time is unbounded on the handset. |
| `include/arch/arm/arch/object/ellis_wifi/table.h` | [table.h](../../platform/sel4/board/wifi/table.h) | The four permitted operations and their fixed monitor arguments: reads of four registers of regions 25 and 29 by either reader, the two programming steps of the one connectivity window, and the random number call. | Kernel-mode C that was never verified: the argument check that keeps every other secure-monitor call unreachable. Its only assurance is the host test. |
| `libsel4/sel4_plat_include/ellis/sel4/plat/api/constants.h` | upstream | A copy of upstream's imx8mm-evk/sel4/plat/api/constants.h, made by kernel/sel4/build.py. | None: a user-level header; the kernel does not include it. |

### Build options

| Option | Value | Why | What it does to the proofs |
| --- | --- | --- | --- |
| `CMAKE_TOOLCHAIN_FILE` | upstream's llvm.cmake, in the prepared tree | The toolchain file seL4 ships for clang. | The binary verification of AArch32 kernels is for GCC output; no proof says clang's output matches the C semantics the proofs use. |
| `TRIPLE` | arm-none-eabi | The clang target triple llvm.cmake uses. | As CMAKE_TOOLCHAIN_FILE: a clang build. |
| `PYTHON3` | the build's own Python (build/sel4/venv) | The interpreter for the kernel's code generators. | None: runs the same generators. |
| `KernelPlatform` | ellis | The VioletOS board port; no upstream platform, so no upstream proof configuration. | Decides everything platform-specific the proofs take as given. No proof run has ever used this platform. |
| `KernelSel4Arch` | aarch32 | 32-bit ARM kernel on a 64-bit capable core. | The same as the verified AArch32 configurations. |
| `KernelVerificationBuild` | OFF | Not a verification build. | The verified configurations set ON, which also forbids options and code the proof tools cannot handle. OFF means the build was never constrained to the verified subset. |
| `KernelDebugBuild` | OFF | No debug syscalls. | OFF as in the verified configurations. |
| `KernelPrinting` | OFF | No kernel console. | OFF as in the verified configurations. |
| `KernelIsMCS` | OFF, ON with VIOLET_MCS=1 | Optional scheduling contexts (docs/8ms-frame-pipeline-rfc.md). | OFF by default, as in ARM_imx8mm_verified. With VIOLET_MCS=1 the kernel is the MCS variant, verified only as ARM_MCS_verified on imx6. |
| `KernelMaxNumNodes` | VIOLET_CORES, default 1 | Optional SMP on cluster 0. | 1 by default, as in the verified configurations. No multicore configuration is verified: with VIOLET_CORES above 1 none of the proofs' concurrency assumptions hold. |
| `KernelArmExportVCNTUser` | ON | User level reads the virtual counter. | Not set in any verified configuration. User level reads a clock, which the confidentiality proof's exclusion of timing channels does not cover. |
| `KernelArmExportPTMRUser` | ON | The Open Runtime programs the physical timer (PPI 30). | Not set in any verified configuration. User level programs a timer whose interrupt the kernel then delivers: hardware state the machine model does not have. |
| `KernelArmTLSReg` | tpidruro | Linux ARM programs read their thread pointer from TPIDRURO. | The verified configurations keep the default (tpidru). tpidruro also turns on KernelSetTLSBaseSelf, a system call outside the verified API. |
| `VioletQemuBoard` | ON for QEMU, OFF for the handset | Selects the timer frequency and the secure-monitor fixture. | A VioletOS option of the ellis platform; meaningless to the proofs. |

These are all the options the build passes; every other kernel option keeps the default that upstream and the `ellis` platform file give it.

## The kernel extension

On the handset, the connectivity chip's memory window and the hardware random number generator are reached through MediaTek's secure monitor, and only the kernel's privilege level may make that call: a 32-bit kernel without hypervisor mode has no way to hand the SMC instruction to user level, and upstream's own SMC capability exists only for 64-bit hypervisor-mode kernels. So the call stays in the kernel, and everything else has moved out of it:

- **One capability type**, `ellis_wifi_cap`. The kernel creates one at boot, in the root task's `seL4_CapSMC` slot. Deriving it gives a null capability, so it cannot be copied or minted: only the [supervisor](../../system/supervisor/README.md) ever holds it.
- **Four operations with fixed monitor arguments** ([table.h](../../platform/sel4/board/wifi/table.h), 45 lines; [invoke.h](../../platform/sel4/board/wifi/invoke.h), 45 lines; [monitor.S](../../platform/sel4/board/wifi/monitor.S), four stubs): read one of four registers of region 25 or 29 through either of the monitor's two readers, perform one of the two programming steps of the one connectivity window (region 25, 7 MiB at 0x42000000, two fixed permission words), and fetch the monitor's four random words. A caller chooses among these; no value it passes reaches the monitor. Anything else, including the three invocation labels of the earlier interface, is an illegal operation and reaches no monitor call.
- **No policy in the kernel.** Whether the window is empty, already programmed or foreign, whether the secure region overlaps it, the order and read-back of the two programming steps, the fallback between readers and the check that the random words are not an error reply all run in the supervisor at user level ([windows.h](../../platform/sel4/board/wifi/windows.h), unchanged from when it ran in the kernel, and [user.h](../../platform/sel4/board/wifi/user.h)). This is about 100 lines that used to run in kernel mode.
- On QEMU the stubs are C functions that report failure, so the QEMU kernel has no SMC instruction.

What this buys and what it does not: a supervisor that is wrong or compromised can still read those eight registers, program that one window with those permissions at a time the policy would not have, and draw random words. It cannot make any other secure-monitor call. The supervisor is the root task and holds every other authority in the system, so the kernel no longer trusts it with less than it already has; the earlier in-kernel policy protected against nothing the root task could not do otherwise. What remains in the kernel is unverified code in kernel mode, and while the monitor runs, interrupts are masked for as long as it takes.

`make test-unit` builds the kernel's invocation code and the supervisor's half for the host against a model of the monitor ([test_ellis_smc.c](../../targets/moto-g-pure/test_ellis_smc.c)): the kernel accepts exactly the listed operations and makes no monitor call for any other request, and over 1,536 monitor states and requests the new arrangement issues the same monitor calls in the same order with the same results as the policy calling the monitor directly. **It has not been run on the handset**: the monitor's real behaviour with each call arriving as a separate kernel entry, instead of all in one, is untested until someone runs `build/moto-package scan-wifi` on a phone flashed with this kernel.

Removing the extension altogether would mean making the calls once in the loader, before seL4 starts, and passing the results on. That was tried during bring-up and the monitor answered every read with all-ones at that point ([handset README](../../targets/moto-g-pure/README.md)); it would also end re-preparing the window after a Radio restart and turn the random number source into a single boot-time seed. It was not done.
