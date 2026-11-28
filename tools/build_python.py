"""Cross-build the open apps' C library and Python for ARMv7 Linux (musl, hard float).

    python3 tools/build_python.py musl      # build/open-toolchain/sysroot
    python3 tools/build_python.py python    # build/open-toolchain/python-root

The second step produces a tree for the /apps image: a static /bin/python3 and
the standard library under /lib/python3.12. Point VIOLET_INITFS_EXTRA at it
when building the image (make qemu-image does this when the tree exists).
Downloads are pinned by SHA-256, as for the rest of the build. The Python build
needs a host Python of the same version (3.12) to run its build steps.
"""
from pathlib import Path
import hashlib
import os
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / "build/open-toolchain"
SYSROOT = WORK / "sysroot"
PYROOT = WORK / "python-root"
MUSL = ("musl-1.2.5", "https://musl.libc.org/releases/musl-1.2.5.tar.gz",
        "a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4")
PYTHON = ("Python-3.12.16", "https://www.python.org/ftp/python/3.12.16/Python-3.12.16.tgz",
          "710c3e75dac8b2f258d9cd0f76e5c4c2e9175fac9e249c98901552f7cf33b7dd")
ZLIB = ("zlib-1.3.1", "https://zlib.net/fossils/zlib-1.3.1.tar.gz",
        "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23")
FLAGS = "--target=armv7-linux-musleabihf -march=armv7-a -mfpu=vfpv3-d16 -mfloat-abi=hard"


def ndk_bin():
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    return next(ndk.glob("toolchains/llvm/prebuilt/*/bin"))


def fetch(package):
    name, url, digest = package
    archive = ROOT / "build/downloads" / Path(url).name
    archive.parent.mkdir(parents=True, exist_ok=True)
    if not archive.exists():
        subprocess.run(["curl", "-fLsS", "--retry", "2", url, "-o", archive], check=True)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        archive.unlink()
        sys.exit(f"Unexpected download checksum for {name}")
    source = WORK / name
    if not source.is_dir():
        WORK.mkdir(parents=True, exist_ok=True)
        subprocess.run(["tar", "xf", archive, "-C", WORK], check=True)
    return source


def build_musl():
    source = fetch(MUSL)
    b = ndk_bin()
    env = dict(os.environ, CC=f"{b / 'clang'} {FLAGS}", AR=str(b / "llvm-ar"), RANLIB=str(b / "llvm-ranlib"))
    subprocess.run(["./configure", "--prefix=/", "--target=arm-linux-musleabihf", "--disable-shared"],
                   cwd=source, env=env, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"-j{os.cpu_count()}", f"AR={env['AR']}", f"RANLIB={env['RANLIB']}"],
                   cwd=source, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"DESTDIR={SYSROOT}", "install"], cwd=source, check=True, stdout=subprocess.DEVNULL)
    print("musl sysroot:", SYSROOT)


def compiler_wrapper():
    """A cc that compiles for the target and links fully static against musl:
    a static position-independent executable (musl's self-relocating rcrt1.o,
    no text relocations), which the Open Runtime loads at a random address,
    with its RELRO sealed read-only before main (tools/relro_seal.rs).
    VIOLET_OPEN_PIE=0 links at a fixed address instead, as tools/build_initfs.py
    does (a development build only)."""
    b = ndk_bin()
    builtins = next(b.parent.glob("lib/clang/*/lib/linux/libclang_rt.builtins-arm-android.a"))
    wrapper = WORK / "bin/arm-linux-musleabihf-cc"
    wrapper.parent.mkdir(parents=True, exist_ok=True)
    lib = SYSROOT / "lib"
    pie = not fixed_allowed()
    if pie:
        sys.path.insert(0, str(ROOT / "tools"))
        import build_initfs
        compile_flags, link_flags, start = "-fPIE", f"-static-pie -Wl,-z,text -Wl,-z,relro {build_initfs.relro_seal()}", "rcrt1.o"
    else:
        compile_flags, link_flags, start = "", "-static", "crt1.o"
    text = f"""#!/bin/sh
# Compile for ARMv7 Linux (musl); link statically with musl's start files.
cc="{b / 'clang'} {FLAGS} {compile_flags} --sysroot={SYSROOT} -isystem {SYSROOT / 'include'}"
for a in "$@"; do
  case "$a" in -c|-E|-S|-M|-MM) exec $cc "$@";; esac
done
exec $cc {link_flags} -fuse-ld=lld -nostdlib {lib / start} {lib / 'crti.o'} "$@" -L{lib} -lc {builtins} {lib / 'crtn.o'}
"""
    wrapper.write_text(text)
    wrapper.chmod(0o755)
    return wrapper


def fixed_allowed():
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    return elf_check.fixed_allowed()


def stale(binary, pie):
    """Whether the linked interpreter at `binary` was linked by other rules than
    this build's (PIE or not, RELRO sealed or not): it is relinked then. Asked
    of the binary itself, so an interrupted run cannot leave it behind."""
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    if not binary.is_file():
        return False
    info = elf_check.inspect(binary.read_bytes())
    return info is None or info["pie"] != pie or (pie and not info["sealed"])


def host_python():
    for candidate in (os.environ.get("VIOLET_HOST_PYTHON312"), "/opt/homebrew/opt/python@3.12/bin/python3.12",
                      shutil.which("python3.12")):
        if candidate and Path(candidate).exists():
            return candidate
    sys.exit("Install Python 3.12 on the Mac (brew install python@3.12) to cross-build Python 3.12")


