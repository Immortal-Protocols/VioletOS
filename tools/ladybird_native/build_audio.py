"""Build the pinned upstream AudioServer mixer probe and the VioletOS platform backend."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from build_runtime import TARGET, build as build_support, ndk_bin


def build(out, support, document=None):
    out.mkdir(parents=True, exist_ok=True)
    rustc = os.environ.get("RUSTC") or shutil.which("rustc") or HERE.parents[1] / "build/ladybird-native/toolchain/bin/rustc"
    os.environ.setdefault("RUSTC", str(rustc))
    os.environ.setdefault("CARGO", str(Path(rustc).with_name("cargo")))
    if not (support / "cpp/libviolet-js-support.a").is_file():
        build_support(support)
    upstream, sysroot = support / "sources/ladybird", support / "sources/sysroot"
    ndk = ndk_bin()
    rustroot = Path(subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip())
    crt = rustroot / "lib/rustlib" / TARGET / "lib/self-contained"
    cpp = next((sysroot / "usr/include/c++").iterdir())
    generated = out / "include"
    for module in ("Audio", "IPC", "Gfx", "WebCommon", "URL"):
        folder = generated / ("Lib" + module)
        folder.mkdir(exist_ok=True, parents=True)
        (folder / "Export.h").write_text(f"#pragma once\n#define {module.upper()}_API\n")
    url = generated / "LibURL"
    url.mkdir(exist_ok=True)
    cargo = os.environ.get("CARGO") or Path(rustc).with_name("cargo")
    env = dict(os.environ, RUSTC=str(rustc), CARGO_TARGET_DIR=str(support / "cargo"))
    env.setdefault("CARGO_HOME", str(HERE.parents[1] / "build/cargo-home"))
    env["DYLD_FALLBACK_LIBRARY_PATH"] = os.pathsep.join(filter(None, [str(rustroot/"lib"), env.get("DYLD_FALLBACK_LIBRARY_PATH")]))
    result = subprocess.run([cargo, "build", "--manifest-path", upstream / "Cargo.toml", "-p", "liburl_rust",
                             "--target", TARGET, "--release", "--locked", "--message-format=json-render-diagnostics"],
                            check=True, stdout=subprocess.PIPE, text=True, env=env)
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "build-script-executed":
            dest = Path(message["out_dir"])
            if dest.parent.name.startswith("liburl_rust-") and TARGET in dest.parts:
                shutil.copyfile(dest / "RustFFI.h", url / "RustFFI.h")
    flags = [os.environ.get("CXX", "clang++"), f"--target={TARGET}", f"--sysroot={sysroot}", "-O2",
             "-ffunction-sections", "-fdata-sections", "-std=c++23", "-fno-exceptions", "-nostdinc++",
             "-isystem", cpp, "-isystem", cpp / "aarch64-alpine-linux-musl", "-D_GNU_SOURCE",
             "-Wno-unqualified-std-cast-call", "-Wno-user-defined-literals"]
    for path in (upstream, upstream / "Libraries", upstream / "Services", generated, support / "cpp/include",
                 support / "sources/mimalloc/include", support / "sources/simdutf/include", support / "sources/llvm-project"):
        flags += ["-I", path]
    sources = [HERE / "PlaybackStreamViolet.cpp", HERE / "audio_probe.cpp",
               upstream / "Services/AudioServer/PlaybackStreamMixer.cpp",
               upstream / "Libraries/LibAudio/SharedAudioFrameRing.cpp",
               upstream / "Libraries/LibAudio/AudioFrameRingBase.cpp"]
    objects = []
    for source in sources:
        obj = out / (source.stem + ".o")
        subprocess.run([*flags, "-c", source, "-o", obj], check=True)
        objects.append(obj)
    subprocess.run([ndk / "llvm-ar", "rcs", out / "libviolet-audio-backend.a", objects[0]], check=True)
    lib = sysroot / "usr/lib"
    def link(elf, inputs, archives, extra=()):
        subprocess.run([ndk / "ld.lld", "-static", "--gc-sections", *extra, "-o", elf,
                        crt / "crt1.o", crt / "crti.o", crt / "crtbegin.o", *inputs, "--start-group", *archives,
                        support / "cpp/libviolet-js-support.a",
                        *[lib / f"lib{name}.a" for name in ("icui18n", "icuuc", "icudata", "stdc++")],
                        *lib.glob("gcc/*/*/libgcc.a"), crt / "libc.a", crt / "libunwind.a",
                        "--end-group", crt / "crtend.o", crt / "crtn.o"], check=True)

    link(out / "audio-mixer-probe", objects, [])
    print(f"Built {out / 'audio-mixer-probe'}")
    if document is None:
        return
    engine = document / "libweb.a"
    if not engine.is_file():
        raise SystemExit(f"Build the pinned document package first (missing {engine})")
    # Use the same generated IPC endpoints and core libraries as WebContent.
    service = upstream / "Services/AudioServer"
    archive_sources = [service / (base + ".cpp") for base in ("CaptureActivity", "CaptureDevices", "ControlConnection",
                       "ServerConnection", "Tabs", "PlatformAudio", "SandboxLinux", "main")]
    archive_sources += [upstream / "Libraries/LibSandbox" / (base + ".cpp") for base in
                        ("ConnectBroker", "FileBroker", "GpuDriverPaths", "Sandbox", "Seccomp")]
    archive_sources += [upstream / "Libraries/LibCore/CrashHandlerPOSIX.cpp"]
    service_objects = [objects[0], objects[2]]
    for source in archive_sources:
        obj = out / (source.parent.name + "-" + source.stem + ".o")
        # musl uses socklen_t for msg_controllen. The broker message contains at most three descriptors.
        compat = ["-Wno-c++11-narrowing", "-include", "asm-generic/ioctl.h"] if source.name == "Seccomp.cpp" else []
        subprocess.run([*flags, *compat, "-I", document / "include", "-I", document / "include/LibGC",
                        "-I", document / "sysroot/usr/include", "-DAUDIOSERVER_PLAYBACK_BACKEND=1",
                        "-c", source, "-o", obj], check=True)
        service_objects.append(obj)
    elf = out / "root/opt/ladybird/libexec/AudioServer"
    elf.parent.mkdir(exist_ok=True, parents=True)
    rustlib = support / "cargo" / TARGET / "release"
    archive_deps = [engine, *[a for a in sorted(rustlib.glob("*.a")) if a.name != "liblibjs_rust.a"]]
    link(elf, service_objects, archive_deps, ["-s", "--allow-multiple-definition"])
    print(f"Built the upstream service at {elf}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    base = HERE.parents[1] / "build/ladybird-native"
    parser.add_argument("--build-dir", type=Path, default=base / "audio")
    parser.add_argument("--support-dir", type=Path, default=base / "static")
    parser.add_argument("--test", action="store_true", help="boot the mixer probe in seL4/QEMU and measure its PCM output")
    parser.add_argument("--server", action="store_true", help="also link the actual upstream AudioServer service")
    parser.add_argument("--document-dir", type=Path, default=base / "document", help="output of build_document.py")
    opts = parser.parse_args()
    build(opts.build_dir.resolve(), opts.support_dir.resolve(), opts.document_dir.resolve() if opts.server else None)
    if opts.test:
        guest_tools = HERE.parents[1] / "targets/qemu/aarch64"
        sys.path.insert(0, str(guest_tools))
        from desktop import sdk_directories
        env = dict(os.environ, **sdk_directories())
        env["PATH"] = os.pathsep.join([str(HERE.parents[1] / "build/sel4/venv/bin"), str(ndk_bin()), env["PATH"]])
        env.setdefault("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
        output = opts.build_dir.resolve()
        guest = output / "qemu"
        subprocess.run([sys.executable, guest_tools / "build.py", "--out", guest, "--elf", output / "audio-mixer-probe"], env=env, check=True)
        subprocess.run([sys.executable, guest_tools / "viewer.py", "--out", guest, "--test", "audio-mixer"], env=env, check=True)
