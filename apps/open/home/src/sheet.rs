//! Home's power sheet, which the held power key brings up (runtime/open/power.rs
//! sends KEY_POWER to Home). The screen lock is the Open Runtime's own
//! (runtime/open/trusted_ui.rs); Home draws none.
use violet::paper::{slab, ALERT, CREAM, PAPER, PAPER_PRESSED, PAPER_RAISED};
use violet::{rgb, Canvas, Fonts, Rect, H, W};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SheetHit {
    Off,
    Cancel,
}

/// The sheet the held power key brings up.
#[derive(Default)]
pub struct Sheet {
    pub pressed: Option<SheetHit>,
    /// Power off was chosen: the phone is going down.
    pub going: bool,
}

pub const CARD: Rect = Rect::new(60, 520, W as i32 - 60, 1010);
pub const OFF: Rect = Rect::new(100, 690, W as i32 - 100, 810);
pub const CANCEL: Rect = Rect::new(100, 850, W as i32 - 100, 970);

pub fn sheet_hit(x: i32, y: i32) -> Option<SheetHit> {
    if OFF.contains(x, y) {
        Some(SheetHit::Off)
    } else if CANCEL.contains(x, y) {
        Some(SheetHit::Cancel)
    } else {
        None
    }
}

pub fn draw_sheet(c: &mut Canvas, f: &Fonts, s: &Sheet) {
    c.round_rect_alpha(Rect::new(0, 0, W as i32, H as i32), 0.0, rgb(0, 0, 0), 0.6);
    if s.going {
        f.regular(44.0).centered(c, "Powering off…", 0, W as i32, H as i32 / 2, CREAM);
        return;
    }
    slab(c, CARD, 40.0, PAPER);
    f.regular(48.0).centered(c, "Power", CARD.x0, CARD.x1, 620, CREAM);
    let face = f.medium(36.0);
    let pressed = |h| s.pressed == Some(h);
    violet::button(c, &face, OFF, "Power off", if pressed(SheetHit::Off) { PAPER_PRESSED } else { ALERT }, if pressed(SheetHit::Off) { ALERT } else { PAPER });
    violet::button(c, &face, CANCEL, "Cancel", if pressed(SheetHit::Cancel) { PAPER_PRESSED } else { PAPER_RAISED }, CREAM);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_buttons() {
        assert_eq!(sheet_hit(360, 750), Some(SheetHit::Off));
        assert_eq!(sheet_hit(360, 900), Some(SheetHit::Cancel));
        assert_eq!(sheet_hit(360, 300), None);
        assert!(OFF.y1 < CANCEL.y0 && CANCEL.y1 < CARD.y1);
    }
}
