"""Build the open apps' file system image: a cpio "newc" archive that QEMU loads
at 0x4a000000 for the Open Runtime. Each folder in apps/open becomes /apps/NAME.
A folder with main.rs is built as a static Rust std program for
armv7-unknown-linux-musleabihf, and one with main.go as a Go program for
linux/arm (GOARM=7); any other files are copied as they are, so a script with
a #! line runs without a build step.
"""
from pathlib import Path
import glob
import os
import re
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
    # Any rustup toolchain under build/ with the target (Ladybird's, for one).
    candidates += sorted(glob.glob(str(ROOT / "build/*/rustup/toolchains/*/bin/rustc")))
    for rustc in filter(None, candidates):
        if not Path(rustc).exists():
            continue
        sysroot = Path(subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip())
        host = next(line.split()[1] for line in subprocess.run([rustc, "-vV"], capture_output=True, text=True).stdout.splitlines()
                    if line.startswith("host:"))
        if (sysroot / "lib/rustlib" / TARGET).is_dir():
            return Path(rustc), lld_for(sysroot, host)
    raise SystemExit(f"Install the Rust target first: rustup target add {TARGET}")


def lld_for(sysroot, host):
    """The toolchain's rust-lld. Newer toolchains link it against the
    libLLVM.dylib in the sysroot's lib folder, which only rustc's own
    invocations find, so add that folder to the dynamic loader's fallback path."""
    lib = str(sysroot / "lib")
    paths = [p for p in os.environ.get("DYLD_FALLBACK_LIBRARY_PATH", "").split(":") if p]
    if lib not in paths:
        os.environ["DYLD_FALLBACK_LIBRARY_PATH"] = ":".join(paths + [lib])
    return sysroot / "lib/rustlib" / host / "bin/rust-lld"


# Both phones' cores are Cortex-A53s (QEMU runs -cpu max): tune for them and
# let rustc vectorize fills, copies and blends with NEON, which the
# armv7-unknown-linux-musleabihf target leaves off (docs/8ms-frame-pipeline-rfc.md).
CPU_FLAGS = ["-C", "target-cpu=cortex-a53", "-C", "target-feature=+neon"]


def pie_flags(rustc):
    """VIOLET_OPEN_PIE=1 links the Rust apps as static position-independent
    executables, which the Open Runtime loads at a random address. rustc has no
    static-PIE mode for this target, so the link is spelled out: musl's
    self-relocating start file (rcrt1.o) in place of crt1.o, and no text
    relocations. Off by default: the result links, and tools/elf_check.py sees
    a PIE, but it has not been run on the emulator or the phone yet."""
    if os.environ.get("VIOLET_OPEN_PIE") != "1":
        return []
    sysroot = Path(subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip())
    crt = sysroot / "lib/rustlib" / TARGET / "lib/self-contained"
    link = ["-pie", "--no-dynamic-linker", "-static", "-z", "text", crt / "rcrt1.o", crt / "crti.o",
            crt / "crtbeginS.o", f"-L{crt}", crt / "crtendS.o", crt / "crtn.o"]
    return ["-C", "relocation-model=pic", "-C", "link-self-contained=no",
            *[part for arg in link for part in ("-C", f"link-arg={arg}")]]


def build_rust(source, out):
    rustc, lld = toolchain()
    subprocess.run([rustc, "--edition=2021", "-O", "-C", "strip=symbols", "--target", TARGET, *CPU_FLAGS,
                    *pie_flags(rustc), "-C", f"linker={lld}", "-C", "linker-flavor=ld.lld", source, "-o", out], check=True)


