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
parser.add_argument("--diagnostics-trial", choices=("fault", "timeout", "confirm"),
                    help="start at the post-activation boundary with a test Diagnostics candidate")
parser.add_argument("--arch", choices=("aarch32", "aarch64"), default="aarch32",
                    help="aarch64: the AArch64 handset build (make moto64) on QEMU's AArch64 kernel")
args = parser.parse_args()


def run_aarch64():
    """The AArch64 handset build (targets/moto-g-pure/build64.py, make moto64) on QEMU.

    The loader (loader64.S, loader.c) is relinked around the AArch64 QEMU kernel
    (make qemu64-image QEMU64_STACK=1: QEMU's interrupt controller and timer,
    the Wi-Fi extension's failing stubs); the supervisor, catalog and the open
    apps' image are the handset build's. QEMU starts the CPU at EL2 with the
    image away from its link address, as the handset's LK may: the loader moves
    itself, drops to EL1 and enters the kernel. Checks the level it was entered
    at, the four supervisor stages, Home presented and Radio's memory IPC
    through the kernel extension. Peripheral responses need the phone."""
    import importlib.util
    moto64 = repo / "build/sel4/moto-aarch64"
    qemu64 = repo / "build/sel4/qemu-aarch64"
    for need, how in ((moto64 / "loader.json", "make moto64"), (qemu64 / "kernel/kernel.elf", "make qemu64-image QEMU64_STACK=1")):
        if not need.is_file():
            raise SystemExit(f"{need} is missing: {how}")
    assert layout(moto64) == layout(qemu64), "kernel memory reservations differ"
    spec = importlib.util.spec_from_file_location("violet_build64", repo / "targets/moto-g-pure/build64.py")
    build64 = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(build64)
    with TemporaryDirectory() as tmp:
        out = Path(tmp)
        build64.link_loader(moto64, out, qemu64 / "kernel/kernel.elf")
        cc = Path(json.loads((moto64 / "loader.json").read_text())["cc"])
        # The handset's firmware routes the virtual timer's PPI to nonsecure
        # Group 1; QEMU's does not. Then x0 is the device tree, as LK passes it.
        (out / "entry.S").write_text(".text\n.global _start\n_start:\n  ldr x9, =0x080b0080\n  mov w10, #0xffffffff\n"
                                     "  str w10, [x9]\n  dsb sy\n  ldr x0, =0x47880000\n  ldr x9, =0x45000000\n  br x9\n.ltorg\n")
        subprocess.run([cc, "--target=aarch64-none-elf", "-c", out / "entry.S", "-o", out / "entry.o"], check=True)
        subprocess.run([cc.with_name("llvm-objcopy"), "-O", "binary", "-j", ".text", out / "entry.o", out / "entry.bin"], check=True)
        dts = (repo / "platform/sel4/board/qemu.dts").read_text()
        start = "linux,initrd-start = <0x51b00000>;"
        end = 0x51b00000 + (moto64 / "initfs.cpio").stat().st_size
        (out / "handoff.dts").write_text(dts.replace(start, start + f" linux,initrd-end = <{end:#x}>;"))
        subprocess.run(["dtc", "-q", "-I", "dts", "-O", "dtb", "-o", out / "handoff.dtb", out / "handoff.dts"], check=True)
        symbols = subprocess.check_output([cc.with_name("llvm-nm"), moto64 / "root.elf"], text=True)
        def pa(name):
            return build64.ROOT_PA + int(next(l.split()[0] for l in symbols.splitlines() if l.endswith(" " + name)), 16) - 0x100000
        cmd = ["qemu-system-aarch64", "-machine", "virt,gic-version=3,virtualization=on", "-cpu", "cortex-a53", "-m", "3G",
               "-accel", "tcg", "-display", "none", "-serial", "none", "-monitor", "none", "-no-reboot",
               "-device", f"loader,file={out}/Image,addr=0x45000000,force-raw=on",
               "-device", f"loader,file={out}/handoff.dtb,addr=0x47880000,force-raw=on",
               "-device", f"loader,file={moto64}/initfs.cpio,addr=0x51b00000,force-raw=on",
               "-device", f"loader,file={out}/entry.bin,addr=0x44000000,force-raw=on,cpu-num=0", "-qmp", "stdio"]
        guest = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            def receive():
                with selectors.DefaultSelector() as sel:
                    sel.register(guest.stdout, selectors.EVENT_READ)
                    if not sel.select(5):
                        raise TimeoutError("QEMU monitor timed out")
                return json.loads(guest.stdout.readline())

            def command(op, arguments):
                guest.stdin.write(json.dumps({"execute": op, "arguments": arguments}) + "\n")
                guest.stdin.flush()
                while True:
                    msg = receive()
                    if "return" in msg:
                        return msg["return"]
                    if "error" in msg:
                        raise RuntimeError(msg)

            def word(address):
                value = command("human-monitor-command", {"command-line": f"xp /1wx {address:#x}"})
                return int(value.strip().rsplit(None, 1)[1], 16)

            def wait(what, test, seconds=20):
                deadline = time.monotonic() + seconds
                while not test():
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"AArch64 Moto boot: {what}")
                    time.sleep(.05)

            assert "QMP" in receive()
            command("qmp_capabilities", {})
            wait("the supervisor's four stages", lambda: word(pa("boot_stages")) == 15)
            el = word(0x43ffffd0)
            assert el == 0x454c0002, f"loader entry level word {el:#x}, not EL2"
            for x, y, color in [(0, 0, 0xff0c0a10), (160 * 3, 55 * 4, 0xff2b2a27)]:
                wait(f"Home at ({x}, {y})", lambda: word(0x79450000 + 4 * (720 * y + x)) == color)
            wait("Radio's Wi-Fi memory IPC", lambda: word(pa("wifi_memory_checks")) > 0)
            assert word(pa("check_failed")) == 0, f"a supervisor check failed at line {word(pa('check_failed'))}"
            print("AArch64 Moto loader entered at EL2, dropped to EL1 and started seL4; the supervisor wired every "
                  "process and the Open Runtime presented Home; Radio's memory IPC reached the kernel extension.")
        finally:
            finish(guest)