def build_zlib():
    source = fetch(ZLIB)
    b = ndk_bin()
    env = dict(os.environ, CC=f"{b / 'clang'} {FLAGS} --sysroot={SYSROOT} -isystem {SYSROOT / 'include'}",
               AR=str(b / "llvm-ar"), RANLIB=str(b / "llvm-ranlib"), CHOST="arm-linux-musleabihf")
    subprocess.run(["./configure", "--prefix=/", "--static"], cwd=source, env=env, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"-j{os.cpu_count()}", "libz.a"], cwd=source, env=env, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"DESTDIR={SYSROOT}", "install"], cwd=source, env=env, check=True, stdout=subprocess.DEVNULL)


def build_python():
    if not (SYSROOT / "lib/libc.a").exists():
        build_musl()
    if not (SYSROOT / "lib/libz.a").exists():
        build_zlib()
    source = fetch(PYTHON)
    cc = compiler_wrapper()
    b = ndk_bin()
    build = WORK / "python-build"
    build.mkdir(exist_ok=True)
    site = build / "config.site"
    # Answers configure cannot find out by running target programs.
    # Modules a phone app does not need, left out so Python fits the handset's app
    # space (tools/build_python_package.py): the CJK codecs alone are over a megabyte.
    # _posixsubprocess stays: subprocess (and so asyncio) imports it, and the
    # Open Runtime runs fork, vfork and execve.
    unused = ["_codecs_cn", "_codecs_hk", "_codecs_iso2022", "_codecs_jp", "_codecs_kr", "_codecs_tw",
              "_multibytecodec", "_xxsubinterpreters", "_xxinterpchannels", "_lsprof", "spwd", "syslog",
              "termios", "_posixshmem", "_multiprocessing", "_crypt", "audioop"]
    answers = ("ac_cv_file__dev_ptmx=no\nac_cv_file__dev_ptc=no\nac_cv_buggy_getaddrinfo=no\n" +
               "".join(f"py_cv_module_{m}=n/a\n" for m in unused))
    # Configure again when the answers change (a module added or left out).
    if not site.exists() or site.read_text() != answers:
        (build / "Makefile").unlink(missing_ok=True)
    site.write_text(answers)
    # pkg-config sees only the target's libraries, never the Mac's.
    env = dict(os.environ, CC=str(cc), AR=str(b / "llvm-ar"), RANLIB=str(b / "llvm-ranlib"),
               READELF=str(b / "llvm-readelf"), CONFIG_SITE=str(site),
               PKG_CONFIG_LIBDIR=str(SYSROOT / "lib/pkgconfig"), PKG_CONFIG_PATH="",
               PKG_CONFIG_SYSROOT_DIR=str(SYSROOT))
    if not (build / "Makefile").exists():
        subprocess.run([source / "configure", "--host=arm-linux-musleabihf", "--build=aarch64-apple-darwin",
                        "--prefix=/", f"--with-build-python={host_python()}", "--disable-shared",
                        "--without-ensurepip", "--disable-test-modules", "--with-pymalloc",
                        "MODULE_BUILDTYPE=static"], cwd=build, env=env, check=True)
    # The objects are position-independent either way (clang's default for
    # this target); only the link differs, so a binary linked otherwise goes.
    for name in ("python", "python.exe"):   # python.exe on a case-insensitive disk
        if stale(build / name, not fixed_allowed()):
            (build / name).unlink()
    subprocess.run(["make", f"-j{os.cpu_count()}", "python"], cwd=build, env=env, check=True)
    if PYROOT.exists():
        shutil.rmtree(PYROOT)
    subprocess.run(["make", f"DESTDIR={PYROOT}", "libinstall", "bininstall"], cwd=build, env=env, check=True,
                   stdout=subprocess.DEVNULL)
    lib = PYROOT / "lib/python3.12"
    # Tests, IDLE and the Tk and Windows support are not needed on the phone.
    for name in ("test", "idlelib", "tkinter", "turtledemo", "ensurepip", "lib2to3", "pydoc_data"):
        shutil.rmtree(lib / name, ignore_errors=True)
    for cache in list(lib.rglob("__pycache__")):
        shutil.rmtree(cache, ignore_errors=True)
    for extra in ("lib/pkgconfig", "share", "include"):
        shutil.rmtree(PYROOT / extra, ignore_errors=True)
    for archive in (PYROOT / "lib").glob("*.a"):
        archive.unlink()
    for config in lib.glob("config-3.12-*"):
        shutil.rmtree(config, ignore_errors=True)
    shutil.rmtree(lib / "lib-dynload", ignore_errors=True)
    python = PYROOT / "bin/python3"
    for link in list((PYROOT / "bin").iterdir()):
        if link.name != "python3.12":
            link.unlink()
    (PYROOT / "bin/python3.12").rename(python)
    subprocess.run([b / "llvm-strip", python], check=True)
    # What the image build will check again: a PIE the loader takes, whose
    # start code can relocate it (fail here, minutes earlier).
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    elf_check.check({"bin/python3": python.read_bytes()}, allow_fixed=elf_check.fixed_allowed())
    size = sum(p.stat().st_size for p in PYROOT.rglob("*") if p.is_file())
    print(f"Python tree: {PYROOT} ({size >> 20} MiB)")


if __name__ == "__main__":
    step = sys.argv[1] if len(sys.argv) > 1 else "python"
    {"musl": build_musl, "zlib": build_zlib, "python": build_python}[step]()
