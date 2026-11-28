#!/usr/bin/env python3
"""Build the XT2163DL AArch64 entry probe as a boot_b image (see ../README.md, "AArch64 boot path").

The kernel is a polyglot: word 0 is 0xEA00001F, which AArch32 runs as
`b 0x84` and AArch64 as `tst x0, x0`; word 1 is the AArch64 `b 0x1000`. So the
same bytes reach a32.S at 0x84 when the bootloader enters in AArch32, and
a64.S at 0x1000 when it enters in AArch64. The arm64 Image header (magic
"ARM\\x64" at 0x38, flags LE/4K/anywhere) is in place either way. `--a32 spin`
or `--a64 spin` replaces that half with a loop, so only the other half can
set the RTC fast-boot flag: fastboot with reason "Reboot mode set to fastboot"
then names the state the bootloader used. `--a64-only` builds the A64-only
probe instead: word 0 is the AArch64 `b 0x1000`, word 1 a `nop`, and no
AArch32 half (AArch32 entry runs undefined bytes).

Output: probe-boot.img, probe-boot.simg and probe.json (the options, the
header's kernel_addr and both sha256s), so each probe run can be recorded
against the exact image it flashed (README, "AArch64 boot path").

probe-boot.simg is a sparse image that writes only the probe's blocks and leaves the rest of boot_b (the storage window and the
installed apps) as it is. Flash it with `fastboot flash boot_b probe-boot.simg`.
"""
import argparse
import gzip
import hashlib
import json
import struct
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import phone_image  # noqa: E402

STOCK = Path("/Users/rose/VioletOS/build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock/boot.img")
SPIN32 = "        .syntax unified\n        .arm\n        .text\n        cpsid aif\n1:      b 1b\n"
SPIN64 = "        .text\n        msr daifset, #0xf\n1:      b 1b\n        .balign 4096\n"
R_ARM_CALL = 28


def text(obj):
    """The .text of a one-section object, with A32 R_ARM_CALL resolved in place."""
    d = obj.read_bytes()
    wide = d[4] == 2
    if wide:
        shoff, = struct.unpack_from("<Q", d, 0x28)
        size, count, names = struct.unpack_from("<HHH", d, 0x3a)
        form = "<IIQQQQIIQQ"
    else:
        shoff, = struct.unpack_from("<I", d, 0x20)
        size, count, names = struct.unpack_from("<HHH", d, 0x2e)
        form = "<IIIIIIIIII"
    secs = [struct.unpack_from(form, d, shoff + i * size) for i in range(count)]
    table = secs[names]
    name = lambda s: d[table[4] + s[0]:d.index(b"\0", table[4] + s[0])].decode()
    index = next(i for i, s in enumerate(secs) if name(s) == ".text")
    body = bytearray(d[secs[index][4]:secs[index][4] + secs[index][5]])
    for s in secs:
        if name(s).startswith(".rela") or (name(s).startswith(".rel") and name(s) != ".rel.text"):
            raise SystemExit(f"{obj}: unexpected {name(s)}")
        if name(s) != ".rel.text":
            continue
        symbols = secs[s[6]]
        for off in range(s[4], s[4] + s[5], 8):
            where, info = struct.unpack_from("<II", d, off)
            if info & 0xff != R_ARM_CALL:
                raise SystemExit(f"{obj}: relocation type {info & 0xff}")
            _, value, _, _, _, shndx = struct.unpack_from("<IIIBBH", d, symbols[4] + (info >> 8) * 16)
            if shndx != index:
                raise SystemExit(f"{obj}: call outside .text")
            word, = struct.unpack_from("<I", body, where)
            struct.pack_into("<I", body, where, (word & 0xff000000) | (((value - where - 8) >> 2) & 0xffffff))
    return bytes(body)


def assemble(source, target, obj):
    subprocess.run(["clang", f"--target={target}", "-c", str(source), "-o", str(obj)], check=True)
    return text(obj)


