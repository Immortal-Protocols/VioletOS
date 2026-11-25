"""The built-in apps as open apps, in QEMU: Stopwatch, Sounds and Weather.

First boot: Stopwatch runs for about a second and pauses, which saves its
time in /data; Sounds changes the volume and plays the chime (checked at the
emulated sound card); Weather fetches live conditions for Oakland from
wttr.in through the host relay (skipped if the Mac is offline). Second boot
on the same disk: Stopwatch starts from the saved time.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import socket
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
card = mmap.mmap(ram.fileno(), 9 * 4096, offset=0x49204000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
APPS = json.loads(os.environ["OPEN_APPS"])
STAGE = os.environ["OPEN_STAGE"]
ONLINE = os.environ["OPEN_ONLINE"] == "1"
RESULT = os.environ["OPEN_RESULT"]

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
    print("open apps test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
def card_word(i):
    return struct.unpack_from("<I", card, 4 * i)[0]
def on_list():
    return pixel(60, 300) == 0xff302a39 and pixel(5, 5) == 0xff191620
def on_home():
    # The Home app (apps/open/home): its gradient starts at 2a1740.
    return pixel(5, 5) == 0xff2a1740
def open_apps():
    if on_list():
        return
    if not on_home():
        tap(570, 276)  # the classic launcher's APPS button shows Home
        until("Home app", on_home, 90)
    tap(360, 1141)  # Home's Programs tile
    until("app list", on_list)
def start(name):
    tap(360, 260 + APPS.index(name) * 80 + 34)
def home():
    tap(100, 60)  # every app's "Home" button
    until("back on Home", on_home, 6)
def accent_pixels(y0, y1):
    # The app accent (e6 8c c8) drawn anywhere in a band of rows.
    return sum(1 for y in range(y0, y1, 4) for x in range(0, 720, 4) if pixel(x, y) & 0xffffff == 0xe68cc8)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
time.sleep(3)
open_apps()
start("stopwatch")
until("stopwatch starts", lambda: "stopwatch: starting at" in everything(), 60)
saved = re.findall(r"stopwatch: starting at (\d\d:\d\d\.\d)", everything())[-1]
if STAGE == "second":
    expected = open(RESULT).read().strip()
    if saved != expected:
        fail(f"stopwatch restarted at {saved}, saved {expected}")
    print(f"stopwatch: kept {saved} across the restart", file=sys.stderr)
    sys.exit(0)
until("stopwatch digits", lambda: accent_pixels(600, 720) > 50, 20)
tap(360, 1065)  # Start
until("stopwatch running", lambda: "stopwatch: started" in everything())
time.sleep(1.3)
tap(360, 1065)  # Pause
until("stopwatch paused", lambda: "stopwatch: paused at" in everything())
paused = re.findall(r"stopwatch: paused at (\d\d:\d\d\.\d)", everything())[-1]
if not "00:01.0" <= paused <= "00:03.0":
    fail(f"stopwatch paused at {paused} after about 1.3 s")
open(RESULT, "w").write(paused)
time.sleep(1)  # the runtime writes /data back on its next pass
print(f"stopwatch: paused at {paused}", file=sys.stderr)
home()

open_apps()
start("sounds")
until("sounds draws", lambda: accent_pixels(250, 460) > 100, 60)
before = card_word(8)
tap(530, 530)  # +
tap(190, 780)  # Chime
# The card counts every sound it starts (word 8): the click from "+" and the
# chime. Counting does not depend on catching the chime while it plays.
until("click and chime reach the sound card", lambda: card_word(8) >= before + 2, 15)
print("sounds: chime played", file=sys.stderr)
home()

open_apps()
start("weather")
until("weather draws", lambda: accent_pixels(800, 1460) == 0 and pixel(60, 830) != 0xff191620, 60)
tap(190, 894)  # Oakland
if ONLINE:
    until("weather for Oakland", lambda: re.search(r"weather: Oakland -?\d+°F", everything()), 60)
    print("weather:", re.search(r"weather: Oakland .*", everything()).group(0), file=sys.stderr)
else:
    print("weather: skipped (the Mac is offline)", file=sys.stderr)
time.sleep(1)
'''


def online():
    if os.environ.get("OPEN_FORCE_OFFLINE") == "1":  # CI: no live weather
        return False
    try:
        socket.create_connection(("wttr.in", 443), timeout=5).close()
        return True
    except OSError:
        return False


def listed_apps(cpio):
    """The shell's app list for this image: the titled apps in apps/.home's
    order, then the other programs in /apps, sorted (Home is not listed)."""
    names, home, at = set(), [], 0
    while cpio[at:at + 6] == b"070701":
        size, namesize = int(cpio[at + 54:at + 62], 16), int(cpio[at + 94:at + 102], 16)
        name = cpio[at + 110:at + 110 + namesize - 1].decode()
        data = (at + 110 + namesize + 3) & ~3
        if name == "TRAILER!!!":
            break
        if name == "apps/.home":
            home = [line.split("\t")[0] for line in cpio[data:data + size].decode().splitlines() if "\t" in line]
        elif name.startswith("apps/") and name.count("/") >= 1:
            names.add(name.split("/")[1])
        at = (data + size + 3) & ~3
    others = sorted(n for n in names if n not in home and not n.startswith(".") and n != "home")
    return home + others


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
    apps = listed_apps((original / "initfs.cpio").read_bytes())
    result = Path(tmp) / "stopwatch.txt"
    os.environ.update(OPEN_APPS=json.dumps(apps), OPEN_ONLINE="1" if online() else "0", OPEN_RESULT=str(result))
    for stage in ("first", "second"):
        os.environ["OPEN_STAGE"] = stage
        status = run.main(str(viewer))
        if status != 0:
            raise SystemExit(f"open apps test failed in the {stage} boot (viewer status {status})")
    print("Open apps verified: Stopwatch (with /data across a restart), Sounds, Weather")