moto = repo / "build/sel4/moto"
qemu = repo / "build/sel4/qemu"
layout_path = "kernel/gen_headers/plat/machine/devices_gen.h"
def layout(board):
    return (board / layout_path).read_text().split("avail_p_regs[] = {", 1)[1].split("};", 1)[0]

if args.arch == "aarch64":
    run_aarch64()
    sys.exit(0)
# The 32-bit handset build (make moto32) on the 32-bit QEMU kernel (make qemu32-image).
os.environ["VIOLET_ARCH"] = "aarch32"
assert layout(moto) == layout(qemu), "kernel memory reservations differ"


def image_end(blob):
    # A VPK2 record is header[24] | image[size] | signature trailer[80].
    return 24 + struct.unpack_from("<I", blob, 16)[0]


def signed(blob):
    """An edited package made valid again: its checksum, and its signature by this
    build's developer key (the supervisor runs nothing a pinned key did not sign)."""
    sys.path.insert(0, str(repo / "image/packages"))
    import sign
    signer = sign.config(repo, "moto")
    target, pinned = sign.load_keys(moto / "package-keys.json")
    assert target == "moto" and pinned.get(signer.key_id) == signer.keys[signer.key_id], \
        "build/sel4/moto was not built with this developer key: run make moto again"
    return bytearray(sign.resign(bytes(blob), signer))


