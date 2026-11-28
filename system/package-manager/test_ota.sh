#!/usr/bin/env bash
# Host tests for full-system updates: the supervisor's manifest check
# (ota.rs) against the real BearSSL built for the host and manifests signed
# through openssl, the bundle and QEMU bootloader tools, the slot metadata
# codec (targets/moto-g-pure/slot.rs), Storage's slot writer and the update
# staging path policy. Run by
# `make test-unit`; a few seconds (BearSSL is compiled once).
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
src=build/sel4/bearssl-0.6
out=build/tests/ota
[[ -d "$src/src" ]] || { echo "BearSSL sources are missing: run make qemu-image (or make setup) first" >&2; exit 1; }
mkdir -p "$out/bearssl"
if [[ ! -f "$out/libpackagecrypto.a" || system/package-manager/verify_crypto.c -nt "$out/libpackagecrypto.a" ]]; then
  root=$PWD
  (cd "$out/bearssl" && cc -O2 -I "$root/$src/inc" -I "$root/$src/src" -c \
    "$root/$src"/src/int/i31_*.c "$root/$src"/src/int/i32_div32.c "$root/$src"/src/codec/*.c \
    "$root/$src"/src/hash/sha2small.c "$root/$src"/src/ec/ecdsa_i31_vrfy_raw.c \
    "$root/$src"/src/ec/ecdsa_i31_bits.c "$root/$src"/src/ec/ec_p256_m31.c "$root/$src"/src/ec/ec_secp*.c \
    "$root/system/package-manager/verify_crypto.c")
  rm -f "$out/libpackagecrypto.a"
  ar rcs "$out/libpackagecrypto.a" "$out"/bearssl/*.o
fi
python3 tools/test_ota_bundle.py
rm -rf "$out/fixtures"
python3 tools/test_ota_bundle.py --fixtures "$out/fixtures"
VIOLET_PACKAGE_KEYS_RS="$PWD/$out/fixtures/package_keys.rs" "$RUSTC" --edition=2021 --test -O \
  system/package-manager/test_ota.rs -o "$out/manifest_test" -L "$out" -l static=packagecrypto
OTA_FIXTURES="$out/fixtures" "$out/manifest_test"
"$RUSTC" --edition=2021 --test -O targets/moto-g-pure/test_slot.rs -o "$out/slot_test"
"$out/slot_test"
"$RUSTC" --edition=2021 --test -O services/storage/tests/slotwrite_host.rs -o "$out/slotwrite_test"
"$out/slotwrite_test"
# Where Settings and the Agent stage update pieces: theirs alone (runtime/open/update.rs).
"$RUSTC" --edition=2021 --test -O runtime/open/tests/ota_staging_host.rs -o "$out/staging_test"
"$out/staging_test"
