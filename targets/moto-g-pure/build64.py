"""Build VioletOS for the XT2163DL in AArch64: `make moto64`.

    VIOLET_STOCK_BOOT=.../boot.img python3 targets/moto-g-pure/build64.py [--out DIR] [--probe-stage N]

Writes DIR/boot.img (default build/sel4/moto-aarch64), the handset's boot_b
image in the same layout as the AArch32 one (platform/sel4/loader/build.py):
the Android v2 boot image, then the storage marker at 16 MiB, the 1 MiB
volume, the boot history and the two catalog banks. What differs:

- the boot header asks MediaTek LK for a 64-bit kernel (tools/phone_image.py
  aarch64_v2: bootopt=64S3,32N2,64N2, an arm64 Image.gz, kernel_addr
  0x40080000; README.md "AArch64 boot path" has the measurements);
- the kernel is the delta-checked AArch64 seL4 (kernel/sel4/aarch64, --target
  moto: the factory device tree, the 13 MHz timer, EL1);
- the loader is boot/loader/loader64.S with the shared loader.c: the same
  handoff checks, panel detection, radio profile, firmware staging, catalog
  bank and boot history (stages 21-23) as the 32-bit one, then the drop to
  EL1, the boot page tables and the kernel's elfloader-style entry;
- the supervisor, services, Storage, Sentinel and Open Runtime are the AArch32
  build's sources for VIOLET_ARCH=aarch64 (image/packages/build.py), and the
  open apps are aarch64 Linux programs (tools/build_initfs.py).

Not in this image, and why: Camera (its four frame sections in slots 8-11 are
AArch32 frames; supervisor.c skips it on AArch64), and anything needing the
32-bit kernel's secure-monitor extension (Wi-Fi's memory windows and the
random number call: refused, so Wi-Fi does not start). The user timer is off
as on qemu64 (KernelArmExportPTMRUser=OFF): no timekeeper, the Open Runtime
polls. --probe-stage N makes the loader set the RTC fast-boot flag and reset at
its stage N (1 entered C, 2 handoff checked, 3 boot history open, 4 about to
enter the kernel), for bring-up without a console.
"""
import argparse
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
ARCH = "aarch64"
STOCK_SHA256 = "36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a"
# Physical layout (boot/loader/loader64.S, loader64.ld). LK puts the Image at
# 0x40080000, inside the kernel's own destination (0x40000000 to the end of
# its zeroed data), so the kernel is staged and moved by a trampoline.
KERNEL_PA = 0x40000000
KERNEL_REGION = 0x00400000      # two 2 MiB blocks in the boot tables
KERNEL_STAGE = 0x43100000       # up to 1 MiB (the kernel's file part)
TRAMPOLINE = 0x43000000
TABLES_PA = 0x43001000          # five pages of boot page tables
ROOT_PA = 0x42800000            # kernel RAM 0x42700000-0x43c00000, clear of the loader image
ROOT_BYTES = 0x200000           # the supervisor with its built-in catalog, then its stack
# The handset's packages (platform/sel4/loader/build.py) less Camera.
SELECTED = {"launcher", "runtime", "touch", "display", "storage", "power", "radio", "usb-diagnostics", "audio",
            "open", "sentinel"}

sys.path.insert(0, str(REPO / "targets/board"))
sys.path.insert(0, str(REPO / "image/packages"))
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "targets/qemu64"))


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


def kernel_image(elf):
    """(file bytes to stage, bytes the kernel spans from KERNEL_PA, virtual entry, virtual base)."""
    import image as layout
    full, entry, vbase = layout.place_kernel(elf, KERNEL_PA, KERNEL_REGION)
    span = len(full) + (-len(full) % 8)
    data = full.rstrip(b"\0")
    data += bytes(-len(data) % 8)
    return data, span, entry, vbase


