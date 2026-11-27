"""Drive the Power service's wake locks and governors in QEMU.

Local only (make test-power; not part of CI). A scripted viewer takes the
place of the window. It opens the Power Test program
(targets/qemu/power-test-apps/powertest), which takes a wake lock and then
asks Power for its state four times a second, and meanwhile writes a
temperature and a CPU load into the power fixture page (targets/qemu/power.rs
documents its words) and checks what the guest decides: the operating point
follows the load, heat caps it at once and lets go slowly, a failing sensor
counts as hot, the lock is accounted and ends with the app, and a critical
temperature powers the machine off.

Sleep itself is not driven here: only the boot launcher may ask Runtime for
it, and nothing on screen does today. The postponed suspend, the screen-off
limit on app locks and a power-off that sleep or wake cannot cancel are
covered on the host (services/power/policy.rs,
runtime/open/tests/powerctl_host.rs).
"""

from functools import partial
from pathlib import Path
from tempfile import TemporaryDirectory
import os
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import mmap, os, struct, sys, time

ram = open(sys.argv[1], "r+b")
# The Mac maps files in 16 KiB steps: map from the step below the page.
fixture = mmap.mmap(ram.fileno(), 0x4000, offset=0x49000000 - 0x40000000)
BASE = 0x2000
log = mmap.mmap(ram.fileno(), 4096, offset=0x49220000 - 0x40000000, prot=mmap.PROT_READ)

def everything():
    count = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:])
    if count <= len(data):
        return data[:count].decode(errors="replace")
    start = count % len(data)
    return (data[start:] + data[:start]).decode(errors="replace")
def word(i): return struct.unpack_from("<I", fixture, BASE + 4 * i)[0]
def put(i, v): struct.pack_into("<I", fixture, BASE + 4 * i, v)
def fail(why):
    sys.exit(f"power test failed: {why}\nfixture {[word(i) for i in range(12)]}\n{everything()[-1500:]}")
def until(what, check, timeout=20):
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
TEMP, LOAD, LEVEL, OPP, KHZ, UV, FLAGS, LOCKS, KINDS, RUNS = 2, 3, 4, 5, 6, 7, 8, 9, 10, 11

until("Home did not start", lambda: "home: ready" in everything(), 60)
if word(TEMP) != 0 or word(LOCKS) != 0:
    fail("the fixture must start with no sensor and no locks")
open_icon("powertest")

# Wake locks: taken, renewed without becoming two, and accounted.
until("the app did not take its lock", lambda: "powertest: wakelock ok" in everything(), 60)
until("the renewal was refused", lambda: "powertest: renew ok" in everything())
until("a bad argument was accepted", lambda: "powertest: bad error" in everything())
until("the lock is not in Power's table", lambda: word(LOCKS) == 1 and word(KINDS) == 1)
until("the report does not list the holder",
      lambda: "powertest: locks held pid:" in everything() and "holding=1 taken=1 lapsed=0" in everything())
if "powertest: before none" not in everything():
    fail("locks were held before the app took one")

# No sensor: reported as absent, never as cool; full speed without a load figure.
until("no governor report", lambda: "powertest: power sensor=0" in everything())
until("without a load figure the top operating point", lambda: word(OPP) == 3 and word(KHZ) == 2000000 and word(UV) == 950000)
if word(LEVEL) != 0:
    fail("a level without a sensor")

# The operating point follows the load: down after a hold, up at once.
put(LOAD, 1)      # 0 %
until("idle did not lower the operating point", lambda: word(OPP) == 0 and word(KHZ) == 600000 and word(UV) == 650000, 5)
put(LOAD, 51)     # 50 %
until("half load did not pick the 1.5 GHz point", lambda: word(OPP) == 2 and word(KHZ) == 1500000, 5)
put(LOAD, 101)    # 100 %
until("full load did not raise the operating point", lambda: word(OPP) == 3 and word(UV) == 950000, 5)

# Heat caps it at once, whatever the load.
put(TEMP, 40000)
until("the sensor did not appear", lambda: "sensor=1 temp_mc=40000 level=0" in everything(), 5)
put(TEMP, 65000)
until("warm did not cap the operating point", lambda: word(LEVEL) == 1 and word(OPP) == 2, 5)
put(TEMP, 80000)
until("hot did not cap the operating point", lambda: word(LEVEL) == 2 and word(OPP) == 1 and word(KHZ) == 1000000, 5)
# Cooling lets go one level at a time, five seconds each.
put(TEMP, 40000)
start = time.monotonic()
until("the level never fell", lambda: word(LEVEL) == 0 and word(OPP) == 3, 20)
if time.monotonic() - start < 8:
    fail(f"the level fell from hot to normal in {time.monotonic() - start:.1f} s (two dwells of 5 s expected)")

# A sensor that stops answering is treated as hot, and says so.
put(TEMP, 0xffffffff)
until("a failing sensor was not treated as hot", lambda: word(LEVEL) == 2 and word(FLAGS) & 4 and word(OPP) == 1, 8)
until("the failure is not reported", lambda: "sensor=2" in everything(), 5)
if word(1) == 2:
    fail("a failing sensor powered the machine off")
put(TEMP, 40000)
until("the sensor did not recover", lambda: word(FLAGS) & 4 == 0 and word(LEVEL) < 2, 10)

# The app ends holding its lock: the lock ends with it.
until("the app did not finish", lambda: "powertest: done" in everything(), 90)
until("the lock outlived its app", lambda: word(LOCKS) == 0 and word(KINDS) == 0, 15)

# Critical: lowest point and power off. tools/run.py ends the run when it
# sees the shutdown command, so say what was verified first.
open(os.environ["POWER_TEST_REACHED"], "w").write("critical\n")
put(TEMP, 96000)
until("a critical temperature did not power the machine off", lambda: word(1) == 2, 15)
time.sleep(5)
fail("the emulator kept running after the shutdown command")
'''

with TemporaryDirectory(prefix="violet-power-") as directory:
    source = run.IMAGE
    image = Path(directory)
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(source / name)
    viewer = image / "viewer"
    viewer.write_text(home_grid.viewer(SCRIPT, (image / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    reached = image / "reached"
    os.environ["POWER_TEST_REACHED"] = str(reached)
    run.IMAGE = image
    run.machine = partial(run.machine, image=image)
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"power test failed (viewer status {status})")
    if not reached.exists():
        raise SystemExit("power test failed: the machine powered off before the checks finished")
    print("Power verified: wake-lock accounting, load and thermal governors, sensor failure, critical shutdown")
