"""Build the open apps' file system image: a cpio "newc" archive that QEMU loads
at 0x4a000000 for the Open Runtime. Each folder in apps/open becomes /apps/NAME.
A folder with main.rs is built as a static Rust std program for
armv7-unknown-linux-musleabihf, and one with main.go as a Go program for
linux/arm (GOARM=7); any other files are copied as they are, so a script with
a #! line runs without a build step.

VIOLET_ARCH=aarch64 builds the same image for AArch64 programs instead
(aarch64-unknown-linux-musl, Go linux/arm64, C against the AArch64 musl that
tools/build_python.py builds, Python from its AArch64 tree), for the AArch64
system's image (targets/qemu64/stack.py). ARCHES below holds every
architecture-specific choice; the AArch32 ones are the build's original values.
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

# Both phones' cores are Cortex-A53s (QEMU runs -cpu max): tune for them and
# let rustc vectorize fills, copies and blends with NEON, which the
# armv7-unknown-linux-musleabihf target leaves off (docs/8ms-frame-pipeline-rfc.md).
CPU_FLAGS = ["-C", "target-cpu=cortex-a53", "-C", "target-feature=+neon"]

# Each architecture's app build. NEON is part of AArch64 (no feature to add);
# its atomics stay ARMv8.0 load/store-exclusive (no LSE, which the A53 lacks,
# and no run-time HWCAP probe), as for its C library (tools/build_python.py).
ARCHES = {
    "aarch32": {"target": TARGET, "cpu": CPU_FLAGS,
                "cflags": "--target=armv7-linux-musleabihf -march=armv7-a -mfpu=neon -mfloat-abi=hard",
                "c_flags": ["--target=armv7-linux-musleabihf", "-march=armv7-a", "-mfpu=vfpv3-d16", "-mfloat-abi=hard"],
                "go": {"GOARCH": "arm", "GOARM": "7"}, "loader": "lib/ld-musl-armhf.so.1",
                "seal": ROOT / "build/open-toolchain/relro-seal.o", "elf": (1, 40)},
    "aarch64": {"target": "aarch64-unknown-linux-musl",
                "cpu": ["-C", "target-cpu=cortex-a53", "-C", "target-feature=-outline-atomics"],
                "cflags": "--target=aarch64-linux-musl -march=armv8-a -mcpu=cortex-a53 -mno-outline-atomics",
                "c_flags": ["--target=aarch64-linux-musl", "-march=armv8-a", "-mcpu=cortex-a53", "-mno-outline-atomics"],
                "go": {"GOARCH": "arm64"}, "loader": "lib/ld-musl-aarch64.so.1",
                "seal": ROOT / "build/open-toolchain/aarch64/relro-seal.o", "elf": (2, 183)},
}


def arch_of(arch=None):
    """`arch`, or VIOLET_ARCH (aarch64 unless set)."""
    if arch is None:
        sys.path.insert(0, str(ROOT / "image/packages"))
        import violet_arch
        arch = violet_arch.selected()
    return arch


def app(arch=None):
    return ARCHES[arch_of(arch)]


def toolchain(target=TARGET):
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
        if (sysroot / "lib/rustlib" / target).is_dir():
            return Path(rustc), lld_for(sysroot, host)
    raise SystemExit(f"Install the Rust target first: rustup target add {target}"
                     + (" (or python3 targets/qemu64/toolchain.py)" if target != TARGET else ""))


def lld_for(sysroot, host):
    """The toolchain's rust-lld. Newer toolchains link it against the
    libLLVM.dylib in the sysroot's lib folder, which only rustc's own
    invocations find, so add that folder to the dynamic loader's fallback path."""
    lib = str(sysroot / "lib")
    paths = [p for p in os.environ.get("DYLD_FALLBACK_LIBRARY_PATH", "").split(":") if p]
    if lib not in paths:
        os.environ["DYLD_FALLBACK_LIBRARY_PATH"] = ":".join(paths + [lib])
    return sysroot / "lib/rustlib" / host / "bin/rust-lld"


def pie():
    """Whether programs are built position-independent (the default; VIOLET_OPEN_PIE=0 turns it off)."""
    return os.environ.get("VIOLET_OPEN_PIE") != "0"


