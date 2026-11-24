"""Build the open apps' file system image: a cpio "newc" archive that QEMU loads
at 0x4a000000 for the Open Runtime. Each folder in apps/open becomes /apps/NAME.
A folder with main.rs is built as a static Rust std program for
armv7-unknown-linux-musleabihf, and one with main.go as a Go program for
linux/arm (GOARM=7); any other files are copied as they are, so a script with
a #! line runs without a build step.
"""
from pathlib import Path
import os
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LIMIT = 0x06000000
TARGET = "armv7-unknown-linux-musleabihf"


def toolchain():
    """rustc with the musl target installed, and the rust-lld that ships with it."""
    import shutil
    candidates = [os.environ.get("VIOLET_HOST_RUSTC"), shutil.which("rustc"),
                  str(ROOT / "build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc")]
    for rustc in filter(None, candidates):
        if not Path(rustc).exists():
            continue
        sysroot = Path(subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip())
        host = next(line.split()[1] for line in subprocess.run([rustc, "-vV"], capture_output=True, text=True).stdout.splitlines()
                    if line.startswith("host:"))
        if (sysroot / "lib/rustlib" / TARGET).is_dir():
            return Path(rustc), sysroot / "lib/rustlib" / host / "bin/rust-lld"
    raise SystemExit(f"Install the Rust target first: rustup target add {TARGET}")


def build_rust(source, out):
    rustc, lld = toolchain()
    subprocess.run([rustc, "--edition=2021", "-O", "-C", "strip=symbols", "--target", TARGET,
                    "-C", f"linker={lld}", "-C", "linker-flavor=ld.lld", source, "-o", out], check=True)


def build_go(folder, out):
    """A standard Go program for linux/arm (ARMv7, hard float), statically linked."""
    import shutil
    go = os.environ.get("VIOLET_GO") or shutil.which("go") or "/opt/homebrew/bin/go"
    if not Path(go).exists():
        raise SystemExit("Install Go to build Go apps (for example: brew install go)")
    env = dict(os.environ, GOOS="linux", GOARCH="arm", GOARM="7", CGO_ENABLED="0",
               GOCACHE=str(out.parent / "go-cache"), GOPATH=str(out.parent / "go-path"), GOFLAGS="-trimpath")
    if not (folder / "go.mod").exists():
        env["GO111MODULE"] = "off"
    subprocess.run([go, "build", "-ldflags=-s -w", "-o", out.resolve(), "."], cwd=folder, env=env, check=True)


def c_toolchain():
    """clang from the Android NDK with the musl sysroot built by tools/build_python.py."""
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bin_dir = next(ndk.glob("toolchains/llvm/prebuilt/*/bin"))
    sysroot = ROOT / "build/open-toolchain/sysroot"
    builtins = next(ndk.glob("toolchains/llvm/prebuilt/*/lib/clang/*/lib/linux/libclang_rt.builtins-arm-android.a"))
    if not (sysroot / "lib/libc.a").exists():
        raise SystemExit("Build the musl sysroot first: python3 tools/build_python.py musl")
    flags = ["--target=armv7-linux-musleabihf", "-march=armv7-a", "-mfpu=vfpv3-d16", "-mfloat-abi=hard",
             f"--sysroot={sysroot}", "-isystem", str(sysroot / "include")]
    link = ["-static", "-fuse-ld=lld", "-nostdlib", str(sysroot / "lib/crt1.o"), str(sysroot / "lib/crti.o")]
    tail = [f"-L{sysroot / 'lib'}", "-lc", str(builtins), str(sysroot / "lib/crtn.o")]
    return bin_dir / "clang", flags, link, tail


def build_c(folder, out):
    """A static C program against musl, from every .c file in the folder."""
    clang, flags, link, tail = c_toolchain()
    sources = sorted(str(p) for p in folder.glob("*.c"))
    subprocess.run([clang, *flags, "-O2", *link, *sources, "-lm", *tail, "-o", out], check=True)


def root_certificates():
    """The build machine's trusted roots: the macOS system store (its
    /etc/ssl/cert.pem is a smaller, older set) or the Linux bundle."""
    keychain = "/System/Library/Keychains/SystemRootCertificates.keychain"
    if Path(keychain).exists():
        pem = subprocess.run(["security", "find-certificate", "-a", "-p", keychain],
                             capture_output=True, check=True).stdout
        if pem.count(b"BEGIN CERTIFICATE") > 50:
            return pem
    for bundle in ("/etc/ssl/certs/ca-certificates.crt", "/etc/ssl/cert.pem"):
        if Path(bundle).is_file():
            return Path(bundle).read_bytes()
    return b""


