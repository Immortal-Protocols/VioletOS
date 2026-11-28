"""What the Open Runtime's loader will make of a program (system/supervisor/procmgr.rs).

`inspect` reads a 32-bit ARM ELF's program headers and dynamic relocations.
`problems` lists what makes the loader refuse it, or what would end it as it
starts: a segment both writable and executable, or code sharing a 64 KiB
hardware page with any other segment (the hardware runs the whole page, so
constants there would be code too), or a dynamic interpreter other than
musl's (LOADER, which the image must then hold; the loader checks it by the
same rules), or, in a static position-independent program, a relocation its
own start code (musl's rcrt1) cannot apply: anything but a relative one, or
one that patches memory the program may not write (code and constants are
mapped read-only, so the patch would be the program's first and last fault),
or a static PIE that would leave its constant pointer tables writable: those
(vtables, function-pointer tables, the GOT) sit in PT_GNU_RELRO, writable
while rcrt1 relocates them, and only tools/relro_seal.rs, linked into every
static PIE, makes that range read-only afterwards. So a static PIE needs
PT_GNU_RELRO ending on a 4 KiB page boundary, PT_PHDR (how the seal finds
its load address) and the seal's mark (SEAL_MARK).

`check` is the release gate tools/build_initfs.py runs over every program in
the image: besides the above it refuses a program that is not
position-independent (ET_EXEC), whose code would sit at the same address on
every start while only its stack, heap and mappings move. Names in
`fixed_ok` are let through and returned (test programs the toolchain cannot
link as PIE: Go, see build_initfs.py, and only the folders FIXED_TEST_PROGRAMS
names); nothing shipped may be among them. `fixed_allowed` is the one switch
for a development build with fixed-address programs (VIOLET_OPEN_PIE=0); a
release (VIOLET_RELEASE=1) refuses both.
"""
import os
import struct
from pathlib import Path

PAGE = 0x10000
# The one dynamic loader the Open Runtime runs (runtime/open/elf.rs).
LOADER = "/lib/ld-musl-armhf.so.1"
R_ARM_NONE, R_ARM_RELATIVE = 0, 23
PT_PHDR, PT_GNU_RELRO = 6, 0x6474E552
# tools/relro_seal.rs's VIOLET_RELRO_SEAL: the program seals its RELRO.
SEAL_MARK = b"violet:relro-seal:1\0"
# The Go test programs (Go links linux/arm PIEs only through cgo), by folder
# under the repository: the only programs a test image may carry at a fixed address.
FIXED_TEST_PROGRAMS = ("targets/qemu/abi-test-apps/goabi", "targets/qemu/open-test-apps/gohello",
                       "targets/qemu/open-test-apps/gonet")


def release(environ=os.environ):
    """VIOLET_RELEASE=1 (image/packages/sign.py checks the value itself)."""
    return environ.get("VIOLET_RELEASE") == "1"


def fixed_allowed(environ=os.environ):
    """Whether this is a development build whose programs may all keep a
    fixed code address (VIOLET_OPEN_PIE=0). Refused for a release."""
    if environ.get("VIOLET_OPEN_PIE") != "0":
        return False
    if release(environ):
        raise SystemExit("VIOLET_OPEN_PIE=0 builds fixed-address programs: not with VIOLET_RELEASE=1")
    return True


def fixed_test_program(root, folder, environ=os.environ):
    """Whether the app in `folder` is one of FIXED_TEST_PROGRAMS (never in a release)."""
    folder = Path(folder).resolve()
    return not release(environ) and any(folder == (Path(root) / name).resolve() for name in FIXED_TEST_PROGRAMS)
# The most relocations looked at (Python's interpreter has about 70 000).
MAX_RELOCATIONS = 1 << 22


