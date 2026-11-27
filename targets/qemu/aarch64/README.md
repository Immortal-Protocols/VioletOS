# AArch64 Open Runtime in QEMU

This development target boots a 64-bit seL4 kernel, a VioletOS root task, and a Linux AArch64 ELF in a separate address space. The default ELF reserves 64 GiB and another 4,432,406,249,472 bytes of virtual memory, commits a page at the far end, checks zero-fill and read/write access, and unmaps the region. These reservation sizes occurred in a reference run of the pinned Ladybird JavaScript engine.

Use seL4 17.0.0, the Microkit 2.4.0 SDK and matching source, Python with seL4's build dependencies, LLVM including ld.lld, CMake, Ninja, dtc, make, and qemu-system-aarch64. The existing seL4 isolation spike uses the same SDK and source releases.

```sh
export MICROKIT_SDK=/path/to/microkit-sdk-2.4.0
export MICROKIT_SRC=/path/to/microkit-2.4.0
export SEL4_SRC=/path/to/seL4-17.0.0
python3 targets/qemu/aarch64/build.py --test
```

To run another executable, supply its path with `--elf`. For dynamically linked ELFs, `--root` takes an unpacked directory of files, including the AArch64 ELF interpreter at its requested guest path and the target shared libraries. If the executable is under this directory, its relative path becomes its guest pathname. Other executables appear as `/apps/program`. The guest opens immutable copies of the supplied files. Each `--arg` supplies one argument after argv[0].

These examples use the statically linked LibJS frontend host and the upstream dynamically linked interpreter respectively:

```sh
python3 targets/qemu/aarch64/build.py --elf build/ladybird-native/compile-js \
    --arg 'const answer = 6 * 7;' --expect 'LibJS compiled' --test
python3 targets/qemu/aarch64/build.py --root build/ladybird-native/upstream-js-root \
    --elf build/ladybird-native/upstream-js-root/apps/program \
    --arg=-c --arg 'console.log([2, 3, 7].reduce((a, b) => a * b))' --expect 42 --test
```

The build writes `violet.img` and `serial.log` under `build/sel4/qemu-aarch64`. Use `--out` to give another run a separate build directory and log. The `--test` launcher personalizes a temporary boot image with host time and 4 KiB of fresh entropy for that boot. The graphical launcher also supplies fresh host entropy through guest memory for getrandom and the guest random devices. `--trace` prints syscall numbers and results over the serial console.

The Cortex-A57 seL4 configuration supports 44-bit user virtual addresses. QEMU emulates a Neoverse N1 to support the LSE atomic instructions in upstream binaries. The root task builds the Linux initial stack, maps ET_EXEC or ET_DYN plus PT_INTERP, and handles Linux SVC 0 in an isolated seL4 process. Native kernel calls use SVC 1. The patch applies to a build-local copy of seL4. The AArch64 target has 4 GiB of guest RAM. Its pager tracks up to 2.5 GiB of resident mappings across 32 processes. Processes can share whole read-only pages from the application bundle. Private writes get their own physical pages. A separate 128 MiB pool holds temporary files and shared media buffers.

A successful run of the interpreter exercises UTF-8 input, JavaScript evaluation and serial output. Displaying a web page also requires a browser frontend, WebContent, IPC with the other browser processes, and platform devices. The regular 32-bit VioletOS emulator starts with `make emulator`.

## Process launch

The Linux clone path supports thread groups, fork with a separate seL4 address space, and the vfork pattern used by musl's posix_spawn. Exec loads and validates the new image before switching the calling TCB to it. A parent can inherit or duplicate descriptors, close them on exec, and collect a child's exit status with waitpid. The spawn regression forks, checks parent/child memory isolation, then spawns a fresh copy of itself twelve times using an inherited pipe and an explicit environment.

With the AArch64 open-app toolchain built, run:

```sh
build/open-toolchain/bin/aarch64-linux-musl-cc -O2 -D_GNU_SOURCE \
    targets/qemu/aarch64/spawn.c -o build/ladybird-native/spawn-parent
python3 targets/qemu/aarch64/build.py --elf build/ladybird-native/spawn-parent --test
```