A64_B_0x1000, A64_NOP = 0x14000000 | (0x1000 // 4), 0xD503201F


def layout(half32, half64, a64_only=False):
    """The probe kernel from its halves' code: the arm64 Image header with
    the polyglot words 0 and 1 (or, `a64_only`, an A64 branch to 0x1000 and
    no AArch32 half), the AArch32 half at 0x84, the AArch64 one at 0x1000."""
    if len(half32) > 0x1000 - 0x84:
        raise SystemExit("AArch32 half does not fit below 0x1000")
    image = bytearray(0x1000)
    if a64_only:
        struct.pack_into("<II", image, 0, A64_B_0x1000, A64_NOP)
    else:
        struct.pack_into("<II", image, 0, 0xEA00001F, 0x14000000 | ((0x1000 - 4) // 4))
        image[0x84:0x84 + len(half32)] = half32
    image += half64
    image += bytes(-len(image) % 4096)
    struct.pack_into("<QQQ", image, 8, 0, len(image), 0xa)  # text_offset, image_size, flags
    image[0x38:0x3c] = b"ARM\x64"
    return bytes(image)


def kernel(work, a32, a64, a64_only=False):
    (work / "a32-spin.S").write_text(SPIN32)
    (work / "a64-spin.S").write_text(SPIN64)
    half32 = b"" if a64_only else assemble(HERE / "a32.S" if a32 == "flag" else work / "a32-spin.S", "armv7a-none-eabi", work / "a32.o")
    half64 = assemble(HERE / "a64.S" if a64 == "flag" else work / "a64-spin.S", "aarch64-none-elf", work / "a64.o")
    return layout(half32, half64, a64_only)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--a32", choices=("flag", "spin"), default="flag")
    parser.add_argument("--a64", choices=("flag", "spin"), default="flag")
    parser.add_argument("--a64-only", action="store_true", help="no polyglot: an A64 branch to the AArch64 half")
    parser.add_argument("--gzip", action="store_true", help="ship the kernel as Image.gz")
    parser.add_argument("--kernel-addr", type=lambda v: int(v, 0), help="override the header's kernel_addr")
    parser.add_argument("--bootopt", help="replace the header's bootopt= (MediaTek LK reads its last field as the kernel's state, e.g. 64S3,32N2,64N2)")
    parser.add_argument("--stock", type=Path, default=STOCK)
    parser.add_argument("--out", type=Path, default=ROOT / "build/probe64")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.a64_only and args.a32 != "flag":
        raise SystemExit("--a64-only has no AArch32 half to spin")
    payload = kernel(args.out, args.a32, args.a64, args.a64_only)
    if args.gzip:
        payload = gzip.compress(payload, 9, mtime=0)
    name = b"TRAILER!!!\0"  # an empty newc ramdisk: v2 requires one
    cpio = b"070701" + b"".join(b"%08X" % v for v in (0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, len(name), 0)) + name
    cpio += bytes(-len(cpio) % 4)
    cpio += bytes(-len(cpio) % 512)
    boot = bytearray(phone_image.rebuild_v2(args.stock.read_bytes(), cpio, kernel=payload))
    boot += bytes(-len(boot) % 512)
    if args.kernel_addr is not None:
        struct.pack_into("<I", boot, 12, args.kernel_addr)
    if args.bootopt:
        line = boot[64:64 + 512].split(b"\0")[0]
        words = [w for w in line.split(b" ") if not w.startswith(b"bootopt=")]
        line = b" ".join([b"bootopt=" + args.bootopt.encode()] + words)
        if len(line) >= 512:
            raise SystemExit("command line too long")
        boot[64:64 + 512] = line + bytes(512 - len(line))
        # The v2 header's id (SHA-1 over the parts) does not cover the command line; LK does not check it.
    if len(boot) >= 16 << 20:
        raise SystemExit("probe image would reach the storage marker at 16 MiB")
    (args.out / "probe-boot.img").write_bytes(boot)
    block, total = 512, (32 << 20) // 512
    used = len(boot) // block
    sparse = (struct.pack("<IHHHHIIII", 0xED26FF3A, 1, 0, 28, 12, block, total, 2, 0)
              + struct.pack("<HHII", 0xCAC1, 0, used, 12 + len(boot)) + boot
              + struct.pack("<HHII", 0xCAC3, 0, total - used, 12))
    (args.out / "probe-boot.simg").write_bytes(sparse)
    kernel_addr, = struct.unpack_from("<I", boot, 12)
    record = {"a32": None if args.a64_only else args.a32, "a64": args.a64, "a64_only": args.a64_only, "gzip": args.gzip,
              "kernel_addr": f"{kernel_addr:#x}", "bootopt": args.bootopt, "stock": str(args.stock),
              "probe-boot.img": hashlib.sha256(boot).hexdigest(), "probe-boot.simg": hashlib.sha256(sparse).hexdigest()}
    (args.out / "probe.json").write_text(json.dumps(record, indent=2) + "\n")
    print(f"{args.out / 'probe-boot.simg'}: {len(boot)} bytes, sha256 {record['probe-boot.img']}; recorded in probe.json")


if __name__ == "__main__":
    main()
