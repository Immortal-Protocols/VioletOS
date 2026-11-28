#!/usr/bin/env bash
# Violet Shield's host tests that need nothing but rustc and python3 (seconds;
# part of `make test-unit`): the policy (built-in rules, rate limits,
# quarantine), the on-device scorer against its Python reference, and the
# replay of the recorded traces in targets/qemu/shield-traces.
#
#   targets/qemu/test_shield_host.sh             run the tests
#   targets/qemu/test_shield_host.sh replay ...  build and run shield-replay
#                                                (make shield-fp-report)
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
out=build/tests/shield; mkdir -p "$out"
mods='extern crate alloc;
#[path = "../../../runtime/open/policy.rs"] mod policy;
#[path = "../../../runtime/open/shield.rs"] mod shield;
#[path = "../../../runtime/open/scorer_model.rs"] mod scorer_model;
#[path = "../../../runtime/open/scorer.rs"] mod scorer;
#[path = "../../../runtime/open/scorer_replay.rs"] mod scorer_replay;'
if [[ "${1:-}" == replay ]]; then
  shift
  printf '%s\nfn main() { scorer_replay::main() }\n' "$mods" > "$out/replay.rs"
  "$RUSTC" --edition=2021 -O -A dead_code -A unused "$out/replay.rs" -o "$out/shield-replay"
  exec "$out/shield-replay" "$@"
fi
printf '#![no_std]\nextern crate std;\n%s\n' "$mods" > "$out/host_test.rs"
"$RUSTC" --edition=2021 --test -O -A dead_code "$out/host_test.rs" -o "$out/host_test"
"$out/host_test"
python3 tools/shield-trainer/test_tiny.py
