//! What a power key press means, from polled samples (power.rs applies it).
//! Host tests: `make test-unit`. The passcode itself is the trusted UI's
//! (sysui.rs `Pass`); a record saved by the earlier lock in this file's place
//! is read by sysui.rs `Legacy`.

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Press {
    /// Turn the screen off (and lock), or back on.
    Short,
    /// Held: show the power sheet.
    Long,
}

pub const LONG_NS: u64 = 800_000_000;

/// The power key, from polled samples. With the screen off a press acts as
/// the key goes down, so the screen wakes at once; with it on, a short press
/// acts on release, so that holding the key shows the power sheet instead.
#[derive(Default)]
pub struct Key {
    seen: bool,
    count: u32,
    /// When the key went down, and whether that press has acted already.
    down: Option<(u64, bool)>,
    /// Held since before the first sample (the press that started the phone).
    stale: bool,
}

impl Key {
    /// `down` is the key's level and `count` the board's press counter (0 where
    /// it has none), which catches a press that came and went between samples.
    pub fn update(&mut self, down: bool, count: u32, now_ns: u64, screen_on: bool) -> Option<Press> {
        let first = !self.seen;
        let counted = self.seen && count != self.count;
        self.seen = true;
        self.count = count;
        if first {
            self.stale = down;
            return None;
        }
        if self.stale {
            self.stale = down;
            return None;
        }
        match (down, self.down) {
            (true, None) => {
                self.down = Some((now_ns, !screen_on));
                (!screen_on).then_some(Press::Short)
            }
            (true, Some((since, _))) if since != u64::MAX && now_ns.saturating_sub(since) >= LONG_NS => {
                self.down = Some((u64::MAX, true));
                Some(Press::Long)
            }
            (true, Some(_)) => None,
            (false, Some((_, acted))) => {
                self.down = None;
                (!acted).then_some(Press::Short)
            }
            (false, None) => counted.then_some(Press::Short),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn short_press_acts_on_release_while_on() {
        let mut k = Key::default();
        assert_eq!(k.update(false, 0, 0, true), None);
        assert_eq!(k.update(true, 0, 40 * MS, true), None);
        assert_eq!(k.update(true, 0, 80 * MS, true), None);
        assert_eq!(k.update(false, 0, 120 * MS, true), Some(Press::Short));
        assert_eq!(k.update(false, 0, 160 * MS, true), None);
    }

    #[test]
    fn press_wakes_at_once_while_off() {
        let mut k = Key::default();
        k.update(false, 0, 0, false);
        assert_eq!(k.update(true, 0, 40 * MS, false), Some(Press::Short));
        // The screen is on by the time the key comes up: no second action.
        assert_eq!(k.update(true, 0, 80 * MS, true), None);
        assert_eq!(k.update(false, 0, 120 * MS, true), None);
    }

    #[test]
    fn hold_is_long_once() {
        let mut k = Key::default();
        k.update(false, 0, 0, true);
        assert_eq!(k.update(true, 0, 0, true), None);
        assert_eq!(k.update(true, 0, 799 * MS, true), None);
        assert_eq!(k.update(true, 0, 800 * MS, true), Some(Press::Long));
        assert_eq!(k.update(true, 0, 3000 * MS, true), None);
        assert_eq!(k.update(false, 0, 3100 * MS, true), None);
        // Held from off: wake, then the sheet.
        assert_eq!(k.update(true, 0, 4000 * MS, false), Some(Press::Short));
        assert_eq!(k.update(true, 0, 4900 * MS, true), Some(Press::Long));
        assert_eq!(k.update(false, 0, 5000 * MS, true), None);
    }

    #[test]
    fn counted_press_between_samples() {
        let mut k = Key::default();
        k.update(false, 5, 0, true);
        assert_eq!(k.update(false, 5, 40 * MS, true), None);
        assert_eq!(k.update(false, 6, 80 * MS, true), Some(Press::Short));
        // A press seen as a level too counts once.
        assert_eq!(k.update(true, 7, 120 * MS, true), None);
        assert_eq!(k.update(false, 7, 160 * MS, true), Some(Press::Short));
    }

    #[test]
    fn key_held_since_start_is_ignored() {
        let mut k = Key::default();
        assert_eq!(k.update(true, 0, 0, true), None);
        assert_eq!(k.update(true, 0, 2000 * MS, true), None);
        assert_eq!(k.update(false, 0, 2100 * MS, true), None);
        assert_eq!(k.update(true, 0, 3000 * MS, true), None);
        assert_eq!(k.update(false, 0, 3100 * MS, true), Some(Press::Short));
    }
}
