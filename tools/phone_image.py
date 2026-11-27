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
