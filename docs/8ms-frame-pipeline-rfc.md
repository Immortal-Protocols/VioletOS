# RFC: Sub-8 ms Frame Pipeline & Zero-Jank Compositing on Pure seL4

**Status:** Draft RFC (code skeleton — intentionally not wired into the build yet)  
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

## Skeleton Map

| File | Role |
| --- | --- |
| `services/display/compositor.rs` | Triple-buffered zero-copy `Swapchain` (`buffer_age`), 4-layer `OverlayPlane`, retained `DisplayList` / spring compositor, `DisplayBackend` trait |
| `targets/moto-g-pure/disp_ovl.rs` | MT6765 `DISP_OVL0` / `DISP_MUTEX0` / `DISP_RDMA0` register page-flip, 4 hardware overlay planes, VSync IRQ hook, and `MDP` 2D blitter stub |
| `runtime/open/realtime.rs` | `/dev/violet/screen` acquire/commit/VSync ABI, lock-free `TouchRing`, bound-notification event-driven Open Runtime loop, seL4 priority & SMP pinning |
| `apps/lib/violet/src/fast_render.rs` | Zero-copy `MappedCanvas`, `DamageTracker`, NEON premultiplied-alpha span blitters, scanline rounded rects, flat `GlyphAtlas`, VSync frame loop |
| `services/touch/irq_touch.rs` | Interrupt-driven Novatek `EINT` touch driver feeding `TouchRing` and signaling Open Runtime's bound notification |

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
