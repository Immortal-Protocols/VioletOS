//! Text in the Go fonts (BSD licence, fonts/LICENSE), rasterized by fontdue.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::canvas::{Canvas, Color};

struct Glyph {
    metrics: fontdue::Metrics,
    coverage: Vec<u8>,
}

struct Family {
    font: fontdue::Font,
    cache: RefCell<HashMap<(char, u32), Rc<Glyph>>>,
}

/// The three weights, loaded once.
#[derive(Clone)]
pub struct Fonts {
    regular: Rc<Family>,
    medium: Rc<Family>,
    bold: Rc<Family>,
}

fn family(bytes: &'static [u8]) -> Rc<Family> {
    let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("font");
    Rc::new(Family { font, cache: RefCell::new(HashMap::new()) })
}

impl Fonts {
    pub fn load() -> Fonts {
        Fonts {
            regular: family(include_bytes!("../fonts/Go-Regular.ttf")),
            medium: family(include_bytes!("../fonts/Go-Medium.ttf")),
            bold: family(include_bytes!("../fonts/Go-Bold.ttf")),
        }
    }
    pub fn regular(&self, size: f32) -> Face {
        Face { family: self.regular.clone(), size }
    }
    pub fn medium(&self, size: f32) -> Face {
        Face { family: self.medium.clone(), size }
    }
    pub fn bold(&self, size: f32) -> Face {
        Face { family: self.bold.clone(), size }
    }
}

/// A font at a pixel size.
#[derive(Clone)]
pub struct Face {
    family: Rc<Family>,
    size: f32,
}

impl Face {
    fn glyph(&self, ch: char) -> Rc<Glyph> {
        let key = (ch, (self.size * 4.0) as u32);
        if let Some(g) = self.family.cache.borrow().get(&key) {
            return g.clone();
        }
        let (metrics, coverage) = self.family.font.rasterize(ch, self.size);
        let g = Rc::new(Glyph { metrics, coverage });
        self.family.cache.borrow_mut().insert(key, g.clone());
        g
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn ascent(&self) -> i32 {
        self.family.font.horizontal_line_metrics(self.size).map_or(self.size * 0.8, |m| m.ascent).round() as i32
    }
    pub fn descent(&self) -> i32 {
        self.family.font.horizontal_line_metrics(self.size).map_or(-self.size * 0.2, |m| m.descent).round() as i32
    }

    /// Draw s with its baseline at y; returns the x where it ended.
    pub fn draw(&self, c: &mut Canvas, s: &str, x: i32, y: i32, color: Color) -> i32 {
        let mut pen = x as f32;
        for ch in s.chars() {
            let g = self.glyph(ch);
            let m = &g.metrics;
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = y - m.ymin - m.height as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = g.coverage[row * m.width + col];
                    if a != 0 {
                        c.blend(gx + col as i32, gy + row as i32, color, a as f32 / 255.0);
                    }
                }
            }
            pen += m.advance_width;
        }
        pen.round() as i32
    }

    pub fn width(&self, s: &str) -> i32 {
        s.chars().map(|ch| self.glyph(ch).metrics.advance_width).sum::<f32>().round() as i32
    }

    /// Draw s centred between x0 and x1.
    pub fn centered(&self, c: &mut Canvas, s: &str, x0: i32, x1: i32, y: i32, color: Color) {
        self.draw(c, s, x0 + (x1 - x0 - self.width(s)) / 2, y, color);
    }

    /// Shorten s with an ellipsis until it fits in width pixels.
    pub fn fit(&self, s: &str, width: i32) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        if self.width(s) <= width {
            return s.to_string();
        }
        while chars.len() > 1 {
            chars.pop();
            let t: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.width(&t) <= width {
                return t;
            }
        }
        "…".to_string()
    }
}
