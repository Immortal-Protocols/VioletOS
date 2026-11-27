"""Background lifecycle and the low-memory killer in QEMU (runtime/open/lifecycle.rs).

With the memory test apps (make test-lifecycle builds them in):
- Ticker, left behind Home, keeps ticking through its grace and cached time,
  then freezes (about 15 s) and stops ticking; opened again it runs at once
  and ends;
- Jobs stays in front while its forked child works for 20 s: the child of
  the app in front is never frozen, so the parent's waitpid ends;
- Cache (64 MiB) freezes behind Home too; Pressure then allocates past the
  free pool: background apps are told memory is low, Cache is closed with
  status 137 while Pressure (in front) and Home survive;
- Cache opens again afterwards and starts afresh.
"""

from functools import partial
from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
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

seen = ""
def collect():
    """The whole log so far, kept past the 4 KiB ring."""
    global seen
    now = everything()
    tail = seen[-2048:]
    at = now.find(tail) if tail else 0
    seen += now[at + len(tail):] if at >= 0 else now
    return seen

def fail(why):
    sys.exit(f"{why}\n{collect()[-4000:]}")

def until(what, check, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        collect()
        if check():
            return
        time.sleep(.05)
    fail(what)

def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15)
    os.write(1, bytes(6))

def shown():
    return collect().count("home: shown") + seen.count("home: ready")

until("Home did not start", lambda: "home: ready" in seen, 90)

# Ticker: runs behind Home, then freezes and stops ticking.
n = shown()
open_icon("ticker")
until("Ticker did not start", lambda: "ticker: started" in seen)
started = time.monotonic()
until("Ticker did not return to Home", lambda: shown() > n)
until("Ticker never froze behind Home", lambda: "lifecycle: froze ticker" in seen, 40)
took = time.monotonic() - started
if not 10 <= took <= 25:
    fail(f"Ticker froze after {took:.0f} s; the policy is a 5 s grace and 10 s cached")
if "ticker: tick" not in seen[:seen.find("lifecycle: froze ticker")]:
    fail("Ticker did not run while it was cached")
mark = len(seen)
time.sleep(4)
if "ticker: tick" in collect()[mark:]:
    fail("Ticker kept running while frozen")
n = shown()
open_icon("ticker")
until("Ticker did not run again in front", lambda: "ticker: thawed after" in seen[mark:], 10)
until("Ticker did not return to Home", lambda: shown() > n)
print("lifecycle: Ticker froze behind Home and ran again in front", file=sys.stderr)

# Jobs: a child of the app in front runs past the 15 s a background app gets.
mark = len(seen)
n = shown()
open_icon("jobs")
until("Jobs' child did not finish while its parent was in front", lambda: "jobs: child finished" in seen[mark:], 45)
if "jobs: child finished with 7" not in seen[mark:]:
    fail("Jobs' child did not end with status 7")
if "lifecycle: froze jobs" in seen[mark:]:
    fail("the child of the app in front was frozen")
until("Jobs did not return to Home", lambda: shown() > n)
print("lifecycle: the child of the app in front kept running", file=sys.stderr)

# Cache freezes too; then Pressure needs the pool.
n = shown()
open_icon("cache")
until("Cache did not start", lambda: "memory test: cache resident" in seen)
until("Cache did not return to Home", lambda: shown() > n)
until("Cache never froze", lambda: "lifecycle: froze cache" in seen, 40)
mark = len(seen)
n = shown()
open_icon("pressure")
until("Pressure's allocation failed", lambda: "memory test: foreground allocation survived" in seen[mark:], 60)
log = seen[mark:]
marks = ["memory test: allocating past the free pool", "closed cache in the background"]
at = [log.find(m) for m in marks]
if min(at) < 0 or at != sorted(at):
    fail(f"expected, in order: {marks}")
if "lifecycle: app memory low" not in log:
    fail("background apps were not told memory was low")
if not re.search(r"cache \(pid \d+\) exited with status 137", log):
    fail("Cache was not closed with status 137")
if re.search(r"(home|pressure) \(pid \d+\) exited", log):
    fail("Home or the app in front was closed")
print("lifecycle: low memory closed the frozen Cache, not the app in front or Home", file=sys.stderr)

# Pressure ends; Cache opens again from scratch.
until("Pressure did not end", lambda: re.search(r"pressure \(pid \d+\) exited with status 0", seen[mark:]), 30)
until("Home did not come back", lambda: shown() > n, 15)
mark = len(seen)
open_icon("cache")
until("Cache did not start again after being closed", lambda: "memory test: cache resident" in seen[mark:])
print("lifecycle verified", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-lifecycle-") as directory:
    source = run.IMAGE
    image = Path(directory)
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(source / name)
    viewer = image / "viewer"
    cpio = (image / "initfs.cpio").read_bytes()
    names = home_grid.apps(cpio)
    if not {"ticker", "jobs", "cache", "pressure"} <= set(names):
        raise SystemExit("The image lacks the memory test apps; run make test-lifecycle, which builds them in.")
    viewer.write_text(home_grid.viewer(SCRIPT, cpio))
    viewer.chmod(0o755)
    run.IMAGE = image
    run.machine = partial(run.machine, image=image)
    raise SystemExit(run.main(str(viewer)))
