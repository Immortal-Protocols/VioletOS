//! Host test (`make test-unit`) of the handset's operating points and idle
//! states (power_tables.rs, generated from the factory device tree) against
//! the frequency governor that would use them (services/power/policy.rs).
//! Nothing on the phone applies these tables yet; this checks that they are
//! what the governor requires, so wiring them later starts from sound data.
#![allow(dead_code)]
#[path = "../../services/power/policy.rs"]
mod policy;
use policy::{dvfs_plan, opp_for_load, opps_valid, DvfsStep, Opp};
#[path = "power_tables.rs"]
mod tables;
use tables::{CLUSTER0_OPPS, CLUSTER1_OPPS, IDLE_STATES};

/// MT6357's VPROC buck: 0.51875 V to 1.3125 V in 6.25 mV steps.
const VPROC_UV: core::ops::RangeInclusive<u32> = 518_750..=1_312_500;

#[test]
fn tables_are_what_the_governor_requires() {
    for table in [&CLUSTER0_OPPS[..], &CLUSTER1_OPPS[..]] {
        assert_eq!(table.len(), 16);
        assert!(opps_valid(table));
        for pair in table.windows(2) {
            assert!(pair[0].khz < pair[1].khz && pair[0].uv <= pair[1].uv, "{pair:?}");
        }
        for point in table {
            assert!(VPROC_UV.contains(&point.uv), "{point:?}");
            // On the regulator's grid, within the tree's own rounding (10 uV).
            let off = (point.uv - 518_750) % 6_250;
            assert!(off <= 10 || off >= 6_240, "{point:?}");
        }
    }
    // The figures the tree gives each CPU as its top frequency.
    assert_eq!(CLUSTER0_OPPS[15], Opp { khz: 2_301_000, uv: 1_118_750 });
    assert_eq!(CLUSTER1_OPPS[15], Opp { khz: 1_800_000, uv: 1_118_750 });
    assert_eq!(CLUSTER0_OPPS[0], Opp { khz: 900_000, uv: 600_000 });
}

#[test]
fn every_step_is_safe_in_both_directions() {
    let t = &CLUSTER0_OPPS[..];
    for from in 0..t.len() {
        for to in 0..t.len() {
            let plan = dvfs_plan(t, from, to);
            let steps: Vec<DvfsStep> = plan.into_iter().flatten().collect();
            if from == to {
                assert!(steps.is_empty());
            } else if to > from {
                // Up: the supply rises before the frequency.
                assert_eq!(steps.last(), Some(&DvfsStep::Frequency(t[to].khz)));
                if t[to].uv != t[from].uv {
                    assert_eq!(steps[0], DvfsStep::Voltage(t[to].uv));
                }
            } else {
                // Down: the frequency falls before the supply.
                assert_eq!(steps[0], DvfsStep::Frequency(t[to].khz));
            }
        }
    }
}

#[test]
fn load_picks_a_point_with_headroom() {
    let t = &CLUSTER0_OPPS[..];
    let top = t.len() - 1;
    assert_eq!(opp_for_load(t, 100, top), top);
    assert_eq!(opp_for_load(t, 0, top), 0);
    // A thermal cap is never exceeded.
    assert_eq!(opp_for_load(t, 100, 3), 3);
    // More load never picks a slower point.
    let mut last = 0;
    for load in 0..=100 {
        let p = opp_for_load(t, load, top);
        assert!(p >= last);
        last = p;
    }
}

#[test]
fn idle_states_deepen() {
    assert_eq!(IDLE_STATES.len(), 7);
    assert_eq!(IDLE_STATES[0].name, "STANDBY");
    assert_eq!(IDLE_STATES[6].name, "SUSPEND");
    for pair in IDLE_STATES.windows(2) {
        assert!(pair[0].min_residency_us <= pair[1].min_residency_us);
        assert!(pair[0].exit_us <= pair[1].exit_us);
        assert_ne!(pair[0].psci_param, pair[1].psci_param);
    }
    for s in &IDLE_STATES {
        assert!(s.min_residency_us >= s.entry_us + s.exit_us, "{}", s.name);
    }
}
