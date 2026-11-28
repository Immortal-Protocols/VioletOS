# Recorded Sentinel traces

Record lines as the Sentinel sends them (`t_ms pid app sha8 kind flags idle_s detail target`,
tab separated), one process per file. `benign/` is the false-positive corpus; `attack/` holds
the demo's two bad apps. `make test-unit` replays all of them through the real policy and
scorer (runtime/open/scorer_replay.rs) and `make shield-fp-report` prints the result.

Where they come from: `tools/shield-trainer/traces_from_status.py` rebuilt them from the
windows the companion scored in earlier runs of the Shield demo, in QEMU (`qemu-*`) and on a
Moto G Pure (`moto-*`). Events, order, relative times, paths, hosts and byte counts are as
recorded. Per-event background flags, send targets and pids are reconstructed, and records a
window never showed (EXIT, SHIELD_HIT) are missing; the themes traces stop before its write
to /apps/home because the old demo froze it there. All hosts are the demo's `.test` stand-ins
or wttr.in; nothing here came from personal use.

`qemu-enforce-*.log` are different: the complete records (EXIT included, flags as the phone
set them) of one emulator run of the shipped image under `make test-shield-enforce`, split per
process by `tools/shield-trainer/collect.py ingest --source qemu`. Home's trace still carries
the cross-app mark the phone used to put on its reads of `/apps/.home`; the replay sets flags
again through today's policy, which no longer does. `tools/shield-trainer/test_traces.rs`
holds these four to a budget (docs/shield-data.md).

This is a small regression corpus (6 benign processes), not a measurement: no false positive
in 6 still allows a true rate up to 39% at 95% confidence. Add complete traces from real runs
(`build/shield/qemu-traces/enforce-last.log` after `make test-shield-enforce`, or
`companion_sim.py --record`, converted to tab-separated lines) before quoting a rate.
