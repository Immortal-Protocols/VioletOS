# Board descriptions

One JSON file per board says what is true of that machine: its CPU architecture, where its RAM is, where the boot chain puts each image, and where each device's registers are and which process may reach them. [board.py](board.py) validates the files and generates Rust constants, a C header and the emulator command line from them.

| Board | Architecture | What it is |
| --- | --- | --- |
| [xt2163dl](xt2163dl.json) | AArch32 | The Moto G Pure handset. Its AArch64 image (`make moto64`) uses the same memory map and device windows; the board check also compares the handset timer frequency with the AArch64 kernel's. |
| [qemu](qemu.json) | AArch32 | QEMU `virt` with the handset's memory map: the default test target. |
| [qemu64](qemu64.json) | AArch64 | QEMU `virt` for the [AArch64 bring-up](../qemu64/README.md). |

```sh
python3 targets/board/board.py list
python3 targets/board/board.py check          # what make test-unit runs
python3 targets/board/board.py rust qemu64    # constants for a Rust crate
python3 targets/board/board.py c xt2163dl     # the same as a C header
python3 targets/board/board.py sync           # after changing device mappings in supervisor.c
```

## What is generated from a board file today, and what is only checked

**qemu64 is built from its board file.** Its load addresses, the bootstrap task's constants (`board.rs`), the interrupt controller address in the entry stub, the timer frequency and the QEMU command line all come from `qemu64.json`; no source file repeats them.

**The two 32-bit boards are described and checked, not yet generated.** The Supervisor, the loader and the drivers still state their own addresses. Until they take them from here, `make test-unit` keeps the two in step:

- Every device window the Supervisor maps must be in the board file with the same physical address, process, virtual address, frame size, extent and rights, and the reverse. For each board the check reads the lines that board's build compiles, following `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else` and `defined()` with that board's macro (`VIOLET_QEMU` or `VIOLET_MOTO`) set and the other's not; any other build option counts as possibly set, so a window some build of the board maps is included. It covers every `device`, `board_device`, `map_device` and `map_device_rights` call with a literal physical address, unconditional ones (the display scanout) and QEMU-only ones (the image, camera, report and catalog windows) included, and for QEMU the `devices[]` rows. A call at `base + off` inside a `for (unsigned off = ...)` loop is its whole extent (`frames`), with the loop bounds evaluated from the constants the source assigns on that board; `board_device` is also the supervisor's own mapping at its fourth argument; `map_device_rights(..., seL4_CanRead)` is `"rights": "r"`, the rest `"rw"`. An extent or rights the check cannot read is an error, not a skip. Eight of the handset's devices are named (`storage`, `power`, `watchdog`, ...); the rest are listed under `supervisor_windows`, which `board.py sync` rewrites from the source. A window recorded with owner `context` is one the source maps into whichever process it is building at that point. Windows set up from computed addresses (the `devices[]` loop, `map_shared` frames) are not device windows and are not read.
- The root task's window, the framebuffer, the open apps' and camera's memory, the factory boot image's hash, the timer frequencies and the kernel's memory reservations are compared with the loader, the build scripts and the device trees that repeat them. A fact the check can no longer find in the code is an error, not a pass.

So XT2163DL is one board among three in the data and in the build's architecture table, but adding a fourth 32-bit board still means editing the Supervisor: generating its device tables from these files is the remaining step. Each real board also needs its own drivers; a description does not provide them.

## Rules the validator enforces

Ranges are page aligned and non-empty; image regions lie wholly inside RAM and do not overlap each other or an exclusive device; the kernel's RAM ranges are ordered and inside RAM; a device listed as absent is not also described; every window says its rights (`"r"` or `"rw"`) and a named device's windows fit the device; and every process maps its device windows below 4 GiB on both architectures, so one driver source serves a 32-bit and a 64-bit build.

`ARCHES` in board.py is the build's one table of what an architecture means (compiler triples, Rust targets, ELF class, emulator). The package builder reads it through [image/packages/violet_arch.py](../../image/packages/violet_arch.py): `VIOLET_ARCH` selects the architecture, and the default, `aarch32`, is the build as it always was.