def build_cargo(work):
    """The Cargo workspace in apps/ (the built-in apps and their shared crate),
    for ARMv7 musl. ring, under rustls, compiles a little C with the NDK's clang."""
    import shutil
    rustc, lld = toolchain()
    cargo = rustc.parent / "cargo"
    if not cargo.exists():
        cargo = Path(shutil.which("cargo") or "cargo")
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bins = list(ndk.glob("toolchains/llvm/prebuilt/*/bin"))
    if not bins:
        raise SystemExit("Install the Android NDK (its clang builds ring for the Rust apps)")
    key = TARGET.replace("-", "_")
    env = dict(os.environ, **{
        f"CC_{key}": str(bins[0] / "clang"),
        f"AR_{key}": str(bins[0] / "llvm-ar"),
        f"CFLAGS_{key}": "--target=armv7-linux-musleabihf -march=armv7-a -mfpu=neon -mfloat-abi=hard",
        f"CARGO_TARGET_{key.upper()}_LINKER": str(lld),
        f"CARGO_TARGET_{key.upper()}_RUSTFLAGS": " ".join(["-C", "linker-flavor=ld.lld", *CPU_FLAGS, *pie_flags(rustc)]),
        "CARGO_TARGET_DIR": str(ROOT / "build/open-cargo"),
        "RUSTC": str(rustc),
    })
    env.setdefault("CARGO_HOME", str(ROOT / "build/cargo-home"))
    # Only the apps the image takes (VIOLET_OPEN_SKIP leaves some out: CI skips the slow ones).
    skip = {n for n in os.environ.get("VIOLET_OPEN_SKIP", "").split(",") if n}
    listed = re.search(r"members\s*=\s*\[(.*?)\]", (ROOT / "apps/Cargo.toml").read_text(), re.S).group(1)
    members = [m.split("/")[-1] for m in re.findall(r'"([^"]+)"', listed)]
    chosen = [arg for m in members if m not in skip for arg in ("-p", m)] if skip else ["--workspace"]
    subprocess.run([cargo, "build", "--release", "--target", TARGET, *chosen], cwd=ROOT / "apps", env=env, check=True)
    return ROOT / "build/open-cargo" / TARGET / "release"


def ladybird_toolchain(version):
    """A rustc at least as new as Ladybird's pinned Rust, with the musl target.
    VIOLET_LADYBIRD_RUSTC names one; otherwise rustup's toolchain of that
    version (in ~/.rustup or in a RUSTUP_HOME under build/), or rustc on PATH."""
    import glob
    import shutil
    wanted = tuple(int(n) for n in version.split("."))
    candidates = [os.environ.get("VIOLET_LADYBIRD_RUSTC")]
    candidates += sorted(glob.glob(str(Path.home() / f".rustup/toolchains/{version}-*/bin/rustc")))
    candidates += sorted(glob.glob(str(ROOT / f"build/*/rustup/toolchains/{version}-*/bin/rustc")))
    candidates += [shutil.which("rustc")]
    for rustc in filter(None, candidates):
        if not Path(rustc).exists():
            continue
        info = subprocess.run([rustc, "-vV"], capture_output=True, text=True).stdout
        release = next((line.split()[1] for line in info.splitlines() if line.startswith("release:")), "0")
        have = tuple(int(n) for n in release.split("-")[0].split(".") if n.isdigit())
        host = next((line.split()[1] for line in info.splitlines() if line.startswith("host:")), "")
        sysroot = Path(subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip())
        if have >= wanted and (sysroot / "lib/rustlib" / TARGET).is_dir():
            return Path(rustc), lld_for(sysroot, host)
    raise SystemExit(f"Ladybird needs Rust {version} with the musl target: "
                     f"rustup toolchain install {version} --target {TARGET}")


