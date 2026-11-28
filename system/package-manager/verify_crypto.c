// SHA-256 and P-256 ECDSA verification for package signatures (verify.rs), from
// the pinned BearSSL that the Sentinel and the Radio's TLS also use.
#include <stddef.h>
#include <stdint.h>
#include "bearssl.h"

void package_sha256(const uint8_t *data, size_t len, uint8_t out[32]) {
  br_sha256_context c;
  br_sha256_init(&c);
  br_sha256_update(&c, data, len);
  br_sha256_out(&c, out);
}

// 1 when `signature` (r | s, 32 bytes each) is a valid signature by the
// uncompressed P-256 point `key` over the SHA-256 digest `hash`.
int package_p256_verify(const uint8_t key[65], const uint8_t hash[32], const uint8_t signature[64]) {
  br_ec_public_key pk = {BR_EC_secp256r1, (unsigned char *)key, 65};
  return br_ecdsa_i31_vrfy_raw(&br_ec_p256_m31, hash, 32, &pk, signature, 64) == 1;
}
