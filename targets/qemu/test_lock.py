"""The power key, the screen lock and power off in QEMU.

First boot: the power key turns the screen off and on; without a passcode
nothing locks. Settings > Passcode sets 2580. The power key then locks: the
screen comes back on Home's lock screen, which refuses a wrong passcode and an
app's icon and opens with the right one. Second boot on the same disk: the
phone starts locked. Holding the power key brings the power sheet; Cancel
closes it, and Power off stops the machine.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
# The Power fixture's page (targets/qemu/power.rs): the key's level and press count.
low = mmap.mmap(ram.fileno(), 0x4000, offset=0x49000000 - 0x40000000)
frame = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000)
ICONS = json.loads(os.environ["OPEN_ICONS"])
STAGE = os.environ["OPEN_STAGE"]

def text():
    n = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:4096])
    if n <= len(data):
        return data[:n].decode(errors="replace")
    s = n % len(data)
    return (data[s:] + data[:s]).decode(errors="replace")
seen = ""
def everything():
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("lock test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def count(line):
    return everything().count(line)
def after(what, line, action, timeout=15.0):
    before = count(line)
    action()
    until(what, lambda: count(line) > before, timeout)
def quiet(what, line, action, wait=2.0):
    before = count(line)
    action()
    time.sleep(wait)
    if count(line) != before:
        fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)
def power(seconds=0.15):
    struct.pack_into("<I", low, 0x2008, 1)
    struct.pack_into("<I", low, 0x200c, struct.unpack_from("<I", low, 0x200c)[0] + 1)
    time.sleep(seconds)
    struct.pack_into("<I", low, 0x2008, 0)
    time.sleep(.3)
def black():
    return all(frame[4 * (y * 720 + x):4 * (y * 720 + x) + 3] == b"\0\0\0" for x, y in [(10, 10), (360, 800), (700, 1590), (144, 407)])
# The number pad (apps/lib/violet/src/keypad.rs).
def key(k):
    slot = {"ok": 9, "0": 10, "del": 11}.get(k, int(k) - 1 if k.isdigit() else 0)
    tap(68 + 88 + slot % 3 * 204, 820 + 66 + slot // 3 * 156)
def code(digits):
    for d in digits:
        key(d)
    key("ok")

until("Home starts", lambda: "home: ready" in everything(), 90)
time.sleep(2)

if STAGE == "first":
    # No passcode: the key only turns the screen off and on.
    after("the screen turns off", "screen off", power)
    time.sleep(.5)
    if not black():
        fail("the screen is not black while off")
    quiet("a tap woke the screen", "screen on", lambda: tap(360, 800), 1.0)
    after("the screen turns on", "home: shown", power)
    if "locked" in everything():
        fail("locked without a passcode")
    print("lock: the power key turns the screen off and on", file=sys.stderr)

    tap(*ICONS["settings"])
    until("Settings starts", lambda: "settings: ready" in everything(), 60)
    time.sleep(1)
    tap(360, 250 + 5 * 124 + 54)  # the Passcode row
    until("Passcode opens", lambda: "settings: Passcode" in everything())
    tap(360, 595)  # Set a passcode
    time.sleep(.5)
    code("2580")
    # A different second entry starts over.
    quiet("a mismatched passcode was accepted", "passcode set", lambda: code("2581"), 1.0)
    code("2580")
    after("the passcode is set", "settings: Passcode set", lambda: code("2580"))
    print("lock: passcode set in Settings", file=sys.stderr)

    # Now the key locks, from inside Settings.
    after("the screen turns off and locks", "screen off, locked", power)
    time.sleep(.5)
    if not black():
        fail("the screen is not black while locked and off")
    after("Home's lock screen shows", "home: shown", power)
    time.sleep(1)
    quiet("an app opened while locked", "home: opening", lambda: tap(*ICONS["settings"]))
    for _ in range(8):
        key("del")
    after("a wrong passcode is refused", "wrong passcode", lambda: code("1111"))
    if "home: unlocked" in everything():
        fail("a wrong passcode unlocked the phone")
    after("the passcode unlocks", "home: unlocked", lambda: code("2580"))
    after("apps open again", "home: opening settings", lambda: tap(*ICONS["settings"]))
    print("lock: locked by the power key, unlocked with the passcode", file=sys.stderr)
    time.sleep(3)  # the runtime writes /data back on its next passes
    sys.exit(0)

# Second boot: locked from the start.
if "locked at start" not in everything():
    fail("the phone did not start locked")
quiet("an app opened on a phone that started locked", "home: opening", lambda: tap(*ICONS["settings"]))
for _ in range(8):
    key("del")
after("the passcode unlocks after a restart", "home: unlocked", lambda: code("2580"))
after("holding the key shows the power sheet", "home: power sheet", lambda: power(1.3))
time.sleep(.5)
tap(360, 910)  # Cancel
after("the sheet comes back", "home: power sheet", lambda: power(1.3))
time.sleep(.5)
print("lock: started locked; powering off", file=sys.stderr)
open(os.environ["LOCK_MARK"], "w").write("off")
tap(360, 750)  # Power off
time.sleep(15)
fail("the machine did not power off")
'''


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make qemu-image).")
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    apps = home_grid.apps((original / "initfs.cpio").read_bytes())
    os.environ["OPEN_ICONS"] = json.dumps({name: home_grid.icon(i, apps) for i, name in enumerate(apps)})
    mark = Path(tmp) / "power-off"
    os.environ["LOCK_MARK"] = str(mark)
    for stage in ("first", "second"):
        os.environ["OPEN_STAGE"] = stage
        status = run.main(str(viewer))
        if status != 0:
            raise SystemExit(f"lock test failed in the {stage} boot (viewer status {status})")
    # The second boot ends when the guest asks for power off (tools/run.py).
    if not mark.exists():
        raise SystemExit("lock test failed: the second boot ended before Power off was tapped")
    print("Lock verified: screen off and on, a passcode set in Settings, lock and unlock, locked after a restart, power off")