def relro_seal(out=None, arch="aarch32"):
    """tools/relro_seal.rs as an object for every static PIE's link (its
    .init_array entry makes PT_GNU_RELRO read-only before main), built with
    the apps' rustc when the source is newer than the object. An object with
    no .init_array entry seals nothing, so the build stops rather than link it."""
    a = ARCHES[arch]
    source = ROOT / "tools/relro_seal.rs"
    out = Path(out or a["seal"])
    if not out.exists() or out.stat().st_mtime < source.stat().st_mtime:
        rustc, _ = toolchain(a["target"])
        out.parent.mkdir(parents=True, exist_ok=True)
        part = out.with_name(f"{out.name}.{os.getpid()}")
        subprocess.run([rustc, "--edition=2021", "--crate-type=lib", "--emit=obj", "-C", "opt-level=2",
                        "-C", "panic=abort", "-C", "relocation-model=pic", "-C", "codegen-units=1",
                        "--target", a["target"], source, "-o", part], check=True)
        if arch != "aarch32" and not seals(part.read_bytes()):
            part.unlink()
            raise SystemExit(f"tools/relro_seal.rs built for {a['target']} has no .init_array entry: "
                             "it would leave every program's RELRO writable. Port its seal to this architecture first.")
        part.replace(out)
    return out


def seals(obj):
    """Whether a relocatable object has a non-empty .init_array (ELF32 or ELF64)."""
    sys.path.insert(0, str(ROOT / "image/packages"))
    import violet_arch
    rows, names = violet_arch.sections(obj)
    strings = obj[rows[names][4]:rows[names][4] + rows[names][5]]
    return any(strings[r[0]:].split(b"\0")[0].startswith(b".init_array") and r[5] for r in rows)


def pie_flags(rustc, seal=None, arch="aarch32"):
    """The Rust apps are linked as static position-independent executables,
    which the Open Runtime loads at a random address. rustc has no static-PIE
    mode for this target, so the link is spelled out: musl's self-relocating
    start file (rcrt1.o) in place of crt1.o, and no text relocations.
    tools/elf_check.py then refuses any program in the image that is not a
    PIE. VIOLET_OPEN_PIE=0 goes back to fixed-address programs (a development
    build: the image build says so)."""
    if not pie():
        return []
    sysroot = Path(subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip())
    crt = sysroot / "lib/rustlib" / ARCHES[arch]["target"] / "lib/self-contained"
    # -z relro: constant pointer tables in PT_GNU_RELRO, which relro_seal.o
    # makes read-only once rcrt1 has relocated them (tools/relro_seal.rs).
    link = ["-pie", "--no-dynamic-linker", "-static", "-z", "text", "-z", "relro", crt / "rcrt1.o", crt / "crti.o",
            crt / "crtbeginS.o", f"-L{crt}", seal or relro_seal(arch=arch), crt / "crtendS.o", crt / "crtn.o"]
    return ["-C", "relocation-model=pic", "-C", "link-self-contained=no",
            *[part for arg in link for part in ("-C", f"link-arg={arg}")]]


def build_rust(source, out, arch="aarch32"):
    a = ARCHES[arch]
    rustc, lld = toolchain(a["target"])
    subprocess.run([rustc, "--edition=2021", "-O", "-C", "strip=symbols", "--target", a["target"], *a["cpu"],
                    *pie_flags(rustc, arch=arch), "-C", f"linker={lld}", "-C", "linker-flavor=ld.lld", source, "-o", out], check=True)


def build_cargo(work, arch="aarch32"):
    """The Cargo workspace in apps/ (the built-in apps and their shared crate),
    for ARMv7 (or AArch64) musl. ring, under rustls, compiles a little C with the NDK's clang."""
    import shutil
    a = ARCHES[arch]
    TARGET = a["target"]
    rustc, lld = toolchain(TARGET)
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
        f"CFLAGS_{key}": a["cflags"],
        f"CARGO_TARGET_{key.upper()}_LINKER": str(lld),
        f"CARGO_TARGET_{key.upper()}_RUSTFLAGS": " ".join(["-C", "linker-flavor=ld.lld", *a["cpu"], *pie_flags(rustc, arch=arch)]),
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


def ladybird_toolchain(version, TARGET=TARGET):
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


