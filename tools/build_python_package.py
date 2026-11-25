#!/usr/bin/env python3
"""Python for the phone as one installable package (runtime/open/appstore.rs).

    python3 tools/build_python_package.py [OUT]   # default build/python-package.vgz

The handset image has no room for Python, so the agent installs it the first
time the user asks for a Python app. The package is "VGZ1", the gzip length
and the gzip of a cpio archive holding:

- /bin/python3: CPython 3.12, static for armv7 musl (tools/build_python.py python)
- /lib/python312.zip: the standard library, compiled to .pyc on this Mac
  (Homebrew's python3.12, the same release), minus what cannot run on the phone
- /lib/python3.12/violet.py and violet_font.bin: the phone's API for Python apps
  (apps/lib/python) and the Go fonts baked at fixed sizes

About 5 MB, unpacked into /bin and /lib at every start.
"""
import gzip
import io
import os
import py_compile
import stat
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import build_initfs  # noqa: E402

PYTHON_ROOT = ROOT / "build/open-toolchain/python-root"
FONT = ROOT / "build/violet-font.bin"
HOST_PYTHON = os.environ.get("VIOLET_HOST_PYTHON", "/opt/homebrew/bin/python3.12")
# Parts of the standard library that need what the phone lacks (a terminal,
# Tk, a compiler, pip) or that only test CPython.
LEAVE_OUT = {"idlelib", "tkinter", "turtledemo", "test", "ensurepip", "lib2to3", "venv", "site-packages",
             "pydoc_data", "curses", "dbm", "sqlite3", "__phello__", "turtle.py", "idle.py",
             # No processes, consoles, mail or debuggers on the phone.
             "multiprocessing", "concurrent", "unittest", "doctest.py", "pdb.py", "pydoc.py", "bdb.py", "profile.py",
             "cProfile.py", "pstats.py", "trace.py", "tracemalloc.py", "timeit.py", "pty.py", "tty.py", "getpass.py",
             "imaplib.py", "nntplib.py", "poplib.py", "smtplib.py", "ftplib.py", "telnetlib.py", "mailbox.py",
             "mailcap.py", "xmlrpc", "wsgiref", "cgi.py", "cgitb.py", "webbrowser.py", "subprocess.py", "venv",
             "ctypes", "_pydecimal.py", "pydoc.py", "lzma.py", "bz2.py", "crypt.py", "msilib", "zipapp.py",
             "tabnanny.py", "pyclbr.py", "modulefinder.py", "symtable.py", "compileall.py", "aifc.py", "sunau.py",
             "sndhdr.py", "chunk.py", "uu.py", "xdrlib.py", "nturl2path.py", "_aix_support.py", "_osx_support.py",
             "antigravity.py", "this.py", "__hello__.py", "asyncio"}
# Text encodings beyond these are rare in an app; the rest of encodings/ is 1.7 MB.
ENCODINGS = {"__init__.py", "aliases.py", "utf_8.py", "utf_8_sig.py", "ascii.py", "latin_1.py", "utf_16.py",
             "utf_16_le.py", "utf_16_be.py", "utf_32.py", "utf_32_le.py", "utf_32_be.py", "idna.py", "punycode.py",
             "raw_unicode_escape.py", "unicode_escape.py", "charmap.py", "cp1252.py", "cp437.py", "base64_codec.py",
             "hex_codec.py", "zlib_codec.py"}


def font():
    """The glyph atlas from apps/lib/violet/examples/font_atlas.rs."""
    if not FONT.exists():
        rustc, _ = build_initfs.toolchain()
        env = dict(os.environ, RUSTC=str(rustc), CARGO_HOME=str(ROOT / "build/cargo-home"))
        subprocess.run([str(rustc.parent / "cargo"), "run", "-q", "--release", "--example", "font_atlas", "-p", "violet",
                        "--target-dir", str(ROOT / "build/open-cargo-host"), "--", str(FONT)],
                       cwd=ROOT / "apps", env=env, check=True)
    return FONT.read_bytes()


def stdlib_zip():
    """The standard library as sourceless .pyc files in a zip (zipimport reads them)."""
    lib = PYTHON_ROOT / "lib/python3.12"
    check = subprocess.run([HOST_PYTHON, "-c", "import sys; print(sys.version.split()[0])"], capture_output=True, text=True)
    version = check.stdout.strip()
    if not version.startswith("3.12."):
        raise SystemExit(f"Need Python 3.12 on this Mac to compile the library (brew install python@3.12); {HOST_PYTHON} is {version or 'missing'}")
    def wanted(p):
        parts = p.relative_to(lib).parts
        if any(part in LEAVE_OUT for part in parts):
            return False
        return parts[0] != "encodings" or parts[-1] in ENCODINGS
    sources = [p for p in sorted(lib.rglob("*.py")) if wanted(p)]
    buf = io.BytesIO()
    with tempfile.TemporaryDirectory() as tmp:
        # Compile with the Mac's 3.12 (the same bytecode as the phone's), one process.
        listing = Path(tmp) / "files.txt"
        listing.write_text("\n".join(f"{p}\t{Path(tmp) / p.relative_to(lib)}c\t/lib/python3.12/{p.relative_to(lib)}" for p in sources))
        script = ("import py_compile,sys\n"
                  "for line in open(sys.argv[1]).read().splitlines():\n"
                  "    src, out, shown = line.split('\\t')\n"
                  "    try:\n"
                  "        py_compile.compile(src, out, shown, doraise=True, optimize=2,\n"
                  "            invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)\n"
                  "    except py_compile.PyCompileError as e:\n"
                  "        print('skipped', shown, e.msg.splitlines()[-1], file=sys.stderr)\n")
        subprocess.run([HOST_PYTHON, "-c", script, str(listing)], check=True)
        with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for p in sources:
                pyc = Path(tmp) / f"{p.relative_to(lib)}c"
                if pyc.exists():
                    z.write(pyc, f"{p.relative_to(lib)}c")
    return buf.getvalue()


def archive():
    binary = PYTHON_ROOT / "bin/python3"
    if not binary.exists():
        raise SystemExit("Build Python for the phone first: python3 tools/build_python.py python")
    stdlib = stdlib_zip()
    files = {
        "bin/python3": (binary.read_bytes(), 0o100755),
        "lib/python312.zip": (stdlib, 0o100644),
        # The landmark CPython looks for to find its library (the code is in the zip).
        "lib/python3.12/os.py": (b"", 0o100644),
        "lib/python3.12/lib-dynload/.keep": (b"", 0o100644),
        "lib/python3.12/violet.py": ((ROOT / "apps/lib/python/violet.py").read_bytes(), 0o100644),
        "lib/python3.12/violet_font.bin": (font(), 0o100644),
    }
    blob = bytearray()
    dirs = sorted({str(q) for p in files for q in Path(p).parents if str(q) != "."})
    for d in dirs:
        blob += build_initfs.entry(d, b"", stat.S_IFDIR | 0o755)
    for name, (data, mode) in sorted(files.items()):
        blob += build_initfs.entry(name, data, mode)
    blob += build_initfs.entry("TRAILER!!!", b"", 0)
    packed = gzip.compress(bytes(blob), compresslevel=9, mtime=0)
    return b"VGZ1" + struct.pack("<I", len(packed)) + packed, len(blob)


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "build/python-package.vgz"
    data, raw = archive()
    out.write_bytes(data)
    print(f"python package: {len(data)} bytes ({raw} unpacked) -> {out}")


if __name__ == "__main__":
    main()
