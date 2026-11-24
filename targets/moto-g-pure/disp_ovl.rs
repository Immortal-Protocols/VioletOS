//! RFC skeleton: MT6765 (Moto G Pure) `DISP_OVL0` + `DISP_MUTEX0` + VSync driver.
//!
//! The stock bootloader leaves MediaTek's MMSYS pipeline (`DISP_OVL0 -> DISP_RDMA0 -> DSI0`)
//! running in 60 Hz video mode, scanning out `0x7945_0000`. Instead of copying 4.6 MiB
//! into a single live buffer (tearing + ~20 ms DRAM traffic), Display maps these MMIO
//! pages and performs tear-free 0 ms hardware page flips and 4-layer alpha composition.
//!
//! Register layout follows Motorola's GPL `kernel-mtk` MT6765 display driver
//! (`drivers/misc/mediatek/video/mt6765/dispsys/ddp_reg_ovl.h` & `ddp_reg_mutex.h`).

#![allow(dead_code)]

use crate::compositor::{DamageRect, DisplayBackend, OverlayPlane, MAX_HW_LAYERS};

/// Physical MMIO bases on MT6765 (mapped exclusively into `services/display` by supervisor).
pub const DISP_OVL0_PHYS: usize = 0x1400_8000;
pub const DISP_RDMA0_PHYS: usize = 0x1400_a000;
pub const DISP_MUTEX_PHYS: usize = 0x1400_1000;
/// Optional Phase 4 hardware 2D DMA scaler/blitter/rotator (no PowerVR driver needed).
pub const MDP_RDMA0_PHYS: usize = 0x1400_3000;
pub const MDP_WROT0_PHYS: usize = 0x1400_5000;

// DISP_OVL0 register offsets (verify exact offsets against XT2163DL kernel-mtk headers).
const OVL_INTEN: usize = 0x004;
const OVL_INTSTA: usize = 0x008;
const OVL_SRC_CON: usize = 0x02c;      // bit i enables layer i (0..3)
const OVL_L0_CON: usize = 0x030;       // layer stride = 0x20 per layer
const OVL_L0_SRC_SIZE: usize = 0x038;  // (h << 16) | w
const OVL_L0_OFFSET: usize = 0x03c;    // (y << 16) | x
const OVL_L0_PITCH: usize = 0x044;     // stride in bytes
const OVL_L0_ADDR: usize = 0xf40;      // layer i physical address: 0xf40 + i * 0x20

const OVL_INT_FRAME_COMPLETE: u32 = 1 << 1;
const OVL_CON_FMT_BGRA8888: u32 = (0x3 << 12) | (1 << 8); // BGRA8888 + per-pixel alpha enable

// DISP_MUTEX0 shadow-register latch (atomic commit on next SOF/VSync).
const MUTEX0_EN: usize = 0x020;
const MUTEX0_GET: usize = 0x024;
const MUTEX0_RST: usize = 0x028;

pub struct Mt6765DispOvl {
    ovl_va: usize,
    mutex_va: usize,
    /// seL4 notification cap bound to the `DISP_OVL0` / `DISP_RDMA0` VSync GIC IRQ.
    vsync_irq_ntfn: u32,
}

impl Mt6765DispOvl {
    pub const fn new(ovl_va: usize, mutex_va: usize, vsync_irq_ntfn: u32) -> Self {
        Self { ovl_va, mutex_va, vsync_irq_ntfn }
    }

    #[inline]
    unsafe fn rd(&self, base: usize, off: usize) -> u32 {
        ((base + off) as *const u32).read_volatile()
    }

    #[inline]
    unsafe fn wr(&self, base: usize, off: usize, val: u32) {
        ((base + off) as *mut u32).write_volatile(val);
    }

    pub fn enable_vsync_irq(&self) {
        unsafe {
            self.wr(self.ovl_va, OVL_INTSTA, 0);
            self.wr(self.ovl_va, OVL_INTEN, OVL_INT_FRAME_COMPLETE);
        }
    }

    /// Program all 4 hardware overlay planes inside a `DISP_MUTEX0` shadow-latch window
    /// so the display engine switches buffers atomically at vertical blanking.
    fn program_layers(&self, planes: &[OverlayPlane; MAX_HW_LAYERS]) {
        unsafe {
            // Acquire shadow-register mutex (blocks engine from latching half-written state).
            self.wr(self.mutex_va, MUTEX0_GET, 1);
            let mut active_mask = 0u32;
            for (i, p) in planes.iter().enumerate() {
                if !p.enabled {
                    continue;
                }
                active_mask |= 1u32 << i;
                let loff = i * 0x20;
                let con = OVL_CON_FMT_BGRA8888 | u32::from(p.alpha);
                let size = (u32::from(p.src_h) << 16) | u32::from(p.src_w);
                let pos = ((p.dst_y.max(0) as u32) << 16) | (p.dst_x.max(0) as u32);
                self.wr(self.ovl_va, OVL_L0_CON + loff, con);
                self.wr(self.ovl_va, OVL_L0_SRC_SIZE + loff, size);
                self.wr(self.ovl_va, OVL_L0_OFFSET + loff, pos);
                self.wr(self.ovl_va, OVL_L0_PITCH + loff, u32::from(p.stride));
                self.wr(self.ovl_va, OVL_L0_ADDR + loff, p.phys_addr as u32);
            }
            self.wr(self.ovl_va, OVL_SRC_CON, active_mask);
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            // Release mutex: hardware latches all 4 planes cleanly on the next VSync edge.
            self.wr(self.mutex_va, MUTEX0_GET, 0);
            let _ = (MUTEX0_EN, MUTEX0_RST);
        }
    }
}

impl DisplayBackend for Mt6765DispOvl {
    fn wait_vsync(&mut self) -> u64 {
        // Phase 1 integration: `seL4_Wait(self.vsync_irq_ntfn, &badge)` + `seL4_IRQHandler_Ack`.
        // Acknowledge OVL interrupt status and return ARM generic timer timestamp (`CNTVCT`).
        unsafe {
            let status = self.rd(self.ovl_va, OVL_INTSTA);
            self.wr(self.ovl_va, OVL_INTSTA, status & !OVL_INT_FRAME_COMPLETE);
        }
        let _ = self.vsync_irq_ntfn;
        0 // replaced by `now_ns()` from CNTVCT
    }

    fn latch_planes(&mut self, planes: &[OverlayPlane; MAX_HW_LAYERS]) {
        self.program_layers(planes);
    }

    fn clean_dirty_range(&self, _phys_addr: usize, _damage: &[DamageRect]) {
        // If swapchain pages are mapped cached, issue a single dirty-band D-cache clean
        // (`seL4_ARM_PageDirectory_Clean_Data` over bounding [min_y..max_y]) instead of
        // 71 full-screen page cleans—or zero cleans if mapped Write-Combining.
    }
}
