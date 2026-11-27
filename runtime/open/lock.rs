//! The screen lock's rules, apart from the devices (power.rs applies them):
//! what a power key press means, the stored passcode, and how long to wait
//! after wrong guesses. Host tests: `make test-unit`.
use alloc::string::String;

/// The passcode record. Apps cannot open it (syscall.rs refuses the path);
/// only the runtime reads and writes it.
pub const FILE: &str = "/data/.lock";

pub type Sha256 = fn(&[u8]) -> [u8; 32];

/// A passcode is 4 to 8 digits.
pub fn valid_code(code: &str) -> bool {
    (4..=8).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit())
}

const ROUNDS: u32 = 4096;

fn digest(salt: &[u8; 16], code: &str, sha: Sha256) -> [u8; 32] {
    let mut block = [0u8; 16 + 32 + 8];
    block[..16].copy_from_slice(salt);
    block[48..48 + code.len()].copy_from_slice(code.as_bytes());
    let mut h = sha(&block[..16 + 32 + code.len()]);
    for _ in 1..ROUNDS {
        block[16..48].copy_from_slice(&h);
        h = sha(&block[..16 + 32 + code.len()]);
    }
    h
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        s.push(char::from(b"0123456789abcdef"[(b >> 4) as usize]));
        s.push(char::from(b"0123456789abcdef"[(b & 15) as usize]));
    }
    s
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.as_bytes();
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in s.chunks(2).enumerate() {
        let digit = |c: u8| char::from(c).to_digit(16);
        out[i] = (digit(pair[0])? * 16 + digit(pair[1])?) as u8;
    }
    Some(out)
}

/// The record kept for a passcode: "1 SALT HASH", a salted, repeated SHA-256.
/// It keeps the digits themselves off the disk; it cannot make a short
/// passcode hard to guess for someone who reads the disk.
pub fn record(code: &str, salt: [u8; 16], sha: Sha256) -> Option<String> {
    valid_code(code).then(|| alloc::format!("1 {} {}\n", hex(&salt), hex(&digest(&salt, code, sha))))
}

/// Whether `code` is the passcode in `record`.
pub fn matches(record: &str, code: &str, sha: Sha256) -> bool {
    let mut words = record.split_whitespace();
    let (Some("1"), Some(salt), Some(hash)) = (words.next(), words.next(), words.next()) else { return false };
    let (Some(salt), Some(hash)) = (unhex::<16>(salt), unhex::<32>(hash)) else { return false };
    if !valid_code(code) {
        return false;
    }
    let ours = digest(&salt, code, sha);
    ours.iter().zip(&hash).fold(0, |d, (a, b)| d | (a ^ b)) == 0
}

/// The record with its count of wrong guesses in a row ("1 SALT HASH WRONG"),
/// so that restarting the phone does not earn five more free guesses.
pub fn with_wrong(record: &str, wrong: u32) -> String {
    let mut words = record.split_whitespace();
    let (version, salt, hash) = (words.next().unwrap_or(""), words.next().unwrap_or(""), words.next().unwrap_or(""));
    if wrong == 0 {
        alloc::format!("{} {} {}\n", version, salt, hash)
    } else {
        alloc::format!("{} {} {} {}\n", version, salt, hash, wrong)
    }
}

/// The count `with_wrong` kept.
pub fn wrong_in(record: &str) -> u32 {
    record.split_whitespace().nth(3).and_then(|w| w.parse().ok()).unwrap_or(0)
}

/// Wrong guesses: five are free, then each one costs a wait that grows.
#[derive(Default)]
pub struct Attempts {
    failed: u32,
    retry_ns: u64,
}

const FREE: u32 = 5;
const WAITS: [u64; 5] = [30, 60, 300, 900, 3600];

