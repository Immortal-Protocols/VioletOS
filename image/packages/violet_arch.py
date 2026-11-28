"""The CPU architecture a package build targets: VIOLET_ARCH, default aarch32.

One table (targets/board/board.py ARCHES) says what an architecture means for
the compilers. The default, aarch32, gives exactly the flags the build has
always used. aarch64 builds the same components for the 64-bit supervisor
(targets/qemu64/stack.py); require_components() names what is not complete
there and stops while anything in UNPORTED is left.

What is wired in today: the package builder takes its Rust target from
rust_target(), reads its linked components with sections() (either ELF
class), records the architecture in each package's ABI word
(package_format.py) and stops on require_components(). clang_flags() is the
64-bit-ready form of the loader build's flags; targets/qemu64/stack.py calls it.
"""
import os
from pathlib import Path
import struct
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "targets/board"))
from board import ARCHES  # noqa: E402

DEFAULT = "aarch32"

# What still ties a layer to AArch32 (runtime/arch/README.md has the method).
# The supervisor, memory.h, transport.c and the services are ported: they
# compile and link for aarch64 (services/check_aarch64.py, in make test-unit).
# What is left does not stop the component build: the Open Runtime builds and
# starts, but its loader takes ELF32 programs only, so it refuses every AArch64
# app (elf.rs plan) until its dispatch is widened; the build says so.
UNPORTED = []
APP_ABI_UNPORTED = [
    "runtime/arch/enter64.rs: AArch64 calls whose structures are not converted answer ENOSYS "
    "(recvmmsg, sendmmsg, timer_create, waitid, itimers, sigqueueinfo, preadv/pwritev, pselect6, execveat, clone3...)",
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
    """Stop a component build for an architecture whose components cannot run yet;
    name what runs but is not complete."""
    arch = selected(environ)
    if arch != DEFAULT:
        if UNPORTED:
            raise SystemExit(f"VIOLET_ARCH={arch}: the components are not ported to {arch} yet:\n  " + "\n  ".join(UNPORTED)
                             + "\nBuild the 64-bit bring-up image with: make qemu64-image")
        for line in APP_ABI_UNPORTED:
            print(f"VIOLET_ARCH={arch}: not ported yet: {line}", file=sys.stderr)
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
