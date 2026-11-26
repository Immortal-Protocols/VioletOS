"""Settings › Wi-Fi in QEMU, driven by touch like a person would.

First boot: Settings › Wi-Fi (apps/open/settings) opens, which scans and lists the
emulator's three networks (tools/run.py: two open, VIOLET HOME with WPA2).
Choosing VIOLET HOME asks for its password; a wrong one is refused by Radio,
the right one ("violet-home", typed on the on-screen keyboard, symbols page
included) joins it and moves it to the top of the list, and its page then
shows the link's addresses. Then an open
network is chosen, which leaves VIOLET HOME and joins that one. Second boot on the same disk: the runtime rejoins that
open network by itself (the most recently joined saved network), and VIOLET
HOME then joins with one tap, from the key saved in the app's /data.
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
import mmap, os, re, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
STAGE = os.environ["WIFI_STAGE"]

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
    if os.environ.get("WIFI_SHOT"):
        open(os.environ["WIFI_SHOT"], "wb").write(bytes(screen[:]))
    print("wifi app test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def tap(x, y, hold=.12):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(hold); os.write(1, bytes(6)); time.sleep(.2)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
def count(pattern):
    return len(re.findall(pattern, everything()))

# The keyboard's layout (apps/open/settings/src/keyboard.rs).
UNIT, GAP, ROW_Y, KEY_H = 63.0, 8.0, [1072, 1192, 1312, 1432], 104
def rows(page):
    bottom = lambda mode: [(mode, 2.0), ("SPACE", 6.0), (".", 1.5)]
    punct = lambda mode: [(mode, 1.5)] + [(c, 1.0) for c in ".,?!'"] + [("BACK", 1.5)]
    chars = lambda s: [(c, 1.0) for c in s]
    if page == "letters":
        return [chars("qwertyuiop"), chars("asdfghjkl"), [("SHIFT", 1.5)] + chars("zxcvbnm") + [("BACK", 1.5)], bottom("NUMBERS")]
    return [chars("1234567890"), chars("-/:;()$&@\""), punct("SYMBOLS"), bottom("LETTERS")]
def key_center(page, key):
    for r, keys in enumerate(rows(page)):
        width = sum(w for _, w in keys) * UNIT + (len(keys) - 1) * GAP
        x = (720 - width) / 2
        for k, w in keys:
            if k == key:
                return int(x + w * UNIT / 2), ROW_Y[r] + KEY_H // 2
            x += w * UNIT + GAP
    raise KeyError(key)
def type_text(s):
    page = "letters"
    for ch in s:
        want = "letters" if ch.isalpha() else "numbers"
        if want != page:
            tap(*key_center(page, "NUMBERS" if want == "numbers" else "LETTERS"))
            page = want
        tap(*key_center(page, ch))
    if page != "letters":
        tap(*key_center(page, "LETTERS"))

ROW = lambda i: 440 + i * 128 + 58  # the middle of a network's row
CONNECT = (360, 535)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
time.sleep(3)
until("Home", lambda: "home: ready" in everything(), 60)
open_icon("settings")
until("Settings starts", lambda: "wifi: ready" in everything(), 60)
time.sleep(1)
tap(360, 250 + 54)  # Settings' Wi-Fi row
until("scan finds the three networks", lambda: "wifi: found 3 networks" in everything(), 30)
time.sleep(.5)

if STAGE == "second":
    if "wifi: ready, 2 saved networks" not in everything():
        fail("the joined networks were not kept in /data")
    # Nobody taps anything: the runtime rejoins the most recently joined saved
    # network (the open VIOLET LAB) by itself, a few seconds after boot.
    lab = "VIOLET LAB".encode().hex()
    until("VIOLET LAB rejoins on its own", lambda: f"wifi: auto-join {lab} result 0" in everything(), 40)
    print("wifi: VIOLET LAB rejoined by itself after a restart", file=sys.stderr)
    time.sleep(1.5)  # the app notices and shows the list again
    tap(360, ROW(2))
    until("VIOLET HOME joins with its saved key", lambda: "wifi: joined VIOLET HOME" in everything(), 30)
    print("wifi: VIOLET HOME joined with one tap after a restart", file=sys.stderr)
    time.sleep(1.5)
    sys.exit(0)

if STAGE == "weak":
    # VIOLET HOME's handshake now times out (a weak signal): the saved key must
    # survive the failure, and the app must not ask for the password.
    if "wifi: ready, 2 saved networks" not in everything():
        fail("the saved networks were lost before the weak-signal boot")
    home = "VIOLET HOME".encode().hex()
    lab = "VIOLET LAB".encode().hex()
    until("VIOLET HOME (most recent) is tried on its own", lambda: f"wifi: auto-join {home} result 57" in everything(), 40)
    until("then the next saved network, VIOLET LAB", lambda: f"wifi: auto-join {lab} result 0" in everything(), 20)
    time.sleep(1.5)
    tap(360, ROW(2))
    until("a timeout is reported", lambda: "wifi: joining VIOLET HOME failed (57)" in everything(), 30)
    time.sleep(1.5)
    print("wifi: a handshake timeout kept VIOLET HOME's key", file=sys.stderr)
    sys.exit(0)

if STAGE == "after":
    if "wifi: ready, 2 saved networks" not in everything():
        fail("a handshake timeout lost a saved network")
    print("wifi: both saved networks are still there", file=sys.stderr)
    sys.exit(0)

tap(360, ROW(2))  # VIOLET HOME (WPA2): asks for the password
time.sleep(.5)
type_text("wrongpassword")
tap(*CONNECT)
until("a wrong password is refused", lambda: "wifi: joining VIOLET HOME failed (52)" in everything(), 20)
print("wifi: wrong password refused", file=sys.stderr)
for _ in range(len("wrongpassword")):
    tap(*key_center("letters", "BACK"))
type_text("violet-home")
tap(*CONNECT)
until("VIOLET HOME joins", lambda: "wifi: joined VIOLET HOME" in everything(), 20)
print("wifi: joined VIOLET HOME with its password", file=sys.stderr)
time.sleep(1)
# The joined network's page: besides its status, security and password, the
# link Radio reports through "wifi info" (in the emulator its address and
# router, no host name sent, and the radio's address, kind and name).
tap(360, ROW(0))  # the connected network leads the list now
until("VIOLET HOME's page shows the link", lambda: "wifi: showing VIOLET HOME (9 details)" in everything(), 10)
if os.environ.get("WIFI_DETAILS_SHOT"):
    time.sleep(.5)
    open(os.environ["WIFI_DETAILS_SHOT"], "wb").write(bytes(screen[:]))
print("wifi: the joined network's page shows its link", file=sys.stderr)
tap(108, 63)  # Back
time.sleep(.5)
tap(360, ROW(1))  # VIOLET LAB (open), below the connected network: leave VIOLET HOME, scan, join
until("VIOLET LAB joins", lambda: "wifi: joined VIOLET LAB" in everything(), 30)
print("wifi: switched to the open network VIOLET LAB", file=sys.stderr)
time.sleep(1.5)  # the runtime writes /data back on its next pass
'''


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make qemu-image).")
    viewer.write_text(home_grid.viewer(SCRIPT, (original / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    for stage in ("first", "second", "weak", "after"):
        os.environ.pop("VIOLET_NETWORKS", None)
        if stage == "weak":
            os.environ["VIOLET_NETWORKS"] = json.dumps(
                [n if n != run.DEFAULT_NETWORKS[2] else dict(n, stalls=True) for n in run.DEFAULT_NETWORKS])
        os.environ["WIFI_STAGE"] = stage
        status = run.main(str(viewer))
        if status != 0:
            raise SystemExit(f"Wi-Fi app test failed in the {stage} boot (viewer status {status})")
    print("Wi-Fi app verified: scan, wrong and right WPA2 password, the joined link's details, switching networks, rejoining by itself, the saved key after a restart and after a handshake timeout")
