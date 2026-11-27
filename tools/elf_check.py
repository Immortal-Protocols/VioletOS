"""What the Open Runtime's loader will make of a program (system/supervisor/procmgr.rs).

`inspect` reads a 32-bit ARM ELF's program headers. `problems` lists what
makes the loader refuse it: a segment both writable and executable, or code
sharing a 64 KiB hardware page with any other segment (the hardware runs the
whole page, so constants there would be code too), or a dynamic interpreter. `fixed_base` says the program is not position-independent
(ET_EXEC), so its code is at the same address every start; its stack, heap and
mappings still move. tools/build_initfs.py runs both over every program in the
image.
"""
import struct

PAGE = 0x10000


def inspect(data):
    """None if `data` is not a 32-bit little-endian ARM ELF executable, else
    {"pie": bool, "interp": bool, "segments": [(start, end, flags)]}."""
    if len(data) < 52 or data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        return None
    kind, machine = struct.unpack_from("<HH", data, 16)
    if machine != 40 or kind not in (2, 3):
        return None
    phoff = struct.unpack_from("<I", data, 28)[0]
    phentsize, phnum = struct.unpack_from("<HH", data, 42)
    if phentsize < 32 or phoff + phentsize * phnum > len(data):
        return None
    segments, interp = [], False
    for i in range(phnum):
        ptype, _offset, vaddr, _paddr, _filesz, memsz, flags, _align = struct.unpack_from("<8I", data, phoff + i * phentsize)
        interp = interp or ptype == 3
        if ptype == 1 and memsz:
            segments.append((vaddr, vaddr + memsz, flags))
    return {"pie": kind == 3, "interp": interp, "segments": segments}


def problems(info):
    """Why the loader would refuse this program (empty: it loads)."""
    found = []
    if info["interp"]:
        found.append("dynamically linked")
    if any(flags & 3 == 3 for _, _, flags in info["segments"]):
        found.append("a segment is writable and executable")
    code, data = set(), set()
    for start, end, flags in info["segments"]:
        # The loader rounds a segment out to 4 KiB; the hardware page is 64 KiB.
        first, last = (start & ~0xfff) // PAGE, (((end + 0xfff) & ~0xfff) - 1) // PAGE
        (code if flags & 1 else data).update(range(first, last + 1))
    if code & data:
        found.append("code shares a 64 KiB page with data")
    return found


def check(files):
    """`files` maps a name to a program's bytes. Raises SystemExit naming
    every program the loader would refuse; returns the names with a fixed base."""
    refused, fixed = [], []
    for name, data in sorted(files.items()):
        info = inspect(data)
        if info is None:
            continue
        why = problems(info)
        if why:
            refused.append(f"{name}: {'; '.join(why)}")
        elif not info["pie"]:
            fixed.append(name)
    if refused:
        raise SystemExit("the Open Runtime would refuse to load:\n  " + "\n  ".join(refused))
    return fixed