The same parent accepts a bundled child executable followed by its arguments. To start the upstream renderer in a separate child and collect its status:

```sh
python3 targets/qemu/aarch64/build.py --root build/ladybird-native/webcontent-root \
    --elf build/ladybird-native/spawn-parent --arg=/opt/ladybird/libexec/WebContent \
    --arg=--help --expect 'VioletOS parent waited for /opt/ladybird/libexec/WebContent (status 0)' --test
```

## Home and the graphical browser

The 64-bit desktop boots the same Home and Ladybird apps as the phone. Tap the Ladybird tile to browse, or tap Home in its toolbar to return to the launcher. The native seL4 guest paints the 720 × 1600 display and writes decoded stereo PCM to its 48 kHz sound card. The SDL viewer presents the pixels, plays the sound and sends touch events. The QEMU network relay carries the guest's DNS and TCP traffic.

```sh
make emulator-aarch64
make test-ladybird-aarch64-media
```

The desktop build uses the project's Rust musl targets, Android NDK and pinned FFmpeg sources plus the seL4 tools above. If the three seL4 and Microkit directories already exist under `build/spike1`, the build finds them automatically. Otherwise export `MICROKIT_SDK`, `MICROKIT_SRC` and `SEL4_SRC` as shown above. The emulator also requires SDL2. The desktop image is written to `build/sel4/qemu-aarch64-desktop/violet.img`.

When installed at `build/ladybird-native/document/root/usr/libexec/ladybird-dom`, the pinned upstream LibWeb document helper and the resources in its package are included in this image. Set `LADYBIRD_ENGINE_ROOT` to select a different guest package, or use `python3 targets/qemu/aarch64/desktop.py --engine-root /path/to/root`. Without an installed helper, the graphical browser uses its built-in document parser.

The optional media test runs without a window. It opens the graphical browser from Home, taps its first bookmark mapped to a local HTTP fixture, and selects the fixture's Play video link. It checks for alternating red and blue VP9 frames and a 440 Hz Opus tone in the guest PCM ring, and saves `media.png` next to the image. To check navigation alone with a built image, use the following command.

```sh
python3 targets/qemu/aarch64/viewer.py --out build/sel4/qemu-aarch64-desktop --test session
```

## Upstream Hub and its helper processes

On an ARM64 Linux machine with the pinned upstream Ladybird package, DejaVu fonts and Liberation fonts installed, package the browser and its dependencies. Pass a local HTML file to render. For example, a page containing `<p id="answer"></p><script>answer.textContent = "Violet DOM " + (6 * 7)</script>` produces `Violet DOM 42`.

```sh
python3 tools/ladybird_native/package_helper.py Hub WebContent RequestServer SystemProbe Compositor \
    --document document.html > headless-root.tar.gz
```

Extract the archive on the QEMU host and run its Hub. The command below exercises upstream IPC, shared memory, JavaScript, HTML layout and headless text output in separate native VioletOS processes. The helpers install the default upstream sandbox using Landlock filesystem ABI 1 and seccomp syscall filters. The guest’s temporary profile lasts for this boot.

```sh
mkdir -p build/ladybird-native/headless-root
tar -xzf headless-root.tar.gz -C build/ladybird-native/headless-root
python3 targets/qemu/aarch64/build.py --root build/ladybird-native/headless-root \
    --elf build/ladybird-native/headless-root/opt/ladybird/libexec/Hub \
    --arg=--headless=text --arg=--temporary-profile \
    --arg=file:///document.html --expect 'Violet DOM 42' --test
```

The guests share a writable, memory-backed `/tmp`. The native regressions check cross-process descriptor passing, shared memfds, seals, `/proc/self/task` and `/proc/self/fd` enumeration, and the permissions enforced by inherited and stacked Landlock rules.

