#!/usr/bin/env bash
# Host tests for the supervisor's package verifier (verify.rs), with the real
# BearSSL built for the host and packages signed by image/packages/sign.py
# through openssl. Run by `make test-unit`; takes a few seconds the first time.
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
src=build/sel4/bearssl-0.6
out=build/tests/signing
[[ -d "$src/src" ]] || { echo "BearSSL sources are missing: run make qemu-image (or make setup) first" >&2; exit 1; }
mkdir -p "$out/bearssl"
if [[ ! -f "$out/libpackagecrypto.a" || system/package-manager/verify_crypto.c -nt "$out/libpackagecrypto.a" ]]; then
  root=$PWD
  # SHA-256, the P-256 curve and the ECDSA verifier: what verify_crypto.c pulls in.
  (cd "$out/bearssl" && cc -O2 -I "$root/$src/inc" -I "$root/$src/src" -c \
    "$root/$src"/src/int/i31_*.c "$root/$src"/src/int/i32_div32.c "$root/$src"/src/codec/*.c \
    "$root/$src"/src/hash/sha2small.c "$root/$src"/src/ec/ecdsa_i31_vrfy_raw.c \
    "$root/$src"/src/ec/ecdsa_i31_bits.c "$root/$src"/src/ec/ec_p256_m31.c "$root/$src"/src/ec/ec_secp*.c \
    "$root/system/package-manager/verify_crypto.c")
  rm -f "$out/libpackagecrypto.a"
  ar rcs "$out/libpackagecrypto.a" "$out"/bearssl/*.o
fi
rm -rf "$out/fixtures"
python3 image/packages/test_package_format.py --fixtures "$out/fixtures"
VIOLET_PACKAGE_KEYS_RS="$PWD/$out/fixtures/package_keys.rs" "$RUSTC" --edition=2021 --test -O \
  system/package-manager/verify.rs -o "$out/verify_test" -L "$out" -l static=packagecrypto
VPK_FIXTURES="$out/fixtures" "$out/verify_test"
