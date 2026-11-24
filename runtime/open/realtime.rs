//! RFC skeleton: Event-driven Open Runtime wakeup, lock-free TouchRing,
//! real-time seL4 priorities/SMP pinning, and `/dev/violet/screen` v2 ABI.
//!
//! Eliminates:
//! 1. 4 ms polling loop + `yield_now()` spin in `runtime/open/mod.rs::run`.
//! 2. Extra poll-cycle latency from `runtime/lanes.rs::sample`.
//! 3. 40 ms synchronous Audio stall in Runtime.
//! 4. Flat priority 100 contention across single-core seL4.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU32, Ordering};

// ---------- seL4 Real-Time Priority Tiers & SMP Core Affinity ----------

pub const PRIO_DISPLAY_TOUCH_IRQ: u8 = 200; // Display VSync compositor + Touch EINT IRQ
pub const PRIO_OPEN_RT_AUDIO: u8 = 160;     // Open Runtime fault/IPC server + Audio DMA
pub const PRIO_FOREGROUND_APP: u8 = 120;    // Active interactive app UI/render thread
pub const PRIO_BACKGROUND_WORK: u8 = 80;    // Background apps, MP3/JPEG decode, TLS, Shield

pub const CORE_SERVICES: u32 = 0; // Display, Touch IRQ, Audio
pub const CORE_UI: u32 = 1;       // Foreground app UI thread + Open Runtime
pub const CORE_BG_FIRST: u32 = 2; // Cores 2..3 (or 2..7): background & tile workers

/// Optional seL4 MCS scheduling context budget (Phase 3b) so background apps
/// cannot starve the 8 ms foreground frame budget.
#[derive(Clone, Copy, Debug)]
pub struct McsBudget {
    pub budget_us: u32,
    pub period_us: u32,
}

pub const MCS_UI_BUDGET: McsBudget = McsBudget { budget_us: 6_000, period_us: 8_000 };
pub const MCS_BG_BUDGET: McsBudget = McsBudget { budget_us: 2_000, period_us: 16_000 };

// ---------- /dev/violet/screen v2 & /dev/violet/events VSync ABI ----------

pub const SCREEN_IOC_ACQUIRE: u32 = 0x5602;    // out: AcquireReply (zero-copy back buffer)
pub const SCREEN_IOC_COMMIT: u32 = 0x5603;     // in:  CommitFrame (damage rects + mailbox)
pub const SCREEN_IOC_WAIT_VSYNC: u32 = 0x5604; // out: VsyncInfo (choreography wakeup)
pub const EVENT_VSYNC: u16 = 8;                // InputEvent.kind = 8; x = seq_lo, value = period_us

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AcquireReply {
    pub buffer_index: u32, // 0, 1, or 2 inside mmap'd triple-buffer region
    pub buffer_age: u32,   // 0 = full redraw; 1..2 = incremental damage against history
    pub stride_bytes: u32,
    pub offset_bytes: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitFrame {
    pub buffer_index: u32,
    pub damage_count: u32,        // 0 = full screen; 1..=4 = partial dirty rects
    pub damage: [CommitRect; 4],
    pub flags: u32,               // bit 0 = request EVENT_VSYNC callback after flip
}

// ---------- Lock-Free SPSC Touch Ring (Touch IRQ -> Open Runtime) ----------

pub const TOUCH_RING_CAP: usize = 64; // power of two

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TouchSample {
    pub timestamp_ns: u64,
    pub x: i16,
    pub y: i16,
    pub kind: u8, // 1=DOWN, 2=MOVE, 3=UP
    pub _pad: [u8; 3],
}

/// Mapped read-write in Touch service, read-only (header write_idx) in Open Runtime.
#[repr(C)]
pub struct TouchRing {
    pub write_idx: AtomicU32,
    pub read_idx: AtomicU32,
    pub entries: [TouchSample; TOUCH_RING_CAP],
}

impl TouchRing {
    pub fn push(&self, sample: TouchSample) -> bool {
        let w = self.write_idx.load(Ordering::Relaxed);
        let r = self.read_idx.load(Ordering::Acquire);
        if w.wrapping_sub(r) as usize >= TOUCH_RING_CAP {
            return false;
        }
        let slot = (w as usize) & (TOUCH_RING_CAP - 1);
        unsafe { (&self.entries[slot] as *const TouchSample as *mut TouchSample).write(sample) };
        self.write_idx.store(w.wrapping_add(1), Ordering::Release);
        true
    }

    pub fn pop(&self) -> Option<TouchSample> {
        let r = self.read_idx.load(Ordering::Relaxed);
        let w = self.write_idx.load(Ordering::Acquire);
        if r == w {
            return None;
        }
        let slot = (r as usize) & (TOUCH_RING_CAP - 1);
        let s = unsafe { (&self.entries[slot] as *const TouchSample).read() };
        self.read_idx.store(r.wrapping_add(1), Ordering::Release);
        Some(s)
    }
}

// ---------- Event-Driven Open Runtime Loop (Bound seL4 Notification) ----------

/// Badge bits OR'd into Open Runtime's bound seL4 Notification (`seL4_TCB_BindNotification`).
/// Because seL4 checks the bound notification inside blocking `seL4_Recv(FAULT_EP)`,
/// Open Runtime sleeps at 0% CPU and wakes in < 50 us on *either* an app `svc #0`
/// OR a Touch/VSync/Audio/Net hardware event.
pub const NOTIFY_TOUCH: u32 = 1 << 0;
pub const NOTIFY_VSYNC: u32 = 1 << 1;
pub const NOTIFY_AUDIO: u32 = 1 << 2; // async audio completion (no 40 ms Runtime stall)
pub const NOTIFY_NET: u32 = 1 << 3;
pub const NOTIFY_TIMER: u32 = 1 << 4;

pub enum Wakeup {
    /// An app thread trapped on `svc #0` or `VMFault` (upper bits identify app & thread).
    AppFault { app: usize, thread: usize, label: u32 },
    /// One or more hardware/service notification bits arrived on the bound notification.
    Notifications(u32),
}

pub trait EventDrivenRuntime {
    fn blocking_recv_or_notify(&mut self) -> Wakeup;
    fn handle_app_fault(&mut self, app: usize, thread: usize, label: u32);
    fn drain_touch_ring(&mut self);
    fn on_vsync_broadcast(&mut self);
    fn wake_ready_waiters(&mut self);

    fn run_event_driven(&mut self) -> ! {
        loop {
            match self.blocking_recv_or_notify() {
                Wakeup::AppFault { app, thread, label } => {
                    self.handle_app_fault(app, thread, label);
                }
                Wakeup::Notifications(bits) => {
                    if bits & NOTIFY_TOUCH != 0 {
                        self.drain_touch_ring();
                    }
                    if bits & NOTIFY_VSYNC != 0 {
                        self.on_vsync_broadcast();
                    }
                    self.wake_ready_waiters();
                }
            }
        }
    }
}
