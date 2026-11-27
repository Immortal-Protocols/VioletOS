"""Package the SDK's EL2 boot loader, seL4 and the VioletOS root task for QEMU virt."""
import struct
from pathlib import Path

RAM_BASE = 0x40000000
RAM_MIB = 4096
RAM_SIZE = RAM_MIB << 20

# Stage the loader midway through usable RAM, above the kernel and root task.
LOADER_ADDRESS = 0x90000000


class Elf:
    def __init__(self, path):
        self.data = path.read_bytes()
        header = struct.unpack_from("<16sHHIQQQIHHHHHH", self.data)
        ident, _, arch, _, self.entry, phoff, shoff, _, _, phsize, phnum, shsize, shnum, _ = header
        if ident[:6] != b"\x7fELF\x02\x01" or arch != 183:
            raise ValueError(f"{path} must be a little-endian AArch64 ELF")
        self.loads = []
        for i in range(phnum):
            kind, _, offset, va, pa, size, memsize, _ = struct.unpack_from("<IIQQQQQQ", self.data, phoff + i * phsize)
            if kind == 1 and memsize:
                self.loads.append((va, pa, self.data[offset:offset + size].ljust(memsize, b"\0")))
        sections = [struct.unpack_from("<IIQQQQIIQQ", self.data, shoff + i * shsize) for i in range(shnum)]
        self.symbols = {}
        for _, typ, _, _, offset, length, link, _, _, entsize in sections:
            if typ == 2:
                strings = sections[link][4]
                for j in range(offset, offset + length, entsize):
                    name, _, _, _, value, _ = struct.unpack_from("<IBBHQQ", self.data, j)
                    label = self.data[strings + name:self.data.index(0, strings + name)].decode()
                    self.symbols[label] = value


def build_image(loader_path, kernel_path, root_path, image_path):
    loader = Elf(loader_path)
    kernel = Elf(kernel_path)
    root = Elf(root_path)
    [(base, _, segment)] = loader.loads
    image = bytearray(segment)
    kva, kpa, _ = min(kernel.loads)
    root_va = min(v for v, _, _ in root.loads) & -4096
    root_end = (max(v + len(d) for v, _, d in root.loads) + 4095) & -4096
    root_pa = (max(p + len(d) for _, p, d in kernel.loads) + 4095) & -4096

    # Stage 1, 4 KiB granule. The loader still needs its identity mapping
    # and the UART while its caller branches to the high kernel window.
    def entry(table, index, value):
        struct.pack_into("<Q", image, loader.symbols[table] - base + index * 8, value)

    def link(table, index, subtable):
        entry(table, index, loader.symbols[subtable] | 3)

    link("boot_lvl0_lower", 0, "boot_lvl1_lower")
    link("boot_lvl0_lower", kva >> 39 & 511, "boot_lvl1_upper")
    link("boot_lvl1_upper", kva >> 30 & 511, "boot_lvl2_upper")
    entry("boot_lvl1_lower", 0, 0x601)  # device, outer shareable, access flag
    link("boot_lvl1_lower", base >> 30 & 511, "boot_lvl2_lower")
    for va in range(base, base + len(image), 2 << 20):
        entry("boot_lvl2_lower", va >> 21 & 511, (va & -(2 << 20)) | 0x601)
    for i in range(kva >> 21 & 511, 512):
        entry("boot_lvl2_upper", i, kpa + ((i - (kva >> 21 & 511)) << 21) | 0x711)

    regions = [(pa, data) for _, pa, data in kernel.loads]
    regions += [(root_pa + va - root_va, data) for va, _, data in root.loads]
    total = 64 + sum(32 + len(data) for _, data in regions)
    if root_pa + root_end - root_va > base or base + len(image) + total > RAM_BASE + RAM_SIZE:
        raise ValueError("system image exceeds the QEMU boot memory map")
    image.extend(struct.pack("<8Q", 0x5e14dead14de5ead, total, kernel.entry,
                             root_pa, root_pa + root_end - root_va, root_pa - root_va, root.entry, len(regions)))
    offset = 0
    for pa, data in regions:
        image.extend(struct.pack("<4Q", pa, len(data), offset, 1))
        offset += len(data)
    for _, data in regions:
        image.extend(data)
    image_path.write_bytes(image)
    seed_va = root.symbols["boot_entropy"]
    seed_pa = root_pa + seed_va - root_va
    offset = len(image) - sum(len(data) for _, data in regions)
    for pa, data in regions:
        if pa <= seed_pa and seed_pa + 4112 <= pa + len(data):
            return offset + seed_pa - pa
        offset += len(data)
    raise ValueError("boot entropy buffer is outside the root image")
