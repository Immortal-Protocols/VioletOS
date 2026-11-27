"""The trusted system UI in QEMU (`make test-trusted-ui`; local, not in CI).

First boot, no passcode: no lock screen. The UI Probe
(targets/qemu/trusted-ui-test-apps/uiprobe) paints its whole surface red
and presents it over and over: the band's rows stay the runtime's. The
probe's `bench`, `set relay`, `notifications` and `passcode off` are
refused. It has no manifest: `consent mic`, `consent hci` and opening the
camera are refused at once and no prompt comes up (only an Agent-installed
app's declared net, mic or camera is asked of the user). Touches reach the
probe, but not ones on the band. Then a passcode (1357) is set in Settings
on the runtime's keypad, which locks the passcode pad's digits away from apps.

Second boot on the same disk: the lock screen is up at once and a tap where
Home's Settings icon is opens nothing. Five wrong passcodes, each counted;
then the right one is not even checked during the 30 s back-off; after it,
the right one unlocks. Home's Lock locks the phone again, and the passcode
unlocks it. The probe posts a notification and locks the phone:
the lock screen shows only a count, and the right passcode unlocks again.
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
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
STAGE = os.environ["TUI_STAGE"]
# runtime/open/device.rs and trusted_ui.rs
ACCENT, PANEL, BG, INK, BAND_BG, RED = 0xffd1b0fe, 0xff302a39, 0xff191620, 0xffeeeaf8, 0xff0c0a10, 0xffe01020
BAND_H = 32

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
    print("trusted ui test failed:", why, "\n--- log ---\n" + everything()[-3500:], file=sys.stderr)
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
def wait_line(line, timeout=15.0, after=0):
    until(line, lambda: count(line) > after, timeout)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]

# The runtime's keypad (runtime/open/sysui.rs pad_rect): 1-9, DEL 0 OK, CANCEL.
def key_at(n):
    if n == 12:
        return 360, 800 + 4 * 160 + 50
    col, row = n % 3, n // 3
    return 40 + col * 220 + 100, 800 + row * 160 + 70
def key(k):
    tap(*key_at({"DEL": 9, "OK": 11, "CANCEL": 12}.get(k, 10 if k == "0" else (int(k) - 1 if k.isdigit() else 0))))
def enter(pin):
    for d in pin:
        key(d)
    key("OK")
def band_is_system():
    return pixel(2, 2) == ACCENT and pixel(717, 29) == ACCENT
def probe_touches():
    return count("uiprobe: touch ")
DENY, ALLOW = (210, 965), (510, 965)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 90)

if STAGE == "first":
    until("Home starts", lambda: "home: ready" in everything(), 90)
    if "trusted ui: locked" in everything():
        fail("locked with no passcode set")
    time.sleep(2)
    # Home is an app in front: the band is drawn above it.
    until("band over Home", lambda: pixel(700, 2) == BAND_BG)
    open_icon("uiprobe")
    wait_line("uiprobe: ready first", 60)
    wait_line("uiprobe: passcode-off")
    for what, want in (("bench", "error"), ("set-relay", "error"), ("notifications", "error"), ("passcode-off", "error")):
        line = [l for l in everything().splitlines() if l.startswith(f"uiprobe: {what} ")][-1]
        if want not in line:
            fail(f"an app was allowed: {line}")
    # The probe presents red over the whole surface; the band's rows stay the runtime's.
    until("the probe's red below the band", lambda: pixel(360, BAND_H + 8) == RED and pixel(360, 1500) == RED)
    for _ in range(10):
        reds = [x for x in range(0, 720, 4) for y in (0, 10, BAND_H - 1) if pixel(x, y) == RED]
        if reds:
            fail(f"the app drew on the band at x={reds[:5]}")
        time.sleep(0.1)
    if pixel(700, 2) != BAND_BG:
        fail(f"band background is {pixel(700, 2):#x}")
    print("band: kept over a full-screen app", file=sys.stderr)

    # Consent for an app the user's answer cannot decide: the probe is from
    # the image and has no manifest, so policy refuses the microphone,
    # Bluetooth and the camera at once, and no prompt is raised (a DENY
    # there would be shown but not enforced). The prompt itself is raised only
    # for an Agent-installed app (host-tested: runtime/open/tests/sysui_host.rs).
    wait_line("uiprobe: open camera", 20)
    for line in ("uiprobe: consent mic deny", "uiprobe: consent hci deny", "uiprobe: open camera 13"):
        if line not in everything():
            fail(f"expected {line!r}")
    if "trusted ui: prompt" in everything():
        fail("a prompt was raised for an app policy decides alone")
    wait_line("uiprobe: listening")
    print("consent: policy's answer for an image app, no prompt", file=sys.stderr)
    # Touches reach the app, except on the band.
    time.sleep(0.6)
    tap(360, 1300)
    until("touches reach the app", lambda: "uiprobe: touch 3 360 1300" in everything(), 5)
    after = probe_touches()
    tap(360, 10)
    time.sleep(1)
    if probe_touches() != after:
        fail("a touch on the band reached the app")
    print("touch: the app's own; none from the band", file=sys.stderr)
    n = count("home: shown")
    tap(360, 700)    # the probe goes Home
    until("back on Home", lambda: count("home: shown") > n, 10)

    # Settings > Passcode > Set passcode: the runtime's keypad.
    time.sleep(1)
    open_icon("settings")
    wait_line("settings: ready", 60)
    time.sleep(1)
    tap(360, 250 + 5 * 124 + 54)   # Passcode, the menu's sixth row
    wait_line("settings: Passcode")
    time.sleep(0.5)
    tap(360, 1060)                 # Set passcode
    wait_line("trusted ui: passcode pad")
    time.sleep(0.6)
    if not band_is_system() or pixel(45, 805) != PANEL:
        fail("the passcode pad is not on screen")
    enter("1357")
    enter("1357")
    wait_line("trusted ui: passcode set", 60)
    wait_line("settings: Passcode", 10, after=1)
    print("passcode: set on the system keypad", file=sys.stderr)
    time.sleep(2)
    sys.exit(0)

# Second boot: locked from the start.
wait_line("trusted ui: locked", 60)
until("Home starts behind the lock", lambda: "home: ready" in everything(), 90)
time.sleep(1)
until("lock screen drawn", lambda: band_is_system() and pixel(45, 805) == PANEL and pixel(360, 700) == BG, 10)
tap(*HOME_ICONS["settings"])
time.sleep(2)
if "home: opening" in everything() or "settings: ready" in everything():
    fail("a tap on the lock screen opened an app")
for n in range(1, 6):
    enter("0000")
    wait_line("trusted ui: wrong passcode", 60, after=n - 1)
print("lock: five wrong passcodes counted", file=sys.stderr)
enter("1357")
wait_line("trusted ui: passcode not checked: waiting", 10)
if "trusted ui: unlocked" in everything():
    fail("unlocked during the back-off")
print("lock: the right passcode is not checked during the back-off", file=sys.stderr)
time.sleep(31)
enter("1357")
wait_line("trusted ui: unlocked", 60)
until("Home shown", lambda: count("home: shown") > 0, 10)
print("lock: unlocked after the back-off", file=sys.stderr)

# Home's Lock (shown with a passcode set) locks the phone at once.
time.sleep(3)   # Home, shown again, has read "passcode" and drawn Lock
tap(606, 236)   # apps/open/home/src/ui.rs lock_rect
wait_line("home: lock -> ok", 10)
wait_line("trusted ui: locked", 10, after=1)
time.sleep(1)
enter("1357")
wait_line("trusted ui: unlocked", 60, after=1)
print("lock: Home's Lock locks, the passcode unlocks", file=sys.stderr)

# A notification, then the app locks the phone: only a count is shown.
time.sleep(1)
open_icon("uiprobe")
wait_line("uiprobe: ready second", 60)
wait_line("uiprobe: listening", 10)
if "uiprobe: notify ok" not in everything():
    fail("the notification was refused")
time.sleep(0.6)
tap(360, 1500)
wait_line("trusted ui: locked", 10, after=2)
time.sleep(1)
if not any(pixel(x, y) == INK for x in range(200, 520, 2) for y in range(440, 462)):
    fail("the lock screen does not show the notification count")
enter("1357")
wait_line("trusted ui: unlocked", 60, after=2)
print("lock: locked by an app, notification shown as a count, unlocked", file=sys.stderr)
'''


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make test-trusted-ui builds it).")
    cpio = (original / "initfs.cpio").read_bytes()
    if "uiprobe" not in home_grid.apps(cpio):
        raise SystemExit("The image has no UI Probe: build it with make test-trusted-ui.")
    viewer.write_text(home_grid.viewer(SCRIPT, cpio))
    viewer.chmod(0o755)
    # A fresh disk for this test, kept across its two boots.
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    for stage in ("first", "second"):
        os.environ["TUI_STAGE"] = stage
        status = run.main(str(viewer))
        if status != 0:
            raise SystemExit(f"trusted UI test failed in the {stage} boot (viewer status {status})")
    print("Trusted UI verified: band, consent only where the user decides, no app touches on the band, sys commands refused, passcode set in Settings, lock at boot with back-off, Home Lock, privacy-safe notification count")
