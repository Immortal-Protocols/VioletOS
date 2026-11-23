//! Microphone input: the level meter, the recording and its playback.
//! Pure arithmetic, so `rustc --test` can check it on the host.

/// Microphone samples per second on every board.
pub const RATE: u32 = 16000;
/// Playback runs at 48 kHz, three output frames per recorded sample.
const UPSAMPLE: u32 = 3;
/// The meter reports the peak of each 100 ms window.
const WINDOW: u32 = RATE / 10;

/// Peak of a window mapped to 0..=100: -60 dBFS or quieter is 0, full scale 100.
pub fn level(peak: u32) -> u32 {
    if peak == 0 {
        return 0;
    }
    // log2 in sixteenths: the leading bit's position and the next four bits.
    let top = 31 - peak.leading_zeros();
    let frac = if top >= 4 { (peak >> (top - 4)) & 15 } else { (peak << (4 - top)) & 15 };
    let log = top * 16 + frac; // 32768 -> 240, 32 (-60 dB) -> 80
    (log.saturating_sub(80) * 100 / 160).min(100)
}

/// Tracks the input level and removes the ADC's DC offset.
#[derive(Default)]
pub struct Meter {
    dc: i32,
    peak: u32,
    count: u32,
    /// The latest window's level, 0..=100.
    pub level: u32,
}
impl Meter {
    /// Takes a raw sample and returns it with the DC offset removed.
    pub fn feed(&mut self, raw: i16) -> i16 {
        let v = i32::from(raw) << 8;
        self.dc += (v - self.dc) >> 11; // about 8 Hz at 16 kHz
        let s = ((v - self.dc) >> 8).clamp(-32768, 32767);
        self.peak = self.peak.max(s.unsigned_abs());
        self.count += 1;
        if self.count == WINDOW {
            self.level = level(self.peak);
            self.peak = 0;
            self.count = 0;
        }
        s as i16
    }
    pub fn reset(&mut self) {
        self.peak = 0;
        self.count = 0;
        self.level = 0;
    }
}

/// Plays `len` samples of a recording at 48 kHz with linear interpolation,
/// scaled so its peak sits at half of full scale like the built-in sounds.
pub struct Playback {
    len: u32,
    frame: u32,
    gain: i32, // Q8
}
impl Playback {
    pub fn new(len: u32, peak: u32) -> Self {
        // At most 24 dB of make-up gain so a silent recording stays quiet.
        let gain = if peak == 0 { 256 } else { (16384 * 256 / peak as i32).clamp(16, 16 * 256) };
        Playback { len, frame: 0, gain }
    }
    pub fn done(&self) -> bool {
        self.frame >= self.len * UPSAMPLE
    }
    /// The next 48 kHz sample, before the volume gain.
    pub fn next(&mut self, clip: &[i16]) -> i32 {
        if self.done() {
            return 0;
        }
        let at = (self.frame / UPSAMPLE) as usize;
        let frac = (self.frame % UPSAMPLE) as i32;
        let a = i32::from(clip[at]);
        let b = clip.get(at + 1).map_or(a, |&b| i32::from(b));
        self.frame += 1;
        // 2 ms fades at both ends hide the cut.
        let fade = 96;
        let edge = self.frame.min(self.len * UPSAMPLE - self.frame) as i32;
        let s = (a + (b - a) * frac / UPSAMPLE as i32) * self.gain >> 8;
        (s * edge.min(fade) / fade).clamp(-32767, 32767)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn level_spans_sixty_decibels() {
        assert_eq!(level(0), 0);
        assert_eq!(level(32), 0);
        assert_eq!(level(32767), 99);
        assert_eq!(level(32768), 100);
        // -20 dB is two thirds of the way up.
        let l = level(3277);
        assert!((65..=68).contains(&l), "{l}");
    }
    #[test]
    fn meter_removes_dc_and_reports_peak() {
        let mut m = Meter::default();
        let mut last = 0;
        for i in 0..RATE {
            // A 1 kHz square wave at 1000 on a DC offset of 3000.
            let s = if (i / 8) % 2 == 0 { 4000 } else { 2000 };
            last = m.feed(s);
        }
        assert!(last.unsigned_abs() < 1100, "{last}");
        assert_eq!(m.level, level(1000));
    }
    #[test]
    fn playback_upsamples_and_normalises() {
        let clip = [0i16, 1000, 2000, 1000, 0, -1000, -2000, -1000, 0, 0];
        let mut p = Playback::new(clip.len() as u32, 2000);
        let mut out = std::vec::Vec::new();
        while !p.done() {
            out.push(p.next(&clip));
        }
        assert_eq!(out.len(), clip.len() * 3);
        // Peak scaled to about 16384, before fades (the clip is shorter than one).
        let mut p = Playback::new(400, 2000);
        let flat = [2000i16; 400];
        let mid: std::vec::Vec<i32> = (0..600).map(|_| p.next(&flat)).collect();
        assert!((16370..=16384).contains(&mid[300]), "{}", mid[300]);
        assert!(mid[0] < 1000);
    }
}
