# Display service

The [QEMU display process](process.rs) reads the visible app's logical surface, scales its changed rows and writes them into the guest framebuffer. Each app can write only its own surface. Runtime checks foreground ownership before forwarding a present request.

Run `make test-emulator` to check rendered pixels and `make test-isolation` to check that apps cannot directly read scanout or a peer's surface.
