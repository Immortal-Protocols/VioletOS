"""Idle cost at Home, measured from the host: QEMU's own CPU time over 20 s.

Boots the image, waits for Home, lets it settle, then reads the QEMU
process's CPU time (ps) before and after a 20 s window with nobody touching
the screen. It prints the share of one host core QEMU used and the Open
Runtime's own "asleep" figure from its next stats line.

IDLE_IMAGE=DIR measures another image directory (zImage, handoff.dtb,
entry.bin, initfs.cpio), for a before/after comparison. IDLE_MAX_PERCENT=N
fails the run when QEMU used more than N% of a core; without it that number
is only reported (QEMU's own CPU includes the services' seL4_Yield delay
loops, runtime/transport.c, which the Open Runtime does not control). What
the Open Runtime controls is enforced: it must report itself asleep at least
IDLE_MIN_ASLEEP percent of the time (default 50) and run its loop at most
IDLE_MAX_LOOPS times a second (default 620, the figure measured at Home
before the lifecycle work), so a return to short polling for event or pipe
waits fails the run.
"""

from functools import partial
from pathlib import Path
from tempfile import TemporaryDirectory
import os
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import os, re, struct, subprocess, sys, time

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

seen = ""
def collect():
    """The log so far, kept past the 4 KiB ring."""
    global seen
    now = everything()
    overlap = min(len(seen), 2048)
    tail = seen[-overlap:] if overlap else ""
    at = now.find(tail) if tail else 0
    seen += now[at + len(tail):] if at >= 0 else now
    return seen

def fail(why):
    sys.exit(f"{why}\n{collect()[-3000:]}")

def until(what, check, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        collect()
        if check():
            return
        time.sleep(.1)
    fail(what)

def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15)
    os.write(1, bytes(6))

def qemu_pid():
    out = subprocess.run(["pgrep", "-P", str(os.getppid()), "qemu-system-arm"], capture_output=True, text=True).stdout.split()
    if not out:
        fail("could not find the QEMU process")
    return int(out[0])

def cpu_seconds(pid):
    text = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    days, _, rest = text.rpartition("-")
    parts = [float(p) for p in rest.split(":")]
    seconds = 0.0
    for p in parts:
        seconds = seconds * 60 + p
    return seconds + (int(days) * 86400 if days else 0)

until("Home did not start", lambda: "home: ready" in seen, 90)
time.sleep(5)
pid = qemu_pid()
window = float(os.environ.get("IDLE_WINDOW", "20"))
c0, t0 = cpu_seconds(pid), time.monotonic()
time.sleep(window)
c1, t1 = cpu_seconds(pid), time.monotonic()
share = 100 * (c1 - c0) / (t1 - t0)
print(f"idle: QEMU used {c1 - c0:.2f} s of CPU in {t1 - t0:.1f} s at Home ({share:.0f}% of one core)", file=sys.stderr)
start = len(collect())
until("no Open Runtime stats line", lambda: "stats:" in seen[start:], 45)
line = next(l for l in seen[start:].splitlines() if "stats:" in l)
print(f"idle: {line.strip()}", file=sys.stderr)
asleep = int(re.search(r"asleep (\d+)%", line).group(1))
if asleep < int(os.environ.get("IDLE_MIN_ASLEEP", "50")):
    fail(f"the Open Runtime was asleep only {asleep}% of the time at Home")
loops = int(re.search(r"stats: (\d+) loops/s", line).group(1))
if loops > int(os.environ.get("IDLE_MAX_LOOPS", "620")):
    fail(f"the Open Runtime ran its loop {loops} times a second at Home, above {os.environ.get('IDLE_MAX_LOOPS', '620')}")
limit = os.environ.get("IDLE_MAX_PERCENT")
if limit and share > float(limit):
    fail(f"QEMU used {share:.0f}% of a core at Home, above {limit}%")
print("idle measurement done", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-idle-") as directory:
    source = Path(os.environ.get("IDLE_IMAGE", run.IMAGE)).resolve()
    image = Path(directory)
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(source / name)
    viewer = image / "viewer"
    cpio = (image / "initfs.cpio").read_bytes()
    viewer.write_text(home_grid.viewer(SCRIPT, cpio))
    viewer.chmod(0o755)
    run.IMAGE = image
    run.machine = partial(run.machine, image=image)
    raise SystemExit(run.main(str(viewer)))
