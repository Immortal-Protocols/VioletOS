//! Which core an app's threads run on, and when the serve loop must step
//! aside for the services. Plain functions (no kernel calls): the host tests
//! below run them (`make test-unit`).
//!
//! Core 0 holds the services and the Open Runtime, core 1 the app in front,
//! and background apps share every core from 2 up. With one or two cores a
//! tier's core folds onto the ones that exist (sys.rs `tcb_schedule`).

/// The most cores the Open Runtime places threads on. The supervisor hands
/// it a SchedControl capability per core up to this many (supervisor.c).
pub const MAX: u32 = 8;

/// The core for background app `app` when the kernel runs `cores` cores:
/// cores 2 and up in turn, so two background apps share a core only when
/// there are more apps than cores for them. (With four cores this is the
/// earlier rule, 2 or 3 by the app's low bit.)
pub fn background(app: usize, cores: u32) -> u32 {
    let spare = cores.clamp(1, MAX).saturating_sub(2);
    if spare == 0 {
        // One or two cores: the tier's nominal core, folded by the caller.
        return 2 + (app as u32 & 1);
    }
    2 + app as u32 % spare
}

/// How long the serve loop may run without a pause before it lets the
/// services' priority run once (4 ms: half a frame).
pub const RELIEF_NS: u64 = 4_000_000;

/// Whether the serve loop, busy since `busy_since` without sleeping, must
/// step down to the services' priority for a moment at `now`. Only with
/// more than one core: there an app on another core can keep this thread
/// (above the services, on their core) runnable without end; with one core
/// the app must itself be scheduled to call, and the services run then.
pub fn relief_due(cores: u32, busy_since: u64, now: u64) -> bool {
    cores > 1 && busy_since != 0 && now.wrapping_sub(busy_since) >= RELIEF_NS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_cores_keep_the_earlier_placement() {
        for app in 0..12 {
            assert_eq!(background(app, 4), 2 + (app as u32 & 1));
        }
    }

    #[test]
    fn background_apps_never_take_the_services_or_front_core() {
        for cores in 3..=MAX {
            for app in 0..12 {
                let c = background(app, cores);
                assert!((2..cores).contains(&c), "{} cores, app {}: core {}", cores, app, c);
            }
        }
    }

    #[test]
    fn eight_cores_spread_six_background_apps_one_per_core() {
        let mut seen = [0u32; 8];
        for app in 0..6 {
            seen[background(app, 8) as usize] += 1;
        }
        assert_eq!(seen, [0, 0, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn few_cores_fold_as_before() {
        // tcb_schedule takes the tier's core modulo the cores there are.
        for app in 0..4 {
            assert_eq!(background(app, 1) % 1, 0);
            assert_eq!(background(app, 2) % 2, app as u32 & 1);
        }
        // More cores than MAX are not used.
        assert!(background(7, 64) < MAX);
    }

    #[test]
    fn relief_only_with_more_cores_and_only_after_a_busy_stretch() {
        let t = 1_000_000_000;
        assert!(!relief_due(1, t, t + 10 * RELIEF_NS), "one core never needs it");
        assert!(!relief_due(4, 0, t), "not busy");
        assert!(!relief_due(4, t, t + RELIEF_NS - 1));
        assert!(relief_due(4, t, t + RELIEF_NS));
        assert!(relief_due(8, t, t + 3 * RELIEF_NS));
    }
}
