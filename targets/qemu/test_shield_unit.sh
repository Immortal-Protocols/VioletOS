#!/usr/bin/env bash
# Host tests for Violet Shield's policy and the Sentinel's VSH1 verifier, with
# the real BearSSL built for the Mac and envelopes signed by tools/shield-trainer/vsh1.py.
set -euo pipefail
cd "$(dirname "$0")/../.."
PY="${SHIELD_PYTHON:-build/shield/venv/bin/python}"
RUSTC="${HOST_RUSTC:-rustc}"
out=build/tests/shield; mkdir -p "$out/bearssl"
src=build/sel4/bearssl-0.6
if [[ ! -f "$out/libbearssl.a" ]]; then
  for f in "$src"/src/*/*.c "$src"/src/*.c; do
    [[ -f "$f" ]] || continue
    cc -O2 -I "$src/inc" -I "$src/src" -c "$f" -o "$out/bearssl/$(basename "${f%.c}").o" &
  done; wait
  ar rcs "$out/libbearssl.a" "$out"/bearssl/*.o
fi
cc -O2 -I "$src/inc" -c services/sentinel/crypto.c -o "$out/crypto.o"
ar rcs "$out/libsentinelcrypto.a" "$out/crypto.o"
key="$out/test-key.pem"
[[ -f "$key" ]] || openssl ecparam -name prime256v1 -genkey -noout -out "$key"
"$PY" tools/shield-trainer/vsh1.py fixtures "$key" "$out/fixtures.txt"
cat > "$out/shield_test.rs" <<'RS'
#![no_std]
extern crate alloc;
extern crate std;
#[path = "../../../runtime/open/policy.rs"] mod policy;
#[path = "../../../runtime/open/shield.rs"] mod shield;
#[path = "../../../services/sentinel/mod.rs"] mod sentinel;
RS
"$RUSTC" --edition=2021 --test -O -A dead_code "$out/shield_test.rs" -o "$out/shield_test" \
  -L "$out" -l static=sentinelcrypto -l static=bearssl
VSH1_FIXTURES="$out/fixtures.txt" "$out/shield_test"
