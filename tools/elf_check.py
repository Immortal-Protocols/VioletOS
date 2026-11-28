"""What the Open Runtime's loader will make of a program (system/supervisor/procmgr.rs).

`inspect` reads an ARM ELF's program headers and dynamic relocations: a
32-bit AArch32 program, or a 64-bit AArch64 one (VIOLET_ARCH=aarch64,
targets/qemu64/stack.py), each against its own loader, relocation types and
hardware page (ARCHES).
`problems` lists what makes the loader refuse it, or what would end it as it
starts: a segment both writable and executable, or code sharing a 64 KiB
window with any other segment (the Open Runtime's W^X unit on both
architectures; on AArch32 the hardware runs the whole page, so constants
there would be code too), or a dynamic interpreter other than
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
# Per architecture: the dynamic loader, the relocation types a static PIE's
# start code applies (none, relative), and the unit of the code/data rule.
# That unit is the Open Runtime's W^X window (runtime/open/vma.rs WINDOW,
# which procmgr.rs load checks with vma::mixed) on both: 64 KiB, though
# AArch64's hardware pages are 4 KiB. So an AArch64 program whose code and
# data sit on different 4 KiB pages of one 64 KiB window is refused here, as
# the loader refuses it.
ARCHES = {
    "aarch32": {"loader": LOADER, "none": R_ARM_NONE, "relative": R_ARM_RELATIVE, "page": PAGE, "word": 4},
    "aarch64": {"loader": "/lib/ld-musl-aarch64.so.1", "none": 0, "relative": 1027, "page": PAGE, "word": 8},
}
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


def elf_arch(data):
    """"aarch32" for a little-endian ELF32 ARM file, "aarch64" for an ELF64
    AArch64 one, else None."""
    if len(data) < 52 or data[:4] != b"\x7fELF" or data[5] != 1:
        return None
    machine = struct.unpack_from("<H", data, 18)[0]
    if data[4] == 1 and machine == 40:
        return "aarch32"
    if data[4] == 2 and machine == 183 and len(data) >= 64:
        return "aarch64"
    return None


def inspect(data, arch=None):
    """None if `data` is not a little-endian ARM ELF executable (of `arch`,
    when given), else {"arch": "aarch32" or "aarch64", "pie": bool,
     "interp": None or the PT_INTERP path, "segments": [(start, end, flags)],
     "textrel": bool, "relocations": [(address, type)] or None if they cannot be read,
     "relro": (start, end) of PT_GNU_RELRO or None, "phdr": has PT_PHDR,
     "sealed": holds SEAL_MARK}."""
    found = elf_arch(data)
    if found is None or arch not in (None, found):
        return None
    kind = struct.unpack_from("<H", data, 16)[0]
    if kind not in (2, 3):
        return None
    wide = found == "aarch64"
    if wide:
        phoff = struct.unpack_from("<Q", data, 32)[0]
        phentsize, phnum = struct.unpack_from("<HH", data, 54)
        least = 56
    else:
        phoff = struct.unpack_from("<I", data, 28)[0]
        phentsize, phnum = struct.unpack_from("<HH", data, 42)
        least = 32
    if phentsize < least or phoff + phentsize * phnum > len(data):
        return None
    segments, interp, files, dynamic, relro, phdr = [], None, [], None, None, False
    for i in range(phnum):
        if wide:
            ptype, flags, offset, vaddr, _paddr, filesz, memsz, _align = struct.unpack_from("<IIQQQQQQ", data, phoff + i * phentsize)
        else:
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
    textrel, relocations = _dynamic(data, dynamic, files, found)
    return {"arch": found, "pie": kind == 3, "interp": interp, "segments": segments, "textrel": textrel, "relocations": relocations,
            "relro": relro, "phdr": phdr, "sealed": SEAL_MARK in data}


def _dynamic(data, dynamic, files, arch="aarch32"):
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
    word = ARCHES[arch]["word"]
    relative = ARCHES[arch]["relative"]
    tags = {}
    for k in range(size // (2 * word)):
        tag, value = struct.unpack_from("<QQ" if word == 8 else "<II", data, offset + 2 * word * k)
        if tag == 0:
            break
        tags.setdefault(tag, value)
    # DT_TEXTREL, or DF_TEXTREL in DT_FLAGS.
    textrel = 22 in tags or bool(tags.get(30, 0) & 4)
    out = []
    # (table, its size, entry size): DT_REL, DT_RELA, DT_JMPREL (REL unless DT_PLTREL says RELA).
    # ELF64: Elf64_Rel is 16 bytes, Elf64_Rela 24, and the type is r_info's low 32 bits.
    rel, rela = (2 * word, 3 * word)
    tables = [(tags.get(17), tags.get(18, 0), rel), (tags.get(7), tags.get(8, 0), rela),
              (tags.get(23), tags.get(2, 0), rela if tags.get(20) == 7 else rel)]
    for table, total, entry in tables:
        if table is None or total == 0:
            continue
        where = at(table, total)
        if where is None or total // entry > MAX_RELOCATIONS:
            return textrel, None
        for k in range(total // entry):
            if word == 8:
                address, info = struct.unpack_from("<QQ", data, where + entry * k)
                out.append((address, info & 0xffffffff))
            else:
                address, info = struct.unpack_from("<II", data, where + entry * k)
                out.append((address, info & 0xff))
    # DT_RELR: packed relative relocations (an address word, then bitmaps of the words after it).
    if tags.get(36) is not None and tags.get(35, 0):
        where = at(tags[36], tags[35])
        if where is None or tags[35] // word > MAX_RELOCATIONS:
            return textrel, None
        base, bits = 0, 8 * word - 1
        for k in range(tags[35] // word):
            entry = struct.unpack_from("<Q" if word == 8 else "<I", data, where + word * k)[0]
            if entry & 1 == 0:
                out.append((entry, relative))
                base = entry + word
            else:
                out += [(base + word * bit, relative) for bit in range(bits) if entry >> (bit + 1) & 1]
                base += word * bits
            if len(out) > MAX_RELOCATIONS:
                return textrel, None
    return textrel, out


def problems(info, loader=False):
    """Why the loader would refuse this program, or why it would end as it
    starts (empty: it loads and its start code can relocate it). `loader`:
    this is musl's dynamic loader itself, which binds its own symbols."""
    found = []
    rules = ARCHES[info.get("arch", "aarch32")]
    loader_path, page, word = rules["loader"], rules["page"], rules["word"]
    if info["interp"] is not None and info["interp"] != loader_path:
        found.append(f"dynamically linked against {info['interp']} (only {loader_path} is supported)")
    if any(flags & 3 == 3 for _, _, flags in info["segments"]):
        found.append("a segment is writable and executable")
    code, data = set(), set()
    for start, end, flags in info["segments"]:
        # The loader rounds a segment out to 4 KiB; the W^X window is 64 KiB.
        first, last = (start & ~0xfff) // page, (((end + 0xfff) & ~0xfff) - 1) // page
        (code if flags & 1 else data).update(range(first, last + 1))
    if code & data:
        found.append(f"code shares a {page // 1024} KiB page with data")
    if info.get("textrel"):
        found.append("it has text relocations (code is never writable)")
    if info["pie"] and info["interp"] is None and not loader:
        # A static PIE relocates itself before main, with no loader's help.
        relocations = info.get("relocations")
        if relocations is None:
            found.append("its relocations cannot be read")
        else:
            writable = sorted((start, end) for start, end, flags in info["segments"] if flags & 2)
            kinds = sorted({kind for _, kind in relocations} - {rules["none"], rules["relative"]})
            if kinds:
                found.append(f"relocations its start code cannot apply (types {', '.join(map(str, kinds))}; only relative ones)")
            stray = sum(1 for address, kind in relocations if kind == rules["relative"]
                        and not any(start <= address and address + word <= end for start, end in writable))
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


def check(files, fixed_ok=(), allow_fixed=False, arch="aarch32"):
    """`files` maps a name to a program's bytes. Raises SystemExit naming
    every program the loader would refuse and every program that is not
    position-independent, unless its name is in `fixed_ok` (or `allow_fixed`:
    a development build, not a release). Returns the names with a fixed base
    that were let through."""
    refused, fixed = [], []
    loader_path = ARCHES[arch]["loader"]
    for name, data in sorted(files.items()):
        info = inspect(data, arch)
        if info is None:
            continue
        why = problems(info, loader=name == loader_path.lstrip("/"))
        if info["interp"] == loader_path and loader_path.lstrip("/") not in files:
            why.append(f"dynamically linked, and the image has no {loader_path}")
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
            print(f"{path}: not an ARM executable")
            continue
        why = problems(info) + ([] if info["pie"] else ["not position-independent (ET_EXEC)"])
        bad += bool(why)
        print(f"{path}: {'; '.join(why) if why else 'PIE, loads'}")
    sys.exit(1 if bad else 0)
