# USB Diagnostics

The Moto supervisor starts [USB Diagnostics](process.rs) as a separate Rust process from package 21. It owns the handset USB controller and exposes read-only snapshots of Radio, Power and startup status. Mac clients use the same connection to stage signed replacements, request a scan or join, read the saved stopwatch value, and request a reboot. Radio owns Wi-Fi, Storage owns the catalog and checkpoint, and the supervisor verifies package signatures and decides which package runs.

The [host clients](host/) run on the Mac. Build them with `make build/moto-debug build/moto-package`. Run `build/moto-debug` for a JSON snapshot and `build/moto-package diagnostics-status` for the running package checksum and trial generation. Images that answer vendor request 0x59 add `word_bits` (32 or 64: which architecture's VioletOS answered), `entry_el` (the level LK entered the AArch64 loader at), `last_fault` (the supervisor's last fault record on AArch64: process, label, four message words) and `boot_disk` (why the AArch64 loader could not open the boot history, when it could not); older images stall the request and the fields are absent.

Run `build/moto-package reboot-bootloader` to restart the handset into fastboot, or `host/moto-flash IMAGE` to flash slot B and wait for VioletOS to return. See [hands-free image flashing](../../targets/moto-g-pure/README.md#hands-free-image-flashing).

## Replace Diagnostics over USB

Build a replacement with `make moto-component NAME=usb-diagnostics`. On a handset running a baseline with package 21, run `tools/moto-install-diagnostics build/sel4/moto/packages/usb-diagnostics.vpk`. The package already carries the build's signature; the installer adds a detached signature by this build's developer key, which a developer image requires to start the trial without a tap on the handset (release images have no such path, so there Diagnostics changes only with the boot image), and waits for a status snapshot from the replacement. The supervisor restarts the managed processes for the trial. It commits the catalog only after the Mac confirms the new generation. A candidate fault or a 45-second confirmation timeout restarts the previous catalog. A reboot before confirmation also loads the previous catalog.

Use `build/moto-package diagnostics-recovery` to read the most recent trial failure in this boot.

Run `make test-moto-diagnostics` to exercise a confirmed replacement, a process fault and a missed confirmation. QEMU runs the Moto supervisor. The test signs its edited packages again with the build's developer key, since the supervisor runs nothing else. The confirmation test substitutes a storage acknowledgment and checks the resulting catalog bytes. The timeout test shortens the trial on a disposable copy of the image. USB enumeration, transfer and persistence across a physical reboot require the Moto.

## Physical validation on the XT2163DL

On November 21, 2026, the Mac flashed slot B once with image `2748c642024e`. The factory Diagnostics payload checksum was `26be4ec3`. The following trials used signed package 21 transfers over the running USB connection. The two working packages differed in the USB device release descriptor so the Mac could distinguish them during enumeration.

| Trial | Handset observation |
| --- | --- |
| Confirm a working replacement | The Mac transferred 12,320 bytes. Diagnostics `52406cf1` answered after confirmation, with trial 0, supervisor generation 3 and catalog bank 2. |
| Withhold confirmation | Diagnostics `cf420aa7` answered in trial 4. After the confirmation window expired, `52406cf1` answered in generation 5 and reported `confirmation timeout`. |
| Crash at process entry | Signed candidate `e0414e57` executed an undefined instruction on startup. The Mac reconnected to `52406cf1` in generation 7 and read `Diagnostics fault`. |

After both failures, Runtime and disk reported ready. Radio remained `7427e789` and the committed catalog remained in bank 2. Both recoveries completed without handset input or another flash.
