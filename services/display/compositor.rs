//! RFC skeleton: VSync-paced, zero-copy triple-buffered system compositor.
//!
//! Replaces the 4.6 MiB memcpy in `runtime/open/device.rs::present` and the
//! uncached volatile scaler in `services/display/process.rs` with:
//! - 3 full 720x1600 BGRA8888 swapchain buffers carved from the bootloader's
//!   existing 15.69 MiB `mblock-16-framebuffer` pool at `0x7945_0000`.
//! - Up to 4 hardware overlay planes (`DISP_OVL0` on MT6765).
//! - Optional retained `DisplayList` + spring animations so scrolling and
//!   transitions stay locked to VSync even when an app thread stalls.

#![allow(dead_code)]

pub const SCREEN_W: u32 = 720;
pub const SCREEN_H: u32 = 1600;
pub const STRIDE_BYTES: u32 = SCREEN_W * 4;
/// 64 KiB page-aligned size of one 720x1600x4 frame (4,653,056 bytes; 3 fit in 0xfb0000).
pub const SLOT_BYTES: usize = ((STRIDE_BYTES * SCREEN_H) as usize + 0xffff) & !0xffff;
pub const FB_POOL_PHYS: usize = 0x7945_0000;
pub const FB_POOL_SIZE: usize = 0x00fb_0000;
pub const SWAPCHAIN_SLOTS: usize = 3;
pub const MAX_HW_LAYERS: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Free,
    AppDrawing,
    Queued { seq: u64 },
    ScanningOut,
}

#[derive(Clone, Copy, Debug)]
pub struct SwapchainSlot {
    pub index: u8,
    pub phys_addr: usize,
    pub state: SlotState,
    /// Monotonic frame sequence last presented from this slot (for `buffer_age`).
    pub last_seq: u64,
}

/// Triple-buffered zero-copy swapchain with EGL/Wayland-style `buffer_age`.
pub struct Swapchain {
    slots: [SwapchainSlot; SWAPCHAIN_SLOTS],
    pub frame_seq: u64,
}

impl Swapchain {
    pub const fn new_in_fb_pool() -> Self {
        let slot = |i: usize| SwapchainSlot {
            index: i as u8,
            phys_addr: FB_POOL_PHYS + i * SLOT_BYTES,
            state: if i == 0 { SlotState::ScanningOut } else { SlotState::Free },
            last_seq: 0,
        };
        Self { slots: [slot(0), slot(1), slot(2)], frame_seq: 0 }
    }

    /// Hand the app a free back buffer and its `buffer_age` (0 = full redraw required,
    /// 1 = matches previous frame, 2 = two frames old, etc.).
    pub fn acquire(&mut self) -> Option<(u8, u32)> {
        let s = self.slots.iter_mut().find(|s| s.state == SlotState::Free)?;
        s.state = SlotState::AppDrawing;
        let age = if s.last_seq == 0 { 0 } else { self.frame_seq.saturating_sub(s.last_seq) as u32 };
        Some((s.index, age))
    }

    /// Queue a finished buffer for the next VSync; supersedes any older unshown queued slot
    /// (mailbox semantics: lowest latency, never blocks the producer).
    pub fn commit(&mut self, index: u8) -> Result<u64, i32> {
        let idx = index as usize;
        if idx >= SWAPCHAIN_SLOTS || self.slots[idx].state != SlotState::AppDrawing {
            return Err(-22); // EINVAL
        }
        for s in &mut self.slots {
            if matches!(s.state, SlotState::Queued { .. }) {
                s.state = SlotState::Free;
            }
        }
        self.frame_seq += 1;
        self.slots[idx].state = SlotState::Queued { seq: self.frame_seq };
        self.slots[idx].last_seq = self.frame_seq;
        Ok(self.frame_seq)
    }

