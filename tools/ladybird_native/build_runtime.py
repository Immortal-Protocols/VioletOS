"""Build the pinned Ladybird JavaScript CLI as a static AArch64-musl program.

Requires clang/clang++, CMake, Ninja, curl, tar, the Android NDK's ELF tools,
and Ladybird's Rust toolchain with the aarch64-unknown-linux-musl target.
RUSTC and CARGO may select an existing toolchain. js-static accepts upstream -c
expressions. js-smoke evaluates an array reduction with no command-line flags.
Install icudt78l.dat under /usr/share/icu/78.3 for full locale support.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from build_python import ndk_bin

TARGET = "aarch64-unknown-linux-musl"


def run(*args, **kwargs):
    return subprocess.run(list(args), check=True, **kwargs)


def download(entry, directory):
    archive = directory / entry["name"]
    if not archive.exists():
        with TemporaryDirectory(dir=directory) as tmp:
            dest = Path(tmp) / "download"
            run("curl", "-fsSL", "--retry", "2", entry["url"], "-o", dest)
            verify(dest, entry)
            dest.replace(archive)
    else:
        verify(archive, entry)
    return archive


def verify(archive, entry):
    digest = hashlib.sha256()
    with archive.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != entry["sha256"]:
        raise SystemExit(f"Checksum mismatch for {archive}")


def prepare(work):
    cache = work / "downloads"
    cache.mkdir(parents=True, exist_ok=True)
    source = work / "sources"
    manifest = HERE / "sources.json"
    entries = json.loads(manifest.read_text())
    state = hashlib.sha256(manifest.read_bytes()).hexdigest()
    stamp = source / ".prepared"
    if stamp.exists() and stamp.read_text() == state:
        return source
    if source.exists():
        raise SystemExit(f"{source} contains another revision. Pass --build-dir with a new location.")
    with ThreadPoolExecutor(max_workers=4) as workers:
        archives = list(workers.map(lambda item: download(item, cache), entries))
    with TemporaryDirectory(dir=work) as staging:
        tree = Path(staging) / "sources"
        root = tree / "sysroot"
        root.mkdir(parents=True)
        for entry, archive in zip(entries, archives):
            if archive.suffix == ".apk":
                run("tar", "-xf", archive, "-C", root)
                continue
            dest = tree / entry["name"]
            dest.mkdir()
            if "subdir" in entry:
                run("tar", "-xf", archive, "-C", dest, "--strip-components", "2", entry["subdir"])
            else:
                run("tar", "-xf", archive, "-C", dest, "--strip-components", "1")
        patch = tree / "ladybird/Meta/CMake/vcpkg/overlay-ports/zmij/001-explicit-array-sizes.patch"
        run("patch", "-s", "-p1", "-i", patch, cwd=tree / "zmij")
        (tree / ".prepared").write_text(state)
        tree.rename(source)
    return source


def build(work):
    source = prepare(work)
    rustc = os.environ.get("RUSTC", "rustc")
    cargo = os.environ.get("CARGO", "cargo")
    rust_sysroot = Path(subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip())
    tools = ndk_bin()
    c = shutil.which(os.environ.get("CC", "clang"))
    cxx = shutil.which(os.environ.get("CXX", "clang++"))
    if not c or not cxx:
        raise SystemExit("clang and clang++ are required")
    env = dict(os.environ, CARGO_TARGET_DIR=str(work / "cargo"), RUSTC=rustc,
               CC_aarch64_unknown_linux_musl=c, AR_aarch64_unknown_linux_musl=str(tools / "llvm-ar"),
               CFLAGS_aarch64_unknown_linux_musl=f"--target={TARGET}")
    # rust-objcopy on macOS uses the LLVM dylib beside rustc.
    env["DYLD_FALLBACK_LIBRARY_PATH"] = os.pathsep.join(
        filter(None, [env.get("DYLD_FALLBACK_LIBRARY_PATH"), str(rust_sysroot / "lib")]))
    compiled = run(cargo, "build", "--manifest-path", source / "ladybird/Cargo.toml",
                   "-p", "libgc_rust", "-p", "libjs_rust", "-p", "libunicode_rust",
                   "--features", "libunicode_rust/idna", "--target", TARGET, "--release",
                   "--locked", "--no-default-features", "--message-format", "json-render-diagnostics",
                   env=env, text=True, stdout=subprocess.PIPE)
    cpp_build = work / "cpp"
    for line in compiled.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "build-script-executed":
            out = Path(msg["out_dir"])
            for prefix, name in (("libgc_rust-", "LibGC"), ("libunicode_rust-", "LibUnicode")):
                if out.parent.name.startswith(prefix) and TARGET in out.parts:
                    dest = cpp_build / "include" / name
                    dest.mkdir(parents=True, exist_ok=True)
                    header = dest / "RustFFI.h"
                    generated = out / "RustFFI.h"
                    if not header.is_file() or header.read_bytes() != generated.read_bytes():
                        shutil.copy2(generated, header)
    sysroot = source / "sysroot"
    run("cmake", "-G", "Ninja", "-S", HERE, "-B", cpp_build, "-DCMAKE_SYSTEM_NAME=Linux",
        f"-DCMAKE_C_COMPILER={c}", f"-DCMAKE_CXX_COMPILER={cxx}",
        f"-DCMAKE_C_COMPILER_TARGET={TARGET}", f"-DCMAKE_CXX_COMPILER_TARGET={TARGET}",
        f"-DCMAKE_AR={tools / 'llvm-ar'}", f"-DCMAKE_RANLIB={tools / 'llvm-ranlib'}",
        "-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY", "-DCMAKE_BUILD_TYPE=Release",
        f"-DSOURCES_DIR={source}", f"-DCMAKE_SYSROOT={sysroot}")
    run("cmake", "--build", cpp_build, "--parallel", str(min(8, os.cpu_count() or 2)))

    crt = rust_sysroot / "lib/rustlib" / TARGET / "lib/self-contained"
    rustlib = work / "cargo" / TARGET / "release"
    lib = sysroot / "usr/lib"
    for name, filename in (("js-static", "js.c"), ("js-smoke", "js_smoke.c")):
        entry = work / f"{name}.o"
        run(c, f"--target={TARGET}", "-c", HERE / filename, "-o", entry)
        run(tools / "ld.lld", "-static", "-s", "--gc-sections", "-o", work / name,
            crt / "crt1.o", crt / "crti.o", crt / "crtbegin.o", entry, "--start-group",
            *(rustlib / f"lib{name}.a" for name in ["libjs_rust", "libgc_rust", "libunicode_rust"]),
            cpp_build / "libviolet-js-support.a", *(lib / f"lib{name}.a" for name in
                                                  ["icui18n", "icuuc", "icudata", "stdc++"]),
            lib / "gcc/aarch64-alpine-linux-musl/15.2.0/libgcc.a", crt / "libc.a",
            crt / "libunwind.a", "--end-group", crt / "crtend.o", crt / "crtn.o")
    print(f"Built {work / 'js-static'} and {work / 'js-smoke'}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", type=Path, default=HERE.parents[1] / "build/ladybird-native/static")
    build(parser.parse_args().build_dir.resolve())
