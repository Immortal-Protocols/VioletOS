#!/usr/bin/env bash
# Build and run the host integration tests (make test-integration; seconds).
#
#   tests/integration/run.sh [test-name-filter] [options of the test binary]
#
# One rustc invocation builds tests/integration/main.rs, which pulls the
# shipped sources in by path. It is built twice, in parallel: with the 4 KiB
# pages runtime/open/sys.rs ships, and with `--cfg page64` for the 64 KiB
# configuration procmgr.rs still supports (as runtime/open/tests/paging_host.rs
# does). Both run. The OTA scenarios verify real P-256 signatures:
# BearSSL's verifier is compiled for the host (as
# system/package-manager/test_ota.sh does) whenever any of its sources
# changed, and the signed manifests are made afresh on every run by
# tools/test_ota_bundle.py --fixtures (a tenth of a second), so they can
# never be older than the code that reads them. Everything goes under
# build/tests/integration (INTEGRATION_OUT to change it).
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
out="${INTEGRATION_OUT:-build/tests/integration}"
src=build/sel4/bearssl-0.6
[[ -d "$src/src" ]] || { echo "BearSSL sources are missing: run make qemu-image (or make setup) first" >&2; exit 1; }
mkdir -p "$out/bearssl"
lib="$out/libpackagecrypto.a"
if [[ ! -f "$lib" || -n "$(find system/package-manager/verify_crypto.c "$src/src" "$src/inc" -newer "$lib" -print -quit)" ]]; then
  root=$PWD
  rm -f "$out"/bearssl/*.o
  (cd "$out/bearssl" && cc -O2 -I "$root/$src/inc" -I "$root/$src/src" -c \
    "$root/$src"/src/int/i31_*.c "$root/$src"/src/int/i32_div32.c "$root/$src"/src/codec/*.c \
    "$root/$src"/src/hash/sha2small.c "$root/$src"/src/ec/ecdsa_i31_vrfy_raw.c \
    "$root/$src"/src/ec/ecdsa_i31_bits.c "$root/$src"/src/ec/ec_p256_m31.c "$root/$src"/src/ec/ec_secp*.c \
    "$root/system/package-manager/verify_crypto.c")
  rm -f "$lib"
  ar rcs "$lib" "$out"/bearssl/*.o
fi
rm -rf "$out/fixtures"
python3 tools/test_ota_bundle.py --fixtures "$out/fixtures" >/dev/null
# The supervisor's own update check (verify.rs `ota_verify`, the C entry
# supervisor.c calls) as the image builds it, with a rollback floor of 4:
# not a test build, so its `root` module and the build-time floor are there.
VIOLET_SYSTEM_VERSION=4 VIOLET_PACKAGE_KEYS_RS="$PWD/$out/fixtures/package_keys.rs" "$RUSTC" --edition=2021 -O \
  --crate-type=rlib --crate-name violet_verify system/package-manager/verify.rs -o "$out/libviolet_verify.rlib" \
  -L "$out" -l static=packagecrypto  # its own link to BearSSL, so a linker that minds the order (GNU ld) finds it after the rlib
build() {
  VIOLET_PACKAGE_KEYS_RS="$PWD/$out/fixtures/package_keys.rs" "$RUSTC" --edition=2021 --test -O \
    "$@" tests/integration/main.rs -L "$out" -l static=packagecrypto --extern violet_verify="$out/libviolet_verify.rlib"
}
build -o "$out/integration" &
small=$!
build --cfg page64 -o "$out/integration-page64" &
large=$!
status=0
wait "$small" || status=1
wait "$large" || status=1
[[ $status == 0 ]] || exit 1
echo "--- 4 KiB pages (as shipped) ---"
OTA_FIXTURES="$out/fixtures" "$out/integration" "$@"
echo "--- 64 KiB pages (--cfg page64) ---"
OTA_FIXTURES="$out/fixtures" "$out/integration-page64" "$@"
