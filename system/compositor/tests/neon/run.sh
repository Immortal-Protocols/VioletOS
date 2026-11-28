#!/bin/sh
# Build main.rs as the Display package is built and run it on an emulated Arm
# core. Needs HOST_RUSTC (with the armv7a-none-eabi target), ld.lld (LLD) and
# qemu-system-arm. Output goes to build/tests/display/neon.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
out="$repo/build/tests/display/neon"
mkdir -p "$out"
"${HOST_RUSTC:-rustc}" --edition=2021 --crate-type=staticlib --target=armv7a-none-eabi \
  -Cpanic=abort -Copt-level=s -Ccodegen-units=1 "$here/main.rs" -o "$out/neon.a"
"${LLD:-ld.lld}" --gc-sections -T "$here/link.ld" "$out/neon.a" -o "$out/neon.elf"
exec qemu-system-arm -M virt -cpu cortex-a15 -m 64 -display none -serial none -monitor none \
  -semihosting-config enable=on,target=native -kernel "$out/neon.elf"
