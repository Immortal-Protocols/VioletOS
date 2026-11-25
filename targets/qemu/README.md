# QEMU emulator

The [QEMU launcher](../../tools/run.py) boots seL4 and connects the guest framebuffer and input to the desktop viewer. Run `make emulator` to use Home and Settings with the mouse. Use `make test-emulator` and `make test-isolation` to check touch, the watchdog and access boundaries.

Tests that tap apps find their icons with [home_grid.py](home_grid.py). [open-test-apps](open-test-apps) holds the test programs (Rust, C, Go, Python and the Sounds app) that `make test-open` and `make test-audio` build into their image. The main [README](../../README.md) lists the end-to-end test commands.
