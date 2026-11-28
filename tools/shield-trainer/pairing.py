"""VSP1 companion pairing: the reference for both ends (docs/shield-pairing.md).

The phone's side is services/sentinel/pairing.rs; this file is the companion's
side and, for the tests, a second implementation of the phone's checks.
Standard library only: P-256 ECDSA is written out below (deterministic nonces,
RFC 6979) so the tests need no package and no network.

The companion's key here is a SOFTWARE key: a number in a file on this
computer. It stands in for a key held in a Secure Enclave. Anyone who can read
the file can sign Shield commands, which is why the phone records such a key
as software and never gives it owner authority.

    python3 pairing.py selftest
    python3 pairing.py fixtures OUT.txt        vectors for the Rust tests
    python3 pairing.py keygen KEY.json         a new software key (mode 0600)
    python3 pairing.py enroll KEY.json OFFER_HEX
    python3 pairing.py confirm KEY.json OFFER_HEX ENROLL_HEX CONFIRM_HEX
"""
from __future__ import annotations

import hashlib
import hmac as hmac_mod
import json
import os
import sys
from pathlib import Path

VERSION = 1
KIND_SOFTWARE, KIND_ATTESTED = 1, 2
TIER_AUTO, TIER_OWNER = 1, 2
OFFER_LEN, ENROLL_HEAD, CONFIRM_LEN = 38, 91, 47

# NIST P-256.
P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551
B = 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B
G = (0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296,
     0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5)


def _add(a, b):
    if a is None:
        return b
    if b is None:
        return a
    (x1, y1), (x2, y2) = a, b
    if x1 == x2:
        if (y1 + y2) % P == 0:
            return None
        s = (3 * x1 * x1 - 3) * pow(2 * y1, -1, P) % P
    else:
        s = (y2 - y1) * pow(x2 - x1, -1, P) % P
    x3 = (s * s - x1 - x2) % P
    return x3, (s * (x1 - x3) - y1) % P


def _mul(k, point):
    out = None
    while k:
        if k & 1:
            out = _add(out, point)
        point = _add(point, point)
        k >>= 1
    return out


def on_curve(point) -> bool:
    x, y = point
    return 0 <= x < P and 0 <= y < P and (y * y - (x * x * x - 3 * x + B)) % P == 0


def public_point(d: int) -> bytes:
    x, y = _mul(d, G)
    return b"\x04" + x.to_bytes(32, "big") + y.to_bytes(32, "big")


def _rfc6979(d: int, digest: bytes) -> int:
    x = d.to_bytes(32, "big")
    h = (int.from_bytes(digest, "big") % N).to_bytes(32, "big")
    v, k = b"\x01" * 32, b"\x00" * 32
    k = hmac_mod.new(k, v + b"\x00" + x + h, hashlib.sha256).digest()
    v = hmac_mod.new(k, v, hashlib.sha256).digest()
    k = hmac_mod.new(k, v + b"\x01" + x + h, hashlib.sha256).digest()
    v = hmac_mod.new(k, v, hashlib.sha256).digest()
    while True:
        v = hmac_mod.new(k, v, hashlib.sha256).digest()
        cand = int.from_bytes(v, "big")
        if 1 <= cand < N:
            return cand
        k = hmac_mod.new(k, v + b"\x00", hashlib.sha256).digest()
        v = hmac_mod.new(k, v, hashlib.sha256).digest()


