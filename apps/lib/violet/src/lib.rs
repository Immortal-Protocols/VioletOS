//! A small library for VioletOS open apps written in Rust: the /dev/violet
//! devices as ordinary Linux files (runtime/open/abi.rs), drawing on a
//! 720x1600 canvas, text in the Go fonts, and HTTPS with rustls.
pub mod camera;
pub mod canvas;
pub mod compositor;
pub mod device;
pub mod http;
pub mod text;
pub mod wifi;

pub use canvas::{mix, rgb, Canvas, Color, Rect};
pub use compositor::Compositor;
pub use device::{events, fields, home, log, sys, Event, Events, Screen, H, W};
pub use text::{Face, Fonts};

/// The shared look of the open apps.
pub mod colors {
    use super::{rgb, Color};
    pub const BACKGROUND: Color = rgb(0x14, 0x0e, 0x1e);
    pub const PANEL: Color = rgb(0x2a, 0x20, 0x3c);
    pub const INK: Color = rgb(0xee, 0xea, 0xf8);
    pub const MUTED: Color = rgb(0xa8, 0x9c, 0xc4);
    pub const ACCENT: Color = rgb(0xe6, 0x8c, 0xc8);
}

/// The cities Weather offers (and Home shows the weather for); Weather keeps
/// the chosen index in /data/weather.
pub const CITIES: [&str; 20] = [
    "San Francisco", "Oakland", "Los Angeles", "Seattle", "New York",
    "Chicago", "Austin", "Miami", "Denver", "Boston",
    "Honolulu", "Anchorage", "Mexico City", "London", "Paris",
    "Berlin", "Tokyo", "Sydney", "Mumbai", "Cairo",
];

/// Where every app draws its "‹ Home" button.
pub const HOME_BOX: Rect = Rect::new(28, 34, 188, 92);

/// A rounded button with a centred label.
pub fn button(c: &mut Canvas, face: &Face, r: Rect, label: &str, bg: Color, ink: Color) {
    c.round_rect(r, 22.0, bg);
    let base = r.y0 + (r.height() + face.ascent() - face.descent()) / 2;
    face.centered(c, label, r.x0, r.x1, base, ink);
}

/// The "‹ Home" button and a title.
pub fn header(c: &mut Canvas, fonts: &Fonts, title: &str) {
    button(c, &fonts.medium(24.0), HOME_BOX, "‹ Home", colors::PANEL, colors::INK);
    fonts.bold(44.0).draw(c, title, 40, 180, colors::INK);
}
