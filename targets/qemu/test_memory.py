"""Verify that a foreground allocation can reclaim a background app in QEMU."""

from functools import partial
from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import home_grid
import os
# This suite boots the 32-bit (AArch32) image: build/sel4/qemu, make qemu32-image.
os.environ["VIOLET_ARCH"] = "aarch32"
import run

SCRIPT = r'''#!/usr/bin/env python3
import os, struct, sys, time

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
    sys.exit(f"{why}\n{everything()}")

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

until("Home did not start", lambda: "home: ready" in everything())
shown = home_shown()
open_icon("cache")
until("Cache did not return to Home", lambda: home_shown() > shown)
open_icon("pressure")
until("Foreground allocation failed",
      lambda: "memory test: foreground allocation survived" in everything())
log = everything()
marks = ["memory test: allocating past the free pool",
         "closed cache in the background",
         "memory test: foreground allocation survived"]
positions = [log.find(mark) for mark in marks]
if min(positions) < 0 or positions != sorted(positions):
    fail("Background memory was not reclaimed during the foreground allocation")
print("Foreground allocation reclaimed the background cache", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-memory-") as directory:
    source = run.IMAGE
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
