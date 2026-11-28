#!/usr/bin/env bash
# Host tests for the 802.1X supplicant core (targets/moto-g-pure/eap.rs).
#
#   targets/moto-g-pure/test_eap.sh          the standards' test vectors and the
#                                            state machine over a stand-in TLS
#                                            (seconds; part of `make test-unit`)
#   targets/moto-g-pure/test_eap.sh hostap   EAP-TLS and PEAP/MSCHAPv2 end to end:
#                                            eap.rs on BearSSL against hostap's
#                                            own EAP server on OpenSSL (local;
#                                            needs build/sel4 from an image build
#                                            and Homebrew's openssl@3 or
#                                            OPENSSL_PREFIX)
#
# Every run works in its own directory under build/tests/frontier, so runs do
# not disturb each other.
set -euo pipefail
cd "$(dirname "$0")/../.."
RUSTC="${HOST_RUSTC:-rustc}"
out="build/tests/frontier/eap"; mkdir -p "$out"
if [[ "${1:-}" != hostap ]]; then
  "$RUSTC" --edition=2021 --test -O targets/moto-g-pure/test_eap.rs -o "$out/eap"
  exec "$out/eap"
fi

HOSTAP="${HOSTAP_SRC:-build/sel4/wpa_supplicant-2.11}/src"
BEARSSL="${BEARSSL_SRC:-build/sel4/bearssl-0.6}"
OPENSSL_PREFIX="${OPENSSL_PREFIX:-$(brew --prefix openssl@3 2>/dev/null || echo /opt/homebrew/opt/openssl@3)}"
OPENSSL="$OPENSSL_PREFIX/bin/openssl"
[[ -f "$HOSTAP/eap_server/eap_server.c" && -f "$BEARSSL/inc/bearssl.h" ]] || { echo "test_eap.sh: build an image first (hostap and BearSSL sources are in build/sel4)" >&2; exit 2; }
ref="$out/hostap"; mkdir -p "$ref/obj" "$ref/bearssl"

# hostap's EAP server and what it stands on.
flags=(-O1 -w -DCONFIG_NO_STDOUT_DEBUG -DCONFIG_NO_RANDOM_POOL -DEAP_SERVER -DEAP_SERVER_IDENTITY -DEAP_SERVER_TLS -DEAP_SERVER_PEAP
       -DEAP_SERVER_MSCHAPV2 -DEAP_TLS_FUNCS -DEAP_TLS_OPENSSL -DCONFIG_SHA256 -DCONFIG_ECC -DIEEE8021X_EAPOL
       '-DTLS_DEFAULT_CIPHERS="DEFAULT:!EXP:!LOW"'
       -I "$HOSTAP" -I "$HOSTAP/utils" -I "$OPENSSL_PREFIX/include")
sources=(eap_server/eap_server eap_server/eap_server_methods eap_server/eap_server_identity eap_server/eap_server_tls
         eap_server/eap_server_tls_common eap_server/eap_server_peap eap_server/eap_server_mschapv2
         eap_common/eap_common eap_common/eap_peap_common eap_common/chap
         crypto/tls_openssl crypto/crypto_openssl crypto/ms_funcs crypto/sha1-prf crypto/sha1-tlsprf crypto/sha256-prf
         crypto/sha256-tlsprf crypto/sha256-kdf crypto/dh_groups
         utils/common utils/wpabuf utils/wpa_debug utils/os_unix utils/base64)
for f in "${sources[@]}"; do
  o="$ref/obj/$(basename "$f").o"
  [[ -f "$o" && "$o" -nt "$HOSTAP/$f.c" ]] || cc "${flags[@]}" -c "$HOSTAP/$f.c" -o "$o"
done
cc "${flags[@]}" -Wall -c targets/moto-g-pure/test_eap_hostap.c -o "$ref/obj/harness.o"
rm -f "$ref/libeapref.a"; ar rcs "$ref/libeapref.a" "$ref"/obj/*.o

# BearSSL for the Mac, and the adapter under test.
if [[ ! -f "$ref/libbearssl.a" ]]; then
  for f in "$BEARSSL"/src/*/*.c "$BEARSSL"/src/*.c; do
    [[ -f "$f" ]] || continue
    cc -O2 -I "$BEARSSL/inc" -I "$BEARSSL/src" -c "$f" -o "$ref/bearssl/$(basename "${f%.c}").o" &
  done; wait
  ar rcs "$ref/libbearssl.a" "$ref"/bearssl/*.o
fi
cc -O2 -Wall -Wextra -I "$BEARSSL/inc" -c targets/moto-g-pure/eap_tls_bearssl.c -o "$ref/eap_tls_bearssl.o"
rm -f "$ref/libeaptls.a"; ar rcs "$ref/libeaptls.a" "$ref/eap_tls_bearssl.o"

# A throwaway authority, a server, clients, and a second authority nobody trusts.
pki="$ref/pki"
if [[ ! -f "$pki/done" ]]; then
  rm -rf "$pki"; mkdir -p "$pki"
  q() { "$OPENSSL" "$@" 2>/dev/null; }
  ca() { # name
    q req -x509 -newkey rsa:2048 -nodes -keyout "$pki/$1.key" -out "$pki/$1.pem" -days 30 -subj "/CN=Violet test $1" \
      -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign"
    q x509 -in "$pki/$1.pem" -outform DER -out "$pki/$1.der"
  }
  leaf() { # name ca keyspec subject extensions
    q req -newkey "$3" -nodes -keyout "$pki/$1.key" -out "$pki/$1.csr" -subj "$4"
    printf '%s\n' "$5" > "$pki/$1.ext"
    q x509 -req -in "$pki/$1.csr" -CA "$pki/$2.pem" -CAkey "$pki/$2.key" -CAcreateserial -days 30 -extfile "$pki/$1.ext" -out "$pki/$1.pem"
    q x509 -in "$pki/$1.pem" -outform DER -out "$pki/$1.der"
  }
  ca ca; ca other-ca
  leaf server ca rsa:2048 "/CN=radius.violet.test" $'subjectAltName=DNS:radius.violet.test\nextendedKeyUsage=serverAuth'
  leaf client-rsa ca rsa:2048 "/CN=phone@violet.test" "extendedKeyUsage=clientAuth"
  q ecparam -name prime256v1 -out "$pki/p256.param"
  leaf client-ec ca "ec:$pki/p256.param" "/CN=phone@violet.test" "extendedKeyUsage=clientAuth"
  leaf stranger other-ca "ec:$pki/p256.param" "/CN=phone@violet.test" "extendedKeyUsage=clientAuth"
  q pkey -in "$pki/client-rsa.key" -traditional -outform DER -out "$pki/client-rsa.key.der"
  for k in client-ec stranger; do q ec -in "$pki/$k.key" -outform DER -out "$pki/$k.key.der"; done
  touch "$pki/done"
fi

"$RUSTC" --edition=2021 --test -O --cfg hostap targets/moto-g-pure/test_eap.rs -L "$ref" -l static=eapref -l static=eaptls -l static=bearssl \
  -L "$OPENSSL_PREFIX/lib" -l ssl -l crypto -o "$ref/eap"
EAP_TEST_DIR="$pki" exec "$ref/eap"