```sh
build/open-toolchain/bin/aarch64-linux-musl-cc -static -O2 targets/qemu/aarch64/unix_test.c -o build/ladybird-native/unix-test
python3 targets/qemu/aarch64/build.py --elf build/ladybird-native/unix-test --expect 'file seals and named connect passed' --test
build/open-toolchain/bin/aarch64-linux-musl-cc -static -pthread -O2 targets/qemu/aarch64/landlock_test.c -o build/ladybird-native/landlock-test
python3 targets/qemu/aarch64/build.py --elf build/ladybird-native/landlock-test --expect 'inherited and stacked rules passed' --test
```

On macOS the AArch64 viewer has its own application identifier and ad-hoc signature. To rebuild just the window and verify its signature, run these commands from the repository root.

```sh
make build/sel4/VioletOS-AArch64.app/Contents/MacOS/viewer
codesign --verify --strict --verbose=2 build/sel4/VioletOS-AArch64.app
```

## Upstream AudioServer

The VioletOS platform backend implements Ladybird's `Audio::create_platform_playback_stream` device factory. The pinned AudioServer mixer pulls float samples from its shared per-client rings and applies the playback group's mute, authorization and gain. The backend converts the mixed stereo float samples to signed 16-bit PCM and writes them to `/dev/violet/audio`. The implementation accepts upstream resume, drain, discard and volume commands.

```sh
make test-ladybird-aarch64-audio-mixer
```

The test boots the upstream mixer inside a dedicated headless seL4 guest, submits 48,000 440 Hz frames to a real `Audio::SharedAudioFrameRing`, and waits for the mixer to drain. The host independently reads the guest sound card to verify the frequency and number of frames. The target also produces `build/ladybird-native/audio/libviolet-audio-backend.a` for the ARM64 musl ABI.

To link the full upstream AudioServer service, first build the pinned document package, which provides its matching IPC endpoints and shared C++ dependencies. Use the same pinned Rust toolchain, target and Cargo setup as the document build.

```sh
python3 tools/ladybird_native/build_document.py
python3 tools/ladybird_native/build_audio.py --server
```

The service is built at `build/ladybird-native/audio/root/opt/ladybird/libexec/AudioServer`. It retains upstream control connections, shared buffers and platform sandbox setup. The separate mixer test exercises the output path in one guest process. The release build below exercises browser-to-AudioServer IPC from a playing HTML video.


## Full upstream browser window

Set `LADYBIRD_BROWSER_ROOT` to a directory containing the complete, matching AArch64 release helpers, runtime libraries, fonts, fontconfig configuration and resources. The package helper can collect these from an installed ARM64 release. The release used for the full browser bring-up was 2026.47.8 (source commit `0be55a57cc07ed258362c631a13a10bb4f6d707a`). The document build uses its own pinned helper set.

```sh
export LADYBIRD_BROWSER_ROOT=/path/to/unpacked/browser-root
make test-ladybird-upstream-view
make emulator-aarch64
```

The test boots a networkless guest with the upstream sandbox enabled. It verifies the page's initial color in native scanout, sends a tap through the VioletOS input device and checks the color changed by its JavaScript click handler. The interactive image boots Home. Tapping Ladybird launches Hub, which sends the software compositor’s RGB frames through its terminal graphics protocol to the VioletOS screen. The address bar and Home button use the native VioletOS keyboard and launcher.

## Full upstream video and sound

The interactive terminal frontend in Ladybird 2026.47.8 can use the VioletOS speaker. Build its matching Hub and AudioServer with the native AArch64 toolchain. The first command supplies the third-party libraries and headers. The release builder downloads the checksum-pinned 2026.47.8 source and generates that release's IPC endpoints.

```sh
python3 tools/ladybird_native/build_document.py
python3 tools/ladybird_native/build_release_services.py
export LADYBIRD_BROWSER_ROOT=/path/to/unpacked/2026.47.8-browser-root
export LADYBIRD_RELEASE_SERVICES="$PWD/build/ladybird-native/release-audio/root"
make test-ladybird-upstream-media
make emulator-aarch64
```

