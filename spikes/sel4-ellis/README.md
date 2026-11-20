# Spike 2 · seL4 on the Moto G Pure XT2163DL

Historical bring-up log. Paths and commands below refer to the spike as it ran. See the [current build and architecture guide](../../README.md) for the maintained targets.

The comparison target is the Linux slot B build on this handset: a visible clock, opening Stopwatch with a physical tap, and advancing digits after Start. A Linux display and a registered input node alone were insufficient during the original bring-up.

## Mac-side board build

The factory image for S3RHS32.20-42-13-2-3-2 has an Android v2 header and a 32-bit zImage. Its Android DT table contains one MT6765 base tree. The eight Cortex-A53 nodes advertise PSCI. The interrupt controller is GICv3, distributor at 0x0c000000 and redistributors at 0x0c100000; the architectural timer is specified at 13 MHz. Generated kernel headers select virtual timer IRQ 27. The historical Linux touch logs identify the active panel as mipi_mot_vid_djn_nt36525b_a_hdp_652.

Using the same Clang, virtual environment and CMake setup as spike 1, extract seL4 17.0.0 to a **disposable** source directory. Run:

```sh
sh spikes/sel4-ellis/build-kernel.sh path/to/seL4-17.0.0 path/to/factory/boot.img build/spike2/kernel
```

This produces an AArch32 Cortex-A53 kernel ELF for Ellis, single-core, non-MCS. It reuses upstream GICv3 and ARM timer support. The AArch64 Microkit image and the debug settings of spike 1 are not used. The compile-time IRQ ceiling is architectural and needs handset validation. LLD reported a .bss alignment warning. This configuration has no verified-platform claim.

## Physical handoff

The factory Android header specifies kernel load address 0x40008000. The trial seL4 ELF's first load segment starts at 0x40000000 and its entry is virtual 0xe0000000. Its upstream entry convention expects a prepared initial task and memory mappings. An ELF substituted directly into the stock image does not supply this handoff.

The base tree specifies 512 MiB at 0x40000000 and contains size-only firmware reservations. The first compile generated a single available RAM range covering all 512 MiB. Do not use that build's memory map for a handset trial. Obtain the live RAM/reservation map and establish an observable bootloader or display signal before constructing a bootable port. Linux's USB serial gadget is provided by the running Linux driver and cannot report on seL4 early boot.

The existing working boot_b image is kept locally as build/step7/violet-touch-log.img. The read-only board reporting scripts collect Linux memory/display information over the same serial link. A Linux report is not evidence of a seL4 boot.

## Slot B measurement on the physical handset

The Linux measurement image booted and returned a complete board report over USB.
The live tree describes 3 GiB of DRAM beginning at 0x40000000. It supplies 22
explicit reserved-region entries. Inside the original compile-time RAM range,
Linux reports system RAM only through 0x47c7ffff before the first reservation.
The first 64 MiB (0x40000000 through 0x43ffffff) falls within that first Linux
system RAM interval and overlaps none of the 22 observed explicit reservations.
The amended compile-only platform now uses that conservative window; the
generated seL4 available-memory table was inspected after a fresh link.
Linux kernel code/data occupied part of that window during this measurement.
The replacement loader must lay out its own payloads there and check the
actual handed-off DTB on each boot. The report did not expose debugfs memblock
tables, so the capture is not a complete inventory of temporary boot allocations.

The bootloader video tag and framebuffer ioctl independently reported base
0x79450000. The reserved display pool is 0xfb0000 bytes. Linux exposes
0xd7a000 bytes with 2944 bytes per row, 32 bits per pixel, visible 720 × 1600
and virtual 736 × 4800. These facts identify a candidate early display signal;
whether LK's scanout updates after a bare-metal write still needs a physical test.
Linux reports that all CPUs entered in HYP mode. A non-hypervisor AArch32 seL4
port needs the loader to establish the kernel's expected supervisor-mode handoff.
No seL4 image was written during this measurement.

## Combined loader and userspace trial

