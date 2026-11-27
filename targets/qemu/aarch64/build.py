"""Build the 64-bit VioletOS Open Runtime boot image and optionally test in QEMU."""
import argparse
import mmap
import os
import re
from pathlib import Path
import selectors
import tempfile
import shutil
import subprocess
import time

from image import LOADER_ADDRESS, RAM_MIB, RAM_SIZE, build_image
from kernel import build_kernel
from bundle import pack


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def run(*args, cwd=None):
    subprocess.run(args, cwd=cwd, check=True)


def find_linker():
    linker = os.environ.get("LD_LLD") or shutil.which("ld.lld")
    if linker:
        return [linker] if not linker.endswith("rust-lld") else [linker, "-flavor", "gnu"]
    rustc = os.environ.get("RUSTC", "rustc")
    version = subprocess.check_output([rustc, "-vV"], text=True)
    host = next(line.split()[1] for line in version.splitlines() if line.startswith("host:"))
    sysroot = subprocess.check_output([rustc, "--print", "sysroot"], text=True).strip()
    return [Path(sysroot) / f"lib/rustlib/{host}/bin/rust-lld", "-flavor", "gnu"]


def boot(image, expect, expected_exit, timeout, ram_path=None):
    machine = "virt,virtualization=on,accel=tcg"
    ram_args = []
    memory, entropy, entropy_ready = None, None, False
    if ram_path is not None:
        from network import start_relay
        from viewer import positions, refill_entropy
        entropy = positions(image.parent, with_entropy=True)[3]
        with ram_path.open("w+b") as ram:
            ram.truncate(RAM_SIZE)
            memory = mmap.mmap(ram.fileno(), RAM_SIZE)
        machine += ",memory-backend=ram"
        ram_args = ["-object", f"memory-backend-file,id=ram,size={RAM_SIZE},mem-path={ram_path},share=on"]
    relay = None
    proc = subprocess.Popen([
        "qemu-system-aarch64", "-machine", machine, *ram_args, "-cpu", "neoverse-n1",
        "-m", f"{RAM_MIB}M", "-smp", "1", "-display", "none", "-serial", "stdio", "-monitor", "none", "-nic", "none",
        "-device", f"loader,file={image},addr={LOADER_ADDRESS:#x},cpu-num=0",
    ], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    serial = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ)
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if entropy_ready:
                    refill_entropy(memory, entropy)
                if not selector.select(.05):
                    continue
                chunk = os.read(proc.stdout.fileno(), 8192)
                if not chunk:
                    break
                serial.extend(chunk)
                if entropy is not None and not entropy_ready and b"VioletOS AArch64 Open Runtime" in serial:
                    entropy_ready = True
                if ram_path is not None and relay is None:
                    match = re.search(rb"VioletOS AArch64 network at 0x[0-9a-f]+(?: slots 0x[0-9a-f]+)?\r?\n", serial)
                    if match:
                        relay = start_relay(ram_path, match[0].decode())
                status = re.search(rb"VioletOS AArch64: exit ([0-9a-f]{16})", serial)
                if status:
                    if int(status[1], 16) == expected_exit and (expect is None or expect.encode() in serial):
                        return
                    break
                if re.search(rb"(unhandled page fault|guest exception)[^\n]*\n", serial) or b"unsupported guest ELF" in serial:
                    break
        raise SystemExit("Guest did not complete successfully; inspect serial.log")
    finally:
        if relay is not None:
            relay.stop()
        proc.terminate()
        proc.wait(timeout=5)
        if memory is not None:
            memory.close()
        image.with_name("serial.log").write_bytes(serial)
        print(serial.decode(errors="replace"), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, default=os.environ.get("MICROKIT_SDK"), help="Microkit SDK 2.4.0")
    parser.add_argument("--sel4", type=Path, default=os.environ.get("SEL4_SRC"), help="seL4 17.0.0 source directory")
    parser.add_argument("--microkit-src", type=Path, default=os.environ.get("MICROKIT_SRC"), help="Microkit 2.4.0 sources for the Cortex-A57 loader")
    parser.add_argument("--elf", type=Path, help="static AArch64 ET_EXEC to boot; default builds the memory probe")
    parser.add_argument("--root", type=Path, help="directory of guest files, including the ELF interpreter and shared libraries")
    parser.add_argument("--network", action="store_true", help="include the VioletOS QEMU socket relay device")
    parser.add_argument("--trace", action="store_true", help="print Linux syscall numbers and results to the serial console")
    parser.add_argument("--arg", action="append", default=[], help="an argument passed to the guest executable")
    parser.add_argument("--expect", help="required text in the guest serial output")
    parser.add_argument("--timeout", type=float, help="seconds to wait for guest exit (default 30, 120 with networking)")
    parser.add_argument("--expect-exit", type=int, default=0, help="expected Linux exit status")
    parser.add_argument("--out", type=Path, default=ROOT / "build/sel4/qemu-aarch64")
    parser.add_argument("--test", action="store_true")
    args = parser.parse_args()
    if args.timeout is None:
        args.timeout = 120 if args.network else 30
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if not args.sdk:
        parser.error("supply --sdk or MICROKIT_SDK")
    if not args.sel4:
        parser.error("supply --sel4 or SEL4_SRC")
    board = args.sdk.resolve() / "board/qemu_virt_aarch64/debug"
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    kernel = build_kernel(args.sel4.resolve(), out)
    # Give the native runtime SVC 1; the unmodified guest uses Linux SVC 0.
    # Use the A57 kernel config (44-bit VAs) with the matching SDK headers.
    inc = out / "native-include"
    name = Path("sel4/sel4_arch/syscalls.h")
    (inc / name).parent.mkdir(parents=True, exist_ok=True)
    native = (board / "include" / name).read_text()
    if '"svc #0"' not in native:
        raise ValueError("unexpected libsel4 AArch64 syscall header")
    (inc / name).write_text(native.replace('"svc #0"', '"svc #1"'))
    microkit_src = args.microkit_src or (args.sdk.resolve().parent / "microkit-2.4.0")
    loader_dir = out / f"loader-{LOADER_ADDRESS:x}"
    loader_dir.mkdir(exist_ok=True)
    loader_sdk = out / "loader-sdk"
    loader_sdk.mkdir(exist_ok=True)
    if not (loader_sdk / "include").exists():
        (loader_sdk / "include").symlink_to(kernel / "gen_config", target_is_directory=True)
    run("make", "-C", microkit_src / "loader", "-j4", f"BUILD_DIR={loader_dir}", "ARCH=aarch64",
        "BOARD=qemu_virt_aarch64", f"LINK_ADDRESS={LOADER_ADDRESS:#x}", "GCC_CPU=cortex-a57",
        "TARGET_TRIPLE=aarch64-none-elf", "LLVM=True", "CPP=clang -E -P -x c -target aarch64-none-elf",
        f"SEL4_SDK={loader_sdk}", "all")
    cc = [os.environ.get("CC", "clang"), "--target=aarch64-none-elf", "-ffreestanding", "-fno-stack-protector",
          "-mgeneral-regs-only", "-mstrict-align", "-Wall", "-Wextra", "-Wno-unused-parameter", "-O2", "-I", inc, "-I", kernel / "gen_config", "-I", board / "include"]
    ld = find_linker()
    src = ROOT / "runtime/open/aarch64"
    run(*cc, "-fPIC", "-c", src / "vdso.c", "-o", out / "vdso.o")
    run(*ld, "-shared", "-s", "-z", "max-page-size=4096", "--hash-style=sysv", "--soname=linux-vdso.so.1", "-T", src / "vdso.ld",
        out / "vdso.o", "-o", out / "vdso.so")
    if args.elf is None:
        run(*cc, "-c", HERE / "probe.S", "-o", out / "probe.o")
        run(*ld, "-static", out / "probe.o", "-o", out / "probe.elf")
        elf = out / "probe.elf"
    else:
        elf = args.elf.resolve()
    # A fixed local filename avoids interpolating input paths into assembly.
    target = out / "payload.elf"
    if elf != target:
        shutil.copyfile(elf, target)
    program_name = "/apps/program"
    if args.root and elf.is_relative_to(args.root.resolve()):
        program_name = "/" + str(elf.relative_to(args.root.resolve()))
    argv = [os.fsencode(program_name), *(os.fsencode(arg) for arg in args.arg)]
    if len(argv) > 32 or sum(len(arg) + 1 for arg in argv) > 128 * 1024:
        parser.error("the bootstrap accepts up to 32 arguments and 128 KiB total")
    (out / "argv.bin").write_bytes(b"".join(arg + b"\0" for arg in argv))
    embed = out / "payload.S"
    embed.write_text('.section .rodata\n.balign 8\n.global guest_start, guest_end\nguest_start:\n.incbin "payload.elf"\nguest_end:\n.global argv_start, argv_end\nargv_start:\n.incbin "argv.bin"\nargv_end:\n')
    with embed.open("a") as stream:
        stream.write('.section .rodata\n.balign 16\n.global vdso_start, vdso_end\nvdso_start:\n.incbin "vdso.so"\nvdso_end:\n')
        stream.write(pack(args.root, out, program_name))
    run(*cc, "-c", embed.name, "-o", "payload.o", cwd=out)
    libraries = []
    if args.network:
        import sys
        sys.path.insert(0, str(ROOT / "tools"))
        from build_initfs import toolchain
        target_triple = "aarch64-unknown-linux-musl"
        rustc, _ = toolchain(target_triple)
        run(rustc, "--edition=2021", "--crate-type=staticlib", "--target", target_triple,
            "-C", "relocation-model=static", "-C", "panic=abort", "-O", src / "net.rs", "-o", out / "net.a")
        cc.append("-DVIOLET_NETWORK")
        libraries.append("net.a")
    run(*cc, *(["-DVIOLET_TRACE"] if args.trace else []), "-c", src / "runtime.c", "-o", out / "runtime.o")
    run(*cc, "-c", src / "start.S", "-o", out / "start.o")
    run(*ld, "--image-base=0x200000", "-e", "_start", "start.o", "runtime.o", "payload.o", *libraries, "-o", "root.elf", cwd=out)
    image = out / "violet.img"
    random_offset = build_image(loader_dir / "loader.elf", kernel / "kernel.elf", out / "root.elf", image)
    if args.test:
        expect = args.expect
        if not args.elf and expect is None:
            expect = "64 GiB plus 4432406249472-byte reservation and page commit passed"
        # Personalize a temporary boot image for this invocation. The reusable
        # image carries no entropy and cannot hand out repeated random bytes.
        with tempfile.NamedTemporaryFile(dir=out, suffix=".img") as fresh:
            with image.open("rb") as src:
                shutil.copyfileobj(src, fresh)
            fresh.seek(random_offset)
            fresh.write(b"VOSRAND1" + time.time_ns().to_bytes(8, "little") + os.urandom(4096))
            fresh.flush()
            if args.network:
                with tempfile.TemporaryDirectory(dir=out) as ram_dir:
                    boot(Path(fresh.name), expect, args.expect_exit, args.timeout, Path(ram_dir) / "guest.ram")
            else:
                boot(Path(fresh.name), expect, args.expect_exit, args.timeout)


if __name__ == "__main__":
    main()
