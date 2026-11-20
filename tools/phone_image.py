"""Rebuild Android boot images locally, including a stock v2 round-trip check."""
import argparse
import hashlib
import struct
from pathlib import Path

PAGE = 4096
ROOT = Path(__file__).resolve().parents[1]


def aligned(size):
    return ((size + PAGE - 1) // PAGE) * PAGE


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


def repack(stock, ramdisk):
    if stock[:8] == b"ANDROID!" and len(stock) >= 44 and struct.unpack_from("<I", stock, 40)[0] == 2:
        return rebuild_v2(stock, ramdisk)
    if len(stock) < PAGE or stock[:8] != b"ANDROID!":
        raise ValueError("Expected a raw Android boot.img")
    kernel_size, old_size, _, header_size = struct.unpack_from("<4I", stock, 8)
    version, = struct.unpack_from("<I", stock, 40)
    if (version, header_size) not in {(3, 1580), (4, 1584)}:
        raise ValueError("Unsupported Android boot header version or size")
    offset = PAGE + aligned(kernel_size)
    if not kernel_size or not old_size or offset + old_size > len(stock):
        raise ValueError("Invalid kernel or stock ramdisk extent")
    hdr = bytearray(stock[:PAGE])
    struct.pack_into("<I", hdr, 12, len(ramdisk))
    if version == 4:
        struct.pack_into("<I", hdr, 1580, 0)
    return bytes(hdr) + stock[PAGE:offset] + ramdisk.ljust(aligned(len(ramdisk)), b"\0")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stock", type=Path, help="boot.img from the exact XT2163DL installed build")
    parser.add_argument("output", type=Path, help="new output file (must not already exist)")
    parser.add_argument("--stock-roundtrip", action="store_true",
                        help="reassemble v2 factory components and retain the original trailer after byte comparison")
    args = parser.parse_args()
    stock = args.stock.read_bytes()
    if args.stock_roundtrip:
        data = rebuild_v2(stock)
    else:
        data = repack(stock, (ROOT / "build/violet-armv7.cpio.gz").read_bytes())
    with args.output.open("xb") as output:
        output.write(data)
    print(f"Created {args.output} ({len(data):,} bytes)")