def inspect(data):
    """None if `data` is not a 32-bit little-endian ARM ELF executable, else
    {"pie": bool, "interp": None or the PT_INTERP path, "segments": [(start, end, flags)],
     "textrel": bool, "relocations": [(address, type)] or None if they cannot be read,
     "relro": (start, end) of PT_GNU_RELRO or None, "phdr": has PT_PHDR,
     "sealed": holds SEAL_MARK}."""
    if len(data) < 52 or data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        return None
    kind, machine = struct.unpack_from("<HH", data, 16)
    if machine != 40 or kind not in (2, 3):
        return None
    phoff = struct.unpack_from("<I", data, 28)[0]
    phentsize, phnum = struct.unpack_from("<HH", data, 42)
    if phentsize < 32 or phoff + phentsize * phnum > len(data):
        return None
    segments, interp, files, dynamic, relro, phdr = [], None, [], None, None, False
    for i in range(phnum):
        ptype, offset, vaddr, _paddr, filesz, memsz, flags, _align = struct.unpack_from("<8I", data, phoff + i * phentsize)
        if ptype == 3:
            raw = data[offset:offset + filesz]
            interp = raw[:-1].decode(errors="replace") if raw.endswith(b"\0") and len(raw) == filesz else "(bad PT_INTERP)"
        if ptype == 2:
            dynamic = (offset, filesz)
        if ptype == PT_GNU_RELRO:
            relro = (vaddr, vaddr + memsz)
        phdr |= ptype == PT_PHDR
        if ptype == 1 and memsz:
            segments.append((vaddr, vaddr + memsz, flags))
            files.append((vaddr, offset, filesz))
    textrel, relocations = _dynamic(data, dynamic, files)
    return {"pie": kind == 3, "interp": interp, "segments": segments, "textrel": textrel, "relocations": relocations,
            "relro": relro, "phdr": phdr, "sealed": SEAL_MARK in data}


