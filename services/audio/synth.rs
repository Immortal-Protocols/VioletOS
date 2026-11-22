//! Built-in sounds, synthesized at 48 kHz in integer arithmetic.
use crate::protocol::*;

pub const RATE: u32 = 48000;
// A quarter of a sine wave, 65 points including both ends, full scale.
const QUARTER: [i16; 65] = [
    0, 804, 1608, 2410, 3212, 4011, 4808, 5602, 6393, 7179, 7962, 8739, 9512, 10278, 11039,
    11793, 12539, 13279, 14010, 14732, 15446, 16151, 16846, 17530, 18204, 18868, 19519, 20159,
    20787, 21403, 22005, 22594, 23170, 23731, 24279, 24811, 25329, 25832, 26319, 26790, 27245,
    27683, 28105, 28510, 28898, 29268, 29621, 29956, 30273, 30571, 30852, 31113, 31356, 31580,
    31785, 31971, 32137, 32285, 32412, 32521, 32609, 32678, 32728, 32757, 32767,
];
/// Q15 output gain per volume level, 3 dB apart. Level 0 is silent.
pub const GAIN: [i32; 16] = [
    0, 260, 368, 519, 734, 1036, 1464, 2067, 2920, 4125, 5827, 8231, 11626, 16422, 23197, 32767,
];
fn sine(phase: u32) -> i32 {
    let index = (phase >> 24) as usize; // 256 steps per cycle
    let quadrant = index >> 6;
    let step = index & 63;
    let value = i32::from(QUARTER[if quadrant & 1 == 0 { step } else { 64 - step }]);
    if quadrant < 2 { value } else { -value }
}

#[derive(Clone, Copy)]
struct Note {
    hz: u32,
    ms: u32,
    /// Fade to silence over the whole note instead of holding its level.
    decay: bool,
}
const fn note(hz: u32, ms: u32, decay: bool) -> Note {
    Note { hz, ms, decay }
}
const CLICK: [Note; 1] = [note(2000, 6, true)];
const CHIME: [Note; 2] = [note(880, 160, true), note(1319, 420, true)];
const ALERT: [Note; 5] = [
    note(1000, 110, false), note(0, 70, false), note(1000, 110, false), note(0, 70, false),
    note(1000, 110, false),
];

pub struct Voice {
    notes: [Note; 5],
    count: usize,
    index: usize,
    frame: u32,
    phase: u32,
}
impl Voice {
    /// Returns None for an unknown sound or a tone outside 20 Hz to 20 kHz or 1 ms to 5 s.
    pub fn new(sound: u32, arg: u32) -> Option<Self> {
        let mut notes = [note(0, 0, false); 5];
        let list: &[Note] = match sound {
            SOUND_CLICK => &CLICK,
            SOUND_CHIME => &CHIME,
            SOUND_ALERT => &ALERT,
            SOUND_TONE => {
                let (hz, ms) = (arg & 0xffff, arg >> 16);
                if !(20..=20000).contains(&hz) || !(1..=5000).contains(&ms) {
                    return None;
                }
                notes[0] = note(hz, ms, false);
                &notes[..1]
            }
            _ => return None,
        };
        let count = list.len();
        let mut copy = [note(0, 0, false); 5];
        copy[..count].copy_from_slice(list);
        Some(Self { notes: copy, count, index: 0, frame: 0, phase: 0 })
    }
    pub fn done(&self) -> bool {
        self.index >= self.count
    }
    /// Mono samples at half of full scale, before the volume gain.
    pub fn next(&mut self) -> i32 {
        let Some(n) = self.notes[..self.count].get(self.index).copied() else {
            return 0;
        };
        let length = (n.ms * (RATE / 1000)).max(2);
        // 2 ms linear ramps at both ends keep notes free of clicks.
        let ramp = RATE / 500;
        let edge = self.frame.min(length - 1 - self.frame);
        let mut level = 16384 * edge.min(ramp) as i32 / ramp as i32;
        if n.decay {
            level = level * (length - self.frame) as i32 / length as i32;
        }
        let sample = if n.hz == 0 { 0 } else { sine(self.phase) * level >> 15 };
        self.phase = self.phase.wrapping_add(step(n.hz));
        self.frame += 1;
        if self.frame >= length {
            self.index += 1;
            self.frame = 0;
            self.phase = 0;
        }
        sample
    }
}
fn step(hz: u32) -> u32 {
    ((u64::from(hz) << 32) / u64::from(RATE)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tone_lasts_its_duration_and_stays_in_range() {
        let mut v = Voice::new(SOUND_TONE, 1000 | 10 << 16).unwrap();
        let mut frames = 0;
        let mut peak = 0;
        while !v.done() {
            peak = peak.max(v.next().abs());
            frames += 1;
        }
        assert_eq!(frames, 480);
        assert!(peak > 15000 && peak <= 16384, "{peak}");
    }
    #[test]
    fn rejects_out_of_range_tones() {
        assert!(Voice::new(SOUND_TONE, 10 | 100 << 16).is_none());
        assert!(Voice::new(SOUND_TONE, 440 | 6000 << 16).is_none());
        assert!(Voice::new(99, 0).is_none());
    }
    #[test]
    fn sine_is_symmetric() {
        assert_eq!(sine(0), 0);
        assert_eq!(sine(1 << 30), 32767);
        assert_eq!(sine(3 << 30), -32767);
    }
}