The slot B candidate contains an ARM zImage entry stub, an Ellis build of seL4
17.0.0 and one small C user task. The bootloader supplies the live device tree.
The loader checks the memory range, explicit reservations, framebuffer tag and
panel name. It paints a white band on rows 32–63, stages the kernel clear of
the original image, sets up initial address mappings and enters seL4. The user
task uses seL4 capabilities to obtain a framebuffer page, checks its physical
address and maps it into its own address space. It paints rows 0–21 green.
The bands are near the top edge of the first scanout buffer. A blank screen
alone cannot distinguish a stopped CPU from the display scanning another buffer.

The emulator test uses the same loader and user program, with a separately
linked kernel for QEMU's interrupt controller addresses and a device tree
fixture reconstructed from the Linux measurement. It checks both colors in
physical memory. An initial run caught the default GIC mappings wrapping over
the AArch32 kernel's final page table. The Ellis rule maps one distributor
and the first CPU redistributor, and bounds the driver's scan to that interface.
The measured and fixture builds each generate 48 device pages. Only the boot
CPU participates. The fixture does not model the Moto panel or its bootloader.

Source lives in `trial/` and `platform/`. To reproduce, rebuild the measured
kernel with `build-kernel.sh`, generate its libsel4 userspace headers, and
configure a second kernel called `kernel-fixture` with the same options plus
`-DKernelCustomDTSOverlay=<absolute path to trial/qemu-overlay.dts>`.
Extract upstream dtc v1.7.2 under `build/spike2/dtc-1.7.2` (source archive SHA-256
8f1486962f093f28a2f79f01c1fd82f144ef640ceabe555536d43362212ceb7c).
Run `python3 spikes/sel4-ellis/trial/build.py measured`, then repeat with
`fixture` and run `python3 spikes/sel4-ellis/trial/test-qemu.py`.
The handset candidate is `build/spike2/trial-measured/boot.img`.

The physical flash 2 trial used boot.img SHA-256
`245bf7f6adb1532e0d7131c781eecef655d1b71814159d7ece789551391b2a10`.
Fastboot reported XT2163DL, bootloader mode and current slot B, then acknowledged
sending and writing boot_b and rebooting. The first restart returned to Android.

Flash 2 handset result: the operator saw normal Android. USB debugging on the
same handset confirmed Android boot completed on slot A, build
S3RHS32.20-42-13-2-3-2. Android reported ro.boot.bootreason=reboot and
sys.boot.reason=bootloader. The Android shell could not read /sys/fs/pstore.
Neither diagnostic band was reported on that first restart.

A subsequent read-only bootloader check reported current-slot a, running-slot
_a, slot-bootable:_a Yes, slot-successful:_a yes, slot-retry-count:_a 1,
slot-bootable:_b No, slot-successful:_b no, and slot-retry-count:_b 0.
These values are exposed by getvar all with underscore-prefixed slot suffixes.
The counters show the state at inspection; they do not record the cause or the
number of attempts made during flash 2.

## Confirmed seL4 boot on the Moto G Pure

On November 20, 2026, fastboot accepted `set_active b` on the XT2163DL.
Immediately before reboot, it reported current slot B, B bootable, and seven
retries available. The operator observed both the green and white bands on
the handset after reboot. This retry used the flash 2 image already on B.

White came from the loader writing the framebuffer before entering seL4.
Green came from the initial C user task after seL4 scheduled it and the task
obtained and mapped the framebuffer page. The observed green band verifies
userspace execution and visible framebuffer output on this handset. Clock,
stopwatch timing, and physical touch under seL4 remain to be demonstrated.

## Stopwatch handset trial

The image in `build/spike2/stopwatch-measured/boot.img` contains the seL4
kernel and one initial C task. The task maps the visible framebuffer and the
MT6765 SPI3, pin and clock registers, reads the ARM virtual counter, and renders
the Stopwatch, Start, Pause, Reset and Home controls. This prototype polls
one contact every eight milliseconds. Selecting the read-only counter option
gives user tasks access to a high-resolution timing source. Isolation between
the driver and application, a time-of-day clock and idle power management
remain future work.

