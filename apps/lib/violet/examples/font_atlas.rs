//! Bake the Go fonts into a glyph atlas for Python apps (tools/build_python_package.py):
//! the same faces and rasterizer as violet::Fonts, at fixed sizes, so a Python
//! app's text looks like a Rust app's without a font engine on the phone.
//!
//!     cargo run --example font_atlas -- OUT
//!
//! Format, little-endian: "VFNT", u16 face count; per face: u8 weight (0 regular,
//! 1 bold), u8 0, u16 size, i16 ascent, i16 descent, u16 glyph count, then per
//! glyph: u32 code point, f32 advance, i16 xmin, i16 ymin, u16 width,
//! u16 height, u32 offset of its coverage bytes. Then the coverage (one byte a
//! pixel, rows top to bottom).

const SIZES: [f32; 7] = [24.0, 32.0, 40.0, 48.0, 64.0, 88.0, 120.0];
const EXTRA: &str = "‹›…•°×÷–—‘’“”←→↑↓€£¥±·";

fn main() {
    let out = std::env::args().nth(1).expect("usage: font_atlas OUT");
    let fonts: [(u8, &[u8]); 2] = [
        (0, include_bytes!("../fonts/Go-Regular.ttf")),
        (1, include_bytes!("../fonts/Go-Bold.ttf")),
    ];
    let chars: Vec<char> = (32u8..127).map(char::from).chain(EXTRA.chars()).collect();
    let mut head = Vec::new();
    let mut blob = Vec::new();
    head.extend_from_slice(b"VFNT");
    head.extend_from_slice(&((fonts.len() * SIZES.len()) as u16).to_le_bytes());
    for (weight, bytes) in fonts {
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("font");
        let present: Vec<char> = chars.iter().copied().filter(|&c| c == ' ' || font.lookup_glyph_index(c) != 0).collect();
        for size in SIZES {
            let lines = font.horizontal_line_metrics(size).expect("line metrics");
            head.extend_from_slice(&[weight, 0]);
            head.extend_from_slice(&(size as u16).to_le_bytes());
            head.extend_from_slice(&(lines.ascent.round() as i16).to_le_bytes());
            head.extend_from_slice(&(lines.descent.round() as i16).to_le_bytes());
            head.extend_from_slice(&(present.len() as u16).to_le_bytes());
            for &ch in &present {
                let (m, coverage) = font.rasterize(ch, size);
                head.extend_from_slice(&(ch as u32).to_le_bytes());
                head.extend_from_slice(&m.advance_width.to_le_bytes());
                head.extend_from_slice(&(m.xmin as i16).to_le_bytes());
                head.extend_from_slice(&(m.ymin as i16).to_le_bytes());
                head.extend_from_slice(&(m.width as u16).to_le_bytes());
                head.extend_from_slice(&(m.height as u16).to_le_bytes());
                head.extend_from_slice(&(blob.len() as u32).to_le_bytes());
                blob.extend_from_slice(&coverage);
            }
        }
    }
    head.extend_from_slice(&blob);
    std::fs::write(&out, &head).expect("write");
    println!("font atlas: {} bytes -> {out}", head.len());
}
