"""Drive the setup app and reboot with its saved QEMU network profile."""

from pathlib import Path
import subprocess
import sys
import tempfile

guest = Path(__file__).resolve().parents[2] / "platform/sel4/stopwatch/test.py"
with tempfile.TemporaryDirectory() as temp:
    disk = Path(temp) / "wifi.disk"
    disk.write_bytes(bytes(1024**2))
    for scenario in ("exercise", "async", "save", "restore"):
        if scenario != "restore":
            disk.write_bytes(bytes(1024**2))
        subprocess.run(
            [sys.executable, guest, "--wifi", scenario, "--disk", disk],
            check=True,
            timeout=30,
        )
print("Wi-Fi Setup restored the selected network after a fresh boot.")
