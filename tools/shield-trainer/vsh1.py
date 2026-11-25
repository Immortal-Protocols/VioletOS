"""Build and sign `VSH1` Shield envelopes (services/sentinel/mod.rs).

Header, little endian, 32 bytes:
    magic "VSH1" | version u16 = 1 | tier u8 | op u8 | shield_id u32 | seq u64 |
    ttl_secs u32 | target_pid u32 | payload_len u16 | sig_len u16
then the payload, then an ASN.1 DER ECDSA P-256 / SHA-256 signature over the
header and payload. The companion in companion/mac/VioletAgent/Shield/ShieldSigner.swift
writes the same bytes.

    python3 vsh1.py fixtures KEY.pem OUT.txt   # test vectors for the Rust tests
"""
from __future__ import annotations

import struct
import sys
from pathlib import Path

TIER_AUTO, TIER_OWNER = 1, 2
OP_INSTALL, OP_REVOKE, OP_UNFREEZE, OP_KILL, OP_FREEZE = 1, 2, 3, 4, 5
OP_WARNING = 6
MATCH_PATH, MATCH_HOST, MATCH_WX, MATCH_HASH = 1, 2, 3, 4
VERDICT_LOG, VERDICT_DENY, VERDICT_FREEZE, VERDICT_KILL = 1, 2, 3, 4


def rule_payload(matcher: int, verdict: int, text: str = "", write_only: bool = False, cross_app_only: bool = False) -> bytes:
    raw = text.encode()
    return bytes([matcher, verdict, (1 if write_only else 0) | (2 if cross_app_only else 0), len(raw)]) + raw


# Pushed by the companion as soon as the Sentinel links (and renewed before they expire).
BASELINE = [
    (MATCH_PATH, VERDICT_FREEZE, "/apps/", True, True),
    (MATCH_PATH, VERDICT_FREEZE, "/bin/", True, False),
    (MATCH_PATH, VERDICT_FREEZE, "/lib/", True, False),
]


def load_key(path):
    from cryptography.hazmat.primitives.serialization import load_pem_private_key

    return load_pem_private_key(Path(path).read_bytes(), password=None)


def public_point(key) -> bytes:
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

    return key.public_key().public_bytes(Encoding.X962, PublicFormat.UncompressedPoint)


def envelope(key, op: int, shield_id: int, seq: int, ttl: int, pid: int = 0,
             payload: bytes = b"", tier: int = TIER_AUTO, corrupt: bool = False) -> bytes:
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec

    head = b"VSH1" + struct.pack("<HBBIQIIH", 1, tier, op, shield_id, seq, ttl, pid, len(payload))
    # The signature covers the whole header, whose last field is the signature's
    # own length, and DER signatures vary (70-72 bytes): sign until they agree.
    guess = 71
    while True:
        signed = head + struct.pack("<H", guess) + payload
        sig = key.sign(signed, ec.ECDSA(hashes.SHA256()))
        if len(sig) == guess:
            break
        guess = len(sig)
    if corrupt:
        sig = sig[:-1] + bytes([sig[-1] ^ 1])
    return signed + sig


def fixtures(key_path, out_path):
    """`name:expected hex` lines for services/sentinel/mod.rs's tests, in order."""
    from cryptography.hazmat.primitives.asymmetric import ec

    key = load_key(key_path)
    other = ec.generate_private_key(ec.SECP256R1())
    rule = rule_payload(MATCH_HOST, VERDICT_DENY, "c2-exfil.test")
    cases = [
        ("install_deny_host:ok", envelope(key, OP_INSTALL, 7, 10, 600, payload=rule)),
        ("replay:replayed-sequence", envelope(key, OP_INSTALL, 7, 10, 600, payload=rule)),
        ("older_seq:replayed-sequence", envelope(key, OP_INSTALL, 7, 9, 600, payload=rule)),
        ("freeze_pid:ok", envelope(key, OP_FREEZE, 8, 11, 30, pid=101)),
        ("wrong_key:bad-signature", envelope(other, OP_FREEZE, 9, 12, 30, pid=101)),
        ("flipped_bit:bad-signature", envelope(key, OP_FREEZE, 9, 13, 30, pid=101, corrupt=True)),
        ("no_ttl:ttl-out-of-bounds", envelope(key, OP_INSTALL, 10, 14, 0, payload=rule)),
        ("long_ttl:ttl-out-of-bounds", envelope(key, OP_FREEZE, 10, 15, 7200, pid=101)),
        ("owner_tier:owner-key-not-provisioned", envelope(key, OP_INSTALL, 11, 16, 0, payload=rule, tier=TIER_OWNER)),
        ("bad_rule:bad-rule", envelope(key, OP_INSTALL, 12, 17, 60, payload=b"\x09\x02\x00\x00")),
        ("path_rule:ok", envelope(key, OP_INSTALL, 13, 18, 60, payload=rule_payload(MATCH_PATH, VERDICT_FREEZE, "/apps/launcher/", True))),
        ("unfreeze:ok", envelope(key, OP_UNFREEZE, 8, 19, 0, pid=101)),
        ("revoke:ok", envelope(key, OP_REVOKE, 7, 20, 0)),
    ]
    trunc = envelope(key, OP_REVOKE, 7, 21, 0)
    cases.append(("truncated:truncated", trunc[:-3]))
    bad_magic = b"VSH2" + envelope(key, OP_REVOKE, 7, 22, 0)[4:]
    cases.append(("bad_magic:bad-magic", bad_magic))
    lines = ["key " + public_point(key).hex()] + [f"{n} {b.hex()}" for n, b in cases]
    Path(out_path).write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "fixtures":
        fixtures(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