def build_ladybird(folder, arch="aarch32"):
    """The Ladybird browser: its own Cargo workspace on Ladybird's Rust HTML
    parser, fetched at a pinned commit, built with Ladybird's Rust version."""
    import shutil
    a = ARCHES[arch]
    TARGET = a["target"]
    sys.path.insert(0, str(ROOT / "tools"))
    import fetch_ladybird
    checkout = fetch_ladybird.main()
    version = next(line.split('"')[1] for line in (checkout / "rust-toolchain.toml").read_text().splitlines()
                   if line.strip().startswith("channel"))
    rustc, lld = ladybird_toolchain(version, TARGET)
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
                    f"CFLAGS_{key}": a["cflags"]})
    # The static-PIE link goes to the program's own rustc call only (cargo
    # rustc), not into RUSTFLAGS; the crates under it are position-independent as built.
    build = ["rustc", "--release", "--locked", "--target", TARGET, "--bin", "ladybird", "--", *map(str, pie_flags(rustc, arch=arch))] if pie() \
        else ["build", "--release", "--locked", "--target", TARGET]
    subprocess.run([cargo, *build], cwd=folder, env=env, check=True)
    return ROOT / "build/ladybird-cargo" / TARGET / "release/ladybird"


def build_go(folder, out, arch="aarch32"):
    """A standard Go program for linux/arm (ARMv7, hard float) or linux/arm64, statically linked."""
    import shutil
    go = os.environ.get("VIOLET_GO") or shutil.which("go") or "/opt/homebrew/bin/go"
    if not Path(go).exists():
        raise SystemExit("Install Go to build Go apps (for example: brew install go)")
    env = dict(os.environ, GOOS="linux", **ARCHES[arch]["go"], CGO_ENABLED="0",
               GOCACHE=str(out.parent / "go-cache"), GOPATH=str(out.parent / "go-path"), GOFLAGS="-trimpath")
    if not (folder / "go.mod").exists():
        env["GO111MODULE"] = "off"
    subprocess.run([go, "build", "-ldflags=-s -w", "-o", out.resolve(), "."], cwd=folder, env=env, check=True)


def c_toolchain(arch="aarch32"):
    """clang from the Android NDK with the musl sysroot built by tools/build_python.py."""
    sys.path.insert(0, str(ROOT / "tools"))
    import build_python
    t = build_python.TARGETS[arch]
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bin_dir = next(ndk.glob("toolchains/llvm/prebuilt/*/bin"))
    sysroot = t.sysroot
    builtins = next(ndk.glob(f"toolchains/llvm/prebuilt/*/lib/clang/*/lib/linux/{t.builtins}"))
    if not (sysroot / "lib/libc.a").exists():
        raise SystemExit("Build the musl sysroot first: " + f"VIOLET_ARCH={arch} "
                         + "python3 tools/build_python.py musl")
    flags = [*ARCHES[arch]["c_flags"], f"--sysroot={sysroot}", "-isystem", str(sysroot / "include")]
    link = ["-static", "-fuse-ld=lld", "-nostdlib", str(sysroot / "lib/crt1.o"), str(sysroot / "lib/crti.o")]
    if pie():
        # A static PIE: musl's self-relocating start file, no text relocations,
        # and its RELRO sealed read-only before main (tools/relro_seal.rs).
        flags.append("-fPIE")
        link = ["-static-pie", "-Wl,-z,text", "-Wl,-z,relro", "-fuse-ld=lld", "-nostdlib", str(sysroot / "lib/rcrt1.o"),
                str(sysroot / "lib/crti.o"), str(relro_seal(arch=arch))]
    tail = [f"-L{sysroot / 'lib'}", "-lc", str(builtins), str(sysroot / "lib/crtn.o")]
    return bin_dir / "clang", flags, link, tail


def build_c(folder, out, arch="aarch32"):
    """A static C program against musl, from every .c file in the folder."""
    clang, flags, link, tail = c_toolchain(arch)
    sources = sorted(str(p) for p in folder.glob("*.c"))
    subprocess.run([clang, *flags, "-O2", *link, *sources, "-lm", *tail, "-o", out], check=True)


# The dynamic loader the Open Runtime runs (runtime/open/elf.rs): musl's libc.so.
LOADER = "lib/ld-musl-armhf.so.1"