The driver targets the DJN NT36525B-A panel observed in the Linux report. It
checks the chip ID before uploading the locally saved factory firmware into
controller SRAM. The firmware SHA-256 is pinned in stopwatch/firmware.py.
The board configuration and register sequence were checked against the
[Motorola Ellis kernel](https://github.com/MotorolaMobilityLLC/kernel-mtk/tree/MMI-S3RHS32.20-42-10-4-11)
and [Novatek module](https://github.com/MotorolaMobilityLLC/motorola-kernel-modules/tree/MMI-S3RHS32.20-42-10-4-11/drivers/input/touchscreen/nt36xxx_mtk_mmi).
The factory firmware bytes are kept in the local build tree.

With the trial build environment active, run the following from the repository root.

```sh
sh spikes/sel4-ellis/stopwatch/kernels.sh
python3 spikes/sel4-ellis/trial/build.py fixture --stopwatch
python3 spikes/sel4-ellis/stopwatch/test.py
python3 spikes/sel4-ellis/trial/build.py measured --stopwatch
```

The QEMU test boots real seL4 and exercises the same touchscreen protocol,
contact decoding, time source and screen code. Its test transport checks
firmware writes and supplies single-contact reports through an emulated UART.
It reads digits back from the rendered framebuffer to verify Start, Pause and
Reset. Physical SPI and panel behavior require the handset test. The measured
image fingerprint is `3625cc7c0fea93a28b87ff025a7e2de2d028138e8b993e8244b05d020d64eb1b`.

Flash 3 on the XT2163DL wrote the above stopwatch image to boot_b. Fastboot
acknowledged the write and slot selection, reported B bootable with seven
retries, and accepted the reboot. Physical touch and timing await observation.

During the flash 3 handset test the operator reported pixels smeared across
the display. A readable interface, physical taps and advancing digits remain
unconfirmed. The stopwatch indexes rows at 736 32-bit pixels, matching the
Linux framebuffer ioctl. The earlier band test covered only solid-color
regions. Neither test established the bootloader display layer's actual row
stride or pixel format. QEMU verified the UI using the same assumed geometry.

After the smeared display and apparent restarts, a USB bootloader read
reported current/running slot A, B unbootable and unsuccessful with zero
retries, and A bootable and successful. The menu entry reason was “Volume
down key pressed”. B had seven retries immediately before the flash 3 reboot.
This establishes exhaustion of the B retry allowance. The reset cause is
unknown. The 720-pixel stride candidate has passed the QEMU stopwatch test
and has not been flashed.

Android subsequently completed boot on A. Its retained boot-reason history
contained three reboot entries and one bootloader entry. Android shell access
to pstore and /proc/bootinfo was denied. These observations do not identify
which mechanism reset the seL4 trial.

The next local candidate (SHA-256
`0a00374fbe59ecfa6af71fd8ab0885d81a950a1eb982c486d2475aea84f810b2`)
uses the inferred 720-pixel scanout pitch. It also follows the MT6765 Linux
driver's inherited-watchdog check. If the hardware enable bit is set it
writes the reload key at attachment and periodically during firmware transfer
and UI polling. It leaves the mode and timeout intact. Both fixture runs
passed the stopwatch pixel checks; the RAM-backed register test checked for
the reload key with the enable bit set and no reload with it clear. The
fixture does not simulate watchdog expiry. This image has not been flashed.

Flash 4 wrote the above 720-pixel/watchdog candidate to boot_b on the
XT2163DL. Fastboot acknowledged the write, slot B selection and reboot.
Immediately before restart B was bootable with seven retries. Screen
legibility, tap handling, advancing digits and uptime await handset observation.

Flash 4 handset observation: the UI was readable and displayed CRC SETUP
FAILED; physical taps had no effect. That path follows a matching chip ID
and second reset, and precedes firmware upload. It means that an SPI
transaction timed out during one of six configuration writes. Duration
without a restart has not been confirmed.

A local follow-up enables pause/completion handling and resets the SPI host
at each message boundary. It retries idempotent CRC configuration writes and
reports the bank and field on exhaustion. QEMU now validates all six fields
and injects one transient timeout before successful upload and stopwatch UI
tests. It does not model the physical SPI host. The unflashed candidate has
SHA-256 454586e5361870939c3af3c6fca27a89ba8c0e1d81caff986f512484e5d7e3f5.

The operator subsequently confirmed that the readable flash 4 screen stayed
up for at least one minute without the Motorola logo returning. This meets
the one-minute observation target. The cause of the earlier resets remains
unconfirmed.

Flash 5 wrote the touch-retry image (454586e5…e3f5) to boot_b on the
XT2163DL. The bootloader reported that B was already active, was bootable,
and had six retries remaining. It acknowledged the reboot. The on-screen
driver status and physical stopwatch test await observation.

Flash 5 displayed SPI TRANSFER FAILED. The clock check passed, but the first
controller reset sequence timed out before the chip ID read. Flash 4 had
reached CRC setup. The reason for the earlier failure on flash 5 is unknown.

The next local candidate restores flash 4's standalone SPI completion mode.
It reinitializes the SPI host on timeout, retries the reset sequence's writes
and reports which page or data transfer failed. QEMU passed the stopwatch
test with one injected reset-write timeout and one CRC setup timeout. The
physical SPI controller remains untested with this revision. Candidate:
2dc2e5815810d44dea6de4f2be43731ab3338d9d5de41177f7b080f00b6e4c5b.

Flash 6 wrote the candidate above to boot_b on the XT2163DL. Fastboot
reported B active and bootable with five retries remaining before restart.
The bootloader acknowledged the reboot. Handset touch status awaits observation.

Flash 6 displayed FW WRITE FAILED on the handset. Identification and both
CRC setup banks completed. The upload stopped on a page select or data
transaction; this build did not record which address failed.

The next candidate retries the same SRAM address and switches to at most
three payload bytes per transfer after repeated data timeouts. Persistent
failure reports PAGE or DATA and the six-digit hexadecimal address. The
QEMU transport forced repeated bulk data timeouts at address 0x80, then
checked the complete uploaded byte stream and stopwatch operation. Candidate
SHA-256: 8e376a4ce2ba2d6ab454490fc4312f5f76802f6073a8dc07c933aa890efaac3f.

Flash 7 wrote the firmware-upload retry candidate above to boot_b on the
XT2163DL. B was already active and fastboot reported it bootable with four
retries remaining. Fastboot acknowledged the restart. Touch status and
stopwatch operation await handset observation.

Flash 7 handset status was transcribed as "FW data 002C5". The display
formats six hex digits; the five reported characters are preserved here.
This status indicates exhaustion of data-transfer retries for a firmware
chunk. The Mac QEMU fixture passed on rerun with two injected data timeouts.
Its simulated SPI transport replaces the MT6765 register driver in spi.c.

## QEMU SPI register tests after flash 7

Both stopwatch variants link the same compiled `spi.c` transfer code. The
handset uses `spi_mmio.c` for hardware register access and board setup. The
fixture implements the packet-length, command, configuration, transmit FIFO,
receive FIFO, and completion/status registers used by that code. It checks the
declared packet length against words queued, the command and timing settings,
and the programmed firmware bytes. The model runs inside the QEMU test image.
It does not simulate pin timing, power or the actual MT6765 silicon.

After a firmware **data** timeout the uploader now reads back the affected
controller SRAM range. A matching range permits progress; a mismatch or failed
read follows the bounded retry path. A persistent failure reports the address
and raw SPI STATUS1 sampled before host reset. The setting of the model's
status bits is test data. Physical status must be interpreted separately.

Run from the repo root:

```sh
python3 spikes/sel4-ellis/trial/build.py fixture --stopwatch
for mode in normal delayed drop-before drop-after stuck setup; do
    python3 spikes/sel4-ellis/stopwatch/test.py --spi "$mode" --watchdog-enabled || break
done
python3 spikes/sel4-ellis/trial/build.py measured --stopwatch
cmp build/spike2/stopwatch-fixture/spi.c.o build/spike2/stopwatch-measured/spi.c.o
```

All six scenarios passed. Five reached the pixel-verified stopwatch test.
The stuck case exhausted five attempts and the framebuffer showed
`FW DATA 000080 S1 00000001`. Address 0x80 was chosen for repeatable fault
injection. The host and fixture SPI driver objects compared byte-identical.
The hardware candidate has SHA-256
`a32091821d8e068f207185e894c72e5b9a88cceecfd46971a0f175f18ef52c65`.
Flash 7 remains installed pending a separately authorized hardware trial.

Flash 8 wrote the checksum-verified SRAM readback candidate above to boot_b.
The XT2163DL reported B active and bootable with three retries remaining
before reboot. Fastboot acknowledged the restart. Handset results are pending.

Flash 8 displayed RESET PAGE TIMEOUT. The final attempt to select the
touch reset register page timed out during one of two reset passes. The
status does not identify which pass. Firmware upload and SRAM readback
were never reached on this boot.

A diagnostic build labels the reset before and after chip ID as R1 and R2
and captures raw STATUS1 and CMD before SPI host recovery. Both persistent
reset-page failures and the normal stopwatch path were checked in QEMU by
reading the resulting framebuffer. This change provides instrumentation;
the physical cause is open. Unflashed candidate SHA-256:
2b21309d79cf23ca7c818ca49857c2fbcffd7a578bbc7ed50270eeeaff247d80.

Flash 9 wrote the reset-pass diagnostic above to boot_b on the XT2163DL.
Fastboot reported B active and bootable with two retries remaining before
restart and acknowledged the reboot. Handset observation is pending.

Flash 9 displayed `R1 RESET PAGE S00000001 C00013000`. On the final
attempt to select the reset page, before the first ID read, the driver did
not observe FINISH before its deadline. CMD read back with ACT clear;
the configured finish interrupt and MSB-first flags were retained. These
values alone do not establish whether clocks or data reached the panel.

The fixture now clears ACT on command acceptance to match the observed
readback. A follow-up uses STATUS1 bit 0 as a gate before polling STATUS0
for FINISH. This is a testable inference from the
[MediaTek polled driver](https://chromium.googlesource.com/chromiumos/platform/depthcharge/+/ddfaa837f027ea5f2dc594ac491912120bdcf822/src/drivers/bus/spi/mtk.c)
for a related controller. The delayed fixture flags any early STATUS0
poll as a test violation. A deliberately suppressed completion can still
produce the observed timeout. Physical validation remains pending.

Flash 10 wrote the polling-order candidate (SHA-256
19377b26ea4c304b612599baf7d09ad9a19cc7371e8432a2dddb98a8bc44188d)
to boot_b on the XT2163DL. Immediately before reboot, B was active and
bootable with one retry remaining. Fastboot acknowledged the restart.
The on-screen result is pending.

The operator saw Android after flash 10. ADB confirmed boot completed on
slot A. Android reported reboot for the current and last boot reasons.
B had one retry before restart; the post-trial B flags have not been read.
There was no observed VioletOS diagnostic on this boot. Execution of the
new polling order on hardware remains unconfirmed.

Flash 10 retry without reflashing: before reselection A was current, and
B was unsuccessful, unbootable, with zero retries. After set_active b,
B was current and bootable with seven retries. Fastboot accepted reboot.

## Physical stopwatch milestone

On the Moto G Pure XT2163DL the operator saw `TOUCH READY` and confirmed
the stopwatch worked after the requested Stopwatch → Start tap sequence.
The same flash 10 image on B therefore demonstrated readable display output,
touchscreen initialization, physical tap input and stopwatch timing under
seL4. This observation covers one successful handset boot. Repeated-boot
reliability and the cause of the earlier intermittent failures remain open.
