"""Compile every service package for AArch64, the way image/packages/build.py builds them for AArch32.

    python3 services/check_aarch64.py [--open] [--no-link] [--if-available]

What it proves, in seconds and without a kernel build or an emulator:

1. Rust. Each component crate (the same `#[path]` modules, cfgs and panic
   handler build.py writes) compiles with one rustc for aarch64-unknown-none,
   with full code generation (staticlib), for both the QEMU and the handset
   component sets. Errors here are CP15 code, `cfg(target_arch = "arm")`-only
   items and anything else the 64-bit target does not have.
2. C. Every C file linked into a package (runtime/transport.c, the touch and
   storage shims, the board shims, BearSSL and its glue, the Sentinel's and
   the package verifier's crypto) compiles with clang for aarch64-none-elf
   against the AArch64 kernel's generated libsel4, with the warnings that mark
   a word-size bug made errors: a 32-bit object handed to a seL4_Word pointer
   (an 8-byte store into 4 bytes) and a 32-bit asm operand for a 64-bit
   register. Skipped, with a note, when the AArch64 kernel's build directory
   (build/tests/aarch64/qemu64, made by `make qemu64-image`) is absent.
3. Link (QEMU set). Each package links at 0x100000 with the AArch64 form of
   component.ld, its .text starts there, and its image fits the package window
   the supervisor gives that identity (package_format.package_window). The
   sizes are printed beside the AArch32 packages of the last QEMU build.

Nothing it writes leaves build/tests/aarch64-services. It changes no build:
the packages are made by image/packages/build.py (for AArch64 through
targets/qemu64/stack.py); this is the seconds-long compile check of the same
crates that make test-unit runs. The crate table below must stay in step
with build.py's; a component added there and not here is reported.
"""
from concurrent.futures import ThreadPoolExecutor
import argparse
import glob
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]
OUT = REPO / "build/tests/aarch64-services"
TARGET = "aarch64-unknown-none"
KERNEL = REPO / "build/tests/aarch64/qemu64/kernel"
sys.path.insert(0, str(REPO / "image/packages"))
import violet_arch  # noqa: E402
from package_format import package_window  # noqa: E402

# The AArch64 form of build.py's component.ld. The same layout (.entry first
# at 0x100000, code and constants, then data, then zeroed data) plus the
# AArch64 unwind tables in the discard list; the linker is also told 4 KiB
# pages (-z max-page-size), as lld's AArch64 default is 64 KiB.
LINK_SCRIPT = """ENTRY(process_entry)
SECTIONS {
 . = 0x100000;
 .text : { KEEP(*(.entry)) *(.text*) *(.rodata*) . = ALIGN(4096); }
 .data : { *(.data*) *(.got*) }
 .bss (NOLOAD) : { *(.bss*) *(COMMON) }
 image_end = .;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.eh_frame*) *(.eh_frame_hdr*) }
}"""

# Package identities (image/catalog/components.json, plus the two build.py adds).
IDENTITY = {"package_manager": 9, "usb-diagnostics": 21}


