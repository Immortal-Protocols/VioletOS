# Power service

The [QEMU power process](process.rs) reads a virtual battery level and records sleep and wake requests. Runtime blanks the display on sleep and consumes the first press to wake. The virtual CPU continues executing while the screen sleeps.

Run `make test-stack` to check both the device state and the framebuffer. Hardware suspend, charging and shutdown require a handset implementation.
