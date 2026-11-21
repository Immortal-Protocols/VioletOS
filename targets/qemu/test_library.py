"""Install an independently built application, unplug its distribution media and reboot."""

from pathlib import Path
import binascii
import json
import shutil
import struct
import os
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
guest = ROOT / "targets/qemu/test_guest.py"
with tempfile.TemporaryDirectory() as tmp:
    disk = Path(tmp) / "app.disk"
    disk.write_bytes(bytes(1024**2))

    second = Path(tmp) / "second"
    second.mkdir()
    (second / "app.json").write_text(json.dumps({"id": 12, "title": "SECOND"}))
    shutil.copyfile(ROOT / "targets/qemu/test-app/process.rs", second / "process.rs")
    subprocess.run(["make", "app", "APP=" + str(second)], cwd=ROOT, check=True)
    for scenario in ("install", "install-second", "open", "remove", "absent"):
        env = os.environ.copy()
        env.pop("VIOLET_INBOX", None)
        if scenario == "install":
            env["VIOLET_INBOX"] = str(ROOT / "build/sel4/qemu/packages/app-11.vpk")
        if scenario == "install-second":
            env["VIOLET_INBOX"] = str(ROOT / "build/sel4/qemu/packages/app-12.vpk")
        subprocess.run(
            [sys.executable, guest, "--library", scenario, "--disk", disk],
            env=env,
            check=True,
            timeout=30,
        )

    # Install a working service, replace it with one that stops responding,
    # and verify the supervisor's in-guest rollback survives a cold boot.

    radio = ROOT / "build/sel4/qemu/packages/radio.vpk"
    broken = bytearray(radio.read_bytes())
    struct.pack_into("<I", broken, 24, 0xEAFFFFFE)
    struct.pack_into("<I", broken, 20, binascii.crc32(broken[24:]))
    replacement = Path(tmp) / "radio.vpk"
    replacement.write_bytes(broken)
    for scenario, inbox in [
        ("radio-install", radio),
        ("radio-rollback", replacement),
        ("radio-reboot", None),
    ]:
        env = os.environ.copy()
        if inbox:
            env["VIOLET_INBOX"] = str(inbox)
        else:
            env.pop("VIOLET_INBOX", None)
        subprocess.run(
            [sys.executable, guest, "--library", scenario, "--disk", disk],
            env=env,
            check=True,
            timeout=30,
        )
