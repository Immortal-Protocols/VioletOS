"""Host test of tools/power_tables.py: the device-tree text it must read."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import power_tables

IDLE = "".join(f"""
        {label}: s{i} {{
            compatible = "arm,idle-state";
            arm,psci-suspend-param = <{i + 1:#x}>;
            entry-latency-us = <0x258>;
            exit-latency-us = <0x258>;
            min-residency-us = <0x4b0>;
        }};""" for i, label in enumerate(power_tables.IDLE))

TREE = """
/ {
    cluster0_opp: opp_table0 {
        compatible = "operating-points-v2";
        opp1 { opp-hz = <0x00 0x684ee180>; opp-microvolt = "", "\\f5"; };
        opp0 { opp-hz = <0x00 0x35a4e900>; opp-microvolt = <0x927c0>; };
    };
    cluster1_opp: opp_table1 {
        opp0 { opp-hz = <0x00 0x17d78400>; opp-microvolt = <0x927c0>; };
    };
    idle-states {""" + IDLE + """
    };
};
"""


class Tables(unittest.TestCase):
    def test_cells(self):
        self.assertEqual(power_tables.cells("<0x00 0x35a4e900>"), [0, 900_000_000])
        # dtc prints a cell whose bytes are printable as strings: 00 0c 35 00.
        self.assertEqual(power_tables.cells('"", "\\f5"'), [800_000])
        self.assertEqual(power_tables.cells("[00 0c 35 00]"), [800_000])
        with self.assertRaises(ValueError):
            power_tables.cells('"ab"')

    def test_points_sorted_with_string_cell(self):
        self.assertEqual(power_tables.opps(TREE, "cluster0_opp"), [(900_000, 600_000), (1_750_000, 800_000)])

    def test_refuses_a_table_that_does_not_rise(self):
        bad = TREE.replace("<0x927c0>; };\n    };\n    cluster1", "<0xf4240>; };\n    };\n    cluster1")
        with self.assertRaises(ValueError):
            power_tables.opps(bad, "cluster0_opp")
        with self.assertRaises(ValueError):
            power_tables.opps(TREE, "cci_opp")

    def test_render(self):
        text = power_tables.render(TREE)
        self.assertIn("pub static CLUSTER0_OPPS: [Opp; 2] = [", text)
        self.assertIn("Opp { khz: 1750000, uv: 800000 },", text)
        self.assertIn('IdleState { name: "SUSPEND", psci_param: 0x7, entry_us: 600, exit_us: 600, min_residency_us: 1200 },', text)

    def test_checked_in_table_is_this_generator_s_output(self):
        # With an image build's factory tree at hand, the checked-in file must match it.
        root = Path(__file__).resolve().parents[1]
        tree = root / "build/sel4/moto/upstream/src/plat/ellis/factory-base.dts"
        if not tree.exists():
            self.skipTest("no factory device tree (needs a handset image build)")
        self.assertEqual(power_tables.render(tree.read_text()), (root / "targets/moto-g-pure/power_tables.rs").read_text())


if __name__ == "__main__":
    unittest.main()
