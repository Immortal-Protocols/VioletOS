"""Open-app memory protection in QEMU (`make test-wx`; local, not in CI).

Launches targets/qemu/wx-test-apps/wxprobe seven times from Home. Each launch
checks the calls the W^X policy and the copy checks must refuse (among them a
file mapped read-only into a page of code, and unmapping the trampoline
page), then does one thing the hardware must stop: write to code, write to a
constant, run the stack, run the heap, read PROT_NONE memory, write to memory
made read-only after it was mapped. The runtime must end the app at
each (status 139, with the kind of access in the log). Across the launches
the stack, heap, mmap area and AT_RANDOM must move.
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

PROBES = [("write-code", "Write"), ("write-constant", "Write"), ("run-stack", "Exec"),
          ("run-heap", "Exec"), ("read-none", "Read"), ("write-readonly", "Write")]
layouts = []
ended = set()

until("Home did not start", lambda: "home: ready" in everything())
if "entropy: seeded" not in everything() and "too little timer jitter" in everything():
    fail("The random generator was not seeded")
for n in range(len(PROBES) + 1):
    shown = home_shown()
    open_icon("wxprobe")
    # The log is a 4 KiB ring: collect this launch's lines while they are in it.
    seen = {}
    def collect():
        log = everything()
        for key, pattern in [("checks", rf"wx {n}: checks ok"),
                             ("layout", rf"wx {n}: layout main=(\S+) stack=(\S+) heap=(\S+) mmap=(\S+) random=([0-9a-f]{{32}})"),
                             ("probe", rf"wx {n}: probe (\S+)"),
                             ("done", rf"wx {n}: all probes done")]:
            found = re.findall(pattern, log)
            if found:
                seen[key] = found[-1]
        # Earlier launches' last lines may still be in the ring: tell them apart by pid.
        for pid, access in re.findall(r"wxprobe \(pid (\d+)\): bad memory access \((\w+)\) at", log):
            if pid not in ended:
                seen["fault"] = access
        for pid, status in re.findall(r"wxprobe \(pid (\d+)\) exited with status (-?\d+)", log):
            if pid not in ended:
                seen["pid"], seen["status"] = pid, status
        if "wx: FAIL" in log:
            fail("A refusal the policy promises did not happen")
        if "SURVIVED" in log:
            fail("The hardware let a forbidden access through")
        return "status" in seen and home_shown() > shown
    until(f"launch {n} of the probe did not end", collect)
    ended.add(seen["pid"])
    if "checks" not in seen or "layout" not in seen:
        fail(f"launch {n}: the checks did not all pass")
    layouts.append(seen["layout"])
    if n < len(PROBES):
        name, access = PROBES[n]
        if seen.get("probe") != name or seen.get("fault") != access or seen["status"] != "139":
            fail(f"launch {n}: expected {name} to end the app on a {access} fault, saw {seen}")
    elif "done" not in seen or seen["status"] != "0":
        fail(f"launch {n}: the harmless run did not exit cleanly: {seen}")
    time.sleep(.3)

mains, stacks, heaps, mmaps, randoms = (set(column) for column in zip(*layouts))
print(f"layouts: {len(mains)} image bases, {len(stacks)} stacks, {len(heaps)} heaps, "
      f"{len(mmaps)} mmap areas, {len(randoms)} AT_RANDOM values in {len(layouts)} launches", file=sys.stderr)
# 19 bits for the stack and 128 for AT_RANDOM never repeat in seven launches;
# mmap has 12 bits and the heap 8, so allow a rare repeat there.
if len(stacks) != len(layouts) or len(randoms) != len(layouts) or len(mmaps) < len(layouts) - 1 or len(heaps) < 3:
    fail(f"The address-space layout did not vary: {layouts}")
if any(r == "00" * 16 for r in randoms):
    fail("AT_RANDOM is zero")
print("W^X, Execute-Never, copy checks and layout randomization hold", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-wx-") as directory:
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
