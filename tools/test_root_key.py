#!/usr/bin/env python3
"""Host test for tools/root_key.py (`make test-unit`, well under a second).

The digests here are asserted, byte for byte the same, by the Rust side
(services/storage/crypt/tests/host.rs and kat.rs): two implementations of
the blob, the key block and the region keys that must agree.
"""
import hashlib
import os
import stat
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import root_key


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    root = bytes(range(32))
    entropy = bytes((i * 7 + 1) & 0xFF for i in range(48))
    volume_key, salt = entropy[:32], entropy[32:]

    blob = root_key.blob(root)
    assert blob.hex() == ("56524b3101000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                          "c2cdb6c60ad47d77ebb28dccaabdcb15e6e286cce6506275")
    assert len(blob) == root_key.BLOB_LEN and root_key.parse(blob) == root
    assert root_key.parse(blob + bytes(4032)) == root, "a whole page parses"
    assert root_key.parse(bytes(4096)) is None and root_key.parse(blob[:63]) is None
    for at in range(64):
        bad = bytearray(blob)
        bad[at] ^= 0x40
        assert root_key.parse(bytes(bad)) is None, at
    for bad in (bytes(32), bytes(31)):
        try:
            root_key.blob(bad)
        except ValueError:
            continue
        raise AssertionError("a bad key was accepted")

    # format: generation 1 in sector 0, generation 2 in sector 1.
    assert digest(root_key.key_block(root, volume_key, salt, 1)) == "a31beae4c744d28183f4cce218db2b0a1e862f918f0ae940174bd76887dc1fcb"
    assert digest(root_key.key_block(root, volume_key, salt, 2)) == "bf15f18f8ca670f02fa84d31eb0e1b3f3ff7333ed902fcc795c3c2e4b7dc803e"
    # set_pin: PBKDF2 of "123456", then generations 3 and 4.
    pin_salt = b"S" * 16
    secret = hashlib.pbkdf2_hmac("sha256", b"123456", pin_salt, 1000, 32)
    assert secret.hex() == "1aa3ccf34b61cdb14344034c187d105bbd5694c380e0867d0460950c46576bdc"
    kek_salt = bytes(range(0xA0, 0xB0))
    pin = (secret, pin_salt, 1000)
    assert digest(root_key.key_block(root, volume_key, kek_salt, 3, 0, pin)) == "6ed939bdfd30ce9803adbab1119fe176a88983c1fd56e1d45ee7214145d3c853"
    assert digest(root_key.key_block(root, volume_key, kek_salt, 4, 0, pin)) == "2d76f529aaf8eee2bdab9bf632becaac56711dab6814a97bb8822167af6930e9"
    assert digest(root_key.cipher_key(volume_key, root_key.DATA)) == "11da99417eeb48773122bfed484af8775aecd6f5f468d3b568f5ee10427a987d"
    assert digest(root_key.cipher_key(volume_key, root_key.APPS)) == "24a0b324be6eeb9974830feb146fed65b5cf4cd80c2d10467b3c93e9b0ed1441"
    # The volume key is not in the block, and the passcode changes the wrap.
    block = root_key.key_block(root, volume_key, kek_salt, 3, 0, pin)
    assert volume_key[:8] not in block and secret[:8] not in block and root[:8] not in block
    assert block[64:96] != root_key.key_block(root, volume_key, kek_salt, 3)[64:96]

    with tempfile.TemporaryDirectory(prefix="violet-rootkey-") as tmp:
        path = Path(tmp) / "disk.rootkey"
        root_key.ensure(path)
        first = path.read_bytes()
        assert root_key.parse(first) is not None
        assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
        root_key.ensure(path)
        assert path.read_bytes() == first, "an existing key is kept"
        other = Path(tmp) / "other.rootkey"
        root_key.ensure(other)
        assert other.read_bytes() != first
        path.write_bytes(b"not a key")
        try:
            root_key.ensure(path)
        except ValueError:
            pass
        else:
            raise AssertionError("a damaged blob was accepted")
    print("root key: ok")


if __name__ == "__main__":
    main()
