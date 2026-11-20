# VioletOS

A small Linux phone prototype with a clock, stopwatch and system screen. A native SDL simulator and a Linux framebuffer program share the same drawing, timing and navigation code. State lives in RAM. VioletOS boots on the Moto G Pure XT2163DL with a locally built Linux kernel. Display and touch were confirmed on slot B.

[Architecture and roadmap](docs/VioletOS-architecture.pdf) · [Handset kernel build](kernel/ellis/README.md)

## Try it on this Mac

Run `make sim` for the native window. Click Stopwatch, then Start. Home returns to the clock while the stopwatch continues. Power off opens a confirmation screen. Cancel returns Home. Shut down closes the simulator on the Mac and requests a kernel power-off in the Linux guest. Escape also closes the simulator. The host build needs a C compiler, Make and SDL2 with `sdl2-config`.

Run `make emulator` for the Linux guest. Close its QEMU window to exit. The first build uses pinned Alpine inputs and checks SHA-256 hashes. Subsequent builds use the local cache. To boot existing artifacts directly, run `python3 tools/run.py`.

## Choosing a mode

| Development question | Best place to answer it | What it exercises |
| --- | --- | --- |
| Does a tap do the right thing? Is the layout clear? | Simulator | Shared interface in a resizable native window, host clock, mouse input. A C rebuild is enough for UI edits. |
| Does the system boot and respond under a tight memory budget? | Emulator | Actual Linux kernel, PID 1, BusyBox, framebuffer and Linux input events. One virtual CPU, 128 MiB guest RAM, no disk or network required at runtime. |
| Does the phone wake, display pixels, accept touch or place calls? | XT2163DL | Motorola kernel, panel, digitizer, power controller and modem. QEMU models a generic ARM board. |

For a week of UI work, use the simulator for each edit and boot QEMU when integrating. For a kernel, init, or driver change, start in QEMU. Measure battery life, suspend/resume, thermals and radio behavior on the handset. The current prototype implements the clock and stopwatch only. Cellular service, audio, Wi-Fi, persistence and power management are future device work.

## Other desktops

Copy `tools/run.py`, `build/Image` and `build/violet-aarch64.cpio.gz` with their relative paths intact. Install Python 3 and QEMU with `qemu-system-aarch64` on PATH. Run `python3 tools/run.py` (Windows: `py -3 tools/run.py`). Software translation works across CPU architectures and avoids dependence on host virtualization APIs. Add `--headless` for serial diagnostics. Only the Apple Silicon Mac has been tested. On macOS or Linux with SDL2 development headers and Make, `make sim` builds the native simulator.

## Rebuild after source changes

`src/screen.c` is shared. `src/simulator.c` handles desktop events. `src/framebuffer.c` handles framebuffer layouts and Linux pointer events. Boot uses the prebuilt static Linux executables in `assets/`. To update both after a C edit, point `ANDROID_NDK_HOME` at NDK 28.2.13676358 and run the following from the project directory on macOS or Linux.

```sh
sh tools/native.sh
python3 tools/image.py
```

The NDK supplies a static C runtime for each ARM ABI. Neither executable needs an installed Android framework. Image building requires Python 3.9+ and packages named in `tools/image.py`. Keep the verified downloads when moving the project offline. Alpine package mirrors can retire superseded kernel packages.

## Moto G Pure XT2163DL bring-up

Read-only ADB inspection of the USB handset found SKU XT2163DL, device ellis, Android build S3RHS32.20-42-13-2-3-2, active slot A, kernel 4.19.191+, a 32-bit ARM kernel and userspace, flash lock 0 and verified boot orange. The running kernel has gzip initramfs, framebuffer and evdev support. Android exposes fb0 and two input devices advertising touch coordinates up to 720 × 1600.

The exact factory package contains a v2 boot image with a 2,048-byte page, the ARM kernel, a gzip ramdisk and the base device tree. The package lists its device tree overlays separately. Its manifest has no separate vendor_boot image.

For the local stock round-trip check, choose a fresh output path and run:

```sh
python3 tools/phone_image.py --stock-roundtrip /path/to/stock/boot.img /path/to/rebuilt.img
cmp /path/to/stock/boot.img /path/to/rebuilt.img
```

The v2 builder recalculates section sizes, page padding and the legacy image ID. In round-trip mode it carries over the original partition trailer only after the regenerated payload matches the input byte for byte. The verified XT2163DL stock image and its reconstruction both have SHA-256 `36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a`.

The default two-path invocation substitutes the ARMv7 development ramdisk and omits stock verified-boot metadata. The stock round-trip check establishes local packaging fidelity only. The XT2163DL bootloader rejected temporary RAM boot. Hardware trials used slot B.

### Package an XT2163DL candidate

The device-specific builder adds the Novatek touch modules and panel firmware to the RAM filesystem, where Violet init can load them before launching the UI. Use the exact stock boot image and the five vendor files staged in their original relative paths under `stock/touch/`. The builder verifies each input against its expected SHA-256. To package a candidate with the factory kernel, choose a new output file in an existing directory:

```sh
firmware_dir=build/xt2163dl/S3RHS32.20-42-13-2-3-2
python3 tools/xt2163dl_candidate.py "$firmware_dir/stock/boot.img" "$firmware_dir/stock/touch" "$firmware_dir/rebuilt.img"
```

Pass `--kernel /path/to/zImage` to package the locally built kernel with the same VioletOS filesystem. The candidate is unsigned. The hardware trial used an unlocked handset.

The diagnostic build exposes a USB serial stream of touchscreen messages. Both factory touchscreen modules loaded and the Novatek device registered. Physical touch worked on the diagnostic build after earlier trials received zero input reports. The cause of recovery and repeatability across boots remain open. Persistent storage, Wi-Fi and suspend/resume are planned.
