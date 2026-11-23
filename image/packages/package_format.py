"""Wire formats shared with the QEMU supervisor. One MiB per boot catalog."""

import binascii
import struct

LIMIT = 1 << 20


def package_abi(identity):
    # Runtime also boots as service workers using the supervisor's mailbox ABI.
    return 2 if identity == 3 else 1


def inspect(blob):
    if len(blob) < 24:
        raise ValueError("truncated package")
    magic, abi, identity, code, size, crc = struct.unpack_from("<4s5I", blob)
    if (
        magic != b"VPK1"
        or (abi != package_abi(identity) and (identity, abi) != (7, 3))
        or not (1 <= identity <= 8 or 10 <= identity <= 14 or identity in (21, 22, 24))
    ):
        raise ValueError("unsupported package or ABI")
    if not 0 < code <= size <= 0x3E000 or code % 4096 or len(blob) != 24 + size:
        raise ValueError("invalid segment sizes")
    if identity == 3 and size > 0x38000:
        raise ValueError("package overlaps its process stack")
    if binascii.crc32(blob[24:]) != crc:
        raise ValueError("package checksum mismatch")
    if 11 <= identity <= 14:
        title = blob[-16:].rstrip(b"\0")
        if (
            size > 65512
            or size < code + 20
            or blob[-20:-16] != b"APP1"
            or not title
            or any(
                chr(ch) not in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -" for ch in title
            )
        ):
            raise ValueError("invalid application manifest")
    return identity


def pack(packages):
    records = {}
    for blob in packages:
        identity = inspect(blob)
        if identity in records:
            raise ValueError("duplicate package identity")
        records[identity] = blob
    if not {1, 3, 4, 5} <= records.keys():
        raise ValueError("required component missing")
    body = bytearray()
    for _, blob in sorted(records.items()):
        body += blob
        body += bytes(-len(body) % 4)
    result = (
        struct.pack("<4s3I", b"VCT1", len(body), len(records), binascii.crc32(body))
        + body
    )
    if len(result) > LIMIT:
        raise ValueError("catalog exceeds boot window")
    return result


def unpack(data):
    if len(data) < 16:
        raise ValueError("truncated catalog")
    magic, size, count, crc = struct.unpack_from("<4s3I", data)
    if (
        magic != b"VCT1"
        or size + 16 != len(data)
        or len(data) > LIMIT
        or binascii.crc32(data[16:]) != crc
    ):
        raise ValueError("invalid catalog")
    pos = 16
    blobs = {}
    for _ in range(count):
        if pos + 24 > len(data):
            raise ValueError("truncated package header")
        length = 24 + struct.unpack_from("<I", data, pos + 16)[0]
        blob = data[pos : pos + length]
        identity = inspect(blob)
        if identity in blobs:
            raise ValueError("duplicate component")
        blobs[identity] = blob
        pos += (length + 3) & ~3
    if pos != len(data) or pack(blobs.values()) != data:
        raise ValueError("invalid catalog contents")
    return blobs
