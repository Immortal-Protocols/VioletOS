"""Build musl for AArch64 (static only), for the qemu64 target's C test program.

The same pinned musl release the AArch32 apps use (tools/build_python.py), built
with the same NDK clang for the ARMv8.0 baseline of the handset's Cortex-A53
(no LSE atomics). Everything goes under the qemu64 build directory.
"""
import hashlib
import os
from pathlib import Path
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools"))
FLAGS = "--target=aarch64-linux-musl -march=armv8-a -mno-outline-atomics"


def build(out):
    """Returns (cc command list for compiling, function linking objects into a static program)."""
    from build_python import MUSL, ndk_bin
    name, url, digest = MUSL
    archive = REPO / "build/downloads" / Path(url).name
    if not archive.exists():
        archive.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["curl", "-fLsS", "--retry", "2", url, "-o", archive], check=True)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        sys.exit(f"Unexpected checksum for {archive}")
    work = out / "musl"
    sysroot = work / "sysroot"
    b = ndk_bin()
    if not (sysroot / "lib/libc.a").exists():
        work.mkdir(parents=True, exist_ok=True)
        subprocess.run(["tar", "xf", archive, "-C", work], check=True)
        source = work / name
        env = dict(os.environ, CC=f"{b / 'clang'} {FLAGS}", AR=str(b / "llvm-ar"), RANLIB=str(b / "llvm-ranlib"))
        subprocess.run(["./configure", "--prefix=/", "--target=aarch64-linux-musl", "--disable-shared"],
                       cwd=source, env=env, check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["make", f"-j{os.cpu_count()}", f"AR={env['AR']}", f"RANLIB={env['RANLIB']}"],
                       cwd=source, check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["make", f"DESTDIR={sysroot}", "install"], cwd=source, check=True, stdout=subprocess.DEVNULL)
    builtins = next(b.parent.glob("lib/clang/*/lib/linux/libclang_rt.builtins-aarch64-android.a"))
    cc = [str(b / "clang"), *FLAGS.split(), f"--sysroot={sysroot}", "-isystem", str(sysroot / "include"), "-O2", "-fno-pic"]
    lib = sysroot / "lib"

    def link(objects, output):
        # Fixed address, fully static, 4 KiB pages; no page is both writable and executable.
        subprocess.run([str(b / "ld.lld"), "-static", "-z", "max-page-size=4096", "-z", "separate-code",
                        "--image-base=0x400000", str(lib / "crt1.o"), str(lib / "crti.o"), *map(str, objects),
                        f"-L{lib}", "-lc", str(builtins), str(lib / "crtn.o"), "-o", str(output)], check=True)

    return cc, link