def link_loader(out, work, kernel_elf, probe_stage=None):
    """Link the loader (loader64.S, loader.c, libfdt, the boot history) in `work`
    around `kernel_elf` and the root task, firmware and flags the build left in
    `out`: (Image bytes, the addresses it was linked with). The handset build
    calls it with its kernel; targets/moto-g-pure/test_boot.py --arch aarch64
    with QEMU's (the same loader and supervisor on QEMU's interrupt controller)."""
    import image as layout
    sys.path.insert(0, str(REPO / "targets/qemu64"))
    qemu64 = module("qemu64_image_build", REPO / "targets/qemu64/build.py")
    saved = json.loads((out / "loader.json").read_text())
    cc, defines = Path(saved["cc"]), saved["defines"]
    ld, objcopy, nm = (cc.with_name(n) for n in ("ld.lld", "llvm-objcopy", "llvm-nm"))
    work.mkdir(parents=True, exist_ok=True)

    def run(command):
        subprocess.run([str(c) for c in command], cwd=work, check=True)

    common = [cc, "--target=aarch64-none-elf", "-mcpu=cortex-a53", "-mgeneral-regs-only", "-mstrict-align", "-Oz",
              "-ffreestanding", "-fno-builtin", "-fno-stack-protector", "-fno-unwind-tables",
              "-fno-asynchronous-unwind-tables", "-fno-pic", "-ffunction-sections", "-fdata-sections"]
    root_entry = qemu64.symbols(nm, out / "root.elf")["_start"]
    # The kernel, staged, and the boot page tables the trampoline turns the MMU on with.
    kernel_data, kernel_span, kernel_entry, kernel_va = kernel_image(Path(kernel_elf).read_bytes())
    (work / "kernel.bin").write_bytes(kernel_data)
    tables, lower, upper = layout.boot_tables(TABLES_PA, kernel_va, KERNEL_PA, KERNEL_REGION)
    (work / "boot-tables.bin").write_bytes(tables)
    (work / "payload.S").write_text(
        '.section .payload,"a"\n.balign 8\n.global kernel,kernel_end,root,root_end,boot_tables,boot_tables_end\n'
        'kernel: .incbin "kernel.bin"\nkernel_end:\n'
        f'.balign 8\nroot: .incbin "{out / "root.bin"}"\nroot_end:\n'
        '.balign 8\nboot_tables: .incbin "boot-tables.bin"\nboot_tables_end:\n'
        f'.balign 8\n.global radio_firmware,radio_firmware_end\nradio_firmware: .incbin "{out / "wifi-firmware.bin"}"\n'
        'radio_firmware_end:\n')
    places = {"KERNEL_PA": KERNEL_PA, "KERNEL_STAGE": KERNEL_STAGE, "KERNEL_FILE": len(kernel_data),
              "KERNEL_SPAN": kernel_span, "TABLES_PA": TABLES_PA, "TABLE_LOWER": lower, "TABLE_UPPER": upper,
              "ROOT_PA": ROOT_PA, "ROOT_PA_START": ROOT_PA, "ROOT_PA_END": ROOT_PA + ROOT_BYTES,
              "ROOT_PV_OFFSET": ROOT_PA - 0x100000, "ROOT_ENTRY": root_entry, "KERNEL_ENTRY": kernel_entry}
    loader_defines = defines + [f"-D{k}={v:#x}" for k, v in places.items()]
    if probe_stage is not None:
        loader_defines.append(f"-DVIOLET_PROBE64_STAGE={probe_stage}")
    fdt = REPO / "build/sel4/dtc-1.7.2/libfdt"
    ndk = cc.parent.parent / "sysroot/usr/include"
    headers = ["-I" + str(fdt), "-isystem", ndk / "aarch64-linux-android", "-isystem", ndk]
    objects = []
    for source in [REPO / "boot/loader/loader64.S", REPO / "boot/loader/loader.c",
                   REPO / "platform/sel4/loader/strings.c", work / "payload.S"] + \
            [fdt / (n + ".c") for n in ("fdt", "fdt_ro", "fdt_check", "fdt_addresses")]:
        obj = "loader-" + source.name + ".o"
        run(common + loader_defines + headers + ["-c", source, "-o", obj])
        objects.append(obj)
    objects.append(out / "loader-history.a")
    run([ld, "--gc-sections", "-z", "max-page-size=4096", f"--defsym=ROOT_PA={ROOT_PA:#x}", "-T",
         REPO / "boot/loader/loader64.ld", *objects, "-o", "loader.elf"])
    run([objcopy, "-O", "binary", "loader.elf", "Image"])
    image = (work / "Image").read_bytes()
    loader_symbols = qemu64.symbols(nm, work / "loader.elf")
    if image[0x38:0x3c] != b"ARM\x64" or struct.unpack_from("<Q", image, 16)[0] != len(image) or \
            loader_symbols["_start"] != 0x40080000:
        sys.exit("loader: the Image header or link address is not what LK's 64-bit path takes")
    return image, places


