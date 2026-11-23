# Display service

The [display process](process.rs) reads the visible app's surface and writes its changed rows into the framebuffer. A classic 240×400 surface is scaled 3×4. A present with `HD_PRESENT` set copies 720×1600 rows unscaled. The supervisor maps every surface read-only into Display, in its own 5 MiB window starting at `0x20000000`. Each app can write only its own surface. Runtime checks foreground ownership before forwarding a present request.

Run `make test-emulator` to check rendered pixels and `make test-isolation` to check that apps cannot directly read scanout or a peer's surface.
