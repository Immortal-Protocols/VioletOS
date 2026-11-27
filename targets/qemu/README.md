# QEMU emulator

The [QEMU launcher](../../tools/run.py) boots seL4 and connects the guest framebuffer and input to the desktop viewer. Run `make emulator` to use Home and Settings with the mouse. Use `make test-emulator` and `make test-isolation` to check touch, the watchdog and access boundaries.

Tests that tap apps find their icons with [home_grid.py](home_grid.py). [open-test-apps](open-test-apps) holds the test programs (Rust, C, Go, Python and the Sounds app) that `make test-open` and `make test-audio` build into their image. The main [README](../../README.md) lists the end-to-end test commands.

Run `make test-ladybird-media` to verify native WebM playback from the Ladybird icon. The test measures pixels and PCM in a fresh VioletOS guest. To try a public clip, open Ladybird and select **Play a flower video** on its start page.

## Full Ladybird on VioletOS

The AArch64 target boots VioletOS Home on seL4. Install the matching ARM64 browser package, set `LADYBIRD_BROWSER_ROOT` and run `make emulator-aarch64`. The native window accepts touch, and browser subprocesses share video buffers through the Open Runtime. [AArch64 build and tests](aarch64/README.md)

## Upstream Ladybird YouTube reference on Ubuntu

Run this check from a checkout inside an ARM64 Ubuntu 24.04 QEMU guest with internet access. It uses the upstream graphical browser, measures actual YouTube video frames, and samples PCM from the guest's default PulseAudio sink. Use an isolated audio session so only the browser feeds the sink. The seL4 image built by `make qemu-image` currently runs VioletOS's parser-based browser.

Install the upstream ARM64 Ubuntu 24.04 package. The reference run uses Ladybird 2026.47.8. [Ladybird downloads](https://ladybird.org/download/)

```sh
sudo apt-get install ./ladybird_2026.47.8-1~ubuntu24.04_arm64.deb \
    libavcodec60 libgl1-mesa-dri make pulseaudio pulseaudio-utils xauth xvfb
pulseaudio --start --exit-idle-time=-1
xvfb-run -a env LIBGL_ALWAYS_SOFTWARE=1 /opt/ladybird/bin/WebDriver \
    --listen-address 127.0.0.1 --port 8000 --force-cpu-painting
```

In another guest terminal, run `make test-ladybird-youtube-reference`. This checks about 20 seconds of playback and saves the final frame to `build/ladybird-youtube.png`. Set `LADYBIRD_YOUTUBE_SECONDS=300` for five minutes, or set `LADYBIRD_YOUTUBE_URL` to test another public watch URL. Playback availability can vary by video and network.

On November 25, 2026, the 20-second check passed at 854 × 480 with 486 displayed frames, zero dropped frames, and a PCM peak of −22.9 dBFS. The five-minute WebDriver attempt stopped around 43 seconds with YouTube player error `ump.spsrejectfailure`. A local 360p VP9/Opus clip played through Media Source Extensions for 65 seconds with zero dropped frames.

The same video reached 5:13 in a regular Ladybird window at 854 × 480 and 24 fps with AV1 and Opus. YouTube reported 7,522 frames and zero drops at that point. The guest's audio sink was still receiving non-silent PCM. For this check, launch `ladybird --force-cpu-painting` on the guest's graphical display, select the YouTube tab, and use the player's Stats for nerds overlay.

The Ubuntu browser and its helpers used about 1.37 GiB of proportional resident memory in one sample during regular playback. Measurements above were taken in the Ubuntu guest. [VioletOS Open Runtime](../../runtime/open/README.md)
