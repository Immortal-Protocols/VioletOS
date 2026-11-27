"""Check that the emulated phone sets its clock from the network.

Local only (make test-time; not part of CI): it needs the Mac to reach public
NTP servers over UDP, because the emulator's sockets are the Mac's
(tools/net_relay.py). The Time Test program
(targets/qemu/power-test-apps/timetest) prints its wall clock and the
runtime's "time" answer once a second; the clock starts at the image's build
time and must arrive within a few seconds of the Mac's.
"""

from functools import partial
from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import os, re, struct, sys, time

ram = open(sys.argv[1], "rb")

def everything():
    ram.seek(0x09220000)
    log = ram.read(4096)
    count = struct.unpack_from("<I", log, 4)[0]
    data = log[8:]
    if count <= len(data):
        return data[:count].decode(errors="replace")
    start = count % len(data)
    return (data[start:] + data[:start]).decode(errors="replace")
def fail(why):
    sys.exit(f"time test failed: {why}\n{everything()[-1500:]}")
def until(what, check, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15)
    os.write(1, bytes(6))
def reports():
    """(the app's clock, the runtime's, synced) for each complete line in the log."""
    found = re.findall(r"timetest: now=(\d+) unix=(\d+) synced=(\d) age=(-?\d+) rounds=(\d+) failed=(\d+) refused=(\d+)\n", everything())
    return [(int(a), int(b), c == "1") for a, b, c, *_ in found]

until("Home did not start", lambda: "home: ready" in everything(), 60)
open_icon("timetest")
until("the app did not report", lambda: reports(), 60)
# The first round starts five seconds after the network is up and lasts at most eight.
until("the clock was never set (can this Mac reach NTP servers over UDP?)",
      lambda: any(r[2] for r in reports()), 90)
time.sleep(1.5)
app, runtime, synced = reports()[-1]
host = time.time()
if not synced:
    fail("the clock lost its sync")
if abs(runtime - host) > 5:
    fail(f"the runtime's clock is {runtime - host:+.0f} s from the Mac's")
if abs(app - host) > 5:
    fail(f"the app's clock (CLOCK_REALTIME) is {app - host:+.0f} s from the Mac's")
if "[open] time: synced=1" not in everything():
    fail("the runtime did not log the round")
print(f"clock set from the network: {runtime - host:+.1f} s from the Mac", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-time-") as directory:
    source = run.IMAGE
    image = Path(directory)
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(source / name)
    viewer = image / "viewer"
    viewer.write_text(home_grid.viewer(SCRIPT, (image / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    run.IMAGE = image
    run.machine = partial(run.machine, image=image)
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"time test failed (viewer status {status})")
    print("Network time verified: SNTP round, CLOCK_REALTIME, the time command")