def build_ladybird(folder):
    """The Ladybird browser: its own Cargo workspace on Ladybird's Rust HTML
    parser, fetched at a pinned commit, built with Ladybird's Rust version."""
    import shutil
    sys.path.insert(0, str(ROOT / "tools"))
    import fetch_ladybird
    checkout = fetch_ladybird.main()
    version = next(line.split('"')[1] for line in (checkout / "rust-toolchain.toml").read_text().splitlines()
                   if line.strip().startswith("channel"))
    rustc, lld = ladybird_toolchain(version)
    cargo = rustc.parent / "cargo"
    if not cargo.exists():
        cargo = Path(shutil.which("cargo") or "cargo")
    key = TARGET.replace("-", "_")
    # No target RUSTFLAGS: the parser's build script runs cargo metadata for the
    # host, which would inherit them. rustc infers the lld flavor from rust-lld.
    env = dict(os.environ, **{
        f"CARGO_TARGET_{key.upper()}_LINKER": str(lld),
        "CARGO_TARGET_DIR": str(ROOT / "build/ladybird-cargo"),
        "RUSTC": str(rustc),
        # rust-lld finds libLLVM through rustup's proxy, which is bypassed here.
        "DYLD_FALLBACK_LIBRARY_PATH": str(lld.parents[3]),
        "LD_LIBRARY_PATH": str(lld.parents[3]),
    })
    env.setdefault("CARGO_HOME", str(ROOT / "build/cargo-home"))
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bins = list(ndk.glob("toolchains/llvm/prebuilt/*/bin"))
    if bins:
        env.update({f"CC_{key}": str(bins[0] / "clang"), f"AR_{key}": str(bins[0] / "llvm-ar"),
                    f"CFLAGS_{key}": "--target=armv7-linux-musleabihf -march=armv7-a -mfpu=neon -mfloat-abi=hard"})
    subprocess.run([cargo, "build", "--release", "--locked", "--target", TARGET], cwd=folder, env=env, check=True)
    return ROOT / "build/ladybird-cargo" / TARGET / "release/ladybird"


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


# The dynamic loader the Open Runtime runs (runtime/open/elf.rs): musl's libc.so.
LOADER = "lib/ld-musl-armhf.so.1"


def musl_loader():
    """musl's shared libc.so, which is also its dynamic loader, built once
    (out of tree, from the musl source tools/build_python.py fetches) into
    build/open-toolchain/musl-shared. Only images that set VIOLET_OPEN_LDSO=1
    (make test-abi) ship it, as /lib/ld-musl-armhf.so.1."""
    work = ROOT / "build/open-toolchain/musl-shared"
    lib = work / "lib/libc.so"
    if lib.exists():
        return lib
    sys.path.insert(0, str(ROOT / "tools"))
    import build_python
    source = build_python.fetch(build_python.MUSL)
    b = build_python.ndk_bin()
    builtins = next(b.parent.glob("lib/clang/*/lib/linux/libclang_rt.builtins-arm-android.a"))
    tools = {"AR": str(b / "llvm-ar"), "RANLIB": str(b / "llvm-ranlib")}
    work.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CC=f"{b / 'clang'} {build_python.FLAGS}", **tools)
    subprocess.run([source / "configure", "--prefix=/", "--target=arm-linux-musleabihf", "--disable-static"],
                   cwd=work, env=env, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"-j{os.cpu_count()}", *[f"{k}={v}" for k, v in tools.items()], f"LIBCC={builtins}", "lib/libc.so"],
                   cwd=work, check=True, stdout=subprocess.DEVNULL)
    return lib


def build_c_dynamic(folder, out):
    """A C program linked against musl's libc.so, run by /lib/ld-musl-armhf.so.1
    (a folder with a file named "dynamic"; only with VIOLET_OPEN_LDSO=1)."""
    if os.environ.get("VIOLET_OPEN_LDSO") != "1":
        raise SystemExit("dynamically linked programs need VIOLET_OPEN_LDSO=1")
    clang, flags, _, _ = c_toolchain()
    sysroot = ROOT / "build/open-toolchain/sysroot"
    lib = musl_loader()
    builtins = next(Path(clang).parent.parent.glob("lib/clang/*/lib/linux/libclang_rt.builtins-arm-android.a"))
    sources = sorted(str(p) for p in folder.glob("*.c"))
    subprocess.run([clang, *flags, "-O2", "-fPIE", "-pie", "-fuse-ld=lld", "-nostdlib", sysroot / "lib/Scrt1.o",
                    sysroot / "lib/crti.o", *sources, f"-L{lib.parent}", "-lc", builtins, sysroot / "lib/crtn.o",
                    f"-Wl,-dynamic-linker,/{LOADER}", "-o", out], check=True)


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