def rustc():
    """A rustc whose toolchain has aarch64-unknown-none: $RUSTC_AARCH64, else one under build/."""
    if os.environ.get("RUSTC_AARCH64"):
        return Path(os.environ["RUSTC_AARCH64"])
    for candidate in (os.environ.get("HOST_RUSTC"), shutil.which("rustc")):
        if not candidate:
            continue
        root = subprocess.run([candidate, "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
        if root and (Path(root) / "lib/rustlib" / TARGET).is_dir():
            return Path(candidate)
    common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=REPO,
                            capture_output=True, text=True).stdout.strip()
    roots = [REPO / "build"] + ([Path(common).parent / "build"] if common else [])
    for root in roots:
        for std in sorted(glob.glob(str(root / "*/rustup/toolchains/*/lib/rustlib" / TARGET))):
            tool = Path(std).parents[2] / "bin/rustc"
            if tool.exists():
                return tool
    return None


def generated(out):
    """The inputs build.py generates before it compiles: glyphs, touch fixture firmware, test package keys."""
    out.mkdir(parents=True, exist_ok=True)
    glyphs = (REPO / "src/glyphs.h").read_text()
    alphabet = re.search(r'alphabet\[\] = "([^"]+)"', glyphs)[1]
    rows = re.findall(r"\{([\d, ]+)\}", glyphs)
    (out / "glyphs.rs").write_text('const ALPHABET:&[u8]=b"' + alphabet + '";\nconst GLYPHS:&[[u8;5]]=&['
                                   + ",".join("[" + row + "]" for row in rows) + "];")
    sys.path.insert(0, str(REPO / "services/touch"))
    from firmware import prepare
    # The QEMU fixture firmware stands in on both sets: the crate's code is
    # the same, only the bytes it carries differ.
    prepare(out, "qemu", None)
    # No key is trusted by these builds; they are never loaded.
    (out / "package_keys.rs").write_text("pub const TARGET: u32 = 0;\npub const RELEASE: bool = false;\n"
                                         "pub const KEYS: &[(u32, [u8; 65])] = &[];\n")
    env = dict(os.environ, VIOLET_GLYPHS_RS=str(out / "glyphs.rs"), VIOLET_FIRMWARE_RS=str(out / "firmware.rs"),
               VIOLET_CATALOG_VOLUME_RS=str(REPO / "system/package-manager/catalog_volume.rs"),
               VIOLET_PACKAGE_KEYS_RS=str(out / "package_keys.rs"))
    env.setdefault("VIOLET_BUILD_EPOCH", "1795000000")
    return env


def crates(target, out, with_open):
    """(name, identity, crate text, rustc flags) for each component: build.py's table."""
    board = REPO / "targets" / ("moto-g-pure" if target == "moto" else "qemu")
    scripts = {
        "launcher": REPO / "apps/launcher/process.rs",
        "runtime": REPO / "runtime/process.rs",
        "display": REPO / "services/display/process.rs",
        "radio": REPO / "services/radio/process.rs",
        "power": REPO / "services/power/process.rs",
        "audio": REPO / "services/audio/process.rs",
        "camera": REPO / "services/camera/process.rs",
        "open": REPO / "runtime/open/mod.rs",
        "sentinel": REPO / "system/sentinel/process.rs",
        "package_manager": REPO / "system/package-manager/process.rs",
    }
    if target == "moto":
        scripts["radio"] = board / "radio.rs"
        scripts["usb-diagnostics"] = REPO / "services/usb-diagnostics/process.rs"
    manifest = json.loads((REPO / "image/catalog/components.json").read_text())
    for item in manifest:
        if "entry" in item:
            scripts[item["name"]] = REPO / item["entry"]
    if target == "moto":
        manifest.append({"id": 21, "name": "usb-diagnostics"})
        keep = {"launcher", "runtime", "touch", "display", "storage", "power", "radio", "usb-diagnostics",
                "audio", "open", "camera", "sentinel"}
        manifest = [item for item in manifest if item["name"] in keep]
    manifest.append({"id": 9, "name": "package_manager"})
    result = []
    for item in manifest:
        name = item["name"]
        if name == "open" and not with_open:
            continue
        modules = {"protocol": REPO / "runtime/protocol.rs"}
        if name in ("power", "audio", "camera"):
            modules[name + "_backend"] = board / (name + ".rs")
        if name == "camera" and target == "moto":
            tables = out / "sensor_tables.rs"
            sys.path.insert(0, str(REPO / "tools"))
            import sensor_tables
            downloads = REPO / "build/downloads"
            if not all((downloads / Path(path).name).exists() for path, _, _ in sensor_tables.SOURCES.values()):
                print(f"{target} camera: skipped, Motorola's sensor tables are not in build/downloads")
                continue
            sensor_tables.generate(tables, downloads)
            modules["sensor_tables"] = tables
        if name in scripts:
            modules["process"] = scripts[name]
        if name == "launcher" or "entry" in item:
            modules.update(sdk=REPO / "runtime/sdk.rs", drawing=REPO / "apps/drawing.rs")
        if name == "open":
            # The real key is the companion's; a fixed test point is enough to compile.
            key = out / "companion_key.rs"
            key.write_text("pub const COMPANION_KEY: [u8; 65] = [4" + ",0" * 64 + "];\n")
            modules["companion_key"] = key
        if name == "sentinel" and target == "moto":
            seal = out / "sentinel_key.rs"
            seal.write_text("pub const SEAL_SECRET: [u8; 32] = [0; 32];\n")
            modules["sentinel_key"] = seal
        body = "process::run()"
        if name == "runtime":
            modules["lanes"] = REPO / "runtime/lanes.rs"
            body = "if identity>=15 {lanes::work(identity-15+4)} else {process::run()}"
        if name == "touch":
            modules.update(touch=REPO / "services/touch/touch.rs", spi=REPO / "services/touch/spi.rs")
            body = 'unsafe { extern "C" {fn touch_task();} touch_task(); }'
        if name == "storage":
            modules.update(storage=REPO / "services/storage/storage.rs",
                           block=(board / "emmc.rs") if target == "moto" else REPO / "services/storage/block.rs",
                           journal=REPO / "services/storage/journal.rs",
                           blockdev=REPO / "services/storage/blockdev.rs")
            body = "storage::storage_task()"
        prelude = "extern crate alloc;\n" if name in ("open", "sentinel") else ""
        text = ("#![no_std]\n#![allow(dead_code)]\n" + prelude
                + "".join(f"#[path={json.dumps(str(v))}] mod {k};\n" for k, v in modules.items())
                + '#[panic_handler] fn panic(_: &core::panic::PanicInfo)->!{unsafe{core::arch::asm!("udf #1",options(noreturn))}}\n'
                + '#[no_mangle] pub extern "C" fn component_main(identity:u32){let _=identity;' + body + "}\n")
        small = name in ("open", "sentinel")
        flags = ["-Copt-level=z" if small else "-Copt-level=s", *(["-Clto=fat"] if small else []), "-Ccodegen-units=1"]
        if target == "moto" and name in ("power", "open", "launcher"):
            flags += ["--cfg", "moto_board"]
        if name == "sentinel" and target != "moto":
            flags += ["--cfg", "device_root_key"]
        if target == "moto" and name == "storage":
            flags += ["--cfg", "boot_trace", "--cfg", "package_catalog"]
        result.append((name, item["id"], text, flags))
    return result


def compile_rust(tool, env, target, out, crate):
    name, _, text, flags = crate
    source = out / (name + ".rs")
    source.write_text(text)
    lib = out / (name + ".a")
    run = subprocess.run([tool, "--edition=2021", "--crate-type=staticlib", "--target=" + TARGET, "-Cpanic=abort",
                          *flags, source, "-o", lib], env=env, capture_output=True, text=True)
    (out / (name + ".log")).write_text(run.stderr)
    errors = len(re.findall(r"^error", run.stderr, re.M))
    return name, run.returncode == 0, errors


def kernel_compiler():
    """clang and the AArch64 libsel4 include flags, from the AArch64 kernel's own build commands."""
    if not (KERNEL / "build.ninja").exists() or not shutil.which("ninja"):
        return None
    commands = subprocess.run(["ninja", "-C", KERNEL, "-t", "commands", "sel4"], capture_output=True, text=True).stdout
    line = next((s for s in commands.splitlines() if " -c " in s and s.endswith("/sel4_bootinfo.c")), None)
    if not line:
        return None
    args = shlex.split(line)
    incs = [a for a in args if a.startswith("-I") and ("libsel4" in a or "autoconf" in a or "gen_config" in a)]
    return Path(args[0]), incs


def c_flags(cc):
    """The QEMU package build's C flags (platform/sel4/loader/build.py `common`) with the AArch64 arch part.

    -mstrict-align is AArch64's -mno-unaligned-access: services touch device
    memory, where an unaligned access faults."""
    return [cc, *violet_arch.clang_flags({"VIOLET_ARCH": "aarch64"}), "-mstrict-align", "-Oz", "-ffreestanding",
            "-fno-builtin", "-fno-stack-protector", "-fno-unwind-tables", "-fno-pic", "-ffunction-sections",
            "-fdata-sections", "-Werror=incompatible-pointer-types", "-Werror=asm-operand-widths",
            "-Werror=implicit-function-declaration", "-Werror=int-conversion"]


def compile_c(common, incs, out, with_open):
    """Compile every C file the QEMU packages link; return (component objects, per-package extras, failures)."""
    include = REPO / "platform/sel4/include"
    defines = ["-DNDEBUG", "-DROOT_BYTES=0x180000", "-DVIOLET_CORES=1", "-DVIOLET_QEMU"]
    sys.path.insert(0, str(REPO / "services/touch"))
    from firmware import prepare
    prepare(out, "qemu", None)  # firmware.h, for events.c
    failures = []

    def cc(source, extra, name=None):
        obj = out / ((name or source.name) + ".o")
        run = subprocess.run([*common, *extra, "-c", source, "-o", obj], capture_output=True, text=True)
        (out / ((name or source.name) + ".log")).write_text(run.stderr)
        if run.returncode:
            failures.append(f"{source.relative_to(REPO)}: {run.stderr.strip().splitlines()[0] if run.stderr else 'failed'}")
        return obj

    transport = cc(REPO / "runtime/transport.c", [*incs, "-I", include])
    shims = [cc(s, [*incs, *defines, "-DVIOLET_COMPONENT", "-I", out, "-I", include, "-std=gnu11"], "component-" + s.name)
             for s in (REPO / "services/touch/events.c", REPO / "services/storage/storage_ipc.c",
                       REPO / "platform/sel4/board/watchdog.c", REPO / "targets/qemu/fixture.c",
                       REPO / "platform/sel4/loader/strings.c")]
    extras = {}
    bearssl = REPO / "build/sel4/bearssl-0.6"
    if (bearssl / "src").exists():
        sys.path.insert(0, str(REPO / "services/radio"))
        from build_tls import build as build_tls
        try:
            # Kept across runs (build_tls caches by its flags) beside the per-target folders.
            tls = build_tls(REPO, OUT, list(common))
        except subprocess.CalledProcessError as error:
            failures.append(f"BearSSL / services/radio/tls.c: {error}")
            tls = []
        free = ["-I", REPO / "services/radio/freestanding"]
        platform = cc(REPO / "targets/qemu/tls_platform.c", free)
        crypto = cc(REPO / "services/sentinel/crypto.c", ["-I", bearssl / "inc", *free])
        verify = cc(REPO / "system/package-manager/verify_crypto.c", ["-I", bearssl / "inc", *free])
        # verify_crypto.c is compiled to prove it builds; the supervisor links it, not a package.
        del verify
        extras = {"radio": [*tls, platform]}
        if with_open:
            shim = cc(REPO / "runtime/open/sel4.c", [*incs, "-DNDEBUG", "-std=gnu11", "-I", include])
            extras["open"] = [shim, crypto, *tls[1:]]
    else:
        print("C: BearSSL sources absent (build/sel4/bearssl-0.6): radio, the Sentinel's crypto and the verifier not compiled")
    return [transport, *shims], extras, failures


def link(cc, out, name, identity, objects, extras):
    lib = out / (name + ".a")
    elf = out / (name + ".elf")
    run = subprocess.run([cc.with_name("ld.lld"), "--gc-sections", "-z", "max-page-size=4096", "-T", out / "component.ld",
                          "--start-group", *objects, *extras, lib, "--end-group", "-o", elf], capture_output=True, text=True)
    if run.returncode:
        return f"link failed: {run.stderr.strip().splitlines()[0]}"
    data = elf.read_bytes()
    if data[4] != 2 or int.from_bytes(data[18:20], "little") != 183:
        return "not an AArch64 ELF64"
    # The supervisor starts every package at its first byte: process_entry (.entry).
    if int.from_bytes(data[24:32], "little") != 0x100000:
        return f"not an AArch64 ELF64 entered at 0x100000 (entry {int.from_bytes(data[24:32], 'little'):#x})"
    rows, names = violet_arch.sections(data)
    strings = data[rows[names][4]: rows[names][4] + rows[names][5]]
    allocated = [r for r in rows if r[2] & 2 and r[5]]
    code = next(r for r in allocated if strings[r[0]:].split(b"\0")[0] == b".text")
    if code[3] != 0x100000 or code[5] % 4096:
        return f".text at {code[3]:#x}, {code[5]:#x} bytes"
    size = max(r[3] + r[5] for r in allocated) - 0x100000
    window = package_window(identity)
    old = REPO / "build/sel4/qemu/packages" / (name + ".vpk")
    before = f", AArch32 {old.stat().st_size:#x} (last build, with header)" if old.exists() else ""
    if size > window:
        return f"OVER: {size:#x} > its {window:#x} window"
    return f"{size:#x} bytes, fits its {window:#x} window{before}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--open", action="store_true", help="also the Open Runtime (runtime/open), with fat LTO: slower")
    parser.add_argument("--no-link", action="store_true", help="Rust and C only")
    parser.add_argument("--targets", default="qemu,moto")
    parser.add_argument("--if-available", action="store_true",
                        help="succeed with a note when no toolchain has aarch64-unknown-none (make test-unit)")
    options = parser.parse_args()
    tool = rustc()
    if tool is None:
        message = f"no Rust toolchain with {TARGET}: set RUSTC_AARCH64, or add the target under build/*/rustup"
        if options.if_available:
            print("aarch64 services: skipped,", message)
            return
        raise SystemExit(message)
    failed = []
    for target in options.targets.split(","):
        out = OUT / target
        if out.exists():
            shutil.rmtree(out)
        env = generated(out)
        table = crates(target, out, options.open)
        with ThreadPoolExecutor(os.cpu_count() or 4) as pool:
            results = list(pool.map(lambda c: compile_rust(tool, env, target, out, c), table))
        for name, ok, errors in results:
            log = out / (name + ".log")
            print(f"{target} {name}: " + ("ok" if ok else f"FAILED ({errors} errors, {log})"))
            if not ok:
                failed.append(f"{target} {name}")
        if target != "qemu":
            continue
        compiler = kernel_compiler()
        if compiler is None:
            print(f"C: skipped, no AArch64 kernel build at {KERNEL.relative_to(REPO)} (make qemu64-image makes one)")
            continue
        cc, incs = compiler
        objects, extras, failures = compile_c(c_flags(cc), incs, out, options.open)
        for failure in failures:
            print("C FAILED:", failure)
        failed += failures
        print(f"C: {'ok' if not failures else f'{len(failures)} failed'} (transport, touch and storage shims, board shims, BearSSL and its glue, Sentinel and verifier crypto)")
        if options.no_link or failures:
            continue
        (out / "component.ld").write_text(LINK_SCRIPT)
        for name, identity, _, _ in table:
            if not (out / (name + ".a")).exists():
                continue
            verdict = link(cc, out, name, identity, objects, extras.get(name, []))
            print(f"link {name}: {verdict}")
            if verdict.startswith(("link failed", "not", ".text", "OVER")):
                failed.append(f"link {name}")
    if failed:
        raise SystemExit("aarch64 services: " + ", ".join(failed))
    print("aarch64 services: all compiled")


if __name__ == "__main__":
    main()
