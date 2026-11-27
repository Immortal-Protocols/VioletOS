//! Home's lock screen and its power sheet. The Open Runtime keeps Home in
//! front while the phone is locked and checks the passcode itself ("unlock" in
//! /dev/violet/sys, runtime/open/power.rs); this is only the screen.
use violet::keypad::{self, PadKey};
use violet::paper::{slab, ALERT, CREAM, MUTED, PAPER, PAPER_PRESSED, PAPER_RAISED};
use violet::{rgb, Canvas, Fonts, Rect, H, W};

use crate::clock::Local;

#[derive(Default)]
pub struct Lock {
    pub typed: String,
    pub pressed: Option<PadKey>,
    pub note: String,
    /// Seconds until another try is allowed, after too many wrong ones.
    pub wait: u64,
}

const DOTS_Y: i32 = 650;

/// What typing changes above the pad: the dots and the line under them.
pub fn entry_rect() -> Rect {
    Rect::new(0, DOTS_Y - 40, W as i32, 770)
}

pub fn wait_note(seconds: u64) -> String {
    match seconds {
        0 => String::new(),
        1 => "Try again in 1 second".into(),
        2..=90 => format!("Try again in {seconds} seconds"),
        _ => format!("Try again in {} minutes", seconds.div_ceil(60)),
    }
}

impl Lock {
    /// The runtime's answer to "lock": (locked, seconds to wait).
    pub fn state() -> (bool, u64) {
        let f = violet::fields(&violet::sys("lock"));
        (f.get("locked").is_some_and(|v| v == "1"), f.get("wait").and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn set_wait(&mut self, seconds: u64) {
        if self.wait != 0 || seconds != 0 {
            self.note = wait_note(seconds);
        }
        self.wait = seconds;
    }

    /// A key was released. True when the phone unlocked.
    pub fn key(&mut self, key: PadKey) -> bool {
        if self.wait > 0 {
            return false;
        }
        self.note.clear();
        if !keypad::press(key, &mut self.typed) {
            return false;
        }
        let answer = violet::sys(&format!("unlock {}", self.typed));
        self.typed.clear();
        if answer == "ok" {
            return true;
        }
        match answer.strip_prefix("error wait ").and_then(|w| w.trim().parse().ok()) {
            Some(seconds) => self.set_wait(seconds),
            None => self.note = "Wrong passcode".into(),
        }
        false
    }
}

/// Over the wallpaper: the time, the dots and the number pad.
pub fn draw(c: &mut Canvas, f: &Fonts, now: Option<&Local>, l: &Lock) {
    match now {
        Some(t) => {
            let hour = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
            f.regular(150.0).centered(c, &format!("{}:{:02}", hour, t.minute), 0, W as i32, 330, CREAM);
            f.regular(40.0).centered(c, &t.date(), 0, W as i32, 410, MUTED);
        }
        None => f.regular(72.0).centered(c, "VioletOS", 0, W as i32, 330, CREAM),
    }
    f.regular(36.0).centered(c, "Enter passcode", 0, W as i32, 560, CREAM);
    keypad::dots(c, DOTS_Y, l.typed.len());
    if !l.note.is_empty() {
        f.regular(30.0).centered(c, &l.note, 0, W as i32, 740, ALERT);
    }
    keypad::draw(c, f, if l.wait > 0 { 0 } else { l.typed.len() }, l.pressed);
}

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
    fn waits_read_naturally() {
        assert_eq!(wait_note(0), "");
        assert_eq!(wait_note(1), "Try again in 1 second");
        assert_eq!(wait_note(30), "Try again in 30 seconds");
        assert_eq!(wait_note(300), "Try again in 5 minutes");
        assert_eq!(wait_note(301), "Try again in 6 minutes");
    }

    #[test]
    fn sheet_buttons() {
        assert_eq!(sheet_hit(360, 750), Some(SheetHit::Off));
        assert_eq!(sheet_hit(360, 900), Some(SheetHit::Cancel));
        assert_eq!(sheet_hit(360, 300), None);
        assert!(OFF.y1 < CANCEL.y0 && CANCEL.y1 < CARD.y1);
    }

    /// The pad stays below the line under the dots.
    #[test]
    fn layout() {
        assert!(entry_rect().y1 <= keypad::TOP - 16);
        assert!(entry_rect().contains(360, DOTS_Y) && entry_rect().contains(360, 740));
    }
}