def entry(name, data, mode, mtime=0):
    name = name.encode() + b"\0"
    fields = [0, mode, 0, 0, 1, mtime, len(data), 0, 0, 0, 0, len(name), 0]
    head = b"070701" + b"".join(b"%08x" % f for f in fields)
    blob = head + name
    blob += bytes(-len(blob) % 4) + data
    return blob + bytes(-len(blob) % 4)


# Test programs left out of the handset image, which must fit its boot partition
# (Python alone is 17 MB); QEMU runs them all.
HANDSET_SKIP = {"gonet", "pyhello", "pynet", "pypaint", "pytone"}


def main(out, python=True, compress=False, skip=()):
    """compress=True writes "VGZ1", the gzip length, then the gzip of the
    archive (the handset carries it compressed in its boot image's ramdisk)."""
    out = Path(out)
    work = out.parent / "initfs-build"
    work.mkdir(parents=True, exist_ok=True)
    files = {}
    for app in sorted((ROOT / "apps/open").iterdir()):
        if not app.is_dir() or app.name in skip:
            continue
        if (app / "main.go").exists():
            binary = work / app.name
            try:
                build_go(app, binary)
            except SystemExit as e:
                print(f"skipping {app.name}: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        if (app / "main.c").exists():
            binary = work / app.name
            try:
                build_c(app, binary)
            except (SystemExit, StopIteration) as e:
                print(f"skipping {app.name}: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        if (app / "main.rs").exists():
            binary = work / app.name
            try:
                build_rust(app / "main.rs", binary)
            except SystemExit as e:
                print(f"skipping {app.name}: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        for path in sorted(app.rglob("*")):
            if path.is_file():
                rel = f"apps/{app.name}/{path.relative_to(app)}"
                files[rel] = (path.read_bytes(), 0o100755 if os.access(path, os.X_OK) else 0o100644)
    # Apps with an app.txt are listed first, under their titles, by the shell.
    titles = [f"{a.name}\t{(a / 'app.txt').read_text().strip()}" for a in sorted((ROOT / "apps/open").iterdir())
              if (a / "app.txt").exists() and f"apps/{a.name}" in files]
    files["apps/.home"] = ("\n".join(titles).encode() + b"\n", 0o100644)
    # Name lookups go to the Open Runtime's own DNS answerer; certificates for TLS.
    files["etc/resolv.conf"] = (b"nameserver 10.0.0.53\noptions attempts:2 timeout:2\n", 0o100644)
    files["etc/hosts"] = (b"127.0.0.1 localhost\n", 0o100644)
    files["etc/nsswitch.conf"] = (b"hosts: files dns\n", 0o100644)
    files["etc/ssl/certs/ca-certificates.crt"] = (root_certificates(), 0o100644)
    # Python from tools/build_python.py, when it has been built.
    extra = os.environ.get("VIOLET_INITFS_EXTRA")
    if not extra and python and (ROOT / "build/open-toolchain/python-root/bin/python3").exists():
        extra = str(ROOT / "build/open-toolchain/python-root")
    if extra:
        # A prepared tree (for example /bin/python3 and /lib) merged at the root.
        for path in sorted(Path(extra).rglob("*")):
            if path.is_file():
                rel = str(path.relative_to(extra))
                files[rel] = (path.read_bytes(), 0o100755 if os.access(path, os.X_OK) else 0o100644)
    blob = bytearray()
    dirs = sorted({str(Path(p).parent) for p in files if str(Path(p).parent) != "."} |
                  {str(q) for p in files for q in Path(p).parents if str(q) != "."})
    for d in dirs:
        blob += entry(d, b"", stat.S_IFDIR | 0o755)
    for name, (data, mode) in sorted(files.items()):
        blob += entry(name, data, mode)
    blob += entry("TRAILER!!!", b"", 0)
    if len(blob) > LIMIT:
        raise SystemExit(f"open apps image is {len(blob)} bytes; the window holds {LIMIT}")
    if compress:
        import gzip, struct
        packed = gzip.compress(bytes(blob), compresslevel=9, mtime=0)
        blob = b"VGZ1" + struct.pack("<I", len(packed)) + packed
    out.write_bytes(blob)
    print(f"open apps image: {len(files)} files, {len(blob)} bytes -> {out}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else ROOT / "build/sel4/qemu/initfs.cpio",
         compress="--gzip" in sys.argv)