def _der_int(v: int) -> bytes:
    raw = v.to_bytes((v.bit_length() + 8) // 8 or 1, "big")
    return b"\x02" + bytes([len(raw)]) + raw


def sign(d: int, digest: bytes) -> bytes:
    """ECDSA P-256 over a SHA-256 digest, ASN.1 DER, low S."""
    k = _rfc6979(d, digest)
    r = _mul(k, G)[0] % N
    s = pow(k, -1, N) * (int.from_bytes(digest, "big") + r * d) % N
    assert r and s
    s = min(s, N - s)
    body = _der_int(r) + _der_int(s)
    return b"\x30" + bytes([len(body)]) + body


def verify(point: bytes, digest: bytes, sig: bytes) -> bool:
    try:
        if len(point) != 65 or point[0] != 4 or sig[0] != 0x30 or sig[1] != len(sig) - 2 or sig[2] != 2:
            return False
        rl = sig[3]
        r = int.from_bytes(sig[4:4 + rl], "big")
        if sig[4 + rl] != 2 or 4 + rl + 2 + sig[5 + rl] != len(sig):
            return False
        s = int.from_bytes(sig[6 + rl:], "big")
    except IndexError:
        return False
    q = (int.from_bytes(point[1:33], "big"), int.from_bytes(point[33:], "big"))
    if not on_curve(q) or not (1 <= r < N and 1 <= s < N):
        return False
    w = pow(s, -1, N)
    at = _add(_mul(int.from_bytes(digest, "big") * w % N, G), _mul(r * w % N, q))
    return at is not None and at[0] % N == r


def _mac(secret: bytes, *parts: bytes) -> bytes:
    return hmac_mod.new(secret, b"".join(parts), hashlib.sha256).digest()


def parse_offer(offer: bytes):
    """(phone nonce, secret) from what the phone showed."""
    if len(offer) != OFFER_LEN or offer[:4] != b"VSP1" or int.from_bytes(offer[4:6], "little") != VERSION:
        raise ValueError("not a VSP1 offer")
    return offer[6:22], offer[22:]


def enroll_message(d: int, offer: bytes, companion_nonce: bytes, kind: int = KIND_SOFTWARE, tier: int = TIER_AUTO,
                   attestation: bytes = b"") -> bytes:
    """The companion's ENROLL for the key `d`."""
    nonce, secret = parse_offer(offer)
    assert len(companion_nonce) == 16
    body = b"VSPE" + VERSION.to_bytes(2, "little") + bytes([kind, tier]) + companion_nonce + public_point(d) \
        + len(attestation).to_bytes(2, "little")
    assert len(body) == ENROLL_HEAD
    signed_body = body + attestation
    mac = _mac(secret, b"VSP1 enroll", nonce, signed_body)
    sig = sign(d, hashlib.sha256(b"VSP1 possession" + nonce + signed_body + mac).digest())
    return signed_body + mac + len(sig).to_bytes(2, "little") + sig


def check_confirm(d: int, offer: bytes, enroll: bytes, confirm: bytes) -> bool:
    """Did the phone that showed `offer` enroll our key? (The companion's last step.)"""
    nonce, secret = parse_offer(offer)
    key = public_point(d)
    if len(confirm) != CONFIRM_LEN or confirm[:4] != b"VSPC" or int.from_bytes(confirm[4:6], "little") != VERSION or confirm[6] != 1:
        return False
    digest = hashlib.sha256(key).digest()
    if confirm[7:15] != digest[:8]:
        return False
    want = _mac(secret, b"VSP1 confirm", nonce, enroll[8:24], confirm[6:7], digest)
    return hmac_mod.compare_digest(want, confirm[15:])


def phone_enroll(offer: bytes, msg: bytes):
    """The phone's checks, as services/sentinel/pairing.rs makes them, for the
    tests: (reason, confirm). reason is "" on success."""
    nonce, secret = parse_offer(offer)
    if len(msg) < ENROLL_HEAD + 34 or msg[:4] != b"VSPE":
        return "Malformed", b""
    if int.from_bytes(msg[4:6], "little") != VERSION:
        return "Version", b""
    kind, tier = msg[6], msg[7]
    if kind == KIND_ATTESTED or msg[89:91] != b"\0\0":
        return "AttestationUnsupported", b""
    if kind != KIND_SOFTWARE or len(msg) > ENROLL_HEAD + 34 + 72:
        return "Malformed", b""
    if tier != TIER_AUTO:
        return ("TierNotAllowed" if tier == TIER_OWNER else "Malformed"), b""
    body, mac, rest = msg[:ENROLL_HEAD], msg[ENROLL_HEAD:ENROLL_HEAD + 32], msg[ENROLL_HEAD + 32:]
    sig = rest[2:]
    if int.from_bytes(rest[:2], "little") != len(sig) or len(sig) < 8:
        return "Malformed", b""
    key = body[24:89]
    if key[0] != 4:
        return "Malformed", b""
    if not hmac_mod.compare_digest(_mac(secret, b"VSP1 enroll", nonce, body), mac):
        return "BadMac", b""
    if not verify(key, hashlib.sha256(b"VSP1 possession" + nonce + body + mac).digest(), sig):
        return "BadSignature", b""
    digest = hashlib.sha256(key).digest()
    confirm = b"VSPC" + VERSION.to_bytes(2, "little") + b"\x01" + digest[:8]
    return "", confirm + _mac(secret, b"VSP1 confirm", nonce, body[8:24], b"\x01", digest)


def record(key: bytes, last_seq: int, kind: int = KIND_SOFTWARE, tier: int = TIER_AUTO) -> bytes:
    """The stored key record (pairing.rs `Enrolled::record`)."""
    r = b"VSPK" + VERSION.to_bytes(2, "little") + bytes([kind, tier]) + key + last_seq.to_bytes(8, "little")
    return r + hashlib.sha256(r).digest()


def _det(tag: str, n: int) -> bytes:
    return hashlib.sha256(("vsp1 fixture " + tag).encode()).digest()[:n]


def cases():
    """Named ENROLL messages against one offer, with what the phone must say."""
    random = _det("phone random a", 16) + _det("phone random b", 16)
    offer = b"VSP1" + VERSION.to_bytes(2, "little") + random
    d = int.from_bytes(_det("companion key", 32), "big") % (N - 1) + 1
    other = int.from_bytes(_det("another key", 32), "big") % (N - 1) + 1
    cn = _det("companion nonce", 16)
    good = enroll_message(d, offer, cn)
    out = [("good", good)]
    # The right secret, but the key in the message is not the one that signed:
    # the body is the other key's, the signature is ours.
    theirs = enroll_message(other, offer, cn)
    nonce, secret = parse_offer(offer)
    body = theirs[:ENROLL_HEAD]
    mac = _mac(secret, b"VSP1 enroll", nonce, body)
    sig = sign(d, hashlib.sha256(b"VSP1 possession" + nonce + body + mac).digest())
    out.append(("not-the-holder", body + mac + len(sig).to_bytes(2, "little") + sig))
    wrong_offer = b"VSP1" + VERSION.to_bytes(2, "little") + random[:16] + _det("a guess", 16)
    out.append(("wrong-secret", enroll_message(d, wrong_offer, cn)))
    flipped = bytearray(good)
    flipped[30] ^= 1
    out.append(("key-changed-in-transit", bytes(flipped)))
    out.append(("owner-tier", enroll_message(d, offer, cn, tier=TIER_OWNER)))
    out.append(("claims-attestation", enroll_message(d, offer, cn, kind=KIND_ATTESTED, attestation=b"\x01" * 16)))
    out.append(("truncated", good[:-3]))
    # A point that is not on the curve, signed "correctly" for the secret.
    bad = bytearray(good[:ENROLL_HEAD])
    bad[88] ^= 1
    mac = _mac(secret, b"VSP1 enroll", nonce, bytes(bad))
    sig = sign(d, hashlib.sha256(b"VSP1 possession" + nonce + bytes(bad) + mac).digest())
    out.append(("off-curve", bytes(bad) + mac + len(sig).to_bytes(2, "little") + sig))
    return random, offer, d, out


def fixtures() -> str:
    random, offer, d, messages = cases()
    lines = ["# VSP1 vectors from tools/shield-trainer/pairing.py: name, phone's answer, ENROLL, CONFIRM",
             "random " + random.hex(), "key " + public_point(d).hex(), "record " + record(public_point(d), 7).hex()]
    for name, msg in messages:
        reason, confirm = phone_enroll(offer, msg)
        lines.append(f"case {name} {reason or 'ok'} {msg.hex()} {confirm.hex() or '-'}")
    return "\n".join(lines) + "\n"


def selftest():
    # RFC 6979 A.2.5, P-256 with SHA-256, message "sample".
    d = 0xC9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721
    assert public_point(d).hex() == ("04" "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6"
                                     "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299")
    digest = hashlib.sha256(b"sample").digest()
    assert _rfc6979(d, digest) == 0xA6E3C57DD01ABE90086538398355DD4C3B17AA873382B0F24D6129493D8AAD60
    sig = sign(d, digest)
    r = int.from_bytes(sig[4:4 + sig[3]], "big")
    assert r == 0xEFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716
    assert verify(public_point(d), digest, sig)
    assert not verify(public_point(d), hashlib.sha256(b"sample.").digest(), sig)
    assert not verify(public_point(d + 1), digest, sig)

    random, offer, d, messages = cases()
    want = {"good": "", "not-the-holder": "BadSignature", "wrong-secret": "BadMac", "key-changed-in-transit": "BadMac",
            "owner-tier": "TierNotAllowed", "claims-attestation": "AttestationUnsupported", "truncated": "Malformed",
            "off-curve": "BadSignature"}
    assert {name for name, _ in messages} == set(want)
    for name, msg in messages:
        reason, confirm = phone_enroll(offer, msg)
        assert reason == want[name], (name, reason)
        if not reason:
            assert check_confirm(d, offer, msg, confirm)
            # A phone that did not see the secret cannot confirm.
            forged = bytearray(confirm)
            forged[-1] ^= 1
            assert not check_confirm(d, offer, msg, bytes(forged))
            other_offer = offer[:22] + bytes(16)
            assert not check_confirm(d, other_offer, msg, confirm)
    print("pairing.py: ECDSA matches RFC 6979 A.2.5; 8 enrolment cases behave as specified")


def _load(path: str) -> int:
    data = json.loads(Path(path).read_text())
    assert data.get("kind") == "software", "not a VSP1 software key"
    return int(data["d"], 16)


def main(argv):
    if argv[:1] == ["selftest"]:
        selftest()
    elif argv[:1] == ["fixtures"] and len(argv) == 2:
        Path(argv[1]).write_text(fixtures())
    elif argv[:1] == ["keygen"] and len(argv) == 2:
        d = int.from_bytes(os.urandom(48), "big") % (N - 1) + 1
        fd = os.open(argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump({"kind": "software", "note": "stand-in for a Secure Enclave key; not hardware protected",
                       "d": f"{d:064x}", "public": public_point(d).hex()}, f, indent=1)
        print(f"wrote {argv[1]} (software key {hashlib.sha256(public_point(d)).hexdigest()[:16]})")
    elif argv[:1] == ["enroll"] and len(argv) == 3:
        print(enroll_message(_load(argv[1]), bytes.fromhex(argv[2]), os.urandom(16)).hex())
    elif argv[:1] == ["confirm"] and len(argv) == 5:
        ok = check_confirm(_load(argv[1]), bytes.fromhex(argv[2]), bytes.fromhex(argv[3]), bytes.fromhex(argv[4]))
        print("confirmed" if ok else "NOT confirmed")
        return 0 if ok else 1
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