To collect a complete package on the ARM64 build machine, run the following command with release 2026.47.8 installed and extract the archive into `LADYBIRD_BROWSER_ROOT` on the QEMU host.

```sh
python3 tools/ladybird_native/package_helper.py Hub WebContent RequestServer SystemProbe Compositor \
    MediaServer VideoDecoderServer AudioServer ImageDecoder WebRTCClient > browser-root.tar.gz
```

The opt-in test boots the same terminal frontend and normal browser sandbox in a guest with no network device. It taps Play on a bundled page, appends VP9/Opus WebM to a JavaScript `MediaSource`, observes red and blue HTML video frames in VioletOS scanout, and measures a 440 Hz tone and at least 48,000 stereo frames in the guest speaker ring. The screenshot and image are under `build/sel4/qemu-aarch64-upstream-media`. The test allows six minutes for software emulation.

Hub selects device output for the interactive terminal. Other headless modes use the upstream null output. AudioServer opens and configures the native 48 kHz speaker before it installs its normal filesystem and syscall filters. Its per-tab mixer continues to enforce playback authorization and mute. An upstream CMake build with the release's regular build prerequisites can enable the same backend with `python3 tools/ladybird_native/prepare_release_audio.py --build` on an AArch64 Linux build machine.


To measure the complete six-second fixture, use the dedicated headless image built by the media test.

```sh
python3 targets/qemu/aarch64/viewer.py --out build/sel4/qemu-aarch64-upstream-media --test upstream-duration --timeout 360
```

The duration check reports wall-clock intervals between all six color blocks and samples fresh 440 Hz speaker output through at least 4.3 seconds of the clip. It requires at least 250,000 PCM frames, a maximum four-second interval between picture changes, and no gap greater than 1.2 seconds between tone measurements.

### Playing a public YouTube embed

Run `make emulator-ladybird-youtube` to open the interactive player. The launcher boots VioletOS, taps Play and opens a window on that same guest once it measures six seconds of moving video and audible stereo output. Use the player controls and the VioletOS address bar in that window.

Playback on this software-emulated development target can buffer. The public embed passed the simultaneous picture and sound check. The Big Buck Bunny desktop watch page also started playing, then displayed a buffering spinner in a longer run.

With the browser and matched release services configured above, run `make test-ladybird-youtube-iframe`. The guest opens Google's public sample for video `M7lc1UVf-VE` over HTTPS. The sample displays its actual player state as a colored iframe border. The check taps the native Play control when the sample reports ready, waits for the playing state, and measures changing video pixels, media received from Google's CDN, and audio from the native speaker. It saves the guest framebuffer in `build/sel4/qemu-aarch64-youtube-iframe/playing.png`. [Google's IFrame API reference](https://developers.google.com/youtube/iframe_api_reference)

### Measuring a public YouTube watch page

Use an image built with the complete matched browser, native release services, networking and a normal public watch URL. Each run creates a new QEMU guest. For the Big Buck Bunny watch page at the standard 720 × 1600 geometry:

```sh
python3 targets/qemu/aarch64/desktop.py --open 'https://www.youtube.com/watch?v=YE7VzlLtp-4' --out build/sel4/qemu-aarch64-youtube
python3 targets/qemu/aarch64/viewer.py --out build/sel4/qemu-aarch64-youtube --test youtube --player-tap 355 400 --timeout 420 --screenshot build/sel4/qemu-aarch64-youtube/watch.png
```

The optional `--player-tap X Y` sends a single touch once a textured player appears. Omit it to monitor autoplay. Set `--player-rect LEFT TOP WIDTH HEIGHT` if the watch page moves the player. The default samples 192 locations inside the video, away from the toolbar and player controls. A passing run requires at least six seconds of moving picture and fresh audible stereo PCM with activity across three quadrants of the video. The monitor also requires bytes received by the guest from the public watch site and at least 256 KiB from a YouTube media hostname. It reports what played in that watch session, which may include a preroll advertisement. Public availability and page geometry can vary by region.
