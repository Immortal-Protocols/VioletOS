# Bootloader

The handset bootloader loads the boot image. The [VioletOS loader](loader.c) checks the handoff, prepares memory and starts seL4, which launches the first user process. Use `make qemu-image` or `make moto` to build the corresponding image.

**Verified boot, QEMU only.** The QEMU loader starts the kernel and the root task only if they match a boot manifest signed by a key the image pins, and only if the system is not older than the bootloader's stored rollback floor ([verified.rs](verified.rs); the manifest comes from [tools/boot_manifest.py](../../tools/boot_manifest.py)). A release image's manifest also binds the open apps' image. Otherwise it prints `VioletOS loader: boot refused, reason N` and stops before copying anything. `make test-unit` runs [test_verified.sh](test_verified.sh).

**The handset loader checks no signature.** The phone's bootloader is unlocked and starts whatever boot image is written to it, so a check inside that image would not stop a replaced image. [docs/verified-boot.md](../../docs/verified-boot.md) has the whole chain, the refusal reasons, and what a locked handset would need.
