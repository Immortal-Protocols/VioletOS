# RFC: Sub-8 ms Frame Pipeline & Zero-Jank Compositing on Pure seL4

**Status:** Phases 1–3 implemented on the Moto G Pure and in QEMU (November 24, 2026). See [Implementation](#implementation) for what was built, where it departs from the plan below, and measurements. Phase 4 and multi-core are not done.  
**Targets:** Moto G Pure (`XT2163DL`, MT6765 / 8× Cortex-A53, 720×1600 60 Hz MIPI-DSI) and QEMU on macOS.

## Motivation

Today, VioletOS interactions and renders feel slow and visually janky across both built-in and open apps. Profiling the current pipeline reveals four compounding bottlenecks—raw pixel drawing is only one of them:

1. **Triple-copying 4.6 MiB + 71 cache-clean syscalls + tearing:**
   - Open apps rasterize into a heap `Canvas` (`720 × 1600 × 4 = 4.6 MiB`), copy it in `apps/lib/violet/src/device.rs` (`Screen::show`) into buffer 0 of the `mmap`'d `/dev/violet/screen` surface (hardcoding `x = 0, width = W`), and trap into Open Runtime (`runtime/open/device.rs::present`), which copies page-by-page into scanout (`0x0800_0000`) and calls `seL4_ARM_PageDirectory_Clean_Data` up to **71 times** per frame (`runtime/open/sys.rs::clean_framebuffer`). That generates ~23 MiB of DRAM traffic per full frame and blows Cortex-A53 L2 cache.
   - The built-in Display service (`services/display/process.rs`) maps scanout uncached and scales 3×4 pixels via `write_volatile` (~27 MB/s on the handset ≈ 170 ms/frame).
   - Writes land directly in the live bootloader framebuffer (`0x79450000`) with no VSync, causing mid-scanline tearing.
2. **Scalar `f32` rasterization without NEON + full-screen redraws:**
   - `tools/build_initfs.py` enables `-mfpu=neon` in C `CFLAGS` but omits `-C target-cpu=cortex-a53 -C target-feature=+neon` in Rust `RUSTFLAGS`.
   - `apps/lib/violet/src/canvas.rs` and `text.rs` run per-pixel `sqrt()`, `atan2()`, 4×4 supersampling, `RefCell<HashMap>` glyph lookups, and `u8 → f32 → u32` alpha blends.
   - `apps/open/home/src/main.rs` calls `draw()` + `show_all()` on every `TOUCH_DOWN`/`TOUCH_UP` (redrawing 6 tiles × 8 concentric anti-aliased circles = 48 circles per tap). `apps/open/music` caps itself to 10 fps because of present cost.
3. **Polling chains, flat priority, single core, and 40 ms synchronous stalls:**
   - Only **1 of 8** Cortex-A53 cores is active (`platform/sel4/board/kernel-overlay.dts`), and every process and app thread runs at priority `100` (`supervisor.c`, `runtime/open/sys.rs`).
   - Touch is polled over SPI (`services/touch/touch.rs`), Open Runtime busy-yields (`receive(false)` + `yield_now()`) and polls Runtime every 4 ms (`runtime/open/mod.rs`), and `runtime/lanes.rs::sample` adds another poll cycle of latency.
   - Runtime calls Audio synchronously (`runtime/open/README.md`), stalling all Runtime callers—including focus and touch polls—for up to **~40 ms** on the handset.
4. **QEMU viewer & TCG ceiling:**
   - `tools/emulator/viewer.c` sleeps `SDL_Delay(32)` (~30 fps cap) and uploads the full 4.6 MiB texture every tick; `tools/run.py` runs 32-bit ARM under single-core TCG (`-accel tcg -smp 1`).

## Design Goals

- **≤ 8 ms end-to-end interaction-to-frame-ready budget** (leaving > 50% headroom on the Moto G Pure's 16.67 ms / 60 Hz panel, and ready for 120 Hz panels).
- **Zero visual jank & zero tearing:** hardware VSync-latched triple-buffered page flips, deterministic real-time scheduling, and no head-of-line stalls from Audio, I/O, or Go/Python GC pauses.
- **Pragmatic on MT6765 today, GPU-ready tomorrow:** leverage the handset's openly documented MediaTek `DISP_OVL0` (4 hardware layers + VSync) and `MDP` 2D engine rather than reverse-engineering PowerVR GE8320, while designing a retained `DisplayList` / compositor boundary that cleanly accepts `virtio-gpu` (QEMU) or Freedreno/Panfrost on future open-GPU phones.
- **Backward-compatible `/dev/violet/*`:** existing open apps keep working; apps opt into zero-copy triple buffering, dirty damage, VSync callbacks, or retained display lists incrementally.

## Architecture

```text
  Open Apps (Rust/Go/C/Python)                  Built-in Apps (no_std)
   • Direct draw into mmap'd swapchain slot      • Submit DisplayList or buffer
   • OR submit compact DisplayList + springs     • Zero uncached volatile writes
   • Wake on EVENT_VSYNC & coalesced touch
                 │  zero-copy shm + svc #0 commit
                 ▼
  ┌─────────────────────────────────────────────────────────────────────┐
  │  Open Runtime (Priority 160 · Bound seL4 Notification · Zero Poll)  │
  │   • Single blocking seL4_Recv: wakes on app svc #0 OR hw notif bit  │
  │   • Drains lock-free TouchRing + VSync seq (< 50 us wakeup)         │
  │   • Async Audio lane (eliminates 40 ms Runtime stall)               │
  └───────────────────┬───────────────────────────────▲─────────────────┘
                      │ commit(slot, damage, ops)     │ EINT IRQ (< 0.5 ms)
                      ▼                               │
  ┌──────────────────────────────────────┐   ┌──────────────────────────┐
  │  Display Compositor (Prio 200·Core0) │   │ Touch Service (Prio 200) │
  │   • Triple buffer in 0x79450000 pool │   │ • MT6765 EINT GPIO IRQ   │
  │     (15.69 MiB -> 3 x 4.4 MiB slots) │   │ • Burst SPI read 0x21c00 │
  │   • 4 HW layers via MT6765 DISP_OVL0 │   │ • Lock-free SPSC ring    │
  │   • Retained DisplayList + springs   │   └──────────────────────────┘
  │   • VSync IRQ -> latch + signal apps │
  └───────────────────┬──────────────────┘
                      ▼
       MT6765 DISP_OVL0 -> DISP_RDMA0 -> DSI0 (60 Hz tear-free)
       [QEMU backend: shared dirty-seq + SDL VSync / future virtio-gpu]

  SMP Core Pinning (4-8x Cortex-A53, seL4 MCS optional):
    Core 0: Display/VSync (200), Touch IRQ (200), Audio DMA (160)
    Core 1: Foreground App UI/Render (120), Open Runtime (160)
    Core 2-3: Background apps, MP3/JPEG decode, TLS/Radio, Shield (80)
```

## Phased Milestones

1. **Phase 1 — Zero-copy triple buffering & hardware VSync flip:**
   - Carve 3 × 4.4 MiB buffers out of the bootloader's existing `0xfb0000` (15.69 MiB) reservation at `0x79450000` (`boot/loader/loader.c`).
   - Bring up `targets/moto-g-pure/disp_ovl.rs` in Display to flip `OVL_L0_ADDR` via `DISP_MUTEX0` on VSync IRQ, eliminating the 4.6 MiB memcpy, 71 D-cache clean syscalls (or restricting clean to dirty bounding rects / write-combining), and screen tearing.
   - Map built-in Display cached (or route built-in apps through an `OVL` hardware layer).
   - Quick Mac emulator fix: pass `SDL_RENDERER_PRESENTVSYNC` in `tools/emulator/viewer.c`, remove `SDL_Delay(32)`, and upload only dirty rows signaled by a shared sequence word.
2. **Phase 2 — Sub-4 ms Cortex-A53 NEON rendering & dirty damage:**
   - Add `-C target-cpu=cortex-a53 -C target-feature=+neon` to `RUSTFLAGS` in `tools/build_initfs.py`.
   - Switch `apps/lib/violet` to `MappedCanvas` (zero heap copy), integer NEON span blitters, scanline rounded rects, flat ASCII `GlyphAtlas`, and per-slot `buffer_age` damage tracking.
   - Cache static Home/Music backgrounds & tile icons once; fix `Screen::show` to submit real `(x, y, w, h)` rects.
3. **Phase 3 — Event-driven input, priorities, async Audio, and SMP:**
   - Replace Touch SPI polling with `EINT` GPIO IRQ → `TouchRing` → seL4 bound notification on Open Runtime's fault TCB (removes 4 ms poll, `lanes::sample` lag, and `yield_now()` spin).
   - Move Audio off Runtime's synchronous IPC path to eliminate the 40 ms stall.
   - Apply strict seL4 priority tiers (`200 / 160 / 120 / 80`) and bring up 4–8 Cortex-A53 cores via PSCI + GICv3 redistributors (optionally enabling seL4 MCS budgets).
4. **Phase 4 — Retained DisplayList compositor & GPU portability:**
   - Allow apps to submit compact `DisplayOp` command buffers + declarative scroll/spring animations so the compositor drives smooth 60/120 Hz motion even during Go/Python GC or network stalls.
   - Use MT6765 `DISP_OVL0` (4 layers) + `MDP` (2D DMA blit/scale) on Moto G Pure; target `virtio-gpu` (QEMU) and Freedreno/Panfrost on future open-GPU hardware.
   - Long term: evaluate an **AArch64** port to unlock macOS HVF virtualization (`-accel hvf`, 10–50× faster QEMU) and double the NEON register file (32 × 128-bit vs. 16).

## Target Frame-Time Budget (Moto G Pure, 720×1600 @ 60 Hz)

| Stage | Today | Target (Phases 1–3) |
| --- | --- | --- |
| Touch IRQ → app wakeup | 8–48 ms (4 ms poll + lane lag + 40 ms Audio stall) | **< 0.5 ms** |
| App rasterization | 15–100+ ms (scalar `f32`, full-screen redraw) | **1.0–3.5 ms** |
| Present / copy / cache clean | 15–170 ms (3× 4.6 MiB copies + 71 syscalls) | **< 0.5 ms** |
| Scanout sync | Single-buffered tearing | **0 ms tear-free (VSync flip)** |
| **Total interaction → frame ready** | **~40–300+ ms** | **~2–5 ms (≤ 8 ms budget)** |

## Open Questions

1. **Write-combining vs. dirty D-cache clean on AArch32 seL4:** Does mapping swapchain buffers as Normal Non-Cacheable / Write-Combining (via `TEX`/`C`/`B` attributes in seL4) beat cached RAM + single dirty-band `Clean_Data` for alpha-blending workloads that read back destination pixels? (Likely cached + dirty-band clean wins whenever read-modify-write blending dominates, while WC wins for pure opaque fills/blits.)
2. **seL4 MCS vs. static priorities:** Should Phase 3 enable upstream seL4 MCS scheduling contexts immediately alongside SMP, or start with static priorities (`200 / 160 / 120 / 80`) + core pinning first?
3. **DisplayList ownership:** Should `DisplayList` rasterization live inside `services/display` (full isolation, serves both built-in and open apps) or start inside Open Runtime before migrating?

## Implementation

### Measured on the handset

`frames` in `/dev/violet/sys` measures the pipeline and `bench tap X Y N` injects presses ([runtime/open/frames.rs](../runtime/open/frames.rs)); over USB, `build/moto-package "open-run=@sys bench tap 140 860 20"`, then `"open-run=@sys frames"`, and read `open-log`. Twenty presses on Home's Music tile, each followed by focus handed back to Home (which redraws everything):

| | Before | After |
| --- | --- | --- |
| Press → Home's frame presented | 289 ms average, 519 ms worst | **2.0 ms** average, 2.4 ms worst |
| Full-screen present (copy and cache clean) | 13.7 ms | **3.4 ms**, without tearing |
| Focus → full Home redraw presented (with its status queries) | about 290 ms (a press redrew everything) | **23 ms** average, 27 ms worst |
| Gap between touch polls, worst in a window | 50–140 ms | **8 ms** |
| Open Runtime while idle | spinning (48,000–131,000 passes a second) | **asleep 98–99%** |

The press is measured from the touch event reaching the app to the end of the present that answers it; add up to 4 ms of touch sampling before it and the panel's scan after it. Home's full redraw is now mostly its status queries; drawing the whole screen takes 0.5 ms on the Mac (1.7 ms before), a pressed tile 0.05 ms (1.6 ms).

### What was built

- **Phase 2, rendering** ([apps/lib/violet](../apps/lib/violet)). The canvas clips every primitive to a damage rectangle and anti-aliases per row: circles, rings, arcs and rounded corners fill their solid spans and compute the exact edge coverage only for boundary pixels (one square root per row instead of per pixel, `atan2` only on an arc's edge band); text blends 8-bit coverage rows with integer weights. Host tests check the results are bit-identical to the old per-pixel code. `Screen::canvas()` draws straight into the mapped surface, so `show` copies nothing, and `show` presents the real rectangle (it always sent full-width rows). Home redraws only the pressed tile. Rust apps build with `-C target-cpu=cortex-a53 -C target-feature=+neon` ([tools/build_initfs.py](../tools/build_initfs.py)); Ladybird keeps its own flags, since its build script would inherit them.
- **Phase 1, present** ([runtime/open/device.rs](../runtime/open/device.rs), [scanout.rs](../runtime/open/scanout.rs)). The copy to scanout is a 32-byte `ldm`/`stm` loop with prefetch (the size-optimised `memcpy` moved about 350 MB/s); it cleans each 16-row band as it goes. Presents are tear-free by racing the beam: DISP_RDMA0 reports the line the panel is reading (offset `0xfc`, 0–1600, wrapping every 16.77 ms), and a copy starts only when the beam is above the damaged rows (the copy, about five times faster than the panel, stays ahead) or near their end; otherwise the presenting thread waits, and the Open Runtime wakes when the beam gets there. `SCREEN_IOC_WAIT_VSYNC` (`0x5604`) blocks until the next frame and returns a `VsyncInfo`. The QEMU viewer redraws at the Mac's refresh rate and uploads only rows the guest marks dirty, in words just after the frame, plus a full upload every 250 ms for writers that do not mark.
- **Phase 3, events and priorities.** The kernel exports the physical timer to user mode (`KernelArmExportPTMRUser`). The supervisor gives the Open Runtime an IRQ handler for PPI 30 and binds its notification to the Open Runtime's thread; the Open Runtime arms the timer for its next deadline (touch poll, a thread's timeout, the beam reaching a waiting present, the next frame for a VSync waiter, 2 ms while a thread waits on something only polling sees) and sleeps in one blocking receive at priority 150, above the services that poll at 100. Starting an app and USB installs drop back to 100. If the timer does not fire within 50 ms at start, it polls as before. Touch: each idle sample used to reach the app in front as another `TOUCH_UP` (about 250 a second); now only presses, moves and releases do, and a poll lets Runtime's touch worker run before asking again, so the sample is current rather than a poll old. Audio publishes its answers to `GET VOLUME`, `PLAYBACK` and `MICROPHONE` in a page Runtime reads (a sequence lock), so status requests no longer wait for Audio's hardware loop.

### Where it departs from this RFC

- **No hardware page flip.** The skeleton's MT6765 addresses were wrong (`0x14008000` is `mdp_wdma0`); the phone's device tree puts DISP_MUTEX at `0x14001000`, DISP_OVL0 at `0x1400b000` (layer 0 address at `0xf40` = `0x79450000`, layer 1 enabled at `0x798ce000` but transparent), DISP_RDMA0 at `0x1400d000` and DSI0 at `0x14014000`. The Open Runtime maps these read-only and only reads the scan line. A flip would still need a copy, because apps draw into private surfaces that keep working in the background; racing the beam gives the same tear-free result and latency with no writes to display registers.
- **No touch interrupt.** Touch stays sampled over SPI, now every 4 ms from a timer instead of a spinning loop. Programming the MT6765 EINT controller is left for later; the remaining delivery delay is the 4 ms sampling interval.
- **One core, static priorities.** Only the Open Runtime moved (to 150). App threads and services stay at 100; raising a CPU-bound app above the polling services would starve Audio without MCS budgets. Secondary cores (PSCI, per-core GIC redistributors, an SMP kernel) are not attempted.
- **Phase 4** (retained display lists, compositor-driven animation) is not started.

### Not done yet

- SMP and seL4 MCS (open questions 2 and 3 stand).
- The classic launcher's Display service still scales through uncached scanout on the handset.
- `make test-open` fails at `pytone` on `main` as well: the Programs list shows 13 rows, and with Ladybird and Wi-Fi added the last test programs no longer fit.