def build(out, stock_path, probe_stage=None):
    os.environ["VIOLET_ARCH"] = ARCH
    import violet_arch
    violet_arch.require_components()
    import board as boards
    import image as layout
    stock = Path(stock_path).read_bytes()
    if hashlib.sha256(stock).hexdigest() != STOCK_SHA256:
        sys.exit("VIOLET_STOCK_BOOT is not the XT2163DL S3RHS32.20-42-13-2-3-2 factory boot.img")
    out.mkdir(parents=True, exist_ok=True)
    out = out.resolve()
    qemu64 = module("qemu64_image_build", REPO / "targets/qemu64/build.py")
    packages = module("violet_package_build", REPO / "image/packages/build.py")
    kernel_build = module("violet_kernel64", REPO / "kernel/sel4/aarch64/build.py")
    kernel = kernel_build.build(out, "moto", Path(stock_path))
    cc, incs = qemu64.kernel_tools(kernel)
    ld, objcopy, nm = (cc.with_name(n) for n in ("ld.lld", "llvm-objcopy", "llvm-nm"))
    stack = module("qemu64_stack_build", REPO / "targets/qemu64/stack.py")
    rustc = stack.find_rustc(boards.ARCHES[ARCH]["rust_target"])
    common = [cc, *violet_arch.clang_flags(), "-mstrict-align", "-Oz", "-ffreestanding", "-fno-builtin",
              "-fno-stack-protector", "-fno-unwind-tables", "-fno-asynchronous-unwind-tables", "-fno-pic",
              "-ffunction-sections", "-fdata-sections"]
    platform_include = REPO / "platform/sel4/include"
    loader_dir = REPO / "platform/sel4/loader"
    defines = ["-DNDEBUG", f"-DROOT_BYTES={ROOT_BYTES:#x}", "-DVIOLET_CORES=1", "-DVIOLET_MOTO"]

    def run(command, **kwargs):
        subprocess.run([str(c) for c in command], cwd=out, check=True, **kwargs)

    # The packages, as the AArch32 handset build makes them.
    sys.path.insert(0, str(REPO / "services/touch"))
    from firmware import prepare
    prepare(out, "moto", Path(stock_path).parent)
    os.environ["VIOLET_FIRMWARE_RS"] = str(out / "firmware.rs")
    os.environ["VIOLET_CATALOG_VOLUME_RS"] = str(REPO / "system/package-manager/catalog_volume.rs")
    board_spi = HERE / "spi_mmio.c"
    shims = []
    for source in [REPO / "services/touch/events.c", REPO / "services/storage/storage_ipc.c",
                   REPO / "platform/sel4/board/watchdog.c", board_spi, loader_dir / "strings.c"]:
        obj = out / ("component-" + source.name + ".o")
        run(common + incs + defines + ["-DVIOLET_COMPONENT", "-I", out, "-I", platform_include, "-std=gnu11", "-c", source, "-o", obj])
        shims.append(obj)
    packages.components(REPO, out, cc, common, incs, rustc, shims, None, SELECTED, HERE / "emmc.rs", "moto")

    # The supervisor: root task, built-in catalog, package verifier, boot history.
    (out / "catalog.S").write_text('.section .rodata.catalog,"a"\n.balign 4\n.global builtin_catalog,builtin_catalog_end\n'
                                   'builtin_catalog: .incbin "catalog.bin"\nbuiltin_catalog_end:\n.balign 4\n'
                                   '.global recovery_manager\nrecovery_manager: .incbin "packages/package_manager.vpk"\n')
    run(common + ["-c", out / "catalog.S", "-o", "catalog.o"])
    from sign import config as signing_config, write_keys, summary as sign_summary, DEVELOPER_KEY_ID
    signer = signing_config(REPO, "moto")
    os.environ["VIOLET_PACKAGE_KEYS_RS"] = str(write_keys(signer, out))
    sys.path.insert(0, str(REPO / "services/radio"))
    from build_tls import build as build_crypto
    bearssl = REPO / "build/sel4/bearssl-0.6"
    run(common + ["-I", bearssl / "inc", "-I", REPO / "services/radio/freestanding", "-c",
                  REPO / "system/package-manager/verify_crypto.c", "-o", "verify-crypto.o"])
    board = {"arch": ARCH, "cpu": "cortex-a53"}
    rust = stack.rust_flags(rustc, board)
    run(rust + ["--crate-type=lib", "--emit=obj", REPO / "system/package-manager/verify.rs", "-o", "package-verify.o"])
    if DEVELOPER_KEY_ID in signer.keys:
        (out / "developer-key.h").write_text("static unsigned char developer_key[] = {" +
                                             ",".join(map(str, signer.keys[DEVELOPER_KEY_ID])) + "};\n")
        defines.append("-DVIOLET_DEVELOPER_KEY")
    for name, flags in [("loader-history.a", ["--cfg", "loader_trace", "--cfg", "package_catalog"]),
                        ("root-history.a", ["--cfg", "package_catalog"])]:
        run(rust + ["--crate-type=staticlib", "--cfg", "boot_trace", *flags, HERE / "boot_trace.rs", "-o", name])
    run(common + [f"-DROOT_BYTES={ROOT_BYTES:#x}", "-c", loader_dir / "root64.S", "-o", "supervisor-entry.o"])
    for source in (loader_dir / "strings.c", REPO / "system/supervisor/supervisor.c"):
        run(common + incs + defines + ["-I", out, "-I", platform_include, "-std=gnu11", "-c", source, "-o", source.stem + ".o"])
    run(common + incs + defines + ["-I", platform_include, "-c", board_spi, "-o", "board-map.o"])
    (out / "root.ld").write_text("ENTRY(_start)\nSECTIONS { . = 0x100000; .image : { KEEP(*(.entry)) *(.text*) *(.rodata*) "
                                 ". = ALIGN(4096); writable_start = .; *(.data*) *(.got*) *(.bss*) *(COMMON) image_end = .; } "
                                 "/DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) } }")
    run([ld, "--gc-sections", "-z", "max-page-size=4096", "-T", "root.ld", "supervisor-entry.o", "supervisor.o", "strings.o",
         "board-map.o", "catalog.o", "root-history.a", "package-verify.o", "verify-crypto.o", build_crypto(REPO, out, common)[1],
         "-o", "root.elf"])
    run([objcopy, "-O", "binary", "root.elf", "root.bin"])
    root = (out / "root.bin").read_bytes()
    print(f"root.bin: {len(root)} bytes, {ROOT_BYTES - len(root)} left for the root stack (at least 8192 required)")
    if len(root) > ROOT_BYTES - 8192:
        sys.exit(f"root.bin is {len(root)} bytes: under 8192 bytes would be left for the root task's stack")
    print(sign_summary(signer))
    (out / "loader.json").write_text(json.dumps({"defines": defines, "cc": str(cc)}) + "\n")

    # The open apps for aarch64 Linux (the handset's set), gzip in the ramdisk.
    from build_initfs import main as build_initfs, HANDSET_SKIP
    build_initfs(out / "initfs.cpio", python=False, compress=True, skip=HANDSET_SKIP, arch=ARCH)
    sys.path.insert(0, str(HERE))
    from wifi_firmware import prepare_wifi
    prepare_wifi(Path(stock_path).parent, out / "wifi-firmware.bin")
    image, places = link_loader(out, out, kernel / "kernel.elf", probe_stage)
    kernel_data = (out / "kernel.bin").read_bytes()
    kernel_span = places["KERNEL_SPAN"]

    # The boot image: header for LK's AArch64 path, then the handset's storage layout.
    from phone_image import aarch64_v2, handset_storage
    import package_format
    boot = handset_storage(aarch64_v2(stock, (out / "initfs.cpio").read_bytes(), image), package_format.HANDSET_LIMIT)
    (out / "boot.img").write_bytes(boot)
    record = {"arch": ARCH, "probe_stage": probe_stage, "boot.img": hashlib.sha256(boot).hexdigest(),
              "Image": hashlib.sha256(image).hexdigest(), "image_bytes": len(image), "root_bytes": len(root),
              "kernel_file_bytes": len(kernel_data), "kernel_span": kernel_span,
              "places": {k: f"{v:#x}" for k, v in places.items()}}
    (out / "boot.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"moto64: Image {len(image)} bytes, kernel {len(kernel_data)} of {kernel_span:#x}, supervisor {len(root)} bytes")
    print(hashlib.sha256(boot).hexdigest(), out / "boot.img")
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=REPO / "build/sel4/moto-aarch64")
    parser.add_argument("--stock", type=Path, default=os.environ.get("VIOLET_STOCK_BOOT",
                        REPO / "build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock/boot.img"))
    parser.add_argument("--probe-stage", type=int, choices=(1, 2, 3, 4))
    args = parser.parse_args()
    if not args.stock.is_file():
        sys.exit("Set VIOLET_STOCK_BOOT to the XT2163DL factory boot.img (see README).")
    build(args.out, args.stock, args.probe_stage)


if __name__ == "__main__":
    main()
