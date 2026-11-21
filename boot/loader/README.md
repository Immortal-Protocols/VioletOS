# Bootloader

The handset bootloader loads the boot image. The [VioletOS loader](../../platform/sel4/loader/) checks the handoff, prepares memory and starts seL4, which launches the first user process. Use `make qemu-image` or `make moto` to build the corresponding image.
