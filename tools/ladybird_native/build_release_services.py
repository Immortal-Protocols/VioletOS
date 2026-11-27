"""Cross-build upstream release Hub and AudioServer for AArch64 VioletOS.

The remaining helper processes come from the matching upstream release. The
native document build supplies third-party headers and archives such as ICU and
HarfBuzz. All Ladybird C++ objects, Rust crates, generated message definitions
and the terminal frontend are compiled from the verified release source.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

from build_document import LIBRARIES
from build_runtime import HERE, TARGET, ndk_bin
from document_codegen import copy_rust_headers, generate
from prepare_release_audio import COMMIT, prepare

# CMake lists common sources separately from Unix and optional platform files.
PLATFORM_SOURCES = {
    "AK": "ConditionVariablePOSIX LexicalPath MutexPOSIX RWLockPOSIX",
    "LibCore": """AnonymousBuffer CrashHandlerPOSIX EventLoopImplementationUnix FilePosix FileWatcherInotify
        GeolocationProviderUnimplemented LocalServer MappedFile Platform/ProcessStatisticsLinux Process Socket
        System TCPServer TimeZoneWatcherUnix UDPServer""",
    "LibIPC": "Attachment TransportSocket",
    "LibGfx": "Font/SystemFontMatcherNone",
    "LibSandbox": "FileBroker GpuDriverPaths Seccomp",
    "LibWebView": "AudioDaemonSocket CrashReportPOSIX TerminalInputParser TerminalWebView",
}
MODULES = """AK LibAudio LibCompositing LibCompress LibCore LibCrypto LibDatabase LibDevTools LibFileSystem
LibGfx LibHTTP LibIPC LibImageDecoderClient LibMain LibMedia LibMediaClient LibPlatformFeatures LibRegex
LibRequests LibSandbox LibSyntax LibTLS LibTextCodec LibThreading LibURL LibUnicode LibWebCommon LibWebRTCClient
LibWebView""".split()
RUST_CRATES = """libunicode_rust liburl_rust libgfx_rust libwebcommon_rust libcompositing_rust libtextcodec_rust
libregex_rust libjs_embedding_headers""".split()


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def write_if_changed(path, contents):
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.is_file() or path.read_text() != contents:
        path.write_text(contents)


def source_list(folder):
    cmake = (folder / "CMakeLists.txt").read_text()
    block = re.search(r"^set\(SOURCES[^\S\n]*\n(.*?)^\)", cmake, re.M | re.S)
    return {folder / name for name in re.findall(r"^\s*([\w/]+\.cpp)\s*$", block[1], re.M)}


def headers_and_rust(source, out, support, document, rustc, cargo):
    inc = out / "include"
    inc.mkdir(parents=True, exist_ok=True)
    preload = document / "downloads/transport_security_state_static.json"
    fingerprint = hashlib.sha256((HERE / "document_codegen.py").read_bytes() + COMMIT.encode() + preload.read_bytes()).hexdigest()
    stamp = inc / ".generated"
    if not stamp.exists() or stamp.read_text() != fingerprint:
        generate(source, inc, preload)
        for ipc in sorted((source / "Services/Hub").glob("*.ipc")):
            path = inc / "Hub" / (ipc.stem + "Endpoint.h")
            path.parent.mkdir(exist_ok=True)
            run(sys.executable, source / "Meta/Generators/generate_ipc_definitions.py", "--input", ipc, "--output", path,
                "--implementation", path.with_suffix(".cpp"), "--header-name", path.name)
        stamp.write_text(fingerprint)
    for name in ("Backtrace", "Debug"):
        text = (source / "AK" / (name + ".h.in")).read_text()
        text = re.sub(r"^#\s*cmakedefine01 (\w+).*$", r"#define \1 0", text, flags=re.M)
        text = re.sub(r"^#\s*cmakedefine .*$", "", text, flags=re.M)
        write_if_changed(inc / "AK" / (name + ".h"), text)
    write_if_changed(inc / "AK/Export.h", "#pragma once\n#define AK_API\n#define AK_NO_EXPORT\n")
    write_if_changed(out / "CMakeCache.txt", "LADYBIRD_VIOLET_AUDIO:BOOL=ON\n")
    run(sys.executable, source / "Meta/Generators/generate-build-information.py", source, out,
        inc / "LibWebView/BuildInformation.h", "Clang AArch64-musl", "--depfile", out / "build-information.d",
        "--stamp", out / "build-information.stamp")
    rust_root = Path(subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip())
    sysroot = support / "sources/sysroot"
    env = dict(os.environ, RUSTC=str(rustc), CARGO_TARGET_DIR=str(out / "cargo"), LADYBIRD_LINKS_AK="1",
               LIBWEB_RUST_BINDINGS=str(inc / "LibWeb/Bindings/RustBindings.rs"),
               CC_aarch64_unknown_linux_musl=os.environ.get("CC", "clang"),
               AR_aarch64_unknown_linux_musl=str(ndk_bin() / "llvm-ar"),
               CFLAGS_aarch64_unknown_linux_musl=f"--target={TARGET} --sysroot={sysroot}")
    env["DYLD_FALLBACK_LIBRARY_PATH"] = os.pathsep.join(filter(None, [str(rust_root / "lib"), env.get("DYLD_FALLBACK_LIBRARY_PATH")]))
    env.setdefault("CARGO_HOME", str(HERE.parents[1] / "build/cargo-home"))
    result = run(cargo, "build", "--manifest-path", source / "Cargo.toml", "--locked", "--no-default-features",
                 "--target", TARGET, "--release", "--message-format=json-render-diagnostics", "--features", "libunicode_rust/idna",
                 *(arg for name in RUST_CRATES for arg in ("-p", name)), env=env, stdout=subprocess.PIPE, text=True)
    copy_rust_headers((json.loads(line) for line in result.stdout.splitlines()), inc, source)
    return rust_root


def compile_and_link(source, out, support, document, rust_root, jobs):
    inc = out / "include"
    deps = document / "sysroot/usr"
    vendor = support / "sources"
    sysroot = vendor / "sysroot"
    headers = next((sysroot / "usr/include/c++").iterdir())
    libs = source / "Libraries"
    service = source / "Services"
    dirs = [source, libs, service, inc, vendor / "llvm-project", vendor / "mimalloc/include", vendor / "mimalloc/src",
            vendor / "simdutf/include", vendor / "simdutf/src", vendor / "fast_float/include", vendor / "zmij",
            deps / "include", deps / "include/freetype2", deps / "include/libpng16"]
    common = [f"--target={TARGET}", f"--sysroot={sysroot}", "-O1", "-ffunction-sections", "-fdata-sections", "-D_GNU_SOURCE",
              "-DMI_STATIC_LIB", "-DLADYBIRD_VIOLET_AUDIO=1", "-DAUDIOSERVER_PLAYBACK_BACKEND=1",
              "-Wno-user-defined-literals", "-Wno-unqualified-std-cast-call", "-Wno-invalid-offset-of",
              "-Wno-unknown-warning-option", *["-I" + str(path) for path in dirs]]
    cpp = ["-std=c++23", "-fno-exceptions", "-nostdinc++", "-isystem", headers,
           "-isystem", headers / "aarch64-alpine-linux-musl"]
    tools = ndk_bin()
    ninja = ["rule compile", "  command = $cc $flags -MMD -MF $out.d -c $in -o $out",
             "  description = CC $in", "  depfile = $out.d", "  deps = gcc", "rule archive",
             f"  command = {shlex.quote(str(tools / 'llvm-ar'))} rcs $out @$out.rsp",
             "  rspfile = $out.rsp", "  rspfile_content = $in"]
    def escape(path):
        return str(path).replace("$", "$$").replace(" ", "$ ").replace(":", "$:")
    objects = out / "objects"
    objects.mkdir(exist_ok=True)
    def compile(path):
        obj = objects / (path.stem + "-" + hashlib.sha256(str(path).encode()).hexdigest()[:16] + ".o")
        is_cpp = path.suffix != ".c"
        mod = next((part for part in path.parts if part in MODULES), "AK")
        flags = common + (cpp if is_cpp else []) + ["-I" + str(inc / mod)]
        if mod == "LibURL":
            flags.append("-DENABLE_RUST")
        if path.name == "Seccomp.cpp":
            flags += ["-Wno-c++11-narrowing", "-include", "asm-generic/ioctl.h"]
        ninja.extend([f"build {escape(obj)}: compile {escape(path)}",
                      "  cc = " + shlex.quote(os.environ.get("CXX" if is_cpp else "CC", "clang++" if is_cpp else "clang")),
                      "  flags = " + shlex.join(map(str, flags)).replace("$", "$$")])
        return obj

    sources = set()
    for name in MODULES:
        folder = (source if name == "AK" else libs) / name
        sources.update(source_list(folder))
        sources.update(folder / (name + ".cpp") for name in PLATFORM_SOURCES.get(name, "").split())
    # WebGL command execution lives in the release's separate Compositor process.
    sources -= {path for path in sources if path.parent == libs / "LibCompositing/WebGL"}
    for folder in ("LibGfx", "LibWebCommon", "LibWebView", "LibHTTP", "LibRequests"):
        sources.update((inc / folder).rglob("*.cpp"))
    sources.update(inc.rglob("*Endpoint.cpp"))
    sources.update((libs / "LibDNS").glob("*.cpp"))
    sources.update(source_list(service / "Hub"))
    sources.update(source_list(service / "AudioServer"))
    sources.update([service / "AudioServer/SandboxLinux.cpp", service / "AudioServer/PlaybackStreamViolet.cpp",
                    vendor / "zmij/zmij.cc", vendor / "mimalloc/src/static.c", vendor / "simdutf/src/simdutf.cpp",
                    inc / "LibCompositing/TypesCodecs.cpp", libs / "LibMediaCodecs/Color/ColorMetadata.cpp",
                    libs / "LibMediaCodecs/Color/CodingIndependentCodePoints.cpp", libs / "LibMediaCodecs/DecoderCapabilities.cpp",
                    libs / "LibMediaCodecs/DecoderError.cpp"])
    archive = out / "services.a"
    all_objs = [compile(path) for path in sorted(sources)]
    entries = {name: compile(service / name / "main.cpp") for name in ("Hub", "AudioServer")}
    ninja.append(f"build {escape(archive)}: archive " + " ".join(map(escape, all_objs)))
    write_if_changed(out / "build.ninja", "\n".join(ninja) + "\n")
    run("ninja", "-C", out, "-j", str(jobs), archive, *entries.values())
    system_lib = sysroot / "usr/lib"
    crt = rust_root / "lib/rustlib" / TARGET / "lib/self-contained"
    third_party = [document / f"lib{name}.a" if (document / f"lib{name}.a").is_file() else deps / "lib" / f"lib{name}.a" for name in LIBRARIES]
    rust = sorted((out / "cargo" / TARGET / "release").glob("*.a"))
    output = out / "root/opt/ladybird/libexec"
    output.mkdir(parents=True, exist_ok=True)
    for name, entry in entries.items():
        run(tools / "ld.lld", "-static", "-s", "--gc-sections", "--allow-multiple-definition", "-o", output / name,
            crt / "crt1.o", crt / "crti.o", crt / "crtbegin.o", entry, "--start-group", archive, *rust, *third_party,
            *(system_lib / f"lib{lib}.a" for lib in ("icui18n", "icuuc", "icudata", "stdc++")),
            *system_lib.glob("gcc/*/*/libgcc.a"), crt / "libc.a", crt / "libunwind.a", "--end-group", crt / "crtend.o", crt / "crtn.o")
    print(f"Matching release Hub and AudioServer are in {output}", flush=True)


if __name__ == "__main__":
    base = HERE.parents[1] / "build/ladybird-native"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=base / "release-audio")
    parser.add_argument("--support-dir", type=Path, default=base / "static")
    parser.add_argument("--document-dir", type=Path, default=base / "document")
    parser.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 2))
    args = parser.parse_args()
    out, support, document = (path.resolve() for path in (args.out, args.support_dir, args.document_dir))
    if args.jobs < 1 or not (document / "libpsl.a").is_file() or not (support / "sources/sysroot").is_dir():
        parser.error("A positive job count and third-party archives from build_document.py are required")
    rustc = os.environ.get("RUSTC") or str(base / "toolchain/bin/rustc")
    cargo = os.environ.get("CARGO") or shutil.which("cargo", path=str(Path(rustc).parent)) or "cargo"
    source = prepare(out)
    rust_root = headers_and_rust(source, out, support, document, rustc, cargo)
    compile_and_link(source, out, support, document, rust_root, args.jobs)
