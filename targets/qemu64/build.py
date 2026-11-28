"""Build the qemu64 image: the AArch64 kernel, the bootstrap task with its test program, and the entry stub.

    python3 targets/qemu64/build.py [--out DIR]     default build/sel4/qemu64

Writes DIR/qemu64.bin (one file QEMU loads at the start of RAM) and
DIR/qemu64.json (where to load it and where to start). Nothing here touches
the AArch32 build directories.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "targets/board"))
import board as boards
import image as layout


def find_rustc():
    rustc = os.environ.get("RUSTC") or shutil.which("rustc")
    local = REPO / "build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc"
    if not rustc and local.exists():
        rustc = local
    if not rustc:
        sys.exit("Install Rust and run rustup target add aarch64-unknown-none")
    return str(rustc)


def symbols(nm, elf):
    table = {}
    for line in subprocess.check_output([nm, elf], text=True).splitlines():
        parts = line.split()
        if len(parts) == 3:
            table[parts[2]] = int(parts[0], 16)
    return table


def build_kernel(board, out):
    """The board's AArch64 kernel (its delta-checked build); returns the CMake build directory."""
    spec = importlib.util.spec_from_file_location("violet_kernel64", REPO / board["kernel"]["build"])
    kernel_build = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(kernel_build)
    return kernel_build.build(out)


def kernel_tools(kernel):
    """(cc, libsel4 include flags) from the kernel build's own compile command for libsel4."""
    line = next(s for s in subprocess.check_output(["ninja", "-C", kernel, "-t", "commands", "sel4"], text=True).splitlines()
                if " -c " in s and s.endswith("/sel4_bootinfo.c"))
    args = shlex.split(line)
    incs = [a for a in args if a.startswith("-I") and ("libsel4" in a or "autoconf" in a or "gen_config" in a)]
    return Path(args[0]), incs


def c_flags(cc, board):
    """Freestanding C for the board's CPU: no FP or SIMD registers in C (the kernel saves them lazily)."""
    arch = boards.ARCHES[board["arch"]]
    return [cc, "--target=" + arch["clang_triple"], "-mcpu=" + board["cpu"], "-mgeneral-regs-only", "-Oz", "-ffreestanding",
            "-fno-builtin", "-fno-stack-protector", "-fno-unwind-tables", "-fno-asynchronous-unwind-tables", "-fno-pic",
            "-ffunction-sections", "-fdata-sections"]


def boot_image(out, board, kernel, root, root_entry, common, run):
    """Write out/qemu64.bin and out/qemu64.json: the kernel, the root task's flat image `root`
    (linked at the board's root va, entered at `root_entry`) and the entry stub with its boot
    page tables, behind the stage that copies them to the start of RAM."""
    regions = board["memory"]["regions"]
    address = lambda name, key="pa": boards.number(regions[name][key], name)
    cc = Path(common[0])
    ld, objcopy = (cc.with_name(n) for n in ("ld.lld", "llvm-objcopy"))
    root_va = address("root", "va")
    if len(root) > address("root", "size"):
        sys.exit(f"the root task is {len(root):#x} bytes, over its {address('root', 'size'):#x}-byte region")
    kernel_image, kernel_entry, kernel_va = layout.place_kernel((kernel / "kernel.elf").read_bytes(), address("kernel"), address("kernel", "size"))
    loader_pa = address("loader")
    tables_pa = loader_pa + layout.PAGE
    tables, lower, upper = layout.boot_tables(tables_pa, kernel_va, address("kernel"), address("kernel", "size"))
    redistributor = boards.number(board["devices"]["gic_redistributor"]["pa"], "gic")
    defines = {"GICR_IGROUPR0": redistributor + 0x10080, "TABLE_LOWER": lower, "TABLE_UPPER": upper,
               "ROOT_PA_START": address("root"), "ROOT_PA_END": address("root") + len(root),
               "ROOT_PV_OFFSET": address("root") - root_va, "ROOT_ENTRY": root_entry, "KERNEL_ENTRY": kernel_entry}
    run(common + [f"-D{name}={value:#x}" for name, value in defines.items()] + ["-c", HERE / "entry.S", "-o", "entry.o"])
    (out / "entry.ld").write_text(f"ENTRY(_start)\nSECTIONS {{ . = {loader_pa:#x}; .text : {{ *(.text*) }} /DISCARD/ : {{ *(.comment) *(.note*) }} }}")
    run([ld, "-T", "entry.ld", "entry.o", "-o", "entry.elf"])
    run([objcopy, "-O", "binary", "entry.elf", "entry.bin"])
    stub = (out / "entry.bin").read_bytes()
    if len(stub) > layout.PAGE or len(stub) + len(tables) > address("loader", "size"):
        sys.exit("the entry stub outgrew its page")
    base = boards.number(board["memory"]["ram"]["pa"], "ram")
    blob = layout.assemble([(address("kernel"), kernel_image), (address("root"), root),
                            (loader_pa, stub.ljust(layout.PAGE, b"\0") + tables)], base)
    blob += bytes(-len(blob) % 8)
    # QEMU overwrites the start of RAM at reset (stage.S): load at the staging address, copy at start.
    staging = address("staging")
    run(common + [f"-DIMAGE_BASE={base:#x}", f"-DIMAGE_BYTES={len(blob):#x}", f"-DENTRY={loader_pa:#x}", "-c", HERE / "stage.S", "-o", "stage.o"])
    run([objcopy, "-O", "binary", "-j", ".text", "stage.o", "stage.bin"])
    stage = (out / "stage.bin").read_bytes()
    if len(stage) != layout.PAGE or len(stage) + len(blob) > address("staging", "size"):
        sys.exit("the staged image does not fit its region")
    (out / "qemu64.bin").write_bytes(stage + blob)
    (out / "qemu64.json").write_text(json.dumps({"board": board["name"], "image": "qemu64.bin", "load": staging, "entry": staging}, indent=2) + "\n")
    return len(kernel_image)