impl Attempts {
    /// Whole seconds until another guess is allowed.
    pub fn wait(&self, now_ns: u64) -> u64 {
        self.retry_ns.saturating_sub(now_ns).div_ceil(1_000_000_000)
    }
    pub fn count(&self) -> u32 {
        self.failed
    }
    /// After a restart: `failed` wrong guesses were already made. Past the
    /// free ones, the last wait starts again.
    pub fn restore(&mut self, failed: u32, now_ns: u64) {
        self.failed = failed.saturating_sub(1);
        self.retry_ns = 0;
        if failed > 0 {
            self.wrong(now_ns);
        }
    }
    pub fn wrong(&mut self, now_ns: u64) {
        self.failed += 1;
        if self.failed >= FREE {
            let step = ((self.failed - FREE) as usize).min(WAITS.len() - 1);
            self.retry_ns = now_ns + WAITS[step] * 1_000_000_000;
        }
    }
    pub fn right(&mut self) {
        *self = Attempts::default();
    }
}

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

    /// Not SHA-256, but enough to tell inputs apart in these tests.
    fn sha(data: &[u8]) -> [u8; 32] {
        let mut h = [0x9e37_79b9_7f4a_7c15u64, 0xbf58_476d_1ce4_e5b9, 0x94d0_49bb_1331_11eb, 0x2545_f491_4f6c_dd1d];
        for (i, b) in data.iter().enumerate() {
            let k = i % 4;
            h[k] = (h[k] ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3).rotate_left(23) ^ h[(k + 1) % 4];
        }
        let mut out = [0u8; 32];
        for (i, w) in h.iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    #[test]
    fn codes() {
        for ok in ["0000", "123456", "12345678"] {
            assert!(valid_code(ok));
        }
        for bad in ["", "123", "123456789", "12a4", "12 4", "１２３４"] {
            assert!(!valid_code(bad));
        }
    }

    #[test]
    fn record_matches_only_its_code() {
        let r = record("2580", [7; 16], sha).unwrap();
        assert!(r.starts_with("1 07070707070707070707070707070707 "));
        assert!(!r.contains("2580"));
        assert!(matches(&r, "2580", sha));
        for wrong in ["2581", "0000", "25800", "258", ""] {
            assert!(!matches(&r, wrong, sha));
        }
        // The salt changes the record.
        assert_ne!(Some(&r), record("2580", [8; 16], sha).as_ref());
        assert_eq!(record("25", [7; 16], sha), None);
        for broken in ["", "1", "2 00 00", "1 zz zz", &r[..r.len() - 3]] {
            assert!(!matches(broken, "2580", sha));
        }
    }

    #[test]
    fn waits_grow_after_five_wrong() {
        const S: u64 = 1_000_000_000;
        let mut a = Attempts::default();
        for _ in 0..4 {
            a.wrong(0);
            assert_eq!(a.wait(0), 0);
        }
        a.wrong(10 * S);
        assert_eq!(a.wait(10 * S), 30);
        assert_eq!(a.wait(39 * S + 1), 1);
        assert_eq!(a.wait(40 * S), 0);
        a.wrong(40 * S);
        assert_eq!(a.wait(40 * S), 60);
        for _ in 0..10 {
            a.wrong(0);
        }
        assert_eq!(a.wait(0), 3600);
        a.right();
        assert_eq!(a.wait(0), 0);
        a.wrong(0);
        assert_eq!(a.wait(0), 0);
    }

    #[test]
    fn wrong_guesses_survive_a_restart() {
        const S: u64 = 1_000_000_000;
        let r = record("2580", [7; 16], sha).unwrap();
        assert_eq!(wrong_in(&r), 0);
        let six = with_wrong(&r, 6);
        assert_eq!(wrong_in(&six), 6);
        assert!(matches(&six, "2580", sha) && !matches(&six, "2581", sha));
        assert_eq!(with_wrong(&six, 0), r);
        let mut a = Attempts::default();
        a.restore(wrong_in(&six), 100 * S);
        assert_eq!((a.count(), a.wait(100 * S)), (6, 60));
        a.restore(3, 0);
        assert_eq!((a.count(), a.wait(0)), (3, 0));
        a.restore(0, 0);
        assert_eq!((a.count(), a.wait(0)), (0, 0));
    }

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
