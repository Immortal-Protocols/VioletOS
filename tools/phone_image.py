"""Rebuild Android boot images locally, including a stock v2 round-trip check."""
import hashlib
import struct



# Size fields in Android boot_img_hdr_v2, in on-disk section order.
V2_SECTIONS = (("kernel", 8), ("ramdisk", 16), ("second", 24),
               ("recovery_dtbo", 1632), ("dtb", 1648))
V2_HEADER_SIZE = 1660


def rebuild_v2(stock, replacement=None, *, kernel=None):
    """Recompute layout, padding and the legacy ID from the unpacked sections."""
    if len(stock) < V2_HEADER_SIZE or stock[:8] != b"ANDROID!":
        raise ValueError("Expected a complete Android v2 header")
    page, version = struct.unpack_from("<II", stock, 36)
    header_size, = struct.unpack_from("<I", stock, 1644)
    if version != 2 or header_size != V2_HEADER_SIZE:
        raise ValueError("Expected boot header version 2 and size 1660")
    if page < header_size or page > len(stock) or page & (page - 1):
        raise ValueError("Invalid v2 page size")

    # Retain load addresses, command line, OS version and fixed-width fields.
    header = bytearray(stock[:header_size])
    body = bytearray()
    digest = hashlib.sha1()
    cursor = page
    for name, size_field in V2_SECTIONS:
        size, = struct.unpack_from("<I", stock, size_field)
        end = cursor + size
        next_page = end + (-end % page)
        if next_page > len(stock) or (name in ("kernel", "ramdisk") and not size):
            raise ValueError(f"Invalid v2 {name} extent")
        if name == "recovery_dtbo" and size:
            recorded, = struct.unpack_from("<Q", stock, 1636)
            if recorded != cursor:
                raise ValueError("Unexpected recovery DTBO offset")
            struct.pack_into("<Q", header, 1636, page + len(body))
        section = stock[cursor:end]
        cursor = next_page
        if name == "kernel" and kernel is not None:
            if not kernel:
                raise ValueError("Expected a nonempty kernel")
            section = kernel
        if name == "ramdisk" and replacement is not None:
            if not replacement:
                raise ValueError("Expected a nonempty ramdisk")
            section = replacement
        size_bytes = struct.pack("<I", len(section))
        header[size_field:size_field + 4] = size_bytes
        digest.update(section)
        digest.update(size_bytes)
        body.extend(section)
        body.extend(bytes(-len(section) % page))
    header[576:608] = digest.digest().ljust(32, b"\0")
    rebuilt = header.ljust(page, b"\0") + body
    if replacement is None and kernel is None:
        # Only a byte-identical payload can reuse the original partition trailer.
        if rebuilt != stock[:cursor]:
            raise ValueError("Regenerated v2 payload differs from the stock payload")
        rebuilt.extend(stock[cursor:])
    return bytes(rebuilt)


def v2_payload_length(image):
    """Bytes of an Android v2 boot image up to the end of its last section.

    This is what the bootloader reads of a boot partition, and so what a
    full-system update (tools/ota_bundle.py) puts in a slot: VioletOS's
    boot.img continues with the storage volume, boot history and catalog
    banks, which belong to boot_b and not to the slot image.
    """
    if len(image) < V2_HEADER_SIZE or image[:8] != b"ANDROID!":
        raise ValueError("Expected a complete Android v2 header")
    page, version = struct.unpack_from("<II", image, 36)
    if version != 2 or page < V2_HEADER_SIZE or page & (page - 1):
        raise ValueError("Expected boot header version 2 with a valid page size")
    cursor = page
    for _, size_field in V2_SECTIONS:
        size, = struct.unpack_from("<I", image, size_field)
        cursor += size + (-size % page)
    if cursor > len(image):
        raise ValueError("Boot image sections run past its end")
    return cursor


