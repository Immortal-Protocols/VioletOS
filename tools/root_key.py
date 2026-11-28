#!/usr/bin/env python3
"""The device root key blob for QEMU, and the key block as a reference.

On QEMU the root key of the encrypted volume (services/storage/crypt) comes
from a 64-byte provisioning blob that tools/run.py loads into guest memory at
0x49003000, a page only Storage is given. The blob is a stand-in for a key
that hardware keeps: it lives in a file beside the disk image on the Mac
(mode 0600), never inside the image. Deleting it makes the volume unreadable
for good. docs/encryption-at-rest.md has the rest.

  tools/root_key.py new PATH     make a blob (refuses to replace one)
  tools/root_key.py check PATH   say whether PATH is a well-formed blob

Neither command prints the key or anything computed from it.

`key_block` builds the volume's key block exactly as
services/storage/crypt/keys.rs does, with hashlib only. The host tests of
both sides (tools/test_root_key.py and services/storage/crypt/tests/host.rs)
check the same vectors, so a test that needs a prepared disk can make its
header here.
"""
import hashlib
import hmac
import os
import sys
from pathlib import Path

ADDRESS = 0x49003000
BLOB_LEN = 64
HEADER_SECTORS = 8
DATA, APPS = 1, 2


def blob(key):
    if len(key) != 32 or not any(key):
        raise ValueError("a root key is 32 bytes, not all zero")
    head = b"VRK1" + (1).to_bytes(4, "little") + key
    return head + hashlib.sha256(head).digest()[:24]


def parse(data):
    """The key in a blob, or None."""
    if len(data) < BLOB_LEN or data[:8] != b"VRK1\x01\0\0\0" or not any(data[8:40]):
        return None
    if hashlib.sha256(data[:40]).digest()[:24] != data[40:64]:
        return None
    return data[8:40]


def ensure(path):
    """Make the blob at `path` unless one is there. Returns the path."""
    path = Path(path)
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        if parse(path.read_bytes()) is None:
            raise ValueError(f"{path} is not a root key blob; remove it to lose the volume and start again")
        return path
    with os.fdopen(fd, "wb") as stream:
        stream.write(blob(os.urandom(32)))
    return path


def _mac(key, *parts):
    return hmac.new(key, b"".join(parts), hashlib.sha256).digest()


def cipher_key(volume_key, purpose):
    """The 64-byte XTS-AES-256 key of one region (DATA, APPS)."""
    return b"".join(_mac(volume_key, b"violet volume v1", bytes([purpose, half])) for half in range(2))


def key_block(root, volume_key, kek_salt, generation, failures=0, pin=None):
    """One 512-byte key block sector. `pin`: None, or (secret, salt, iterations)
    with secret = PBKDF2-HMAC-SHA-256(digits, salt, iterations, 32)."""
    assert len(root) == 32 and len(volume_key) == 32 and len(kek_salt) == 16
    flags = (1 if pin else 0).to_bytes(4, "little")
    secret, pin_salt, iterations = pin or (bytes(32), bytes(16), 0)
    kek = _mac(root, b"violet kek v1", flags, kek_salt, secret)
    stream = _mac(kek, b"violet wrap enc v1")
    wrapped = bytes(a ^ b for a, b in zip(volume_key, stream))
    s = bytearray(512)
    s[0:8] = b"VKB1" + (1).to_bytes(4, "little")
    s[8:16] = generation.to_bytes(8, "little")
    s[16:20] = failures.to_bytes(4, "little")
    s[20:24] = flags
    s[24:28] = iterations.to_bytes(4, "little")
    s[32:48] = pin_salt
    s[48:64] = kek_salt
    s[64:96] = wrapped
    s[96:128] = _mac(_mac(kek, b"violet wrap mac v1"), s[0:8], s[20:28], s[32:96])
    s[128:160] = _mac(_mac(root, b"violet keyblock seal v1"), s[0:128])
    return bytes(s)


def main(argv):
    if len(argv) != 3 or argv[1] not in ("new", "check"):
        sys.exit(__doc__)
    path = Path(argv[2])
    if argv[1] == "new":
        if path.exists():
            sys.exit(f"{path} exists; a volume made with it would be lost")
        ensure(path)
        print(f"wrote {path}")
    elif parse(path.read_bytes()) is None:
        sys.exit(f"{path}: not a root key blob")
    else:
        print(f"{path}: a root key blob")


if __name__ == "__main__":
    main(sys.argv)
