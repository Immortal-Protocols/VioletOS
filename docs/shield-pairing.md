# Violet Shield: companion pairing (VSP1)

How the phone comes to trust the key that signs Shield commands. This page is the specification. The phone's side is [pairing.rs](../services/sentinel/pairing.rs); the companion's side and a second implementation of the phone's checks are in [pairing.py](../tools/shield-trainer/pairing.py).

## Status

- **Specified and implemented as a tested module. Not yet used by the phone.** The Sentinel still trusts the development key the image build generates. See [What is not built](#what-is-not-built).
- **The companion's key is a software key.** It stands in for a key held in a Secure Enclave with an attestation the phone can check. Nothing in VSP1 can tell where a private key lives, so every key enrolled today is recorded as software and can only sign automatic-tier commands. A claim of hardware attestation is refused.

## What it protects against

| Threat | Answer |
|---|---|
| An app or a network attacker enrolls its own key | Enrolment needs the one-time secret the phone shows on the trusted screen, after the user starts pairing |
| Someone who saw the secret enrolls a key they do not hold | The message is signed by the key being enrolled |
| A fake phone answers the companion | The confirmation needs the secret |
| An old signed command is replayed after a restart | The last accepted sequence number is stored with the key |
| Guessing the secret | 16 random bytes; three wrong tries or two minutes end the attempt |

It does not protect against a compromised Open Runtime or anyone who can write the stored record: see [Limits](#limits).

## The exchange

```
phone                                               companion
  user starts pairing
  OFFER  (shown as a QR code) ───────────────────▶  scans it
                                ◀──── ENROLL (over the Sentinel link)
  checks; stores the key
  CONFIRM ───────────────────────────────────────▶  checks; shows the fingerprint
```

Integers are little endian. `HMAC` is HMAC-SHA-256 keyed with the secret. Signatures are ECDSA P-256 over SHA-256, ASN.1 DER.

**OFFER**, 38 bytes, shown by the phone and never sent over the link:

| Bytes | Field |
|---|---|
| 4 | `VSP1` |
| 2 | version, 1 |
| 16 | phone nonce |
| 16 | secret |

**ENROLL**, companion to phone:

| Bytes | Field |
|---|---|
| 4 | `VSPE` |
| 2 | version, 1 |
| 1 | key kind: 1 software, 2 hardware with attestation |
| 1 | tier: 1 automatic, 2 owner |
| 16 | companion nonce |
| 65 | public key, uncompressed P-256 point |
| 2 | attestation length (0 for a software key) |
| 32 | `HMAC("VSP1 enroll" ‖ phone nonce ‖ everything above)` |
| 2 | signature length |
| ≤ 72 | signature by the enrolled key over `SHA-256("VSP1 possession" ‖ phone nonce ‖ everything above the signature length)` |

**CONFIRM**, 47 bytes, phone to companion:

| Bytes | Field |
|---|---|
| 4 | `VSPC` |
| 2 | version, 1 |
| 1 | status, 1 |
| 8 | fingerprint: first 8 bytes of SHA-256 of the public key |
| 32 | `HMAC("VSP1 confirm" ‖ phone nonce ‖ companion nonce ‖ status ‖ SHA-256(public key))` |

## The phone's rules

1. Pairing is armed only by the user. Arming takes 32 fresh random bytes, lasts 120 seconds and allows 3 failed tries.
2. An ENROLL is checked in this order: armed, well formed, version, kind and attestation, tier, MAC, signature.
3. A wrong MAC or signature uses up a try. Malformed messages do not, so garbage cannot end a pairing.
4. Kind 2 or any attestation bytes: refused (`AttestationUnsupported`).
5. Owner tier: refused (`TierNotAllowed`). It needs an attested key.
6. Success ends the pairing. The same message again is refused.
7. The secret and nonce are wiped when pairing ends for any reason.

## The stored key

81 bytes and a check:

| Bytes | Field |
|---|---|
| 4 | `VSPK` |
| 2 | version, 1 |
| 1 | kind |
| 1 | tier |
| 65 | public key |
| 8 | last accepted `VSH1` sequence number |
| 32 | SHA-256 of the bytes above |

A record that is not exactly this, or that claims an attested kind or owner tier, is treated as no record: the phone is unpaired. `Enrolled::allows` refuses owner tier for a software key whatever the record says.

## Limits

- **The check on the record is not a seal.** It catches torn writes. Whoever can write the file can write a valid record with their own key. The record needs sealed storage, which the phone does not have for it yet.
- **No hardware root on the phone.** The enrolled key and the sequence number live in ordinary storage, and the code that checks them runs in the process it protects.
- **Trust on first use.** Pairing is as strong as the moment the QR code is scanned. Anyone who can see the phone's screen during those two minutes can enrol.
- **A software key can be copied.** `pairing.py keygen` writes it to a file readable by its owner.
- **Secure Enclave attestation.** Apple's general attestation for app-held keys (App Attest) is verified through Apple's servers. A phone that checks one offline needs Apple's root, the certificate chain format and a policy for what the attested app must be. None of that is here.

## What is not built

- The Sentinel does not call `pairing.rs`. `Verifier` still holds one key set at boot from the image.
- No control in Settings starts pairing, and no trusted screen shows the OFFER.
- The Sentinel link has no ENROLL or CONFIRM frame type.
- The stored record has no file, and `last_seq` is not saved.
- The Mac app does not speak VSP1. Its signer still reads the development key file. `pairing.py` is the only companion that does.
- Unpairing.

## Tests

`make test-unit` runs [test_pairing.sh](../tools/shield-trainer/test_pairing.sh):

- `pairing.py selftest`: its ECDSA against RFC 6979 A.2.5, and eight enrolment cases through the Python phone.
- [test_pairing.rs](../tools/shield-trainer/test_pairing.rs): the Rust phone with the Sentinel's real SHA-256 and P-256 verifier (BearSSL, [crypto.c](../services/sentinel/crypto.c)) against the same eight messages. It must give the same answer for each, the same CONFIRM bytes and the same stored record. It also covers arming, expiry, the try limit, replay and every single-byte change to a stored record.
