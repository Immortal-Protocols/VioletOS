"""The Open Runtime's capability policy in QEMU (`make test-policy`; local, not in CI).

Opens targets/qemu/policy-test-apps/victim from Home, which keeps files under
its own name in /data, then the probe beside it, which has no manifest. The
probe must not see, open, make or remove another app's /data files or the
runtime's (/data/.settings, /data/.grants), must find /apps, /bin and /lib
read-only, must be refused the microphone, camera, Bluetooth, audio and
sockets, and must be answered "error not allowed" for the system's
/dev/violet/sys commands, while its own /data and the shared settings work
(runtime/open/policy.rs, manifest.rs). The runtime logs each refused capability.
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
    sys.exit(f"policy test failed: {why}\n{everything()}")

def until(what, check, timeout=60):
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

until("Home did not start", lambda: "home: ready" in everything(), 90)
shown = home_shown()
open_icon("victim")
until("the victim did not write its files", lambda: "victim: wrote" in everything())
until("the victim did not go Home", lambda: home_shown() > shown)
open_icon("probe")
seen, refused = [], set()
def done():
    # The log is a 4 KiB ring: collect the lines while they are in it.
    log = everything()
    seen.extend(l for l in re.findall(r"policy: FAIL [^\n]*", log) if l not in seen)
    refused.update(re.findall(r"policy: probe \(pid \d+\) may not use (\S+)", log))
    return re.search(r"policy: (ok|FAILED) \d+ checks", log)
until("the probe did not finish", done, 90)
if seen or "policy: FAILED" in everything():
    fail("\n".join(seen) or "the probe failed")
for cap in ("net", "mic", "system", "wifi-admin"):
    if cap not in refused:
        fail(f"the runtime did not log the refused {cap} (saw {sorted(refused)})")
print(re.findall(r"policy: ok \d+ checks", everything())[-1], file=sys.stderr)
print("capability policy holds: per-app /data, read-only /apps, devices, sockets and system commands", file=sys.stderr)
'''

with TemporaryDirectory(prefix="violet-policy-") as directory:
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
