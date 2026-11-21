"""Run the Moto supervisor through launcher start using seL4 on QEMU's GIC.

QEMU supplies its GIC, timer and synthetic handoff. The Moto loader, reserved
RAM layout, supervisor, catalog and device mappings come from the handset build.
This exercises boot through app launch. Peripheral responses require a phone.
"""
from pathlib import Path
from tempfile import TemporaryDirectory
import argparse
import binascii
import json
import struct
import shlex
import selectors
import subprocess
import sys
import time

repo = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(repo / "tools"))
from run import finish, machine

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--stall-services", action="store_true")
parser.add_argument("--radio-package", type=Path, help="exercise an independently built Radio VPK against the same kernel")
args = parser.parse_args()
moto = repo / "build/sel4/moto"
qemu = repo / "build/sel4/qemu"
layout_path = "kernel/gen_headers/plat/machine/devices_gen.h"
def layout(board):
    return (board / layout_path).read_text().split("avail_p_regs[] = {", 1)[1].split("};", 1)[0]

assert layout(moto) == layout(qemu), "kernel memory reservations differ"
with TemporaryDirectory() as tmp:
    out = Path(tmp)
    # Relink the Moto loader with a kernel built for the virtual GIC.
    commands = subprocess.check_output(["ninja", "-C", qemu / "kernel", "-t", "commands", "sel4"], text=True)
    cc = Path(shlex.split(next(line for line in commands.splitlines() if " -c " in line))[0])
    ld, objcopy = cc.with_name("ld.lld"), cc.with_name("llvm-objcopy")
    def run(*args):
        subprocess.run(args, cwd=out, check=True)
    (out / "kernel.bin").symlink_to(qemu / "kernel.bin")
    if args.stall_services or args.radio_package:
        sys.path.insert(0, str(repo / "image/packages"))
        from package_format import pack, unpack
        catalog = (moto / "catalog.bin").read_bytes()
        packages = unpack(catalog)
        if args.radio_package:
            packages[7] = args.radio_package.read_bytes()
        for identity in ((4, 6) if args.stall_services else ()):
            blob = bytearray(packages[identity])
            struct.pack_into("<I", blob, 24, 0xeafffffe)  # ARM branch to self
            struct.pack_into("<I", blob, 20, binascii.crc32(blob[24:]))
            packages[identity] = bytes(blob)
        root = (moto / "root.bin").read_bytes()
        assert root.count(catalog) == 1
        candidate = pack(packages.values())
        if len(candidate) != len(catalog):
            raise ValueError("Fixture substitution requires an equal-sized catalog")
        (out / "root.bin").write_bytes(root.replace(catalog, candidate))
    else:
        (out / "root.bin").symlink_to(moto / "root.bin")
    asm = [cc, "--target=arm-none-eabi", "-mcpu=cortex-a53", "-marm", "-c"]
    (out / "wifi-firmware.bin").symlink_to(moto / "wifi-firmware.bin")
    run(*asm, moto / "payload.S", "-o", "payload.o")
    objects = ["loader.S.o", "loader.c.o", "strings.c.o", "fdt.c.o", "fdt_ro.c.o",
               "fdt_check.c.o", "fdt_addresses.c.o"]
    run(ld, "--gc-sections", "-T", repo / "boot/loader/loader.ld",
        *(moto / obj for obj in objects), "payload.o", moto / "loader-history.a", "-o", "loader.elf")
    run(objcopy, "-O", "binary", "loader.elf", "zImage")
    size = (out / "zImage").stat().st_size
    run(*asm, f"-DIMAGE_BYTES={size}", repo / "platform/sel4/loader/qemu-entry.S", "-o", "entry.o")
    run(objcopy, "-O", "binary", "-j", ".text", "entry.o", "entry.bin")
    (out / "handoff.dtb").symlink_to(qemu / "handoff.dtb")
    symbols = subprocess.check_output([cc.with_name("llvm-nm"), moto / "root.elf"], text=True)
    stage_va = int(next(line.split()[0] for line in symbols.splitlines() if line.endswith(" boot_stages")), 16)
    stage_pa = 0x40200000 + stage_va - 0x100000
    guest = subprocess.Popen(machine(image=out) + ["-serial", "none", "-qmp", "stdio"],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        def receive():
            with selectors.DefaultSelector() as sel:
                sel.register(guest.stdout, selectors.EVENT_READ)
                if not sel.select(5):
                    raise TimeoutError("QEMU monitor timed out")
            return json.loads(guest.stdout.readline())

        def command(op, args):
            guest.stdin.write(json.dumps({"execute": op, "arguments": args}) + "\n")
            guest.stdin.flush()
            while True:
                msg = receive()
                if "return" in msg:
                    return msg["return"]
                if "error" in msg:
                    raise RuntimeError(msg)

        assert "QMP" in receive()
        command("qmp_capabilities", {})
        deadline = time.monotonic() + 8
        while True:
            value = command("human-monitor-command", {"command-line": f"xp /1wx {stage_pa:#x}"})
            bits = int(value.strip().rsplit(None, 1)[1], 16)
            if bits == 15:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Moto supervisor boot stage mask: {bits:#x}")
            time.sleep(.03)
        # This guest has no Moto eMMC or touch controller. Check the first
        # displayed Home frame itself, past the supervisor's start marker.
        for x, y, color in [(0, 0, 0xff191620), (160 * 3, 55 * 4, 0xff302a39)]:
            address = 0x79450000 + 4 * (720 * y + x)
            while True:
                value = command("human-monitor-command", {"command-line": f"xp /1wx {address:#x}"})
                if int(value.strip().rsplit(None, 1)[1], 16) == color:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"Moto launcher did not present Home at ({x}, {y}): {value}")
                time.sleep(.03)
        # The Moto Radio performs a read-only IPC at startup. Require a
        # completed kernel capability call, even when firmware reads fail.
        symbol = next(line for line in symbols.splitlines() if line.endswith(" wifi_memory_checks"))
        counter_pa = 0x40200000 + int(symbol.split()[0],16) - 0x100000
        while True:
            value = command("human-monitor-command", {"command-line": f"xp /1wx {counter_pa:#x}"})
            if int(value.strip().rsplit(None, 1)[1], 16) > 0:
                break
            if time.monotonic() >= deadline:
                policy_symbol = next(line for line in symbols.splitlines() if line.endswith(" wifi_memory_policy"))
                pa = 0x40200000 + int(policy_symbol.split()[0],16) - 0x100000
                observed = command("human-monitor-command", {"command-line": f"xp /1wx {pa:#x}"})
                raise RuntimeError(f"Radio did not complete the runtime Wi-Fi memory IPC: {observed}")
            time.sleep(.03)
        condition = "touch and storage deliberately stalled" if args.stall_services else "handset peripherals absent"
        print(f"Moto launcher presented Home on the 720-pixel scanout with {condition}.")
    finally:
        finish(guest)