def build(out):
    board = boards.load("qemu64")
    arch = boards.ARCHES[board["arch"]]
    regions = board["memory"]["regions"]
    address = lambda name, key="pa": boards.number(regions[name][key], name)
    kernel = build_kernel(board, out)
    out = out.resolve()

    def run(command, **kwargs):
        subprocess.run([str(c) for c in command], cwd=out, check=True, **kwargs)

    # The C compiler and libsel4 include paths the kernel build itself uses.
    cc, incs = kernel_tools(kernel)
    ld, objcopy, nm = (cc.with_name(n) for n in ("ld.lld", "llvm-objcopy", "llvm-nm"))
    common = c_flags(cc, board)
    rustc = find_rustc()
    rust = [rustc, "--edition=2021", "--target=" + arch["rust_target"], "-Cpanic=abort", "-Copt-level=s", "-Ccodegen-units=1",
            "-Crelocation-model=static", "-Ctarget-cpu=" + board["cpu"]]

    # The test program: a static ELF64 with its code, constants and data on separate pages.
    (out / "app.ld").write_text("""ENTRY(_start)
PHDRS { headers PT_PHDR PHDRS; text PT_LOAD FILEHDR PHDRS FLAGS(5); rodata PT_LOAD FLAGS(4); data PT_LOAD FLAGS(6); }
SECTIONS {
 . = 0x400000 + SIZEOF_HEADERS;
 .text : { KEEP(*(.text.start)) *(.text*) } :text
 . = ALIGN(4096);
 .rodata : { *(.rodata*) } :rodata
 . = ALIGN(4096);
 .data : { *(.data*) *(.got*) } :data
 .bss : { *(.bss*) *(COMMON) } :data
 /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}""")
    run(rust + ["--crate-type=staticlib", HERE / "app/hello.rs", "-o", "hello.a"])
    run([ld, "--gc-sections", "-z", "max-page-size=4096", "-T", "app.ld", "hello.a", "-o", "hello.elf"])

    # The C test program: the same checks from an ordinary musl program.
    import musl
    musl_cc, musl_link = musl.build(out)
    run(musl_cc + ["-c", HERE / "app/hello.c", "-o", "hello-c.o"])
    musl_link([out / "hello-c.o"], out / "hello-c.elf")

    # The bootstrap task.
    (out / "board.rs").write_text(boards.rust(board))
    # The generator and SHA-256 are the Open Runtime's and Storage's own sources, unchanged.
    modules = {"arch": REPO / "runtime/arch/mod.rs", "bootstrap": HERE / "bootstrap.rs", "linux": HERE / "linux.rs",
               "space": HERE / "space.rs", "entropy": REPO / "runtime/open/entropy.rs", "sha": REPO / "services/storage/crypt/sha.rs",
               "board": out / "board.rs", "root": HERE / "root.rs"}
    (out / "root-crate.rs").write_text("#![no_std]\n#![allow(dead_code)]\n" + "".join(
        f"#[path={json.dumps(str(path))}] mod {name};\n" for name, path in modules.items()))
    run(rust + ["--crate-type=staticlib", "root-crate.rs", "-o", "root.a"], env={**os.environ, "VIOLET_QEMU64_APP": str(out / "hello.elf"), "VIOLET_QEMU64_C_APP": str(out / "hello-c.elf")})
    run(common + incs + ["-DNDEBUG", "-std=gnu11", "-c", HERE / "sel4.c", "-o", "root-sel4.o"])
    root_va = address("root", "va")
    (out / "root.ld").write_text(f"""ENTRY(_start)
SECTIONS {{
 . = {root_va:#x};
 .image : {{ KEEP(*(.entry)) *(.text*) *(.rodata*) . = ALIGN(4096); *(.data*) *(.got*) *(.bss*) *(COMMON)
            . = ALIGN(16); . += 0x10000; root_stack_top = .; . = ALIGN(4096); image_end = .; }}
 /DISCARD/ : {{ *(.comment) *(.note*) *(.eh_frame*) }}
}}""")
    run([ld, "--gc-sections", "-T", "root.ld", "--start-group", "root-sel4.o", "root.a", "--end-group", "-o", "root.elf"])
    run([objcopy, "-O", "binary", "root.elf", "root.bin"])
    root_symbols = symbols(nm, out / "root.elf")
    root = (out / "root.bin").read_bytes().ljust(root_symbols["image_end"] - root_va, b"\0")
    kernel_bytes = boot_image(out, board, kernel, root, root_symbols["_start"], common, run)
    print(f"qemu64: kernel {kernel_bytes} bytes, bootstrap task {len(root)} bytes "
          f"with both test programs ({(out / 'hello.elf').stat().st_size} and {(out / 'hello-c.elf').stat().st_size} bytes)")
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=REPO / "build/sel4/qemu64")
    build(parser.parse_args().out)


if __name__ == "__main__":
    main()
