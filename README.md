# VioletOS

VioletOS is a smartphone operating system built on the seL4 microkernel. It runs on the Moto G Pure (XT2163DL) and in QEMU on a Mac, with no Linux kernel anywhere. Drivers and services are separate seL4 processes, mostly written in Rust. Apps come in two kinds:

- **System components** (the classic launcher, Wi-Fi Setup, and the Stopwatch package used by the package-system tests) are small `no_std` Rust programs that talk to **Runtime**, VioletOS's own app interface.
- **Open apps** are ordinary Linux ARM programs (Rust `std`, C, Go, or a `#!/bin/python3` script) dropped into `/apps`. The built-in apps (Home, Camera, Wi-Fi, Ladybird, Music, Weather, Stopwatch, Sounds) are open apps written in Rust. [Ladybird](apps/open/ladybird/README.md) is a web browser built on the Ladybird project's own Rust HTML parser. The **Open Runtime** runs them unmodified by answering their Linux system calls. See the [Open Runtime](runtime/open/README.md) and its [RFC](docs/open-app-model-rfc.md).

**Rust is the preferred language for new VioletOS code**, including services, drivers and application logic.

## Run it on the Mac

| Command | What you get |
| --- | --- |
| `make emulator` | A clickable seL4 window. The classic launcher starts; **APPS** opens Home and the open apps. **Sounds** plays through the Mac's speakers and records from its microphone; the arrow keys are the volume buttons and **H** plugs in headphones. Close the window or press Escape to quit. |
| `make sim` | The original native SDL simulator, including its clock and system screens. Use it for quick layout and interaction experiments. |
| `make test-emulator` | Eight QEMU scenarios covering stopwatch behavior, touch transfer failures, recovery and watchdog handling. The tests read the guest's rendered pixels. |
| `make test-stack` | Exercises Home, Quit, relaunch, radio, sleep and wake, package removal, installation and crash rollback inside QEMU. |
| `make test-apps` | Installs a separately built app, reads its file after reboot, uninstalls it, and recovers from a stalled service update. |
| `make test-storage` | Boots seven QEMU guests against a disposable disk to check independent app data, durable reset and torn-sector recovery. |
| `make test-isolation` | Boots a diagnostic image and checks that seL4 faults when the UI tries to read touch registers, disk registers or the disk transfer buffer. |
| `make test-wifi` | Checks connection, gateway replies, link failure, radio-off handling and saved network selection across QEMU boots. |
| `make test-wifi-live` | Validates the certificate in the guest and reads a live UUID from a public HTTPS endpoint. |
| `make test-power` | Verifies guest-driven VM pause, resume and shutdown. |
| `make test-audio` | Holds the virtual volume buttons, plugs in headphones, plays sounds and records a tone from the Sounds app, then checks the PCM, the recording and the on-screen volume. |
| `make test-open` | Runs unmodified Linux ARM programs under the Open Runtime: Rust, C, Go and Python, including drawing, touch, PCM audio and TCP. |
| `make test-wifi-app` | Drives the Wi-Fi app by touch: scans, types a wrong and then the right WPA2 password on the on-screen keyboard, switches to an open network, and rejoins with the saved key after a reboot. |
| `make test-open-apps` | Drives the built-in apps: Stopwatch's time in `/data` across a restart, Sounds, and live Weather. |
| `make test-shield` | Violet Shield end to end: the Sentinel streams telemetry to a companion that scores it with EmbeddingGemma 2 and freezes a data-stealing app with a signed Shield. `make test-shield-ios` uses the real app in the iOS Simulator. See [the proof of concept](docs/violet-shield-poc.md). |
| `make test-agent` | The voice agent in QEMU with the real Mac app: QR pairing through a local relay, an encrypted install that survives a restart, reconnect and unpair. See [the agent](apps/open/agent/README.md) and [Violet Agent for macOS](companion/mac/README.md). |
| `make test-camera` | Opens Camera from Home and checks the synthetic sensor's colour bars on screen, in order, and that the picture moves. |
| `make test-ladybird` | Drives the Ladybird browser by touch: links, a linked style sheet, Back, scrolling and a typed address against a local web server, and HTTPS when the Mac is online. |
| `make` or `make moto` | The handset image at `build/sel4/moto/boot.img`. This command builds locally; installing it is a separate step (see [Handset](#handset)). |
| `make moto-debug` | Reads handset status over USB. See [USB Diagnostics](services/usb-diagnostics/README.md). |

### Prerequisites

On an Apple Silicon Mac, install the Xcode command-line tools, Python 3 and these Homebrew packages. The native simulator needs only the host C toolchain, Make and SDL2.

```sh
brew install qemu sdl2 cmake ninja dtc
```

Both seL4 targets need Rust 1.91.1 with the `armv7a-none-eabi` target and Android NDK **28.2.13676358** for its ARM cross-compiler. The build looks under `~/Library/Android/sdk/ndk/28.2.13676358`; set `ANDROID_NDK_HOME` if it lives elsewhere. Put `rustc` on PATH (or set `RUSTC`). The first build downloads pinned seL4, libfdt and TLS sources, checks their SHA-256 hashes and installs the Python build dependencies in a local virtual environment. QEMU builds and runs without a handset or factory files.

The open apps in [`apps/open`](apps/open) need their own toolchains. Each is optional: a program whose toolchain is missing is left out of the image, and `make test-open` reports it as skipped.

| For | Install |
| --- | --- |
| Rust `std` programs | `rustup target add armv7-unknown-linux-musleabihf` |
| The Ladybird browser | `rustup toolchain install 1.98.0 --target armv7-unknown-linux-musleabihf` (Ladybird's Rust version); the build fetches the pinned Ladybird sources |
| Go programs | `brew install go` |
| C programs | `python3 tools/build_python.py musl` (builds a musl sysroot with the NDK's clang) |
| `/bin/python3` | `brew install python@3.12`, then `python3 tools/build_python.py python` (cross-builds CPython 3.12 with zlib) |

## Architecture

![The open VioletOS stack: apps and the Open Runtime above Rust services, all on seL4, on QEMU and the Moto G Pure.](docs/open-violetos-app-stack.png)

From the bottom up:

1. **seL4 kernel** ([kernel/sel4](kernel/sel4/README.md)). A pinned upstream seL4 for AArch32, built per target with the [board configuration](platform/sel4/board/). The [loader](boot/loader/README.md) checks the handset's live memory map, stages the kernel and first task, and enters seL4.
2. **Supervisor** ([system/supervisor](system/supervisor/README.md)). The first task. It reads the package catalog, builds every process's address space and capabilities, maps each device into exactly one process, starts everything, and restarts the group or rolls back to the previous catalog after a fault or stall. The [package manager](system/package-manager/README.md) installs and removes components.
3. **Services**, one process per device or function. Only the service that owns a device can touch it.
4. **Two app interfaces.** [Runtime](runtime/README.md) gives built-in apps Subscribe, Get and Execute operations over a 14-word IPC message, decides which app is in front, and routes requests to services through worker threads. The [Open Runtime](runtime/open/README.md) gives open apps the Linux ARM system call interface plus `/dev/violet/{screen,events,sys,audio}`, and is itself one of Runtime's clients.
5. **Apps.** Built-in apps draw into a private 240 × 400 surface that Display scales to the screen. Open apps `mmap` a full 720 × 1600 buffer.

USB Diagnostics runs only on the handset and serves developer tools on a connected Mac. The [iPhone companion](companion/ios/README.md) relays the handset's traffic over cellular data (Phone Internet).

**The voice agent.** [Agent](apps/open/agent/README.md) on the phone pairs with [Violet Agent for macOS](companion/mac/README.md) by a one-time QR code and then meets it through a relay (a [Chariot](tools/chariot-relay/relay.py)) from any network, encrypted end to end ([protocol](docs/agent-protocol.md)). You talk to the phone; the Mac hears and speaks with RoseOS's voice models and answers with Codex, which builds the apps you ask for and installs them on the phone, where they stay. Say "Make me a simple scorekeeper" and it is on the phone in under 20 seconds.

### Components

Each component is a package in the [catalog](image/catalog/components.json); its number is also its identity to the supervisor.

| ID | Component | Role |
| --- | --- | --- |
| 1 | [Launcher](apps/launcher/README.md) | Home: status, app list, Wi-Fi or Phone Internet, sleep, the **/APPS** button. |
| 2 | [Stopwatch](apps/stopwatch/README.md) | The first app; keeps its time across reboots. |
| 3 | [Runtime](runtime/README.md) | The built-in app interface, focus and routing. |
| 4 | [Touch](services/touch/README.md) | Novatek touch controller over SPI. |
| 5 | [Display](services/display/README.md) | Presents built-in apps' surfaces to the screen. The Open Runtime draws there itself while an open app is in front. |
| 6 | [Storage](services/storage/README.md) | Checkpoints, per-app files and the package catalog on disk (virtio in QEMU, eMMC on the handset). |
| 7 | [Radio](services/radio/README.md) | Wi-Fi: scan, WPA2, DHCP, DNS, TCP and TLS; hosts the Phone Internet network; serves open apps' TCP sockets. |
| 8 | [Power](services/power/README.md) | Battery, sleep and shutdown; brokers the PMIC for Audio and Camera. |
| 9 | [Package manager](system/package-manager/README.md) | Installs, removes and rolls back components. Always loaded from the factory image. |
| 10 | [Wi-Fi Setup](apps/wifi/README.md) | Chooses a network and tests the connection. |
| 11, 12 | Library apps | Free slots for separately installed apps. |
| 15–20 | Runtime workers | Runtime's per-service request threads. |
| 21 | [USB Diagnostics](services/usb-diagnostics/README.md) | Handset only: status, package staging, flashing and test requests from the Mac. |
| 22 | [Audio](services/audio/README.md) | Speaker, headphones, jack, volume buttons, microphone, and open apps' PCM. |
| 24 | [Open Runtime](runtime/open/README.md) | Runs Linux ARM programs from `/apps`. |
| 25 | [Camera](services/camera/README.md) | The rear camera's sensor, receiver and frames, for open apps' `/dev/violet/camera`. |

Slot 23 is reserved for a Bluetooth service.

### What runs today

| | QEMU on a Mac | Moto G Pure |
| --- | --- | --- |
| Boot, supervisor, recovery and rollback | Yes | Yes, from slot B, with Android kept on slot A |
| Touch and display | Mouse clicks become touch reports | Novatek touch on either DJN panel |
| Storage | Virtual disks | eMMC volume inside boot B; stopwatch time survives reboot |
| Wi-Fi | Virtual access points, open and WPA2; HTTPS through the Mac | Scan, choose and type the password in the Wi-Fi app (WPA2-Personal), or provision over USB; HTTPS UUID |
| Phone Internet | — | Hosts a Wi-Fi network for the iPhone relay; traffic leaves over cellular |
| Audio | Mac speakers and microphone | Speaker, headphones, jack, Volume Down and microphone (Volume Up not yet) |
| Camera | A synthetic sensor (colour bars) | Rear Hi-1336 at 1280 × 720, 30 frames per second; the Camera app shows about 19 |
| Open apps | Rust, C, Go and Python, with screen, touch, audio and TCP | Rust, C, Go and Python; PCM audio; TCP over Wi-Fi |

On the handset, apps are started from Home by touch, or from the Mac over USB (for example `build/moto-package open-run=/apps/hello` and `build/moto-package open-log`).

## Code organization

```
apps/                 Built-in apps (no_std Rust) and open apps
  launcher/ stopwatch/ wifi/ sounds/ weather/   one process.rs each; drawing.rs is shared
  open/               Linux programs for /apps, one folder each (a Cargo crate, main.rs, main.go, main.c or a script)
  lib/violet/         the built-in apps' shared Rust crate: devices, drawing, text, HTTPS
  examples/           Sketches from the open app model RFC
runtime/              Runtime: protocol.rs (wire format and keys), process.rs, lanes.rs (workers), sdk.rs (app side)
  open/               Open Runtime: syscalls, devices, sockets, file system, memory pool
services/             One folder per service: touch, display, storage, radio, power, audio, camera, usb-diagnostics
system/
  supervisor/         The first task (supervisor.c) and the open apps' process manager (procmgr.rs)
  package-manager/    Catalog transactions on the device and manage.py on the Mac
image/                Component catalog and the package format and builder
kernel/sel4/          seL4 download, patches and build
platform/sel4/        Board configuration, shared headers, the loader and image build
boot/loader/          The loader that runs before seL4
targets/
  qemu/               QEMU backends (audio, power, touch fixture) and the QEMU test suites
  moto-g-pure/        Handset backends: Wi-Fi driver, lwIP and WPA2, audio, eMMC, power; handset tests
  mac-simulator/      The native SDL simulator
tools/                Build and run on the Mac: run.py, the emulator viewer, image builders, network and USB helpers
companion/ios/        Violet Relay, the iPhone app for Phone Internet
companion/mac/        Violet Agent, the Mac half of the voice agent
docs/                 Architecture slides, the open app model RFC and the relay plan
spikes/               Earlier experiments and the handset bring-up log
src/, include/        The native simulator's UI, the bitmap font and shared headers
```

Code that differs between QEMU and the handset lives under `targets/` behind a common interface. For example, Audio builds [services/audio/process.rs](services/audio/process.rs) with either [targets/qemu/audio.rs](targets/qemu/audio.rs) or [targets/moto-g-pure/audio.rs](targets/moto-g-pure/audio.rs).

## Where to change the code

| If you want to change… | Start here | Where it runs |
| --- | --- | --- |
| A built-in app's screen or behavior | Its `process.rs` under [apps/](apps/), drawing with [drawing.rs](apps/drawing.rs) and the [bitmap font](src/glyphs.h). The stopwatch's state machine is [state.rs](apps/stopwatch/state.rs). | Moto and QEMU |
| What built-in apps can ask for | [protocol.rs](runtime/protocol.rs) defines the keys; [process.rs](runtime/process.rs) decides who may use them and routes them. | Moto and QEMU |
| Which Linux calls open apps get | [syscall.rs](runtime/open/syscall.rs), with [sockets.rs](runtime/open/sockets.rs), [device.rs](runtime/open/device.rs) for `/dev/violet` and the app list, and [fs.rs](runtime/open/fs.rs) for files. | Moto and QEMU |
| How open apps are loaded and paged | [procmgr.rs](system/supervisor/procmgr.rs) and [pool.rs](runtime/open/pool.rs). | Moto and QEMU |
| What goes into `/apps` | [build_initfs.py](tools/build_initfs.py); Python itself comes from [build_python.py](tools/build_python.py). | Build host |
| Which process gets which device, memory or endpoint | [supervisor.c](system/supervisor/supervisor.c) and [platform.h](platform/sel4/include/platform.h). | Moto and QEMU |
| How a touch becomes a tap | [touch.rs](services/touch/touch.rs) and [spi.rs](services/touch/spi.rs); [spi_mmio.c](targets/moto-g-pure/spi_mmio.c) accesses real registers, [fixture.c](targets/qemu/fixture.c) models them, and [events.c](services/touch/events.c) carries events to Runtime. | Shared driver, one backend per target |
| Durable app data | [storage.rs](services/storage/storage.rs), [files.rs](services/storage/files.rs), [journal.rs](services/storage/journal.rs); [block.rs](services/storage/block.rs) (virtio) or [emmc.rs](targets/moto-g-pure/emmc.rs). | Moto and QEMU |
| Wi-Fi, TCP and TLS on the handset | [radio.rs](targets/moto-g-pure/radio.rs) (Radio's event loop), [wlan_axi.rs](targets/moto-g-pure/wlan_axi.rs) (the chip), [wpa.rs](targets/moto-g-pure/wpa.rs), and lwIP glue in [ip/](targets/moto-g-pure/ip/) ([sockets.c](targets/moto-g-pure/ip/sockets.c) serves open apps). | Moto |
| Networking in QEMU | [services/radio](services/radio/README.md) with the Mac-side [emulator_endpoint.py](tools/emulator_endpoint.py) and, for open apps' sockets, [net_relay.py](tools/net_relay.py). | QEMU and the Mac |
| Sound and the microphone | [services/audio](services/audio/README.md) and the board backend under `targets/`. | Moto and QEMU |
| Watchdog behavior | [watchdog.c](platform/sel4/board/watchdog.c) | Moto and QEMU |
| Where seL4 comes from and how it boots | [kernel/sel4/build.py](kernel/sel4/build.py), the [board configuration](platform/sel4/board/) and the [loader](boot/loader/). | Build host and early ARM boot |
| The handset image layout | [loader/build.py](platform/sel4/loader/build.py) links the payload; [phone_image.py](tools/phone_image.py) writes the Android v2 boot format, with the `/apps` image as its ramdisk. | Build host |
| How the Mac window talks to the guest | [run.py](tools/run.py) starts QEMU and the Mac-side helpers; [viewer.c](tools/emulator/viewer.c) shows the 720 × 1600 framebuffer, plays and records sound, and turns mouse events into touch reports. | Mac |
| The native simulator | [screen.c](src/screen.c) and [simulator.c](targets/mac-simulator/simulator.c). | Native host process |

For example, change the **Start** label in `apps/stopwatch/process.rs` to update both seL4 targets. Change `spi_mmio.c` and only the handset build changes. To mirror a design change in the native simulator, edit `screen.c` as well.

## Try it in QEMU

Start the stopwatch, go **Home**, then reopen it to see the timer still running. **Quit** ends that instance; reopening it restores the last saved checkpoint. **Radio** toggles the virtual modem, **Sleep** pauses QEMU until the next tap, and **Apps → Off → Confirm** shuts the VM down. See the [package manager guide](system/package-manager/README.md) to replace a component independently.

Open **Wi-Fi Setup → Connect → Test Radio** to display a fresh UUID from the configured HTTPS endpoint. The guest verifies the certificate; the Mac only relays TCP bytes. The **Weather** open app fetches wttr.in with its own TLS.

Tap **/APPS** for the open apps. **Ladybird** opens a start page of links; tap the address bar to type any address. `hello`, `chello`, `gohello` and `pyhello` print to the Open Runtime's log; `canvas` and `pypaint` draw and answer taps; `pytone` plays a chord; `pynet` and `gonet` fetch a web page.

### Persistence

Open **Stopwatch → Start → Pause** and note the time. Close the emulator, run `make emulator` again, and open **Stopwatch**: the paused time returns. **Reset** saves zero. Closing while the timer runs restores the last saved checkpoint on the next boot.

QEMU uses a 1 MiB raw disk at `build/sel4/qemu/stopwatch.disk`. The guest keeps two alternating 512-byte sectors per app, each with a version marker, generation number, value and CRC. Storage acknowledges a save after the device flush completes and, on startup, chooses the newest valid record. The screen shows **DISK READY** when storage is available, **RAM ONLY** when it starts without usable storage, and **DISK ERROR** if a save fails. Runtime selects each app's private checkpoint and private files. A separate 4 MiB package disk holds the installed catalog and its rollback copy. The handset uses the same code with an MSDC0 block driver; see the [handset storage layout](targets/moto-g-pure/README.md#handset-storage). Open apps' writes stay in memory for now.

## Handset

The supported handset is the **XT2163DL** (board **ellis**) with firmware **S3RHS32.20-42-13-2-3-2**, on either DJN NT36525B panel revision: the loader reads which panel the bootloader lit, and Touch loads the matching firmware. Supply the stock boot image and both panel firmware files. Next to the stock boot image, create a `wifi/` folder with the files listed in the [radio firmware manifest](targets/moto-g-pure/wifi_firmware.py), copied from the factory vendor image's firmware directory. The build checks each input against its pinned SHA-256 digest.

```sh
export VIOLET_STOCK_BOOT=/path/to/exact-factory/boot.img
export VIOLET_TOUCH_FW=/path/to/djn_nt36525b_a_novatek_ts_fw.bin
make moto
```

VioletOS boots from slot B and leaves Android on slot A. With USB connected, `services/usb-diagnostics/host/moto-flash build/sel4/moto/boot.img` reaches fastboot without touching the phone, flashes slot B and waits for VioletOS to answer. The boot image carries the loader, seL4, the first task and its catalog, the Wi-Fi firmware and, in place of Android's ramdisk, the compressed `/apps` image. A 1 MiB storage volume and the catalog banks follow it in the partition.

Useful tools on the Mac:

- `build/moto-debug` prints a status snapshot; `build/moto-package` stages packages, reboots, plays sounds, reads the Open Runtime's log and starts open apps. See [USB Diagnostics](services/usb-diagnostics/README.md).
- `services/usb-diagnostics/host/moto-wifi-profile` provisions a WPA2 network for the current boot (it asks for the password privately), then `build/moto-package join-wifi` joins it and fetches a UUID over HTTPS.
- `services/usb-diagnostics/host/moto-relay` hosts the Phone Internet network for [Violet Relay](companion/ios/README.md) on an iPhone.

The [handset README](targets/moto-g-pure/README.md) records the bring-up and each validation. The [handset bring-up log](spikes/sel4-ellis/) covers the first trials, and the [original architecture deck](docs/VioletOS-architecture.pdf) the earlier Linux prototype.

## Continuous integration

[GitHub Actions](https://github.com/Immortal-Protocols/VioletOS/actions/workflows/ci.yml) runs on pull requests and pushes to `main`. The Linux job builds the QEMU image and runs the host unit tests (`make test-unit`); the macOS job builds the open apps it needs and runs two QEMU tests (`make test-ci`: one boot of the supervisor and its services, then the open apps). Every other QEMU suite (`make test-open`, `test-agent`, `test-ladybird`, `test-shield`, ...) runs locally.

**Keep CI under 5 minutes.** Don't add QEMU suites to `test-ci` or new jobs to the workflow; add a local `make` target instead and run it before you push. If CI must cover something new, extend an existing test or add a host unit test that runs in seconds, and leave apps CI doesn't need out of its build (`VIOLET_OPEN_SKIP` in the workflow).

The handset image and handset tests need the factory files and run locally. The public HTTPS endpoint test is separate: `make test-wifi-live`.

## Design documents

- [Open app model RFC](docs/open-app-model-rfc.md), with its implementation status.
- [Cellular relay plan](docs/cellular-relay-plan.md) for Phone Internet.
- The original design-review slides: [complete lifecycle stack](docs/violetos-complete-lifecycle-stack.png), [folders and layers](docs/violetos-folders-layer-cake.png), [Runtime contract and updates](docs/violetos-runtime-contract-updates.png) and the [earlier proposed stack](docs/proposed-violetos-stack.png).
