"""Open Music and walk its two screens.

A scripted viewer opens Music from Home, checks the library's accent play
button, opens the player from the mini player, presses play (QEMU usually has
no network, so the stream goes to buffering or offline), checks the player's
controls are drawn, returns to the library and goes Home.

    python3 targets/qemu/test_music_ui.py   (after make qemu-image)
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

SCRIPT = r'''#!/usr/bin/env python3
import mmap, os, struct, sys, time
ram = open(sys.argv[1], "r+b")
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000,
                   prot=mmap.PROT_READ)
ACCENT = 0xffebb799  # Glass Orchard's accent (apps/music/catalog.rs)
TEXT = 0xfff5f2fb

def fail(why):
    print("music UI test failed:", why, file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check(): return
        time.sleep(0.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def tap_logical(x, y): tap(x * 3, y * 4)
def pixel(x, y): return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]

time.sleep(6)
# Home -> Apps -> Next (Wi-Fi Setup) -> Next (Music) -> Open.
tap_logical(190, 68); tap_logical(120, 110); tap_logical(120, 110); tap_logical(120, 164)
until("library shows the album's play button", lambda: pixel(590, 562) == ACCENT)
tap(300, 1510)  # mini player
until("player shows its play button", lambda: pixel(300, 1330) == TEXT and pixel(420, 1330) == TEXT)
tap(360, 1330)  # play: buffering, offline or (with a network) playing
time.sleep(2)
if pixel(300, 1330) != TEXT: fail("player stopped drawing after play")
tap(82, 84)  # back to the library
until("library again", lambda: pixel(590, 562) == ACCENT)
tap(82, 84)  # Home
until("left Music", lambda: pixel(590, 562) != ACCENT)
print("music UI checks passed", file=sys.stderr)
'''

with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = run.ROOT / "build/sel4/qemu"
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"music UI test failed (viewer status {status})")
    print("Music UI verified: library, player, play, back, Home")
