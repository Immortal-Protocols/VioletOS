"""Boot independent QEMU processes against one disposable disk and check guest pixels."""
import re
import subprocess
import sys
import tempfile
from pathlib import Path

with tempfile.TemporaryDirectory(prefix="violet-storage-test-") as tmp:
    disk=Path(tmp)/"volume.raw"
    disk.write_bytes(bytes(1024*1024))
    def boot(action,expected=0):
        return subprocess.check_output([sys.executable,Path(__file__).with_name("test.py"),
            "--disk",disk,"--checkpoint",action,"--expect",str(expected)],text=True,timeout=20)
    saved=int(re.search(r"Checkpoint (\d+)",boot("save"))[1])
    boot("restore",saved)
    boot("reset",saved)
    boot("restore",0)
    # The reset replaced sector one. Tear its checksum and recover sector zero.
    with disk.open("r+b") as stream:
        stream.seek(1023)
        last=stream.read(1)[0]
        stream.seek(1023)
        stream.write(bytes([last^1]))
    boot("restore",saved)
    print(f"Five QEMU boots verified pause at {saved/10:.1f}s, durable reset and torn-sector recovery.")
