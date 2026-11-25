# Camera service

Camera (component 25) runs the image sensor and delivers raw frames to open apps. Apps read them from `/dev/violet/camera` (see the [Open Runtime](../../runtime/open/README.md) and [abi.rs](../../runtime/open/abi.rs)); the built-in **Camera** app ([apps/open/camera](../../apps/open/camera/src/main.rs)) shows a live viewfinder from them.

Only the Open Runtime talks to Camera, through one page the supervisor shares between them and a 4 MiB frame window:

| Where | In Camera | In the Open Runtime |
| --- | --- | --- |
| Shared page | `0x09320000` | `0x09120000` |
| Frame window | `0x0b000000`, read-write | `0x0b000000`, read-only |

The Open Runtime sets word 0 (`W_WANT`) while the app on screen has `/dev/violet/camera` open. It clears it when that app closes the device, leaves the screen, or the phone sleeps. Camera then starts the sensor and fills three frame slots in turn. After a frame completes, Camera publishes the slot's offset (`W_OFFSET`), then counts the frame in `W_FRAMES`. A reader that sees `W_FRAMES` advance by at most one while it copies has a whole frame; the Open Runtime marks any other copy as torn. The other words give the state (off, starting, streaming, failed), the error, width, height, stride, format (the packing and the Bayer order) and the sensor's ID.

Camera keeps a simple auto-exposure: every third frame it measures the frame's mean over a sparse grid and steps exposure by about 12% towards a quarter of full scale. Each board's backend maps the 0–255 exposure level onto its sensor.

## Boards

- **QEMU** ([targets/qemu/camera.rs](../../targets/qemu/camera.rs)): a synthetic sensor that paints 640 × 480 RAW8 frames (BGGR) at about 15 per second. They show eight colour bars and a white square that crosses them every four seconds, at a brightness that follows the exposure. Its frame window is guest RAM at `0x78000000`.
- **Moto G Pure** ([targets/moto-g-pure/camera.rs](../../targets/moto-g-pure/camera.rs)): the rear SK Hynix Hi-1336 delivers 1280 × 720 RAW8 (GBRG) at 30 frames per second, through the CSI1 receiver, SENINF3 and CAMSV1 into physical `0xe0000000`. The [bring-up notes](../../targets/moto-g-pure/camera-bringup.md) describe the path and what the handset showed.

On the handset Camera has no direct access to the PMIC, the shared clocks or the pins:

- **The supervisor** (`camera_platform`) switches the CAM power domain (with its bus protection and SMI clamp), the `seninf_sel`, `camtm_sel` and `camtg_sel` clocks, the camera's MM bus bridge (`mm_smi_cam_ck`), the MIPI 26 MHz gates, I2C2's clocks and pins, MCLK0 and the sensor's AVDD, reset and torch pins.
- **Power** switches the MT6357 camera LDOs and nothing else: VCAMIO (1.8 V), VCAMD (1.1 V for the Hi-1336) and VCAMA, with the last two's voltage selectors.
- **Mapped into Camera:** camsys (clock gates), SMI larb3, the four sensor interfaces, CAMSV1, the six MIPI receiver halves and I2C2. Camera also holds the frame window's four 1 MiB section capabilities, to invalidate each slot's cache lines once CAMSV has written it.

The Hi-1336's register tables come from Motorola's GPL kernel. The build downloads the pinned file, checks its SHA-256 and turns the tables into a Rust module ([tools/sensor_tables.py](../../tools/sensor_tables.py)), so they are not kept in this repository. Without them Camera reports error 2.

### Developer requests over USB

`build/moto-package camera-status` prints Camera's 96 status words: the stage reached, the state and frame count, the supervisor's survey (power domain, clock muxes, pin modes and levels, gates), the LDOs, the sensor ID, I2C counts, SENINF and CAMSV registers, interrupt counts, exposure and SMI larb3. `build/moto-package camera-shot=FILE.pgm` saves a 160 × 90 grey preview of a recent frame. `build/moto-package camera=HEX` sends a request, command in the high byte:

| Request | Effect |
| --- | --- |
| `01000000`, `02000000` | Start streaming without an app (until `02`), and stop. |
| `030000LL` | Set the exposure level, 0 to 255. |
| `10000000`, `11000000` | Power the sensor and read its ID (word 20), and power it off. |
| `12RRRR00`, `13RRRRVV` | Read sensor register `RRRR` into word 25, or write byte `VV` to it. |
| `14AAAAAA`, `15VVVVVV`, `160000VV`, `17AAAAAA` | Read the 32-bit register at Camera's `0x09000000 + A` into word 25; latch a value's low 24 and high 8 bits; write the latch to `0x09000000 + A`. |
| `18000000T`, `1900000P`, `1a00000F` | SENINF test pattern instead of the sensor (`T` = 1), the CAMSV packing, the TG input format. |
| `1b00000T` | Torch on (`T` = 1) or off. |
| `1d00WWOO` | Change supervisor items `WW` to state `OO` (bits as in `camera_platform`). |
| `1eAAAAMV` | Set bit `M` of camera LDO register `AAAA` to `V`. |
| `200000PP`, `23OOOOOO` | Copy a 4 KiB register page, or 64 KiB of the frame window from byte `O` × 16, into the preview; `camera-raw=FILE` reads all 64 KiB. |
| `2100000P`, `220000SS` | The CSI port, and the MIPI data settle count. |

These requests bypass the rule that only the app on screen may use the camera. They exist for bench testing.

## Not yet done

- **The other cameras.** The front 5 MP sensor (SC500CS or Hi-556D) and the 2 MP macro sensor (OV02B1B or GC02M1B) are wired in Motorola's tables but not driven.
- **Still photos.** The app holds the picture on screen but does not save it; `/data` keeps only small files.
- **Focus, flash and colour.** The Hi-1336's lens stays at its rest position, there is no flash sequencing (only the developer torch), and colour comes from grey-world white balance and a fixed gamma, without the vendor's lens shading or colour tuning.
- **Interrupts.** Camera polls CAMSV's status every millisecond while streaming.