def musl_loader(arch="aarch32"):
    """musl's shared libc.so, which is also its dynamic loader, built once
    (out of tree, from the musl source tools/build_python.py fetches) into
    build/open-toolchain/musl-shared (build/open-toolchain/aarch64/musl-shared).
    Only images that set VIOLET_OPEN_LDSO=1 (make test-abi) ship it, as
    /lib/ld-musl-armhf.so.1 (/lib/ld-musl-aarch64.so.1)."""
    sys.path.insert(0, str(ROOT / "tools"))
    import build_python
    t = build_python.TARGETS[arch]
    work = t.work / "musl-shared"
    lib = work / "lib/libc.so"
    if lib.exists():
        return lib
    source = build_python.fetch(build_python.MUSL, t.work)
    b = build_python.ndk_bin()
    builtins = build_python.builtins_for(t, b)
    tools = {"AR": str(b / "llvm-ar"), "RANLIB": str(b / "llvm-ranlib")}
    work.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CC=f"{b / 'clang'} {t.flags}", **tools)
    subprocess.run([source / "configure", "--prefix=/", f"--target={t.triple}", "--disable-static"],
                   cwd=work, env=env, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["make", f"-j{os.cpu_count()}", *[f"{k}={v}" for k, v in tools.items()], f"LIBCC={builtins}", "lib/libc.so"],
                   cwd=work, check=True, stdout=subprocess.DEVNULL)
    return lib


def build_c_dynamic(folder, out, arch="aarch32"):
    """A C program linked against musl's libc.so, run by /lib/ld-musl-armhf.so.1
    (a folder with a file named "dynamic"; only with VIOLET_OPEN_LDSO=1)."""
    if os.environ.get("VIOLET_OPEN_LDSO") != "1":
        raise SystemExit("dynamically linked programs need VIOLET_OPEN_LDSO=1")
    sys.path.insert(0, str(ROOT / "tools"))
    import build_python
    t = build_python.TARGETS[arch]
    clang, flags, _, _ = c_toolchain(arch)
    sysroot = t.sysroot
    lib = musl_loader(arch)
    LOADER = ARCHES[arch]["loader"]
    builtins = next(Path(clang).parent.parent.glob(f"lib/clang/*/lib/linux/{t.builtins}"))
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
def release_gate(files, fixed_ok, allow_fixed, arch="aarch32"):
    """tools/elf_check.py over every ELF file in the image ({name: (bytes,
    mode)}), wherever it is: execve runs any of them. Returns the programs let
    through with a fixed base; raises SystemExit for any it refuses.

    The checker skips any file it cannot read. For AArch32 that is how the
    gate has always worked; for another architecture every ELF file must be
    one of that architecture's programs and one the checker reads, or the
    build stops: an unread program would ship with no W^X, PIE or RELRO check."""
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    elves = {name: data for name, (data, _) in files.items() if data[:4] == b"\x7fELF"}
    if arch != "aarch32":
        unread = refused_by_class(elves, arch, elf_check.inspect)
        if unread:
            raise SystemExit(f"the release gate (tools/elf_check.py) cannot check these {arch} files, "
                             "so the image is not built:\n  " + "\n  ".join(unread))
    return elf_check.check(elves, fixed_ok=fixed_ok, allow_fixed=allow_fixed, arch=arch)


def refused_by_class(elves, arch, inspect):
    """"name: why" for each ELF file that is not `arch`'s, or that `inspect` cannot read."""
    import struct
    elf_class, machine = ARCHES[arch]["elf"]
    out = []
    for name, data in sorted(elves.items()):
        if len(data) < 20 or data[4] != elf_class or data[5] != 1 or struct.unpack_from("<H", data, 18)[0] != machine:
            out.append(f"{name}: not a little-endian {arch} ELF file")
        elif inspect(data, arch) is None:
            out.append(f"{name}: tools/elf_check.py does not read it")
    return out


HANDSET_SKIP = {"gonet", "pyhello", "pynet", "pypaint", "pytone"}


