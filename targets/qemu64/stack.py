"""Build the VioletOS system itself for AArch64 on QEMU virt: the supervisor, its
services, the Open Runtime and Sentinel as signed AArch64 packages, and the open
apps for aarch64 Linux, on the same delta-checked 64-bit kernel and entry stub
as the bring-up image.

    python3 targets/qemu64/stack.py [--out DIR]     default build/sel4/qemu-aarch64
    make qemu64-image QEMU64_STACK=1                the same, after the bring-up image
    VIOLET_ARCH=aarch64 python3 tools/run.py VIEWER boots it

It is the QEMU half of platform/sel4/loader/build.py with one architecture
parameter: every compiler, linker and package step is the AArch32 build's, for
VIOLET_ARCH=aarch64 (image/packages/violet_arch.py). Packages carry AArch64 in
their signed ABI word (image/packages/package_format.py ARCH_ABI), so this
supervisor and the AArch32 one each refuse the other's.

Not here yet, and said where it matters: verified boot (the AArch32 loader
checks a signed manifest; this image is entered by the qemu64 entry stub, which
checks nothing) and the user timer (this kernel does not export it, so the
timekeeper and its guard are off and the Open Runtime polls): both are written
to differences.json, which test_system.py --arch aarch64 prints and checks;
the handset, and parts of the 64-bit app ABI (fork, a 47-bit
layout; violet_arch.APP_ABI_UNPORTED). The build
stops, naming the reason, where a layer is not ported (violet_arch.UNPORTED),
and the release gate (tools/elf_check.py, which reads ELF64) refuses any app
it would not load. Apps whose toolchain is missing (an AArch64 musl and Python
from tools/build_python.py, Rust's aarch64-unknown-linux-musl) are skipped and named.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "targets/board"))
sys.path.insert(0, str(REPO / "image/packages"))
sys.path.insert(0, str(REPO / "tools"))
import board as boards  # noqa: E402


def module(name, path):
    """Load `path` as `name`: two build.py files (this target's and the package builder) take part."""
    spec = importlib.util.spec_from_file_location(name, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded

ARCH = "aarch64"
# The root task's window from its link base: image, built-in catalog, stack.
# The same size as on AArch32 (platform/sel4/loader/build.py), in the board's
# 4 MiB root region; 64-bit code is larger, and the build says when it no
# longer leaves the stack its 8 KiB.
ROOT_BYTES = 0x180000


def rust_flags(rustc, board):
    return [rustc, "--edition=2021", "--target=" + boards.ARCHES[board["arch"]]["rust_target"], "-Cpanic=abort",
            "-Copt-level=s", "-Ccodegen-units=1", "-Ctarget-cpu=" + board["cpu"]]


def find_rustc(target):
    """A rustc with `target`'s standard library: RUSTC, rustc on PATH, then the toolchains under build/."""
    import glob
    import shutil
    for rustc in filter(None, [os.environ.get("RUSTC"), shutil.which("rustc"),
                               *sorted(glob.glob(str(REPO / "build/*/rustup/toolchains/*/bin/rustc")))]):
        sysroot = subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
        if sysroot and (Path(sysroot) / "lib/rustlib" / target).is_dir():
            return rustc
    sys.exit(f"No rustc with the {target} target: rustup target add {target} (or python3 targets/qemu64/toolchain.py)")


def build(out):
    os.environ["VIOLET_ARCH"] = ARCH
    import violet_arch
    # Before minutes of kernel build: stop now if the components cannot be built for this architecture.
    violet_arch.require_components()
    image = module("qemu64_image_build", HERE / "build.py")
    packages = module("violet_package_build", REPO / "image/packages/build.py")
    board = boards.load("qemu64")
    if board["arch"] != ARCH:
        sys.exit(f"the qemu64 board is {board['arch']}, not {ARCH}")
    out.mkdir(parents=True, exist_ok=True)
    out = out.resolve()
    bearssl = REPO / "build/sel4/bearssl-0.6"
    if not bearssl.is_dir():
        sys.exit(f"{bearssl} is missing: make qemu-image fetches it (pinned, kernel/sel4/build.py)")
    kernel = image.build_kernel(board, out)
    cc, incs = image.kernel_tools(kernel)
    # The timer the supervisor and the Open Runtime compile against is the
    # kernel's (vm.h VM_USER_TIMER), or the build stops.
    headers = [p.read_text() for inc in incs for p in Path(inc[2:]).glob("**/gen_config.h")]
    try:
        timer = image.layout.user_timer((Path(kernel) / "CMakeCache.txt").read_text(), headers)
    except ValueError as why:
        sys.exit(f"qemu-aarch64: {why}")
    ld, objcopy = (cc.with_name(n) for n in ("ld.lld", "llvm-objcopy"))
    rustc = find_rustc(boards.ARCHES[ARCH]["rust_target"])
    # The AArch32 build's flags with the architecture part from violet_arch, plus
    # strict alignment: the supervisor parses catalogs and keys in windows it maps
    # as device memory, where an unaligned access faults on AArch64.
    common = [cc, *violet_arch.clang_flags(), "-mstrict-align", "-Oz", "-ffreestanding", "-fno-builtin",
              "-fno-stack-protector", "-fno-unwind-tables", "-fno-asynchronous-unwind-tables", "-fno-pic",
              "-ffunction-sections", "-fdata-sections"]
    platform_include = REPO / "platform/sel4/include"
    loader = REPO / "platform/sel4/loader"
    defines = ["-DNDEBUG", f"-DROOT_BYTES={ROOT_BYTES:#x}", "-DVIOLET_CORES=" + os.environ.get("VIOLET_CORES", "1"),
               "-DVIOLET_QEMU"]

    def run(command, **kwargs):
        subprocess.run([str(c) for c in command], cwd=out, check=True, **kwargs)

    # The packages, exactly as the AArch32 QEMU build makes them.
    sys.path.insert(0, str(REPO / "services/touch"))
    from firmware import prepare
    prepare(out, "qemu", None)
    os.environ["VIOLET_FIRMWARE_RS"] = str(out / "firmware.rs")
    os.environ["VIOLET_CATALOG_VOLUME_RS"] = str(REPO / "system/package-manager/catalog_volume.rs")
    shims = []
    for source in [REPO / "services/touch/events.c", REPO / "services/storage/storage_ipc.c",
                   REPO / "platform/sel4/board/watchdog.c", REPO / "targets/qemu/fixture.c", loader / "strings.c"]:
        obj = out / ("component-" + source.name + ".o")
        run(common + incs + defines + ["-DVIOLET_COMPONENT", "-I", out, "-I", platform_include, "-std=gnu11", "-c", source, "-o", obj])
        shims.append(obj)
    packages.components(REPO, out, cc, common, incs, rustc, shims, None, None, None, "qemu")

    # The supervisor: the root task, with the built-in catalog and the package verifier.
    (out / "catalog.S").write_text('.section .rodata.catalog,"a"\n.balign 4\n.global builtin_catalog,builtin_catalog_end\n'
                                   'builtin_catalog: .incbin "catalog.bin"\nbuiltin_catalog_end:\n.balign 4\n'
                                   '.global recovery_manager\nrecovery_manager: .incbin "packages/package_manager.vpk"\n')
    run(common + ["-c", out / "catalog.S", "-o", "catalog.o"])
    from sign import config as signing_config, write_keys, summary as sign_summary
    signer = signing_config(REPO, "qemu")
    os.environ["VIOLET_PACKAGE_KEYS_RS"] = str(write_keys(signer, out))
    sys.path.insert(0, str(REPO / "services/radio"))
    from build_tls import build as build_crypto
    run(common + ["-I", bearssl / "inc", "-I", REPO / "services/radio/freestanding", "-c",
                  REPO / "system/package-manager/verify_crypto.c", "-o", "verify-crypto.o"])
    rust = rust_flags(rustc, board)
    run(rust + ["--crate-type=lib", "--emit=obj", REPO / "system/package-manager/verify.rs", "-o", "package-verify.o"])
    run(rust + ["--crate-type=staticlib", "--cfg", "package_volume", REPO / "system/package-manager/volume.rs", "-o", "volume.a"])
    root_va = boards.number(board["memory"]["regions"]["root"]["va"], "root va")
    if root_va != 0x100000:
        sys.exit(f"the supervisor's AArch64 entry (platform/sel4/loader/root64.S) puts its stack above 0x100000, not {root_va:#x}")
    run(common + [f"-DROOT_BYTES={ROOT_BYTES:#x}", "-c", loader / "root64.S", "-o", "supervisor-entry.o"])
    for source in (loader / "strings.c", REPO / "system/supervisor/supervisor.c"):
        run(common + incs + defines + ["-I", out, "-I", platform_include, "-std=gnu11", "-c", source, "-o", source.stem + ".o"])
    (out / "root.ld").write_text(f"ENTRY(_start)\nSECTIONS {{ . = {root_va:#x}; .image : {{ KEEP(*(.entry)) *(.text*) *(.rodata*) "
                                 ". = ALIGN(4096); writable_start = .; *(.data*) *(.got*) *(.bss*) *(COMMON) image_end = .; } "
                                 "/DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) } }")
    run([ld, "--gc-sections", "-z", "max-page-size=4096", "-T", "root.ld", "supervisor-entry.o", "supervisor.o", "strings.o",
         "volume.a", "catalog.o", "package-verify.o", "verify-crypto.o", build_crypto(REPO, out, common)[1], "-o", "root.elf"])
    run([objcopy, "-O", "binary", "root.elf", "root.bin"])
    root = (out / "root.bin").read_bytes()
    print(f"root.bin: {len(root)} bytes, {ROOT_BYTES - len(root)} left for the root stack (at least 8192 required)")
    if len(root) > ROOT_BYTES - 8192:
        sys.exit(f"root.bin is {len(root)} bytes: under 8192 bytes would be left for the root task's stack")
    print(sign_summary(signer))
    # The kernel maps the whole window for the root task: image, then stack.
    entry = image.symbols(cc.with_name("llvm-nm"), out / "root.elf")["_start"]
    kernel_bytes = image.boot_image(out, board, kernel, root.ljust(ROOT_BYTES, b"\0"), entry, image.c_flags(cc, board), run)

    # The open apps for aarch64 Linux, through the same release gate, and
    # this target's test program (test-apps/abi64: signals and processes,
    # which targets/qemu/test_system.py --arch aarch64 runs from Home). This
    # image is for development only (no verified boot), as the AArch32 test
    # image with its test apps is.
    from build_initfs import main as build_initfs
    extra = [str(HERE / "test-apps")] + [d for d in os.environ.get("VIOLET_OPEN_EXTRA_APPS", "").split(":") if d]
    saved = os.environ.get("VIOLET_OPEN_EXTRA_APPS")
    os.environ["VIOLET_OPEN_EXTRA_APPS"] = ":".join(extra)
    try:
        build_initfs(out / "initfs.cpio", arch=ARCH)
    finally:
        if saved is None:
            os.environ.pop("VIOLET_OPEN_EXTRA_APPS")
        else:
            os.environ["VIOLET_OPEN_EXTRA_APPS"] = saved
    # What this image gives up against AArch32, for test_system.py --arch aarch64 to print and check.
    differences = image.layout.differences(timer)
    import json
    (out / "differences.json").write_text(json.dumps({"verified_boot": False, "user_timer": timer,
                                                      "differences": differences}, indent=2) + "\n")
    print(f"qemu-aarch64: kernel {kernel_bytes} bytes, supervisor {len(root)} bytes")
    for d in differences:
        print(f"qemu-aarch64: security difference from AArch32: {d}")
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=REPO / "build/sel4/qemu-aarch64")
    build(parser.parse_args().out)


if __name__ == "__main__":
    main()
