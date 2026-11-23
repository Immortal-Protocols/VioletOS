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
STATE = {0xff7cf0a0: "playing", 0xfff0d070: "buffering", 0xfff07070: "offline", 0xff8080a0: "idle"}
def state(): return STATE.get(pixel(60, 950), hex(pixel(60, 950)))

until("sound card ready", lambda: word(2) == 0x31445541, 20)
time.sleep(2)
# Home -> Wi-Fi Setup -> Connect, as in test_guest.py, then back Home.
tap(360, 680); tap(360, 820); time.sleep(3); tap(360, 1376); time.sleep(1)
# Home -> Apps -> Next (Wi-Fi Setup) -> Next (Music) -> Open.
tap(570, 272); tap(360, 440); tap(360, 440); tap(360, 656)
until("Music opens", lambda: state() in STATE.values())
tap(360, 400)  # top half: play track 1
until("music starts", lambda: state() in ("buffering", "playing", "offline"), 5)
start = time.monotonic()
until("music plays", lambda: state() == "playing" and word(11) > 48000, 60)
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
tap(360, 400)  # pause
until("pauses", lambda: state() == "idle" and word(3) == 0, 5)
tap(360, 400)  # resume
until("resumes", lambda: state() == "playing" and word(3) & 1, 10)
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