def factory_dtb(stock):
    """Extract the single MT6765 base tree from the pinned Motorola image."""
    if hashlib.sha256(stock).hexdigest() != "36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a":
        raise ValueError("expected the XT2163DL S3RHS32.20-42-13-2-3-2 factory boot image")
    page, version = struct.unpack_from("<II", stock, 36)
    start = page
    for field in (8, 16, 24, 1632):
        size, = struct.unpack_from("<I", stock, field)
        start += (size + page - 1) // page * page
    magic, total, _, entry_size, count, entries = struct.unpack_from(">6I", stock, start)
    assert version == 2 and magic == 0xd7b7ab1e and count == 1 and entry_size == 32
    size, offset = struct.unpack_from(">II", stock, start + entries)
    assert offset + size <= total and struct.unpack_from(">II", stock, start + offset) == (0xd00dfeed, size)
    return stock[start + offset:start + offset + size]


# --- Android Verified Boot metadata -----------------------------------------
# What a locked XT2163DL bootloader checks before it starts a boot image
# (docs/verified-boot.md): a vbmeta image, signed with RSA, whose hash
# descriptor gives the salted SHA-256 of the first `image_size` bytes of the
# boot partition. avb_verify reads the factory vbmeta the same way (the tests
# check Motorola's own signature with it); avb_vbmeta writes one for a
# VioletOS boot image and a key of the user's. No build uses it: the handset's
# bootloader is unlocked and checks nothing, and whether it would accept a
# user's key is not known from the factory files.
AVB_HEADER = 256
AVB_ALGORITHMS = {2048: 1, 4096: 2}  # SHA256_RSA2048, SHA256_RSA4096
# PKCS #1 v1.5 DigestInfo for SHA-256.
_SHA256_INFO = bytes.fromhex("3031300d060960864801650304020105000420")


def avb_public_key(modulus):
    """AVB's key format: bits, -1/n mod 2^32, n, R^2 mod n (the exponent is 65537)."""
    bits = modulus.bit_length()
    if bits not in AVB_ALGORITHMS:
        raise ValueError("an AVB key here is RSA-2048 or RSA-4096")
    size = bits // 8
    return struct.pack(">II", bits, -pow(modulus, -1, 1 << 32) % (1 << 32)) + \
        modulus.to_bytes(size, "big") + pow(2, 2 * bits, modulus).to_bytes(size, "big")


def _avb_pad(data, to):
    return bytes(data) + bytes(-len(data) % to)


def avb_hash_descriptor(image, salt, partition="boot"):
    name = partition.encode()
    body = struct.pack(">Q32s4I60x", len(image), b"sha256", len(name), len(salt), 32, 0) + name + salt + \
        hashlib.sha256(salt + image).digest()
    body = _avb_pad(body, 8)
    return struct.pack(">QQ", 2, len(body)) + body


def avb_vbmeta(image, modulus, sign, rollback_index, salt, partition="boot"):
    """A vbmeta image for `image` (what the bootloader reads of the partition:
    v2_payload_length bytes of a VioletOS boot image). `sign(message)` returns
    the RSA PKCS #1 v1.5 SHA-256 signature by the key whose modulus is given."""
    if not image or not 0 <= rollback_index < 1 << 64 or len(salt) != 32:
        raise ValueError("vbmeta needs an image, a 64-bit rollback index and a 32-byte salt")
    key = avb_public_key(modulus)
    size = modulus.bit_length() // 8
    descriptors = avb_hash_descriptor(image, salt, partition)
    aux = _avb_pad(descriptors + key, 64)
    auth_size = 32 + size + (-(32 + size) % 64)
    header = b"AVB0" + struct.pack(
        ">2I2QI10QQ2I", 1, 0, auth_size, len(aux), AVB_ALGORITHMS[size * 8], 0, 32, 32, size,
        len(descriptors), len(key), len(descriptors) + len(key), 0, 0, len(descriptors), rollback_index, 0, 0)
    header = (header + b"VioletOS tools/phone_image.py".ljust(48, b"\0")).ljust(AVB_HEADER, b"\0")
    signature = sign(header + aux)
    if len(signature) != size:
        raise ValueError("the signature is not the key's size")
    vbmeta = header + _avb_pad(hashlib.sha256(header + aux).digest() + signature, 64) + aux
    # Never hand out metadata its own key would not verify.
    avb_verify(vbmeta, {partition: image})
    return vbmeta


