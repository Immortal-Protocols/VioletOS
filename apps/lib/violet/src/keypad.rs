//! A number pad for passcodes, shared by Home's lock screen and Settings:
//! the ten digits, delete and OK, and a row of dots for what was typed.
use crate::paper::{slab, CREAM, DIM, MUTED, PAPER_PRESSED, PAPER_RAISED, VIOLET};
use crate::{Canvas, Fonts, Rect, W};

/// A passcode is 4 to 8 digits (runtime/open/lock.rs).
pub const MIN: usize = 4;
pub const MAX: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PadKey {
    Digit(u8),
    Delete,
    Ok,
}

pub const TOP: i32 = 820;
const KEY_W: i32 = 176;
const KEY_H: i32 = 132;
const GAP_X: i32 = 28;
const GAP_Y: i32 = 24;
const LEFT: i32 = (W as i32 - 3 * KEY_W - 2 * GAP_X) / 2;

/// The keys in reading order: 1-9, then OK, 0 and delete.
pub const KEYS: [PadKey; 12] = [
    PadKey::Digit(1), PadKey::Digit(2), PadKey::Digit(3),
    PadKey::Digit(4), PadKey::Digit(5), PadKey::Digit(6),
    PadKey::Digit(7), PadKey::Digit(8), PadKey::Digit(9),
    PadKey::Ok, PadKey::Digit(0), PadKey::Delete,
];

pub fn key_rect(key: PadKey) -> Rect {
    let slot = KEYS.iter().position(|k| *k == key).unwrap_or(0) as i32;
    let (x, y) = (LEFT + slot % 3 * (KEY_W + GAP_X), TOP + slot / 3 * (KEY_H + GAP_Y));
    Rect::new(x, y, x + KEY_W, y + KEY_H)
}

pub fn key_at(x: i32, y: i32) -> Option<PadKey> {
    KEYS.into_iter().find(|k| key_rect(*k).contains(x, y))
}

/// Apply a key to the digits typed so far. True when OK was pressed with
/// enough digits to try.
pub fn press(key: PadKey, typed: &mut String) -> bool {
    match key {
        PadKey::Digit(d) if typed.len() < MAX => typed.push(char::from(b'0' + d)),
        PadKey::Digit(_) => {}
        PadKey::Delete => {
            typed.pop();
        }
        PadKey::Ok => return typed.len() >= MIN,
    }
    false
}

/// The pad. OK is dim until `typed` digits are enough.
pub fn draw(c: &mut Canvas, f: &Fonts, typed: usize, pressed: Option<PadKey>) {
    let (digit, word) = (f.regular(58.0), f.medium(32.0));
    for key in KEYS {
        let r = key_rect(key);
        if !r.inset(-16).touches(c.clip()) {
            continue;
        }
        slab(c, r, 30.0, if pressed == Some(key) { PAPER_PRESSED } else { PAPER_RAISED });
        let base = |face: &crate::Face| r.y0 + (r.height() + face.ascent() - face.descent()) / 2;
        match key {
            PadKey::Digit(d) => digit.centered(c, &d.to_string(), r.x0, r.x1, base(&digit), CREAM),
            PadKey::Ok => word.centered(c, "OK", r.x0, r.x1, base(&word), if typed >= MIN { VIOLET } else { DIM }),
            PadKey::Delete => {
                // A left-pointing tag with a cross.
                let (cx, cy) = ((r.x0 + r.x1) as f32 / 2.0 + 6.0, (r.y0 + r.y1) as f32 / 2.0);
                let ink = if typed > 0 { CREAM } else { DIM };
                for (a, b) in [((-34.0, 0.0), (-12.0, -22.0)), ((-12.0, -22.0), (30.0, -22.0)), ((30.0, -22.0), (30.0, 22.0)),
                               ((30.0, 22.0), (-12.0, 22.0)), ((-12.0, 22.0), (-34.0, 0.0)), ((0.0, -9.0), (18.0, 9.0)), ((18.0, -9.0), (0.0, 9.0))] {
                    c.line(cx + a.0, cy + a.1, cx + b.0, cy + b.1, 4.0, ink);
                }
            }
        }
    }
}

/// Where `dots` draws, for a clipped redraw.
pub fn dots_rect(cy: i32) -> Rect {
    Rect::new(0, cy - 30, W as i32, cy + 30)
}

/// One dot per digit typed, over faint places for the shortest passcode.
pub fn dots(c: &mut Canvas, cy: i32, typed: usize) {
    let n = typed.max(MIN) as i32;
    let step = 52;
    let x0 = W as i32 / 2 - (n - 1) * step / 2;
    for i in 0..n {
        let x = (x0 + i * step) as f32;
        if (i as usize) < typed {
            c.circle(x, cy as f32, 13.0, CREAM);
        } else {
            c.ring(x, cy as f32, 12.0, 3.0, MUTED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_do_not_overlap_and_fit() {
        for (i, a) in KEYS.iter().enumerate() {
            let r = key_rect(*a);
            assert!(r.x0 >= 0 && r.x1 <= W as i32 && r.y1 <= crate::H as i32);
            assert_eq!(key_at((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), Some(*a));
            for b in &KEYS[i + 1..] {
                assert!(!r.touches(key_rect(*b)));
            }
        }
        assert_eq!(key_at(4, TOP + 10), None);
    }

    #[test]
    fn typing() {
        let mut t = String::new();
        assert!(!press(PadKey::Ok, &mut t));
        for d in [2, 5, 8] {
            assert!(!press(PadKey::Digit(d), &mut t));
        }
        assert!(!press(PadKey::Ok, &mut t));
        press(PadKey::Digit(0), &mut t);
        assert_eq!(t, "2580");
        assert!(press(PadKey::Ok, &mut t));
        for _ in 0..9 {
            press(PadKey::Digit(1), &mut t);
        }
        assert_eq!(t.len(), MAX);
        press(PadKey::Delete, &mut t);
        assert_eq!(t, "2580111");
    }
}
