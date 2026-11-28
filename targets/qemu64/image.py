"""Lay out the qemu64 boot image: the kernel, the bootstrap task, and the entry stub with its boot page tables.

Pure functions over bytes and addresses (tested by test_image.py); build.py does the compiling.
"""
import struct

PAGE = 4096
TABLE = 0x3            # descriptor of a next-level table
NORMAL = 0x711         # 1 GiB or 2 MiB block: normal cached memory (attribute 4), inner shareable, accessed
DEVICE = 0x601 | 3 << 53  # block: device memory (attribute 0), accessed, never executable


class ImageError(ValueError):
    pass


def elf64_segments(data, machine=183):
    """(entry, [(vaddr, paddr, bytes padded to the memory size)]) of an AArch64 ELF64 executable."""
    if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != machine:
        raise ImageError("not a little-endian AArch64 ELF64 file")
    entry, phoff = struct.unpack_from("<QQ", data, 24)
    phentsize, phnum = struct.unpack_from("<HH", data, 54)
    segments = []
    for i in range(phnum):
        kind, _, offset, vaddr, paddr, filesz, memsz, _ = struct.unpack_from("<IIQQQQQQ", data, phoff + i * phentsize)
        if kind == 1 and memsz:
            if filesz > memsz or offset + filesz > len(data):
                raise ImageError("a segment lies outside the file")
            segments.append((vaddr, paddr, data[offset:offset + filesz].ljust(memsz, b"\0")))
    if not segments:
        raise ImageError("no loadable segment")
    return entry, segments


def place_kernel(elf, pa, size):
    """The kernel's flat image at `pa`, its virtual entry and its virtual base.

    Every segment must keep one virtual-to-physical offset and lie inside
    [pa, pa + size): the boot tables map the kernel as one window.
    """
    entry, segments = elf64_segments(elf)
    vbase = min(v for v, _, _ in segments)
    offset = vbase - min(p for _, p, _ in segments)
    image = bytearray()
    for vaddr, paddr, data in segments:
        if vaddr - paddr != offset or not pa <= paddr <= paddr + len(data) <= pa + size:
            raise ImageError(f"kernel segment at {paddr:#x} is outside its {size:#x}-byte region at {pa:#x}")
        at = paddr - pa
        image.extend(bytes(max(0, at + len(data) - len(image))))
        image[at:at + len(data)] = data
    if vbase - offset != pa or vbase % (2 << 20) or not vbase <= entry < vbase + len(image):
        raise ImageError("the kernel is not linked at the start of its region")
    return bytes(image), entry, vbase


