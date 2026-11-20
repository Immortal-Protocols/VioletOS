"""Read the boot initializer's CapDL payload through its actual loader mappings."""
import argparse
import os
from pathlib import Path
import struct
import subprocess


def symbols(elf):
    output = subprocess.check_output([os.getenv("LLVM_NM", "llvm-nm"), elf], text=True)
    return {name: int(addr, 16) for line in output.splitlines()
            if len(fields := line.split()) == 3 for addr, _, name in [fields]}


def extract(image, sdk):
    elfs = sdk / "board/qemu_virt_aarch64/debug/elf"
    loader = symbols(elfs / "loader.elf")
    init = symbols(elfs / "initialiser.elf")
    offset = loader["_bss_end"] - loader["_start"]
    magic, size, _, low, high, delta, _, count = struct.unpack_from("<8Q", image, offset)
    if magic != 0x5e14dead14de5ead or size != len(image) - offset:
        raise ValueError("unexpected SDK loader header or image length")
    payload = offset + 64 + count * 32
    regions = []
    for i in range(count):
        pa, length, start, kind = struct.unpack_from("<4Q", image, offset + 64 + i * 32)
        if kind != 1 or payload + start + length > len(image):
            raise ValueError("invalid loader data region")
        regions.append((pa, length, payload + start))

    def read(va, length):
        pa = (va + delta) % (1 << 64)
        if not low <= pa < pa + length <= high:
            raise ValueError("outside initializer")
        matches = [pos + pa - base for base, n, pos in regions
                   if base <= pa and pa + length <= base + n]
        if len(matches) != 1:
            raise ValueError("ambiguous or absent loader data")
        return image[matches[0]:matches[0] + length]

    table, = struct.unpack("<Q", read(init["sel4_phdrs_patched__vaddr"], 8))
    number, = struct.unpack("<H", read(init["sel4_phdrs_patched__phnum"], 2))
    headers = struct.iter_unpack("<II6Q", read(table, 56 * number))
    specs = [read(va, length) for kind, _, _, va, _, length, _, _ in headers
             if kind == 0x64c34003]
    if len(specs) != 1:
        raise ValueError("expected one CapDL payload")
    return specs[0]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_bytes(extract(args.image.read_bytes(), Path(os.environ["MICROKIT_SDK"])))
