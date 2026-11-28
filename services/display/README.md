# Display service

The [Display process](process.rs) is the system [compositor](../../system/compositor/README.md) over the real framebuffer. It alone maps both scanout and the built-in apps' surfaces. A built-in app draws into its private 240 × 400 surface and names the rows it changed; Runtime forwards that only from the app in front, and the compositor clips it, scales the changed rows (NEON on the device) and writes them to scanout, timing each composition against an 8 ms budget.

The Open Runtime does not present through Display yet: it still maps scanout and draws open apps and the system band, lock screen and prompts itself. The compositor README says what moving it takes.

`make test-unit` runs the compositor's host tests (every pixel compared with the loop this process used before). Run `make test-emulator` to check rendered pixels in QEMU and `make test-isolation` to check that apps cannot directly read scanout or a peer's surface.
