//! A small library for VioletOS open apps written in Rust: the /dev/violet
//! devices as ordinary Linux files (runtime/open/abi.rs), drawing on a
//! 720x1600 canvas, text in the Go fonts, and HTTPS with rustls.
pub mod camera;
pub mod canvas;
pub mod compositor;
pub mod device;
pub mod http;
pub mod keyboard;
pub mod paper;
pub mod text;
pub mod time;
pub mod wifi;

pub use canvas::{mix, rgb, Canvas, Color, Rect};
pub use compositor::Compositor;
pub use device::{events, fields, home, log, sys, Event, Events, Screen, H, W};
pub use text::{Face, Fonts};

/// The shared look of the open apps: dark paper and cream ink (see paper.rs).
pub mod colors {
    use super::{paper, Color};
    pub const BACKGROUND: Color = paper::PAPER;
    pub const PANEL: Color = paper::PAPER_RAISED;
    pub const INK: Color = paper::CREAM;
    pub const MUTED: Color = paper::MUTED;
    pub const ACCENT: Color = paper::VIOLET;
}

/// The cities Settings offers for the clock; Home shows the date there. The
/// chosen index is the shared setting time.city.
pub const CITIES: [&str; 20] = [
    "San Francisco", "Oakland", "Los Angeles", "Seattle", "New York",
    "Chicago", "Austin", "Miami", "Denver", "Boston",
    "Honolulu", "Anchorage", "Mexico City", "London", "Paris",
    "Berlin", "Tokyo", "Sydney", "Mumbai", "Cairo",
];

/// Where every app draws its "‹ Home" button.
pub const HOME_BOX: Rect = Rect::new(28, 34, 188, 92);

/// A raised button with a centred label.
pub fn button(c: &mut Canvas, face: &Face, r: Rect, label: &str, bg: Color, ink: Color) {
    paper::slab(c, r, 22.0f32.min(r.height() as f32 / 2.0), bg);
    let base = r.y0 + (r.height() + face.ascent() - face.descent()) / 2;
    face.centered(c, label, r.x0, r.x1, base, ink);
}

/// The "‹ Home" button and a title.
pub fn header(c: &mut Canvas, fonts: &Fonts, title: &str) {
    button(c, &fonts.medium(24.0), HOME_BOX, "‹ Home", colors::PANEL, colors::INK);
    fonts.regular(48.0).draw(c, title, 40, 180, colors::INK);
}
