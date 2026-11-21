# QEMU emulator

The [QEMU launcher](../../tools/run.py) boots seL4 and connects the guest framebuffer and input to the desktop viewer. Run `make emulator` for the clickable stopwatch. Use `make test-stack`, `make test-emulator`, `make test-storage` and `make test-isolation` to check touch, persistent storage and access boundaries.

Use **Apps** to install or remove a package, **Wi-Fi Setup** to test the network, and **Sleep** or **Apps → Off → Confirm** for power control. The main [README](../../README.md) lists the end-to-end test commands.
