// SHA-256 and P-256 ECDSA verification for the Sentinel, from the same pinned
// BearSSL the supervisor uses for signed catalogs (system/supervisor/supervisor.c).
#include <stddef.h>
#include <stdint.h>
#include "bearssl.h"

void sentinel_sha256(const uint8_t *data, size_t len, uint8_t out[32]) {
  br_sha256_context c;
  br_sha256_init(&c);
  br_sha256_update(&c, data, len);
  br_sha256_out(&c, out);
}

// 1 when `sig` (ASN.1 DER) is a valid signature by the uncompressed P-256
// point `key` over the SHA-256 digest `hash`.
int sentinel_p256_verify(const uint8_t key[65], const uint8_t hash[32], const uint8_t *sig, size_t sig_len) {
  br_ec_public_key pk = {BR_EC_secp256r1, (unsigned char *)key, 65};
  return br_ecdsa_i31_vrfy_asn1(&br_ec_prime_i31, hash, 32, &pk, sig, sig_len) == 1;
}
