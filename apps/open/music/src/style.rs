use violet::{rgb, Canvas, Rect};

pub const SURFACE: u32 = rgb(0xcd, 0xe5, 0x0b);
pub const INK: u32 = rgb(0x19, 0x19, 0x19);
pub const MUTED: u32 = rgb(0x48, 0x51, 0x15);
pub const LINE: u32 = rgb(0x7b, 0x89, 0x13);

pub fn outline(c: &mut Canvas, rect: Rect, radius: f32) {
    c.round_rect(rect, radius, LINE);
    c.round_rect(rect.inset(2), radius - 2.0, SURFACE);
}

pub fn record(c: &mut Canvas, x: f32, y: f32, radius: f32, ink: u32, surface: u32) {
    c.circle(x, y, radius, ink);
    for ratio in [0.88, 0.77, 0.66] {
        c.ring(x, y, radius * ratio, (radius * 0.017).max(1.0), surface);
    }
    c.circle(x, y, radius * 0.31, surface);
    c.circle(x, y, radius * 0.07, ink);
}