def boot_tables(base, kernel_va, kernel_pa, kernel_size):
    """Five pages of boot page tables that will sit at physical `base`.

    Lower half (TTBR0): the first 4 GiB mapped at their own addresses, the
    first as device memory (the interrupt controller), the rest as RAM.
    Upper half (TTBR1): the kernel's region at its link address, in 2 MiB blocks.
    Returns (bytes, address of the lower root, address of the upper root).
    """
    if base % PAGE or kernel_va % (2 << 20) or kernel_pa % (2 << 20) or kernel_va >> 48 != 0xffff:
        raise ImageError("boot tables need page-aligned, 2 MiB-aligned addresses and an upper-half kernel")
    lower0, lower1, upper0, upper1, upper2 = (base + i * PAGE for i in range(5))
    pages = {address: [0] * 512 for address in (lower0, lower1, upper0, upper1, upper2)}
    pages[lower0][0] = lower1 | TABLE
    pages[lower1][0] = DEVICE
    for gigabyte in range(1, 4):
        pages[lower1][gigabyte] = gigabyte << 30 | NORMAL
    pages[upper0][kernel_va >> 39 & 511] = upper1 | TABLE
    pages[upper1][kernel_va >> 30 & 511] = upper2 | TABLE
    first = kernel_va >> 21 & 511
    blocks = -(-kernel_size // (2 << 20))
    if first + blocks > 512:
        raise ImageError("the kernel's region crosses a 1 GiB boundary")
    for i in range(blocks):
        pages[upper2][first + i] = kernel_pa + (i << 21) | NORMAL
    data = b"".join(struct.pack("<512Q", *pages[a]) for a in (lower0, lower1, upper0, upper1, upper2))
    return data, lower0, upper0


def translate(tables, base, root, va):
    """Walk the boot tables as the CPU would: the physical address `va` maps to, or None."""
    def entry(table, index):
        return struct.unpack_from("<Q", tables, table - base + index * 8)[0]
    table = root
    for shift in (39, 30, 21):
        descriptor = entry(table, va >> shift & 511)
        if descriptor & 3 == 3 and shift != 21:
            table = descriptor & 0x0000_ffff_ffff_f000
        elif descriptor & 3 == 1 and shift != 39:
            return (descriptor & 0x0000_ffff_ffff_f000 & ~((1 << shift) - 1)) | va & ((1 << shift) - 1)
        else:
            return None
    return None


def assemble(regions, base):
    """One flat file loaded at `base` from [(address, bytes)]; regions must not overlap."""
    image = bytearray()
    for address, data in sorted(regions):
        at = address - base
        if at < len(image):
            raise ImageError(f"the region at {address:#x} overlaps the one before it")
        image.extend(bytes(at - len(image)))
        image.extend(data)
    return bytes(image)


# What this image gives up against the AArch32 one, as test_system.py --arch
# aarch64 prints and checks it (stack.py writes it to differences.json), so a
# boot that passes every layer cannot read as one with these protections.
# (Verified boot is not one: the entry stub checks a signed manifest, verify.c.)
NO_USER_TIMER = ("no user timer (KernelArmExportPTMRUser=OFF and no KernelEllisTimerGrant): the supervisor's timekeeper "
                 "and its stuck-WFI guard are compiled out, and the Open Runtime polls at a fixed priority instead of taking IRQ 30")


def _flag(pattern, text):
    import re
    found = re.search(pattern, text, re.M)
    return None if not found else found.group(1).upper() in ("ON", "1", "TRUE", "YES")


def user_timer(cache, headers):
    """Who may program the EL0 physical timer, from the kernel's CMakeCache.txt text: "exported" (every
    thread, KernelArmExportPTMRUser, as on AArch32), "granted" (only threads with seL4_TCBFlag_ellisTimer,
    KernelEllisTimerGrant: kernel/sel4/aarch64/ellis-timer.patch) or "" (nobody). Checked against every
    gen_config.h text the supervisor compiles against (`headers`): those decide vm.h VM_USER_TIMER, and one
    that says otherwise (a stale build tree) would build a supervisor whose timer writes trap. Raises
    ValueError on a mismatch or a cache that does not say."""
    import re
    exported = _flag(r"^KernelArmExportPTMRUser:BOOL=(\w+)$", cache)
    if exported is None:
        raise ValueError("the kernel's CMakeCache.txt does not set KernelArmExportPTMRUser")
    granted = bool(_flag(r"^KernelEllisTimerGrant:\w+=(\w+)$", cache))
    for text in headers:
        for config, kernel, option in (("CONFIG_EXPORT_PTMR_USER", exported, "KernelArmExportPTMRUser"),
                                       ("CONFIG_ELLIS_TIMER_GRANT", granted, "KernelEllisTimerGrant")):
            if config not in text and not re.search(r"^#define CONFIG_PLAT_ELLIS\s+1", text, re.M):
                continue
            compiled = re.search(rf"^#define {config}\s+1", text, re.M) is not None
            if compiled != kernel:
                raise ValueError(f"the supervisor's headers say {config}={int(compiled)}, the kernel "
                                 f"{option}={'ON' if kernel else 'OFF'}: a stale build tree")
    return "exported" if exported else "granted" if granted else ""


def differences(timer):
    """The AArch64 image's security differences from AArch32, for differences.json."""
    return [] if timer else [NO_USER_TIMER]
