#!/usr/bin/env bash
# Violet Shield's data pipeline (docs/shield-data.md): the scrubbing and
# corpus tools, then the false-positive budget over the real recorded traces
# through the real policy and scorer. Seconds; part of `make test-unit`.
# Works in its own directory under build/tests/frontier.
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
out=build/tests/frontier/traces; mkdir -p "$out"
python3 tools/shield-trainer/test_collect.py
# The Shield's modules as targets/qemu/test_shield_host.sh lists them, plus the traces' tests.
{
  printf '#![no_std]\nextern crate std;\nextern crate alloc;\n'
  grep -o '#\[path = "[^"]*"\] mod [a-z_]*;' targets/qemu/test_shield_host.sh | sed 's|"\.\./\.\./\.\./|"../../../../|'
  echo '#[path = "../../../../tools/shield-trainer/test_traces.rs"] mod real_traces;'
} > "$out/traces_test.rs"
"$RUSTC" --edition=2021 --test -O -A dead_code "$out/traces_test.rs" -o "$out/traces_test"
"$out/traces_test" real_traces
