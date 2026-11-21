"""Observe actual QEMU run state after guest Sleep, wake input and Power Off."""

from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

states = []


class Monitor(run.Monitor):
    def command(self, name):
        result = super().command(name)
        if name in ("stop", "cont"):
            states.append(super().command("query-status")["status"])
        if name == "quit":
            states.append("off")
        return result


run.Monitor = Monitor
with TemporaryDirectory() as tmp:
    run.IMAGE = Path(tmp)
    viewer = run.IMAGE / "input.py"
    viewer.write_text("""#!/usr/bin/env python3
import os,time
def tap(y,x=360):
    os.write(1,bytes([9,x>>4,y>>4,((x&15)<<4)|(y&15),1,1]))
    time.sleep(.18);os.write(1,bytes(6));time.sleep(.18)
time.sleep(1)
tap(1420)
time.sleep(.4)
tap(920)
tap(260,560)
tap(1460,540);tap(1460,540)
time.sleep(4)
""")
    viewer.chmod(0o755)
    assert run.main(str(viewer)) == 0
    assert states == ["paused", "running", "off"], states
    print("Guest power controls verified:", states)