def _dynamic(data, dynamic, files):
    """(has text relocations, [(address, type)]) from PT_DYNAMIC; the list is
    None if a table lies outside the file or is in a form not read here."""
    if dynamic is None:
        return False, []

    def at(vaddr, size):
        """The file offset of `size` bytes at `vaddr`, or None."""
        for start, offset, filesz in files:
            if start <= vaddr and vaddr + size <= start + filesz:
                return offset + vaddr - start
        return None

    offset, size = dynamic
    if offset + size > len(data):
        return False, None
    tags = {}
    for k in range(size // 8):
        tag, value = struct.unpack_from("<II", data, offset + 8 * k)
        if tag == 0:
            break
        tags.setdefault(tag, value)
    # DT_TEXTREL, or DF_TEXTREL in DT_FLAGS.
    textrel = 22 in tags or bool(tags.get(30, 0) & 4)
    out = []
    # (table, its size, entry size): DT_REL, DT_RELA, DT_JMPREL (REL unless DT_PLTREL says RELA).
    tables = [(tags.get(17), tags.get(18, 0), 8), (tags.get(7), tags.get(8, 0), 12),
              (tags.get(23), tags.get(2, 0), 12 if tags.get(20) == 7 else 8)]
    for table, total, entry in tables:
        if table is None or total == 0:
            continue
        where = at(table, total)
        if where is None or total // entry > MAX_RELOCATIONS:
            return textrel, None
        for k in range(total // entry):
            address, info = struct.unpack_from("<II", data, where + entry * k)
            out.append((address, info & 0xff))
    # DT_RELR: packed relative relocations (an address word, then bitmaps of the words after it).
    if tags.get(36) is not None and tags.get(35, 0):
        where = at(tags[36], tags[35])
        if where is None or tags[35] // 4 > MAX_RELOCATIONS:
            return textrel, None
        base = 0
        for k in range(tags[35] // 4):
            word = struct.unpack_from("<I", data, where + 4 * k)[0]
            if word & 1 == 0:
                out.append((word, R_ARM_RELATIVE))
                base = word + 4
            else:
                out += [(base + 4 * bit, R_ARM_RELATIVE) for bit in range(31) if word >> (bit + 1) & 1]
                base += 4 * 31
            if len(out) > MAX_RELOCATIONS:
                return textrel, None
    return textrel, out


def problems(info, loader=False):
    """Why the loader would refuse this program, or why it would end as it
    starts (empty: it loads and its start code can relocate it). `loader`:
    this is musl's dynamic loader itself, which binds its own symbols."""
    found = []
    if info["interp"] is not None and info["interp"] != LOADER:
        found.append(f"dynamically linked against {info['interp']} (only {LOADER} is supported)")
    if any(flags & 3 == 3 for _, _, flags in info["segments"]):
        found.append("a segment is writable and executable")
    code, data = set(), set()
    for start, end, flags in info["segments"]:
        # The loader rounds a segment out to 4 KiB; the hardware page is 64 KiB.
        first, last = (start & ~0xfff) // PAGE, (((end + 0xfff) & ~0xfff) - 1) // PAGE
        (code if flags & 1 else data).update(range(first, last + 1))
    if code & data:
        found.append("code shares a 64 KiB page with data")
    if info.get("textrel"):
        found.append("it has text relocations (code is never writable)")
    if info["pie"] and info["interp"] is None and not loader:
        # A static PIE relocates itself before main, with no loader's help.
        relocations = info.get("relocations")
        if relocations is None:
            found.append("its relocations cannot be read")
        else:
            writable = sorted((start, end) for start, end, flags in info["segments"] if flags & 2)
            kinds = sorted({kind for _, kind in relocations} - {R_ARM_NONE, R_ARM_RELATIVE})
            if kinds:
                found.append(f"relocations its start code cannot apply (types {', '.join(map(str, kinds))}; only relative ones)")
            stray = sum(1 for address, kind in relocations if kind == R_ARM_RELATIVE
                        and not any(start <= address and address + 4 <= end for start, end in writable))
            if stray:
                found.append(f"{stray} relocation(s) patch memory that is not writable (it would fault as it starts)")
        # Its constant pointer tables must not stay writable after relocation.
        relro = info.get("relro")
        if relro is None:
            found.append("no PT_GNU_RELRO: its vtables and GOT would stay writable (link with -z relro)")
        else:
            start, end = relro
            if not any(s <= start and end <= e for s, e, flags in info["segments"] if flags & 2):
                found.append("PT_GNU_RELRO is not inside one writable segment")
            if end % 0x1000:
                found.append("PT_GNU_RELRO does not end on a 4 KiB page: its last page would stay writable")
            if not info.get("sealed") or not info.get("phdr"):
                found.append("it does not seal its RELRO read-only after relocating (link tools/relro_seal.rs; needs PT_PHDR)")
    return found


def check(files, fixed_ok=(), allow_fixed=False):
    """`files` maps a name to a program's bytes. Raises SystemExit naming
    every program the loader would refuse and every program that is not
    position-independent, unless its name is in `fixed_ok` (or `allow_fixed`:
    a development build, not a release). Returns the names with a fixed base
    that were let through."""
    refused, fixed = [], []
    for name, data in sorted(files.items()):
        info = inspect(data)
        if info is None:
            continue
        why = problems(info, loader=name == LOADER.lstrip("/"))
        if info["interp"] == LOADER and LOADER.lstrip("/") not in files:
            why.append(f"dynamically linked, and the image has no {LOADER}")
        if not info["pie"]:
            if allow_fixed or name in fixed_ok:
                fixed.append(name)
            else:
                why.append("not position-independent (ET_EXEC): its code would be at the same address on every start")
        if why:
            refused.append(f"{name}: {'; '.join(why)}")
    if refused:
        raise SystemExit("the Open Runtime would refuse to load, or this is not a release build:\n  " + "\n  ".join(refused))
    return fixed


if __name__ == "__main__":
    # python3 tools/elf_check.py FILE...: say what each program is.
    import sys
    from pathlib import Path
    bad = 0
    for path in sys.argv[1:]:
        info = inspect(Path(path).read_bytes())
        if info is None:
            print(f"{path}: not a 32-bit ARM executable")
            continue
        why = problems(info) + ([] if info["pie"] else ["not position-independent (ET_EXEC)"])
        bad += bool(why)
        print(f"{path}: {'; '.join(why) if why else 'PIE, loads'}")
    sys.exit(1 if bad else 0)
