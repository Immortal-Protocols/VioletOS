"""Stream a track from the music library on the emulator, end to end.

Needs the internet: the Mac relays the guest's TLS connection to the library.
The scripted viewer connects the virtual Wi-Fi, opens Music, starts track 1 and
checks that decoded audio reaches the emulated sound card. Not part of CI.
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
card = mmap.mmap(ram.fileno(), 9 * 4096, offset=0x49204000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
def word(i): return struct.unpack_from("<I", card, 4 * i)[0]
def pixel(x, y): return struct.unpack_from("<I", screen, 4 * (y * 720 + x))[0]
def log(*a): print(*a, file=sys.stderr, flush=True)
def fail(why):
    log("music test failed:", why, [word(i) for i in range(12)], hex(pixel(60, 950)))
    if os.environ.get("MUSIC_SHOT"):
        open(os.environ["MUSIC_SHOT"], "wb").write(bytes(screen))
    sys.exit(1)
def until(what, check, timeout=8.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check(): return
        time.sleep(0.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)
until("sound card ready", lambda: word(2) == 0x31445541, 20)
time.sleep(2)
# Home -> Wi-Fi Setup -> Connect, as in test_guest.py, then back Home.
tap(360, 680); tap(360, 820); time.sleep(3); tap(360, 1376); time.sleep(1)
# Home -> Apps -> Next (Wi-Fi Setup) -> Next (Music) -> Open.
tap(570, 272); tap(360, 440); tap(360, 440); tap(360, 656)
time.sleep(3)
tap(300, 713)  # the first song's row: Glass Orchard, 0:52
start = time.monotonic()
until("music plays", lambda: word(3) & 1 and word(11) > 48000, 60)
log(f"playing after {time.monotonic() - start:.1f} s")
before = word(11); time.sleep(5); after = word(11)
rate = (after - before) / 5
if os.environ.get("MUSIC_DUMP"):
    # Ring snapshot for offline comparison: total frames written, then the ring.
    total = word(5)
    open(os.environ["MUSIC_DUMP"], "wb").write(struct.pack("<I", total) + bytes(card[4096:9 * 4096]))
peak = word(10)
log(f"output {rate:.0f} frames/s, peak {peak}")
if not 40000 < rate < 56000: fail(f"output rate {rate}")
if peak < 1000: fail("output is silent")
# Leave Music: the song ends at 0:52 and Audio starts song 2 by itself.
tap(82, 82)  # Music's Home button
deadline = time.monotonic() + 75
gap = 0
last = word(5); quiet_since = None
while time.monotonic() < deadline:
    time.sleep(0.5)
    now = word(5)
    if now == last:
        quiet_since = quiet_since or time.monotonic()
        if time.monotonic() - quiet_since > 8: fail("output stopped instead of advancing")
    else:
        quiet_since = None
    last = now
total = word(5)
log(f"{total / 48000:.0f} s of audio written; song 2 followed song 1")
if total < 48000 * 60: fail("song 2 did not follow")
print("music checks passed", file=sys.stderr)
'''

with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = ROOT / "build/sel4/qemu"
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"music test failed (viewer status {status})")
    print("Music streamed from the library and played through the sound card")
