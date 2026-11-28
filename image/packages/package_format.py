"""Wire formats shared with the supervisor. One MiB per boot catalog on QEMU,
1.25 MiB on the handset (HANDSET_LIMIT).

A package is a VPK2 record: header[24] | image[size] | signature trailer[80]
(sign.py). inspect() checks a record's shape; given the pinned keys it also
checks the publisher signature, as the supervisor does before running it.
"""

import binascii
import struct

LIMIT = 1 << 20
# The handset's built-in and installed catalogs: its catalog banks
# (system/package-manager/catalog_volume.rs LIMIT under package_catalog) and
# the supervisor's CATALOG_LIMIT. QEMU's staged and installed windows stay 1 MiB.
HANDSET_LIMIT = 0x140000


def catalog_limit(target):
    """The largest catalog the supervisor of `target` boots."""
    return HANDSET_LIMIT if target == "moto" else LIMIT
HEADER = 24
# "VSG1", key ID, security version, target tag, then the raw P-256 signature.
TRAILER = 80


# Services whose stack is outside their image (supervisor.c own_stack) and the
# Open Runtime may fill 0x80000 bytes; Runtime (3) 0x38000; the rest 0x3E000.
OWN_STACK = (4, 5, 6, 7, 8, 21, 22, 24, 25, 26)


def package_window(identity):
    if identity == 3:
        return 0x38000
    return 0x80000 if identity in OWN_STACK else 0x3E000


def package_abi(identity):
    # Runtime also boots as service workers using the supervisor's mailbox ABI.
    return 2 if identity == 3 else 1


def version(blob):
    """The security version in an inspected record's signature trailer."""
    return struct.unpack_from("<I", blob, len(blob) - TRAILER + 8)[0]


def inspect(blob, keys=None, target=None):
    if len(blob) < HEADER + TRAILER:
        raise ValueError("truncated package")
    magic, abi, identity, code, size, crc = struct.unpack_from("<4s5I", blob)
    if magic == b"VPK1":
        raise ValueError("unsigned VPK1 package: rebuild it to get a signed VPK2")
    if (
        magic != b"VPK2"
        or (abi != package_abi(identity) and (identity, abi) not in ((7, 3), (7, 4)))
        or not (1 <= identity <= 8 or 10 <= identity <= 14 or identity in (21, 22, 24, 25, 26))
    ):
        raise ValueError("unsupported package or ABI")
    window = package_window(identity)
    if not 0 < code <= size <= window or code % 4096 or len(blob) != HEADER + size + TRAILER:
        raise ValueError("invalid segment sizes")
    image = blob[HEADER : HEADER + size]
    if blob[HEADER + size : HEADER + size + 4] != b"VSG1" or not version(blob):
        raise ValueError("package has no signature trailer")
    if identity == 3 and size > 0x38000:
        raise ValueError("package overlaps its process stack")
    if binascii.crc32(image) != crc:
        raise ValueError("package checksum mismatch")
    if 11 <= identity <= 14:
        title = image[-16:].rstrip(b"\0")
        if (
            size > 65512
            or size < code + 20
            or image[-20:-16] != b"APP1"
            or not title
            or any(
                chr(ch) not in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -" for ch in title
            )
        ):
            raise ValueError("invalid application manifest")
    if keys is not None:
        from sign import verify

        verify(blob, keys, target)
    return identity


FLOOR_IDS = 32


def floor_record(floors):
    """The floor record that ends a catalog: {component ID: version}, or b"" when empty.

    The supervisor writes one into every catalog it composes: the highest
    version seen of each component where that is above the factory's, so a
    removed component leaves its version behind (verify.rs, Floors).
    """
    if not floors:
        return b""
    if any(not 0 <= i < FLOOR_IDS or not 0 < v < 1 << 32 for i, v in floors.items()):
        raise ValueError("invalid version floor")
    return struct.pack("<4sI", b"VFL1", len(floors)) + b"".join(struct.pack("<2I", i, v) for i, v in sorted(floors.items()))


def pack(packages, floors=None, limit=LIMIT):
    records = {}
    for blob in packages:
        identity = inspect(blob)
        if identity in records:
            raise ValueError("duplicate package identity")
        records[identity] = blob
    if not {1, 3, 4, 5} <= records.keys():
        raise ValueError("required component missing")
    for identity, floor in (floors or {}).items():
        if identity in records and version(records[identity]) < floor:
            raise ValueError("package is older than the catalog's version floor")
    body = bytearray()
    for _, blob in sorted(records.items()):
        body += blob
        body += bytes(-len(body) % 4)
    body += floor_record(floors)
    result = (
        struct.pack("<4s3I", b"VCT1", len(body), len(records), binascii.crc32(body))
        + body
    )
    if len(result) > limit:
        raise ValueError("catalog exceeds boot window")
    return result


def _parse(data, limit=LIMIT):
    if len(data) < 16:
        raise ValueError("truncated catalog")
    magic, size, count, crc = struct.unpack_from("<4s3I", data)
    if (
        magic != b"VCT1"
        or size + 16 != len(data)
        or len(data) > limit
        or binascii.crc32(data[16:]) != crc
    ):
        raise ValueError("invalid catalog")
    pos = 16
    blobs = {}
    for _ in range(count):
        if pos + HEADER > len(data):
            raise ValueError("truncated package header")
        length = HEADER + struct.unpack_from("<I", data, pos + 16)[0] + TRAILER
        blob = data[pos : pos + length]
        identity = inspect(blob)
        if identity in blobs:
            raise ValueError("duplicate component")
        blobs[identity] = blob
        pos += (length + 3) & ~3
    record = {}
    if pos != len(data):
        # Only a floor record may follow the packages.
        if len(data) - pos < 16 or data[pos : pos + 4] != b"VFL1":
            raise ValueError("invalid catalog contents")
        entries = struct.unpack_from("<I", data, pos + 4)[0]
        if not 0 < entries <= FLOOR_IDS or len(data) - pos != 8 + 8 * entries:
            raise ValueError("invalid version floor record")
        for at in range(entries):
            identity, floor = struct.unpack_from("<2I", data, pos + 8 + 8 * at)
            record[identity] = floor
        if list(record) != sorted(record) or len(record) != entries:
            raise ValueError("invalid version floor record")
    # pack() refuses bad floors and packages below them; the bytes must match exactly.
    if pack(blobs.values(), record, limit) != data:
        raise ValueError("invalid catalog contents")
    return blobs, record


def unpack(data, limit=LIMIT):
    """{component ID: record} of a catalog."""
    return _parse(data, limit)[0]


def floors(data):
    """{component ID: version} from a catalog's floor record (empty without one)."""
    return _parse(data)[1]
