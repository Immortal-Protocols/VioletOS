#!/usr/bin/env bash
# Host tests for verified boot: the loader's check (verified.rs) against the
# real BearSSL built for the host and manifests signed through openssl, the
# manifest tool and the bootloader model's rollback floor. Run by
# `make test-unit`; a few seconds. Everything is written under
# build/tests/bootchain, so it runs beside any other test.
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
src=build/sel4/bearssl-0.6
out=build/tests/bootchain
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
python3 tools/test_boot_manifest.py
rm -rf "$out/fixtures"
python3 tools/test_boot_manifest.py --fixtures "$out/fixtures"
VIOLET_PACKAGE_KEYS_RS="$PWD/$out/fixtures/package_keys.rs" "$RUSTC" --edition=2021 --test -O \
  boot/loader/test_verified.rs -o "$out/verified_test" -L "$out" -l static=packagecrypto
BOOT_FIXTURES="$out/fixtures" "$out/verified_test"