def main(out, python=True, compress=False, skip=(), arch=None):
    """compress=True writes "VGZ1", the gzip length, then the gzip of the
    archive (the handset carries it compressed in its boot image's ramdisk).
    `arch` is the programs' architecture: VIOLET_ARCH (aarch64) unless given."""
    arch = arch_of(arch)
    sys.path.insert(0, str(ROOT / "tools"))
    import build_python
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
    fixed_ok = set()
    sys.path.insert(0, str(ROOT / "tools"))
    import elf_check
    allow_fixed = elf_check.fixed_allowed()   # before minutes of building: VIOLET_RELEASE=1 refuses VIOLET_OPEN_PIE=0
    for app in apps:
        if not app.is_dir() or app.name in skip:
            continue
        if app.name == "ladybird":
            try:
                binary = build_ladybird(app, arch)
            except (SystemExit, subprocess.CalledProcessError) as e:
                print(f"skipping ladybird: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        if (app / "Cargo.toml").exists():
            if cargo_out is None:
                try:
                    cargo_out = build_cargo(work, arch)
                except (SystemExit, subprocess.CalledProcessError) as e:
                    print(f"skipping the Rust apps: {e}", file=sys.stderr)
                    cargo_out = False
            if cargo_out:
                files[f"apps/{app.name}"] = ((cargo_out / app.name).read_bytes(), 0o100755)
            continue
        if (app / "main.go").exists():
            binary = work / app.name
            try:
                build_go(app, binary, arch)
            except SystemExit as e:
                print(f"skipping {app.name}: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            # Go links a PIE for linux/arm only through cgo, which these builds
            # do not use: a Go test program keeps a fixed code address. Only the
            # named QEMU test folders get that pass (elf_check.FIXED_TEST_PROGRAMS),
            # and none in a release; any other Go app is refused by the gate.
            if elf_check.fixed_test_program(ROOT, app):
                fixed_ok.add(f"apps/{app.name}")
            continue
        if (app / "main.c").exists():
            binary = work / app.name
            try:
                (build_c_dynamic if (app / "dynamic").exists() else build_c)(app, binary, arch)
            except (SystemExit, StopIteration) as e:
                print(f"skipping {app.name}: {e}", file=sys.stderr)
                continue
            files[f"apps/{app.name}"] = (binary.read_bytes(), 0o100755)
            continue
        if (app / "main.rs").exists():
            binary = work / app.name
            try:
                build_rust(app / "main.rs", binary, arch)
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
    if not extra and python and (build_python.TARGETS[arch].pyroot / "bin/python3").exists():
        extra = str(build_python.TARGETS[arch].pyroot)
        # A python3 linked before the current rules (fixed address, or RELRO not sealed) is relinked, not shipped.
        stale = elf_check.inspect((Path(extra) / "bin/python3").read_bytes())
        if stale is not None and not allow_fixed and (elf_check.problems(stale) or not stale["pie"]):
            raise SystemExit(f"{extra}/bin/python3 was linked before the current release rules "
                             f"({'; '.join(elf_check.problems(stale)) or 'not position-independent'}): "
                             "relink it with python3 tools/build_python.py python (or set VIOLET_OPEN_NO_PYTHON=1)")
    if extra:
        # A prepared tree (for example /bin/python3 and /lib) merged at the root.
        for path in sorted(Path(extra).rglob("*")):
            if path.is_file():
                rel = str(path.relative_to(extra))
                files[rel] = (path.read_bytes(), 0o100755 if os.access(path, os.X_OK) else 0o100644)
    # musl's dynamic loader, for dynamically linked programs (make test-abi).
    if os.environ.get("VIOLET_OPEN_LDSO") == "1":
        files[ARCHES[arch]["loader"]] = (musl_loader(arch).read_bytes(), 0o100755)
    # The loader refuses programs that break W^X: fail here, not on the phone.
    # The release gate: every program is position-independent, so its code
    # moves on every start, and seals its RELRO. Only Go test programs
    # (fixed_ok) are let through. Every ELF file is a program execve may run,
    # wherever it is in the tree.
    fixed = release_gate(files, fixed_ok, allow_fixed, arch)
    if fixed:
        print(f"{'NOT A RELEASE BUILD (VIOLET_OPEN_PIE=0): ' if allow_fixed else 'test programs with a '}"
              f"fixed code address (not PIE; stack, heap and mappings still move): {' '.join(fixed)}", file=sys.stderr)
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
    default = ROOT / ("build/sel4/qemu/initfs.cpio" if arch_of() == "aarch32" else "build/sel4/qemu-aarch64/initfs.cpio")
    main(sys.argv[1] if len(sys.argv) > 1 else default,
         compress="--gzip" in sys.argv)
