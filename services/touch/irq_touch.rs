//! RFC skeleton: Interrupt-driven Novatek touch driver (`services/touch`).
//!
//! Replaces the current SPI polling chain (`services/touch/events.c` +
//! `runtime/lanes.rs::sample` + 4 ms polling in `runtime/open/mod.rs`) with:
//! 1. Blocking wait on MT6765 `EINT` GPIO IRQ (`seL4_Wait` at priority 200).
//! 2. Single 6-byte SPI read of `0x21c00` stamped with `CNTVCT`.
//! 3. Lock-free push into `TouchRing` + `seL4_Signal` to Open Runtime's bound
//!    notification -> < 0.5 ms touch-to-app wakeup, zero CPU when idle.

#![allow(dead_code)]

use crate::realtime::{TouchRing, TouchSample};

pub const EVENT_DOWN: u8 = 1;
pub const EVENT_MOVE: u8 = 2;
pub const EVENT_UP: u8 = 3;

pub trait TouchHardware {
    fn wait_eint_irq(&mut self);
    fn ack_eint_irq(&mut self);
    fn read_contact_spi(&mut self) -> Option<(i16, i16, bool)>;
    fn now_ns(&self) -> u64;
    fn signal_open_runtime(&self);
}

pub struct IrqTouchDriver<'a, H: TouchHardware> {
    hw: H,
    ring: &'a TouchRing,
    was_down: bool,
    last_xy: (i16, i16),
}

impl<'a, H: TouchHardware> IrqTouchDriver<'a, H> {
    pub const fn new(hw: H, ring: &'a TouchRing) -> Self {
        Self { hw, ring, was_down: false, last_xy: (0, 0) }
    }

    pub fn run(&mut self) -> ! {
        loop {
            self.hw.wait_eint_irq();
            let ts = self.hw.now_ns();
            if let Some((x, y, down)) = self.hw.read_contact_spi() {
                let kind = match (down, self.was_down) {
                    (true, false) => Some(EVENT_DOWN),
                    // Suppress duplicate stationary reports to avoid waking apps unnecessarily.
                    (true, true) if (x, y) != self.last_xy => Some(EVENT_MOVE),
                    (false, true) => Some(EVENT_UP),
                    _ => None,
                };
                if let Some(kind) = kind {
                    self.was_down = down;
                    self.last_xy = (x, y);
                    let _ = self.ring.push(TouchSample { timestamp_ns: ts, x, y, kind, _pad: [0; 3] });
                    self.hw.signal_open_runtime();
                }
            }
            self.hw.ack_eint_irq();
        }
    }
}
