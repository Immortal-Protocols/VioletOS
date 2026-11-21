# USB Diagnostics

The Moto supervisor starts [USB Diagnostics](process.rs) as a separate Rust process from package 21. It owns the handset USB controller and exposes read-only snapshots of Radio, Power and startup status. Mac clients use the same connection to stage signed replacements, request a scan or join, read the saved stopwatch value, and request a reboot. Radio owns Wi-Fi, Storage owns the catalog and checkpoint, and the supervisor verifies package signatures and decides which package runs.

The [host clients](host/) run on the Mac. Build them with `make build/moto-debug build/moto-package`. Run `build/moto-debug` for a JSON snapshot and `build/moto-package diagnostics-status` for the running package checksum and trial generation.

## Replace Diagnostics over USB

Build a replacement with `make moto-component NAME=usb-diagnostics`. On a handset running a baseline with package 21, run `tools/moto-install-diagnostics build/sel4/moto/packages/usb-diagnostics.vpk`. The installer signs the VPK using this build's developer key and waits for a status snapshot from the replacement. The supervisor restarts the managed processes for the trial. It commits the catalog only after the Mac confirms the new generation. A candidate fault or a 45-second confirmation timeout restarts the previous catalog. A reboot before confirmation also loads the previous catalog.

Use `build/moto-package diagnostics-recovery` to read the most recent trial failure in this boot.

Run `make test-moto-diagnostics` to exercise a confirmed replacement, a process fault and a missed confirmation. QEMU runs the Moto supervisor. The confirmation test substitutes a storage acknowledgment and checks the resulting catalog bytes. The timeout test shortens the trial on a disposable copy of the image. USB enumeration, transfer and persistence across a physical reboot require the Moto.
