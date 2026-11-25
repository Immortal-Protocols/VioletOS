//! An on-screen keyboard for text fields: every printable ASCII character, on
//! three pages (letters, numbers and symbols, more symbols).
use crate::{colors::*, rgb, Canvas, Color, Face, Fonts, Rect, W};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Shift,
    Back,
    Space,
    Go,
    Page(Page),
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Page {
    #[default]
    Letters,
    Numbers,
    Symbols,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Shift {
    #[default]
    Off,
    /// Capitalise the next letter.
    Once,
    Locked,
}

#[derive(Default)]
pub struct Keyboard {
    address: bool,
    pub page: Page,
    pub shift: Shift,
    /// The key under the finger.
    pub pressed: Option<Key>,
}

pub const TOP: i32 = 1040;
const ROW_Y: [i32; 4] = [1072, 1192, 1312, 1432];
const KEY_H: i32 = 104;
const UNIT: f32 = 63.0;
const GAP: f32 = 8.0;

const BED: Color = rgb(0x1f, 0x1e, 0x1d);
const LETTER: Color = rgb(0x43, 0x40, 0x3b);
const SPECIAL: Color = rgb(0x30, 0x2e, 0x2b);

fn chars(s: &str) -> Vec<(Key, f32)> {
    s.chars().map(|c| (Key::Char(c), 1.0)).collect()
}

/// The keys of each row on a page, with their widths in key units.
fn rows(page: Page, address: bool) -> [Vec<(Key, f32)>; 4] {
    let bottom = |mode: Page| {
        if address {
            vec![(Key::Page(mode), 2.0), (Key::Space, 3.0), (Key::Char('.'), 1.0),
                 (Key::Char('/'), 1.0), (Key::Go, 2.0)]
        } else {
            vec![(Key::Page(mode), 2.0), (Key::Space, 6.0), (Key::Char('.'), 1.5)]
        }
    };
    let punctuation = |mode: Page| {
        let mut r = vec![(Key::Page(mode), 1.5)];
        r.extend(chars(".,?!'"));
        r.push((Key::Back, 1.5));
        r
    };
    match page {
        Page::Letters => {
            let mut third = vec![(Key::Shift, 1.5)];
            third.extend(chars("zxcvbnm"));
            third.push((Key::Back, 1.5));
            [chars("qwertyuiop"), chars("asdfghjkl"), third, bottom(Page::Numbers)]
        }
        Page::Numbers => [chars("1234567890"), chars("-/:;()$&@\""), punctuation(Page::Symbols), bottom(Page::Letters)],
        Page::Symbols => [chars("[]{}#%^*+="), chars("_\\|~<>`"), punctuation(Page::Numbers), bottom(Page::Letters)],
    }
}

/// Every key on the page and where it is.
fn layout(page: Page, address: bool) -> Vec<(Key, Rect)> {
    let mut out = Vec::new();
    for (row, keys) in rows(page, address).iter().enumerate() {
        let units: f32 = keys.iter().map(|k| k.1).sum();
        let width = units * UNIT + (keys.len() - 1) as f32 * GAP;
        let mut x = (W as f32 - width) / 2.0;
        for &(key, w) in keys {
            let kw = w * UNIT;
            out.push((key, Rect::new(x.round() as i32, ROW_Y[row], (x + kw).round() as i32, ROW_Y[row] + KEY_H)));
            x += kw + GAP;
        }
    }
    out
}

impl Keyboard {
    /// The text keyboard with URL punctuation and a submit key on every page.
    pub fn address() -> Self {
        Self { address: true, ..Self::default() }
    }

    /// The key at a point, counting the gaps around keys as theirs.
    pub fn key_at(&self, x: i32, y: i32) -> Option<Key> {
        if y < TOP {
            return None;
        }
        layout(self.page, self.address)
            .into_iter()
            .filter(|(_, r)| y >= r.y0 - GAP as i32 && y < r.y1 + GAP as i32)
            .min_by_key(|(_, r)| if x < r.x0 { r.x0 - x } else if x >= r.x1 { x - r.x1 + 1 } else { 0 })
            .filter(|(_, r)| x >= r.x0 - 2 * GAP as i32 && x < r.x1 + 2 * GAP as i32)
            .map(|(k, _)| k)
    }

    /// Apply a key to the text. Returns false for a key that changed nothing.
    pub fn press(&mut self, key: Key, text: &mut String, max: usize) -> bool {
        match key {
            Key::Char(c) => {
                if text.len() >= max {
                    return false;
                }
                let upper = self.page == Page::Letters && self.shift != Shift::Off;
                text.push(if upper { c.to_ascii_uppercase() } else { c });
                if self.shift == Shift::Once {
                    self.shift = Shift::Off;
                }
            }
            Key::Space => {
                if text.len() >= max {
                    return false;
                }
                text.push(' ');
            }
            Key::Back => return text.pop().is_some(),
            Key::Go => return false,
            Key::Shift => {
                self.shift = match self.shift {
                    Shift::Off => Shift::Once,
                    Shift::Once => Shift::Locked,
                    Shift::Locked => Shift::Off,
                }
            }
            Key::Page(p) => self.page = p,
        }
        true
    }

    pub fn draw(&self, c: &mut Canvas, f: &Fonts) {
        c.fill(Rect::new(0, TOP, W as i32, c.bounds().y1), BED);
        let (big, small) = (f.medium(40.0), f.medium(28.0));
        for (key, r) in layout(self.page, self.address) {
            let down = self.pressed == Some(key);
            let special = !matches!(key, Key::Char(_) | Key::Space);
            let lit = key == Key::Shift && self.shift != Shift::Off;
            let bg = if down || key == Key::Go { ACCENT } else if lit { INK } else if special { SPECIAL } else { LETTER };
            let ink = if down || lit || key == Key::Go { BACKGROUND } else { INK };
            c.round_rect(r, 14.0, bg);
            let mid_y = r.y0 + r.height() / 2;
            let label = |c: &mut Canvas, face: &Face, s: &str| {
                let base = mid_y + (face.ascent() + face.descent()) / 2;
                face.centered(c, s, r.x0, r.x1, base, ink);
            };
            match key {
                Key::Char(ch) => {
                    let shown = if self.page == Page::Letters && self.shift != Shift::Off { ch.to_ascii_uppercase() } else { ch };
                    label(c, &big, &shown.to_string());
                }
                Key::Space => label(c, &small, "space"),
                Key::Go => label(c, &small, "Go"),
                Key::Page(Page::Letters) => label(c, &small, "ABC"),
                Key::Page(Page::Numbers) => label(c, &small, "123"),
                Key::Page(Page::Symbols) => label(c, &small, "#+="),
                Key::Shift => {
                    let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0, mid_y as f32);
                    c.triangle([[cx - 20.0, cy + 2.0], [cx + 20.0, cy + 2.0], [cx, cy - 22.0]], ink);
                    c.fill(Rect::new(cx as i32 - 9, cy as i32, cx as i32 + 9, cy as i32 + 18), ink);
                    if self.shift == Shift::Locked {
                        c.fill(Rect::new(cx as i32 - 14, cy as i32 + 24, cx as i32 + 14, cy as i32 + 28), ink);
                    }
                }
                Key::Back => {
                    // A key cap pointing left with an x in it.
                    let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0 + 6.0, mid_y as f32);
                    c.triangle([[cx - 34.0, cy], [cx - 14.0, cy - 18.0], [cx - 14.0, cy + 18.0]], ink);
                    c.fill(Rect::new(cx as i32 - 14, cy as i32 - 18, cx as i32 + 22, cy as i32 + 18), ink);
                    c.line(cx - 6.0, cy - 8.0, cx + 10.0, cy + 8.0, 4.0, bg);
                    c.line(cx - 6.0, cy + 8.0, cx + 10.0, cy - 8.0, 4.0, bg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_character_can_be_typed() {
        let mut typed = std::collections::HashSet::new();
        for page in [Page::Letters, Page::Numbers, Page::Symbols] {
            for (key, r) in layout(page, false) {
                assert!(r.x0 >= 0 && r.x1 <= W as i32, "{key:?} is off screen");
                if let Key::Char(c) = key {
                    typed.insert(c);
                    typed.insert(c.to_ascii_uppercase());
                }
            }
        }
        typed.insert(' ');
        for b in 32u8..127 {
            assert!(typed.contains(&(b as char)), "{:?} cannot be typed", b as char);
        }
    }

    #[test]
    fn keys_are_found_where_they_are_drawn() {
        let k = Keyboard::default();
        for (key, r) in layout(Page::Letters, false) {
            assert_eq!(k.key_at((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), Some(key));
        }
        assert_eq!(k.key_at(360, 900), None);
    }

    #[test]
    fn address_keys_are_visible_and_tappable_on_every_page() {
        for page in [Page::Letters, Page::Numbers, Page::Symbols] {
            let keyboard = Keyboard { page, ..Keyboard::address() };
            for key in [Key::Char('.'), Key::Char('/'), Key::Go] {
                let (_, r) = layout(page, true).into_iter().rev().find(|(k, _)| *k == key).unwrap();
                assert!(r.x0 >= 0 && r.x1 <= W as i32);
                assert_eq!(keyboard.key_at((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), Some(key));
            }
        }
    }

    #[test]
    fn shift_capitalises_one_letter_unless_locked() {
        let mut k = Keyboard::default();
        let mut s = String::new();
        for key in [Key::Shift, Key::Char('a'), Key::Char('b'), Key::Shift, Key::Shift, Key::Char('c'), Key::Char('d')] {
            k.press(key, &mut s, 63);
        }
        assert_eq!(s, "AbCD");
        k.press(Key::Back, &mut s, 63);
        assert_eq!(s, "AbC");
    }
}
