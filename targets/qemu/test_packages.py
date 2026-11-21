"""Exercise package transactions, replacement faults and recovery inside real QEMU guests."""

import binascii
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "build/sel4/qemu"
MANAGER = ROOT / "system/package-manager/manage.py"
GUEST = ROOT / "platform/sel4/stopwatch/test.py"

with tempfile.TemporaryDirectory() as tmp:
    state = Path(tmp)

    def manage(*args, check=True):
        return subprocess.run(
            [sys.executable, MANAGER, "--state-dir", state, *args], check=check
        )

    def guest(scenario, *extra):
        subprocess.run(
            [sys.executable, GUEST, "--stack", scenario, "--state-dir", state, *extra],
            check=True,
            timeout=40,
        )

    guest("lifecycle")
    manage("uninstall", "stopwatch")
    guest("removed")
    manage("install", OUT / "packages/stopwatch.vpk")
    guest("lifecycle")
    package = state / "test-radio.vpk"
    radio = bytearray((OUT / "packages/radio.vpk").read_bytes())
    package.write_bytes(radio[:-1])
    active = (state / "active.json").read_bytes()
    assert manage("install", package, check=False).returncode != 0
    assert (state / "active.json").read_bytes() == active
    # A fault in any replaceable component must leave a usable launcher and
    # working apps. The stopwatch replacement faults on launch, the rest at boot.
    for component in json.loads((ROOT / "image/catalog/components.json").read_text()):
        blob = bytearray((OUT / "packages" / (component["name"] + ".vpk")).read_bytes())
        struct.pack_into("<I", blob, 24, 0xE7F000F0)  # intentional ARM UDF
        struct.pack_into("<I", blob, 20, binascii.crc32(blob[24:]))
        package.write_bytes(blob)
        manage("install", package)
        guest("recovered", "--fault-id", str(component["id"]))
    # A runnable but nonresponsive process must also trigger supervised recovery.
    image = bytearray((OUT / "packages/radio.vpk").read_bytes())
    struct.pack_into("<I", image, 24, 0xEAFFFFFE)  # deliberate infinite loop at entry
    struct.pack_into("<I", image, 20, binascii.crc32(image[24:]))
    package.write_bytes(image)
    manage("install", package)
    guest("recovered", "--fault-id", "7")
    guest("lifecycle")
    manage("uninstall", "power")
    manage("rollback")
    guest("lifecycle")
