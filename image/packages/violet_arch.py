"""The CPU architecture a package build targets: VIOLET_ARCH, default aarch32.

One table (targets/board/board.py ARCHES) says what an architecture means for
the compilers. The default, aarch32, gives exactly the flags the build has
always used. aarch64 is accepted as a name and stops the component build with
the list of what is not ported yet, rather than producing packages a 64-bit
supervisor could not load; the 64-bit bring-up image is built by
targets/qemu64/build.py instead.

What is wired in today: the package builder takes its Rust target from
rust_target() and stops on require_components(). clang_flags() and
sections() are the 64-bit-ready forms of what the loader build and the
builder's ELF32 section reader do now; no build calls them yet.
"""
import os
from pathlib import Path
import struct
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "targets/board"))
from board import ARCHES  # noqa: E402

DEFAULT = "aarch32"

# What still ties each layer to AArch32, measured by compiling the current
# sources for aarch64 (runtime/arch/README.md has the method and the numbers).
UNPORTED = [
    "system/supervisor/supervisor.c and platform/sel4/board/memory.h: one page-table level, 64 KiB and 1 MiB frames, AArch32 registers",
    "runtime/transport.c, services/power/process.rs, runtime/open/sys.rs: CP15 counter reads (runtime/arch has the 64-bit ones)",
    "runtime/open: ELF32 loader, 64 KiB pool pages, AArch32 system call numbers and structure layouts",
    "this builder: ELF32 section parsing and the VPK header carry no architecture",
]


def selected(environ=None):
    """The architecture name from VIOLET_ARCH; an unknown name stops the build."""
    name = (os.environ if environ is None else environ).get("VIOLET_ARCH", DEFAULT) or DEFAULT
    if name not in ARCHES:
        raise SystemExit(f"VIOLET_ARCH={name}: not an architecture this build knows ({', '.join(sorted(ARCHES))})")
    return name


def rust_target(environ=None):
    return ARCHES[selected(environ)]["rust_target"]


def clang_flags(environ=None):
    """The architecture part of the C compiler flags for freestanding code."""
    arch = selected(environ)
    if arch == "aarch32":
        return ["--target=arm-none-eabi", "-mcpu=cortex-a53", "-marm", "-mfloat-abi=soft", "-mno-unaligned-access"]
    return ["--target=aarch64-none-elf", "-mcpu=cortex-a53", "-mgeneral-regs-only"]


def require_components(environ=None):
    """Stop a component build for an architecture whose components cannot run yet."""
    arch = selected(environ)
    if arch != DEFAULT:
        raise SystemExit(f"VIOLET_ARCH={arch}: the components are not ported to {arch} yet:\n  " + "\n  ".join(UNPORTED)
                         + "\nBuild the 64-bit bring-up image with: make qemu64-image")
    return arch


def sections(data):
    """Section headers of an ELF32 or ELF64 file as (name offset, type, flags, address, file offset, size),
    with the index of the section-name string table: the fields the package builder reads, for either class."""
    if data[:4] != b"\x7fELF" or data[5] != 1:
        raise ValueError("not a little-endian ELF file")
    if data[4] == 1:
        off, = struct.unpack_from("<I", data, 32)
        size, count, names = struct.unpack_from("<3H", data, 46)
        rows = [struct.unpack_from("<6I", data, off + i * size) for i in range(count)]
    elif data[4] == 2:
        off, = struct.unpack_from("<Q", data, 40)
        size, count, names = struct.unpack_from("<3H", data, 58)
        rows = [struct.unpack_from("<IIQQQQ", data, off + i * size) for i in range(count)]
    else:
        raise ValueError("unknown ELF class")
    return rows, names