    /// Called on hardware VSync IRQ: promotes the queued slot to `ScanningOut`
    /// and returns its physical address for hardware latch.
    pub fn latch_on_vsync(&mut self) -> Option<usize> {
        let next = self.slots.iter().position(|s| matches!(s.state, SlotState::Queued { .. }))?;
        for s in &mut self.slots {
            if s.state == SlotState::ScanningOut {
                s.state = SlotState::Free;
            }
        }
        self.slots[next].state = SlotState::ScanningOut;
        Some(self.slots[next].phys_addr)
    }
}

/// One hardware overlay plane (`DISP_OVL0` supports 4 concurrent layers at 0 CPU cost).
#[derive(Clone, Copy, Debug)]
pub struct OverlayPlane {
    pub enabled: bool,
    pub phys_addr: usize,
    pub src_w: u16,
    pub src_h: u16,
    pub stride: u16,
    pub dst_x: i16,
    pub dst_y: i16,
    pub alpha: u8,
    pub premultiplied: bool,
}

impl OverlayPlane {
    pub const fn disabled() -> Self {
        Self {
            enabled: false, phys_addr: 0, src_w: 0, src_h: 0, stride: 0,
            dst_x: 0, dst_y: 0, alpha: 255, premultiplied: true,
        }
    }
}

/// Compact retained display-list ops for Phase 4 compositor-driven rendering & animations.
/// Apps submit small command buffers over shm; the compositor animates springs/scrolls
/// at native VSync independently of app GC or I/O stalls.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum DisplayOp {
    FillRect { x: i16, y: i16, w: u16, h: u16, argb: u32 },
    RoundRect { x: i16, y: i16, w: u16, h: u16, radius_q4: u16, argb: u32 },
    BlitLayer { layer_id: u16, dst_x: i16, dst_y: i16, alpha: u8 },
    GlyphRun { x: i16, y: i16, font_id: u8, size_px: u8, argb: u32, text_off: u16, text_len: u16 },
    SpringScroll { layer_id: u16, offset_px: f32, velocity_px_s: f32, target_px: f32 },
}

/// Hardware or virtual scanout backend (MT6765 `DISP_OVL0`, QEMU dirty-seq/virtio-gpu, future GPU).
pub trait DisplayBackend {
    fn wait_vsync(&mut self) -> u64;
    fn latch_planes(&mut self, planes: &[OverlayPlane; MAX_HW_LAYERS]);
    fn clean_dirty_range(&self, phys_addr: usize, damage: &[DamageRect]);
}

/// Top-level system compositor owned by `services/display` (seL4 priority 200, Core 0).
pub struct Compositor<B: DisplayBackend> {
    pub backend: B,
    pub swapchain: Swapchain,
    /// Layer 0: foreground app; Layer 1: status bar; Layer 2: Shield banner; Layer 3: HUD/cursor.
    pub planes: [OverlayPlane; MAX_HW_LAYERS],
}

impl<B: DisplayBackend> Compositor<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            swapchain: Swapchain::new_in_fb_pool(),
            planes: [OverlayPlane::disabled(); MAX_HW_LAYERS],
        }
    }

    /// Runs once per hardware VSync (~16.67 ms on Moto G Pure): latches any committed
    /// app buffer + system overlay planes and returns `(vsync_ns, frame_seq)` to broadcast.
    pub fn step_vsync(&mut self) -> (u64, u64) {
        let vsync_ns = self.backend.wait_vsync();
        if let Some(phys) = self.swapchain.latch_on_vsync() {
            self.planes[0] = OverlayPlane {
                enabled: true,
                phys_addr: phys,
                src_w: SCREEN_W as u16,
                src_h: SCREEN_H as u16,
                stride: STRIDE_BYTES as u16,
                dst_x: 0,
                dst_y: 0,
                alpha: 255,
                premultiplied: true,
            };
        }
        self.backend.latch_planes(&self.planes);
        (vsync_ns, self.swapchain.frame_seq)
    }
}