with TemporaryDirectory() as tmp:
    out = Path(tmp)
    # Relink the Moto loader with a kernel built for the virtual GIC.
    commands = subprocess.check_output(["ninja", "-C", qemu / "kernel", "-t", "commands", "sel4"], text=True)
    cc = Path(shlex.split(next(line for line in commands.splitlines() if " -c " in line))[0])
    ld, objcopy = cc.with_name("ld.lld"), cc.with_name("llvm-objcopy")
    def run(*args):
        subprocess.run(args, cwd=out, check=True)
    (out / "kernel.bin").symlink_to(qemu / "kernel.bin")
    symbols = subprocess.check_output([cc.with_name("llvm-nm"), moto / "root.elf"], text=True)
    def root_offset(name):
        return int(next(line.split()[0] for line in symbols.splitlines()
                        if line.endswith(" " + name)), 16) - 0x100000

    extra_devices = []
    if args.diagnostics_trial:
        # Seed exactly the volatile state the supervisor sets after signature verification.
        # Edit a disposable copy of the linked first task, never the handset image.
        trial = bytearray((moto / "packages/usb-diagnostics.vpk").read_bytes())
        root = bytearray((moto / "root.bin").read_bytes())
        if args.diagnostics_trial == "confirm":
            pkg = moto / "packages"
            run(cc, "--target=arm-none-eabi", "-mcpu=cortex-a53", "-marm", "-Oz", "-ffreestanding",
                "-c", repo / "services/usb-diagnostics/tests/confirmation.c", "-o", "confirm.o")
            run(ld, "--gc-sections", "-T", pkg / "component.ld", pkg / "transport.o",
                "confirm.o", moto / "strings.c.o", "-o", "confirm.elf")
            run(objcopy, "-O", "binary", "-j", ".text", "confirm.elf", "confirm.bin")
            code = (out / "confirm.bin").read_bytes()
            assert len(code) <= struct.unpack_from("<I", trial, 12)[0]
            struct.pack_into("<I", trial, 12, len(code))
            trial[24:image_end(trial)] = code.ljust(image_end(trial) - 24, b"\0")
            sys.path.insert(0, str(repo / "image/packages"))
            from package_format import HANDSET_LIMIT, pack, unpack
            catalog = (moto / "catalog.bin").read_bytes()
            packages = unpack(catalog, HANDSET_LIMIT)
            storage = bytearray(packages[6])
            assert len(code) <= struct.unpack_from("<I", storage, 12)[0]
            struct.pack_into("<I", storage, 12, len(code))
            storage[24:image_end(storage)] = code.ljust(image_end(storage) - 24, b"\0")
            packages[6] = bytes(signed(storage))
            assert root.count(catalog) == 1
            root = root.replace(catalog, pack(packages.values(), limit=HANDSET_LIMIT))
        else:
            instruction = 0xe7f000f0 if args.diagnostics_trial == "fault" else 0xeafffffe
            struct.pack_into("<I", trial, 24, instruction)
        trial = signed(trial)
        (out / "trial.vpk").write_bytes(trial)
        if args.diagnostics_trial == "timeout":
            offset = root_offset("usb_trial_timeout_ms")
            assert struct.unpack_from("<I", root, offset)[0] == 45000
            struct.pack_into("<I", root, offset, 5)
        struct.pack_into("<I", root, root_offset("usb_trial_bytes"), len(trial))
        (out / "root.bin").write_bytes(root)
        extra_devices = ["-device", f"loader,file={out}/trial.vpk,addr=0x43d40000,force-raw=on"]
    elif args.stall_services or args.radio_package:
        sys.path.insert(0, str(repo / "image/packages"))
        from package_format import HANDSET_LIMIT, pack, unpack
        catalog = (moto / "catalog.bin").read_bytes()
        packages = unpack(catalog, HANDSET_LIMIT)
        if args.radio_package:
            packages[7] = args.radio_package.read_bytes()
        for identity in ((4, 6) if args.stall_services else ()):
            blob = bytearray(packages[identity])
            struct.pack_into("<I", blob, 24, 0xeafffffe)  # ARM branch to self
            packages[identity] = bytes(signed(blob))
        root = (moto / "root.bin").read_bytes()
        assert root.count(catalog) == 1
        candidate = pack(packages.values(), limit=HANDSET_LIMIT)
        if len(candidate) != len(catalog):
            raise ValueError("Fixture substitution requires an equal-sized catalog")
        (out / "root.bin").write_bytes(root.replace(catalog, candidate))
    else:
        (out / "root.bin").symlink_to(moto / "root.bin")
    asm = [cc, "--target=arm-none-eabi", "-mcpu=cortex-a53", "-marm", "-c"]
    (out / "wifi-firmware.bin").symlink_to(moto / "wifi-firmware.bin")
    (out / "initfs.cpio").symlink_to(moto / "initfs.cpio")
    run(*asm, moto / "payload.S", "-o", "payload.o")
    objects = ["loader.S.o", "loader.c.o", "strings.c.o", "fdt.c.o", "fdt_ro.c.o",
               "fdt_check.c.o", "fdt_addresses.c.o"]
    run(ld, "--gc-sections", "-T", repo / "boot/loader/loader.ld",
        *(moto / obj for obj in objects), "payload.o", moto / "loader-history.a", "-o", "loader.elf")
    run(objcopy, "-O", "binary", "loader.elf", "zImage")
    size = (out / "zImage").stat().st_size
    run(*asm, f"-DIMAGE_BYTES={size}", repo / "platform/sel4/loader/qemu-entry.S", "-o", "entry.o")
    run(objcopy, "-O", "binary", "-j", ".text", "entry.o", "entry.bin")
    # The handset's bootloader hands the loader the boot image's ramdisk (the
    # open apps' file system image) with its start and end; without the end
    # the loader stages nothing and the Open Runtime, and so Home, cannot start.
    dts = (repo / "platform/sel4/board/qemu.dts").read_text()
    start = "linux,initrd-start = <0x51b00000>;"
    assert dts.count(start) == 1
    end = 0x51b00000 + (moto / "initfs.cpio").stat().st_size
    (out / "handoff.dts").write_text(dts.replace(start, start + f" linux,initrd-end = <{end:#x}>;"))
    run("dtc", "-q", "-I", "dts", "-O", "dtb", "-o", "handoff.dtb", "handoff.dts")
    extra_devices += ["-device", f"loader,file={moto}/initfs.cpio,addr=0x51b00000,force-raw=on"]
    stage_va = int(next(line.split()[0] for line in symbols.splitlines() if line.endswith(" boot_stages")), 16)
    stage_pa = 0x40200000 + stage_va - 0x100000
    # The handset has 3 GiB; open apps use memory at 0xc0000000.
    cmd = machine(image=out)
    cmd[cmd.index("-m") + 1] = "3G"
    guest = subprocess.Popen(cmd + extra_devices + ["-serial", "none", "-qmp", "stdio"],
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
        if args.diagnostics_trial != "confirm":
            # (Home is an open app: the runtime's trusted band over its top
            # rows, runtime/open/trusted_ui.rs, and Home's paper mid-screen.)
            for x, y, color in [(0, 0, 0xff0c0a10), (160 * 3, 55 * 4, 0xff2b2a27)]:
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
        if args.diagnostics_trial:
            def read_root(name):
                address = 0x40200000 + root_offset(name)
                response = command("human-monitor-command", {"command-line": f"xp /1wx {address:#x}"})
                return int(response.strip().rsplit(None, 1)[1], 16)
            deadline = time.monotonic() + 60
            while read_root("generation") < 2 or read_root("boot_stages") != 15:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"USB trial did not finish: generation={read_root('generation')}, trial={read_root('usb_trial_bytes')}, bank={read_root('volume_bank')}, boot={read_root('boot_stages')}")
                time.sleep(.1)
            assert read_root("usb_trial_generation") == 1, "candidate was never selected"
            assert read_root("usb_trial_bytes") == 0
            if args.diagnostics_trial == "confirm":
                assert read_root("volume_bank") == 1 and read_root("source") == 3
                saved = out / "active-catalog.bin"
                command("human-monitor-command", {"command-line": f'pmemsave 0x43c00000 {len(catalog)} "{saved}"'})
                assert unpack(saved.read_bytes(), HANDSET_LIMIT)[21] == trial
                print("Supervisor confirmed Diagnostics and loaded the replacement from the committed catalog.")
            else:
                reason = read_root("usb_last_failure") & 0xff
                assert reason == {"fault": 1, "timeout": 2}[args.diagnostics_trial], reason
                assert read_root("volume_bank") == 0
                print(f"Supervisor abandoned the {args.diagnostics_trial} candidate and restarted from the factory catalog.")
        if any(line.endswith(" open_setup") for line in symbols.splitlines()):
            base = 0x40200000 + root_offset("open_setup")
            words = [int(command("human-monitor-command", {"command-line": f"xp /1wx {base + 4 * i:#x}"}).strip().rsplit(None, 1)[1], 16) for i in range(4)]
            print(f"Open Runtime setup stage {words[0]}, root CNode slots {words[1]:#x} of {words[3]:#x}")
        condition = "touch and storage deliberately stalled" if args.stall_services else "handset peripherals absent"
        if args.diagnostics_trial != "confirm":
            print(f"Moto launcher presented Home on the 720-pixel scanout with {condition}.")
    finally:
        finish(guest)
