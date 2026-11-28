#!/bin/sh
# Host tests for the AArch64 port and the board descriptions (make test-unit):
# seconds, no kernel build, no emulator. Each binary and scratch file goes
# under build/tests/aarch64, so this can run beside any other test.
set -eu
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
OUT=build/tests/aarch64
mkdir -p "$OUT"
# The architecture module: both ABI tables, against a model of each kernel's page tables.
"$RUSTC" --edition=2021 --test -O -A dead_code runtime/arch/tests/host.rs -o "$OUT/arch_host"
"$OUT/arch_host" -q
# The 64-bit bootstrap task's memory planning and Linux calls, against fakes.
"$RUSTC" --edition=2021 --test -O -A dead_code targets/qemu64/tests/host.rs -o "$OUT/qemu64_host"
"$OUT/qemu64_host" -q
# The Linux call numbers are what the kernel headers say (skipped without the NDK).
python3 runtime/arch/gen_linux.py --check
# Board descriptions: valid, and in step with the code that still repeats their facts.
python3 targets/board/test_board.py
python3 targets/board/board.py check
# The AArch64 kernel's differences from upstream, and the image layout.
python3 kernel/sel4/aarch64/test_delta64.py
python3 targets/qemu64/test_image.py
python3 image/packages/test_arch.py
