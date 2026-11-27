"""Build Ladybird's pinned FFmpeg release for the VioletOS Open Runtime."""
from pathlib import Path
import os
import shlex
import subprocess

from build_python import WORK, SYSROOT, MUSL, fetch, build_musl, compiler_wrapper, ndk_bin

FFMPEG = ("ffmpeg-9.0.2", "https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz",
          "8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e")


def sysroot(arch):
    return WORK / "sysroot-aarch64" if arch == "aarch64" else SYSROOT


def aarch64_cc():
    """Build the pinned musl out of tree so both ARM sysroots can coexist."""
    root = sysroot("aarch64")
    tools = ndk_bin()
    if not (root / "lib/libc.a").exists():
        source = fetch(MUSL)
        work = WORK / "musl-build-aarch64"
        work.mkdir(exist_ok=True)
        env = dict(os.environ, CC=f"{tools / 'clang'} --target=aarch64-linux-musl",
                   AR=str(tools / "llvm-ar"), RANLIB=str(tools / "llvm-ranlib"))
        with (work / "build.log").open("w") as log:
            commands = ([source / "configure", "--prefix=/", "--target=aarch64-linux-musl", "--disable-shared"],
                        ["make", f"-j{os.cpu_count() or 2}"], ["make", f"DESTDIR={root}", "install"])
            for command in commands:
                subprocess.run(command, cwd=work, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    # FFmpeg's configure needs a compiler that can link executable probes.
    wrapper = WORK / "bin/aarch64-linux-musl-cc"
    wrapper.parent.mkdir(exist_ok=True)
    builtins = next(tools.parent.glob("lib/clang/*/lib/linux/libclang_rt.builtins-aarch64-android.a"))
    cc = shlex.join([str(tools / "clang"), "--target=aarch64-linux-musl", f"--sysroot={root}", "-isystem", str(root / "include")])
    lib = root / "lib"
    link = shlex.join(["-static", "-fuse-ld=lld", "-nostdlib", str(lib / "crt1.o"), str(lib / "crti.o")])
    tail = shlex.join([f"-L{lib}", "-lc", str(builtins), str(lib / "crtn.o")])
    wrapper.write_text(f"""#!/bin/sh
for arg in "$@"; do
    case "$arg" in -c|-E|-S|-M|-MM) exec {cc} "$@";; esac
done
exec {cc} {link} "$@" {tail}
""")
    wrapper.chmod(0o755)
    return wrapper


def build(arch="armv7"):
    if arch not in {"armv7", "aarch64"}:
        raise ValueError(f"unsupported media architecture: {arch}")
    if arch == "armv7" and not (SYSROOT / "lib/libc.a").is_file():
        build_musl()
    source = fetch(FFMPEG)
    work = WORK / (FFMPEG[0] + "-" + arch)
    work.mkdir(exist_ok=True)
    prefix = work / "install"
    stamp = prefix / "built"
    if stamp.exists() and stamp.stat().st_mtime >= Path(__file__).stat().st_mtime:
        return prefix
    bin_dir = ndk_bin()
    cc = aarch64_cc() if arch == "aarch64" else compiler_wrapper()
    cpu = ["--arch=aarch64", "--cpu=cortex-a53"] if arch == "aarch64" else ["--arch=arm", "--cpu=armv7-a", "--extra-cflags=-mfpu=neon"]
    options = [
        "--enable-cross-compile", *cpu, "--target-os=linux",
        f"--cc={cc}", f"--ar={bin_dir / 'llvm-ar'}",
        f"--nm={bin_dir / 'llvm-nm'}", f"--ranlib={bin_dir / 'llvm-ranlib'}",
        f"--strip={bin_dir / 'llvm-strip'}", f"--prefix={prefix}", "--pkg-config=false",
        "--disable-autodetect", "--disable-programs", "--disable-doc", "--disable-network",
        "--disable-pthreads", "--disable-debug", "--disable-everything", "--disable-avdevice", "--disable-avfilter",
        "--enable-demuxer=matroska", "--enable-decoder=vp8,vp9,opus,vorbis",
    ]
    with (work / "build.log").open("w") as log:
        for cmd in ([source / "configure", *options], ["make", f"-j{os.cpu_count() or 2}", "install"]):
            result = subprocess.run(cmd, cwd=work, stdout=log, stderr=subprocess.STDOUT)
            if result.returncode:
                raise SystemExit(f"FFmpeg build failed. See {log.name}")
    stamp.touch()
    return prefix


if __name__ == "__main__":
    print(build())