def avb_verify(vbmeta, images=None):
    """Check a vbmeta image's signature with the key it carries, and each hash
    descriptor whose partition is in `images`. Returns what it says: the key's
    SHA-256 (what a bootloader compares with its root of trust), rollback
    index and location, and descriptors. ValueError on any mismatch.
    A valid signature says only that the holder of that key signed it."""
    vbmeta = bytes(vbmeta)
    if len(vbmeta) < AVB_HEADER or vbmeta[:4] != b"AVB0":
        raise ValueError("not a vbmeta image")
    (major, minor, auth_size, aux_size, algorithm, hash_at, hash_size, sig_at, sig_size, key_at, key_size,
     _meta_at, _meta_size, desc_at, desc_size, rollback, flags, location) = struct.unpack_from(">2I2QI10QQ2I", vbmeta, 4)
    if major != 1 or algorithm not in AVB_ALGORITHMS.values() or len(vbmeta) < AVB_HEADER + auth_size + aux_size:
        raise ValueError("unsupported or truncated vbmeta")
    auth = vbmeta[AVB_HEADER:AVB_HEADER + auth_size]
    aux = vbmeta[AVB_HEADER + auth_size:AVB_HEADER + auth_size + aux_size]
    signed = vbmeta[:AVB_HEADER] + aux
    key = aux[key_at:key_at + key_size]
    if hash_size != 32 or hash_at + 32 > len(auth) or sig_at + sig_size > len(auth) or len(key) != key_size or key_size < 8:
        raise ValueError("vbmeta blocks out of range")
    bits, = struct.unpack_from(">I", key)
    size = bits // 8
    if AVB_ALGORITHMS.get(bits) != algorithm or key_size != 8 + 2 * size or sig_size != size:
        raise ValueError("vbmeta key does not fit its algorithm")
    modulus = int.from_bytes(key[8:8 + size], "big")
    if key != avb_public_key(modulus):
        raise ValueError("vbmeta key is not well formed")
    digest = hashlib.sha256(signed).digest()
    if auth[hash_at:hash_at + 32] != digest:
        raise ValueError("vbmeta hash does not match")
    expected = b"\0\1" + b"\xff" * (size - 3 - len(_SHA256_INFO) - 32) + b"\0" + _SHA256_INFO + digest
    if pow(int.from_bytes(auth[sig_at:sig_at + sig_size], "big"), 65537, modulus).to_bytes(size, "big") != expected:
        raise ValueError("vbmeta signature is invalid")
    descriptors, at, table = [], 0, aux[desc_at:desc_at + desc_size]
    while at + 16 <= len(table):
        tag, length = struct.unpack_from(">QQ", table, at)
        body = table[at + 16:at + 16 + length]
        if len(body) != length or length % 8:
            raise ValueError("vbmeta descriptor out of range")
        at += 16 + length
        if tag == 2:
            image_size, algorithm_name, name_len, salt_len, digest_len, _flags = struct.unpack_from(">Q32s4I", body)
            name = body[116:116 + name_len].decode()
            salt = body[116 + name_len:116 + name_len + salt_len]
            stored = body[116 + name_len + salt_len:116 + name_len + salt_len + digest_len]
            if algorithm_name.rstrip(b"\0") != b"sha256" or len(stored) != 32:
                raise ValueError("unsupported hash descriptor")
            if images and name in images:
                image = images[name]
                if len(image) < image_size or hashlib.sha256(salt + image[:image_size]).digest() != stored:
                    raise ValueError(f"{name} does not match its hash descriptor")
            descriptors.append(("hash", name, image_size))
        elif tag == 4:
            chain_location, name_len, chain_key_len = struct.unpack_from(">3I", body)
            descriptors.append(("chain", body[76:76 + name_len].decode(), chain_location,
                                hashlib.sha256(body[76 + name_len:76 + name_len + chain_key_len]).hexdigest()))
        else:
            descriptors.append(({0: "property", 1: "hashtree", 3: "cmdline"}.get(tag, f"tag {tag}"),))
    for name in images or {}:
        if not any(d[:2] == ("hash", name) for d in descriptors):
            raise ValueError(f"vbmeta has no hash descriptor for {name}")
    return {"key_bits": bits, "key_sha256": hashlib.sha256(key).hexdigest(), "rollback_index": rollback,
            "rollback_index_location": location, "flags": flags, "descriptors": descriptors}
