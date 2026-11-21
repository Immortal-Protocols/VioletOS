/// Fixed C ABI for the renderer. Time is in tenths, with a partial interval
/// carried between calls so pausing never rounds away elapsed ticks.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Watch {
    pub tenths: u32,
    remainder: u32,
    pub running: bool,
}

#[no_mangle]
pub extern "C" fn watch_toggle(w: &mut Watch) { w.running = !w.running; }

#[no_mangle]
pub extern "C" fn watch_reset(w: &mut Watch) { *w = Watch::default(); }

#[no_mangle]
pub extern "C" fn watch_advance(w: &mut Watch, delta: u32, period: u32) -> bool {
    if !w.running || period == 0 { return false; }
    let before = w.tenths;
    let whole = delta / period;
    let fraction = delta % period;
    let carry = fraction >= period - w.remainder;
    w.remainder = if carry { fraction - (period - w.remainder) } else { fraction + w.remainder };
    w.tenths = w.tenths.wrapping_add(whole).wrapping_add(carry as u32);
    before != w.tenths
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_action_sequences_match_tick_model() {
        for mut actions in 0..4u32.pow(7) {
            let (mut watch, mut running, mut ticks) = (Watch::default(), false, 0);
            for _ in 0..7 {
                match actions % 4 {
                    0 => { watch_toggle(&mut watch); running = !running; }
                    1 => { watch_reset(&mut watch); running = false; ticks = 0; }
                    n => { watch_advance(&mut watch, n, 5); if running { ticks += n; } }
                }
                assert_eq!((watch.running, watch.tenths, watch.remainder), (running, ticks / 5, ticks % 5));
                actions /= 4;
            }
        }
    }
}
