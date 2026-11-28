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
# The Open Runtime type-checks for AArch64 as well as AArch32 (seconds, no
# code generation; skipped when this rustc lacks the target). services/
# check_aarch64.py --open does the full fat-LTO build of the same crate.
SYSROOT=$("$RUSTC" --print sysroot)
if [ -d "$SYSROOT/lib/rustlib/aarch64-unknown-none" ] && [ -d "$SYSROOT/lib/rustlib/armv7a-none-eabi" ]; then
  OPEN="$OUT/open-typecheck"
  mkdir -p "$OPEN"
  REPO=$(pwd)
  printf 'const ALPHABET:&[u8]=b"";\nconst GLYPHS:&[[u8;5]]=&[];\n' > "$OPEN/glyphs.rs"
  printf 'pub const COMPANION_KEY: [u8; 65] = [4; 65];\n' > "$OPEN/companion_key.rs"
  {
    printf '#![no_std]\n#![allow(dead_code)]\nextern crate alloc;\n'
    printf '#[path="%s/runtime/protocol.rs"] mod protocol;\n' "$REPO"
    printf '#[path="%s/runtime/open/mod.rs"] mod process;\n' "$REPO"
    printf '#[path="%s/companion_key.rs"] mod companion_key;\n' "$REPO/$OPEN"
    printf '#[panic_handler] fn panic(_: &core::panic::PanicInfo)->!{loop{}}\n'
    printf '#[no_mangle] pub extern "C" fn component_main(identity:u32){let _=identity;process::run()}\n'
  } > "$OPEN/open.rs"
  for target in armv7a-none-eabi aarch64-unknown-none; do
    VIOLET_GLYPHS_RS="$REPO/$OPEN/glyphs.rs" "$RUSTC" --edition=2021 --crate-type=lib --emit=metadata \
      --target=$target -Cpanic=abort "$OPEN/open.rs" -o "$OPEN/open-$target.rmeta"
  done
else
  echo "runtime/arch: skipping the Open Runtime AArch64 type-check (no aarch64-unknown-none or armv7a-none-eabi in $SYSROOT)"
fi
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
