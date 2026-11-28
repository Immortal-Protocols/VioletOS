#!/usr/bin/env bash
# VSP1 companion pairing (docs/shield-pairing.md): the reference's self-test,
# then the phone's side (services/sentinel/pairing.rs) with the Sentinel's
# real BearSSL verifier against the reference's messages. Seconds; part of
# `make test-unit`. Works in its own directory under build/tests/frontier.
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
src=build/sel4/bearssl-0.6
out=build/tests/frontier/pairing
[[ -d "$src/src" ]] || { echo "BearSSL sources are missing: run make qemu-image (or make setup) first" >&2; exit 1; }
python3 tools/shield-trainer/pairing.py selftest
mkdir -p "$out/obj"
if [[ ! -f "$out/libpairingcrypto.a" || services/sentinel/crypto.c -nt "$out/libpairingcrypto.a" ]]; then
  root=$PWD
  # SHA-256, P-256 and the ASN.1 ECDSA verifier: what services/sentinel/crypto.c pulls in.
  (cd "$out/obj" && cc -O2 -I "$root/$src/inc" -I "$root/$src/src" -c \
    "$root/$src"/src/int/i31_*.c "$root/$src"/src/int/i32_div32.c "$root/$src"/src/codec/*.c \
    "$root/$src"/src/hash/sha2small.c "$root/$src"/src/ec/ecdsa_i31_vrfy_asn1.c "$root/$src"/src/ec/ecdsa_i31_vrfy_raw.c \
    "$root/$src"/src/ec/ecdsa_i31_bits.c "$root/$src"/src/ec/ecdsa_atr.c "$root/$src"/src/ec/ec_prime_i31.c \
    "$root/$src"/src/ec/ec_secp*.c "$root/services/sentinel/crypto.c")
  rm -f "$out/libpairingcrypto.a"
  ar rcs "$out/libpairingcrypto.a" "$out"/obj/*.o
fi
python3 tools/shield-trainer/pairing.py fixtures "$out/vectors.txt"
"$RUSTC" --edition=2021 --test -O tools/shield-trainer/test_pairing.rs -o "$out/pairing_test" -L "$out" -l static=pairingcrypto
VSP1_FIXTURES="$out/vectors.txt" "$out/pairing_test"