# An app's manifest: what it may use (runtime/open/policy.rs). "caps:" names
# capabilities, "data:" files directly in /data it shares; '#' starts a comment.
# An app without one gets only its own /data, the screen and touch.
MANIFEST = "manifest.txt"
CAPS = ("net", "audio-out", "mic", "camera", "hci", "wifi-admin", "system")
SHARE = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,22}")
SERVE = ("apps", "lan")


def read_manifest(text, where, others=()):
    """The manifest's lines, checked: a typo here would silently take a power
    away (or, in a shared name, hand another app's files out), so it fails the build."""
    out = []
    for n, line in enumerate(text.splitlines(), 1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        key, sep, rest = line.partition(":")
        words = rest.replace(",", " ").split()
        if not sep or key.strip() not in ("caps", "data", "tmp", "serve"):
            raise SystemExit(f"{where}:{n}: expected 'caps: ...', 'data: ...', 'tmp: ...' or 'serve: ...'")
        if key.strip() == "serve":
            # Who its sockets serve (runtime/open/loopback.rs serve_words):
            # other apps over loopback, and the network on the wildcard.
            bad = [w for w in words if w not in SERVE]
            if bad:
                raise SystemExit(f"{where}:{n}: unknown serve: words {bad} (known: {' '.join(SERVE)})")
        elif key.strip() == "caps":
            bad = [w for w in words if w not in CAPS]
            if bad:
                raise SystemExit(f"{where}:{n}: unknown capabilities {bad} (known: {' '.join(CAPS)})")
        elif key.strip() == "tmp":
            # Reserved /tmp names (.NAME and .NAME.*): only image apps that list them.
            bad = [w for w in words if not (w.startswith(".") and SHARE.fullmatch(w[1:]))]
            if bad:
                raise SystemExit(f"{where}:{n}: a tmp: name starts with '.' (got {bad})")
        else:
            for w in words:
                owner = w.split(".", 1)[0]
                if not SHARE.fullmatch(w) or owner in others:
                    raise SystemExit(f"{where}:{n}: cannot share /data/{w} (a hidden name, a path, or another app's own)")
        out.append(f"{key.strip()}: {' '.join(words)}")
    return out


def pack_manifests(apps):
    """/apps/.manifests: "@NAME", then that app's checked manifest lines."""
    names = {a.name for a in apps}
    text = ""
    for a in sorted(apps, key=lambda a: a.name):
        m = a / MANIFEST
        if m.exists():
            lines = read_manifest(m.read_text(), m, names - {a.name})
            text += f"@{a.name}\n" + "".join(line + "\n" for line in lines)
    return text


def envelope(manifest_text, program):
    """An Agent-built program with its manifest after it (the manifest, its
    u16 LE length, "VMF1"), as runtime/open/policy.rs `envelope` reads it.
    At the end, so the program still starts as itself (the Mac tells a
    Python app by its "#!")."""
    import struct
    body = "".join(line + "\n" for line in read_manifest(manifest_text, "manifest")).encode()
    return program + body + struct.pack("<H", len(body)) + b"VMF1"


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
    # VIOLET_OPEN_EXTRA_APPS adds folders of apps (the Violet Shield demo adds
    # apps/shield-demo); VIOLET_OPEN_SKIP leaves out more apps by name.
    folders = [ROOT / "apps/open"] + [Path(d) for d in os.environ.get("VIOLET_OPEN_EXTRA_APPS", "").split(":") if d]
    skip = set(skip) | {n for n in os.environ.get("VIOLET_OPEN_SKIP", "").split(",") if n}
    apps = sorted((a for d in folders for a in d.iterdir()), key=lambda a: a.name)
    cargo_out = None
    for app in apps:
        if not app.is_dir() or app.name in skip:
            continue
        if app.name == "ladybird":
            try:
                binary = build_ladybird(app)
            except (SystemExit, subprocess.CalledProcessError) as e:
                print(f"skipping ladybird: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        if (app / "Cargo.toml").exists():
            if cargo_out is None:
                try:
                    cargo_out = build_cargo(work)
                except (SystemExit, subprocess.CalledProcessError) as e:
                    print(f"skipping the Rust apps: {e}", file=sys.stderr)
                    cargo_out = False
            if cargo_out:
                files[f"apps/{app.name}"] = ((cargo_out / app.name).read_bytes(), 0o100755)
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
                (build_c_dynamic if (app / "dynamic").exists() else build_c)(app, binary)
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
            if path.is_file() and path != app / MANIFEST:
                rel = f"apps/{app.name}/{path.relative_to(app)}"
                files[rel] = (path.read_bytes(), 0o100755 if os.access(path, os.X_OK) else 0o100644)
    # Apps with an app.txt get an icon on Home under their titles (a script
    # app is a folder: /apps/NAME/main.py).
    built = lambda name: f"apps/{name}" in files or any(k.startswith(f"apps/{name}/") for k in files)
    titles = [f"{a.name}\t{(a / 'app.txt').read_text().strip()}" for a in apps
              if (a / "app.txt").exists() and built(a.name)]
    files["apps/.home"] = ("\n".join(titles).encode() + b"\n", 0o100644)
    # What each app may use (its manifest.txt), in one file only the image provides.
    files["apps/.manifests"] = (pack_manifests([a for a in apps if a.is_dir() and built(a.name)]).encode(), 0o100644)
    # Name lookups go to the Open Runtime's own DNS answerer; certificates for TLS.
    files["etc/resolv.conf"] = (b"nameserver 10.0.0.53\noptions attempts:2 timeout:2\n", 0o100644)
    files["etc/hosts"] = (b"127.0.0.1 localhost\n", 0o100644)
    files["etc/nsswitch.conf"] = (b"hosts: files dns\n", 0o100644)
    files["etc/ssl/certs/ca-certificates.crt"] = (root_certificates(), 0o100644)
    # Python from tools/build_python.py, when it has been built.
    extra = os.environ.get("VIOLET_INITFS_EXTRA")
    # VIOLET_OPEN_NO_PYTHON=1 leaves Python out, as on the handset (make test-agent installs it as a package).
    python = python and os.environ.get("VIOLET_OPEN_NO_PYTHON") != "1"
    if not extra and python and (ROOT / "build/open-toolchain/python-root/bin/python3").exists():
        extra = str(ROOT / "build/open-toolchain/python-root")
    if extra:
        # A prepared tree (for example /bin/python3 and /lib) merged at the root.
        for path in sorted(Path(extra).rglob("*")):
            if path.is_file():
                rel = str(path.relative_to(extra))
                files[rel] = (path.read_bytes(), 0o100755 if os.access(path, os.X_OK) else 0o100644)
    # musl's dynamic loader, for dynamically linked programs (make test-abi).
    if os.environ.get("VIOLET_OPEN_LDSO") == "1":
        files[LOADER] = (musl_loader().read_bytes(), 0o100755)
    # The loader refuses programs that break W^X: fail here, not on the phone.
    # Programs that are not position-independent keep a fixed code address.
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    fixed = elf_check.check({name: data for name, (data, _) in files.items() if name.startswith(("apps/", "bin/", LOADER))})
    if fixed:
        print(f"fixed code address (not PIE; stack, heap and mappings still move): {' '.join(fixed)}", file=sys.stderr)
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
