"""Drive the emulated sound card: volume buttons, headphone jack and playback.

A scripted viewer takes the place of the SDL window. It holds the virtual volume
buttons, plugs in headphones, opens Sounds and taps its controls, then checks
the guest's sound card registers, the PCM it wrote and the volume bar on screen.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

SCRIPT = r'''#!/usr/bin/env python3
import math, mmap, os, struct, sys, time
ram = open(sys.argv[1], "r+b")
card = mmap.mmap(ram.fileno(), 9 * 4096, offset=0x49204000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000,
                   prot=mmap.PROT_READ)
ACCENT, PANEL = 0xffd1b0fe, 0xff302a39
GAIN = [0, 260, 368, 519, 734, 1036, 1464, 2067, 2920, 4125, 5827, 8231, 11626, 16422, 23197, 32767]

def word(i): return struct.unpack_from("<I", card, 4 * i)[0]
def put(i, v): struct.pack_into("<I", card, 4 * i, v)
def fail(why):
    print("audio test failed:", why, [word(i) for i in range(12)], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=6.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check(): return
        time.sleep(0.01)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def tap_logical(x, y): tap(x * 3, y * 4)
def hold(bit, seconds):
    put(0, bit); time.sleep(seconds); put(0, 0); time.sleep(0.1)
def pixel(x, y): return struct.unpack_from("<I", screen, (y * 4 * 720 + x * 3) * 4)[0]
def bar(level):
    return all(pixel(14 + i * 14 + 5, 97) == (ACCENT if i < level else PANEL) for i in range(15))
def ring():
    size, end = word(7), word(5)
    base = 4096
    return [struct.unpack_from("<h", card, base + 4 * ((end - n) % size))[0] for n in range(4096, 0, -1)]

until("sound card ready", lambda: word(2) == 0x31445541, 20)
put(1, 0)
until("default volume", lambda: word(4) == 8)
time.sleep(1.5)  # Let Home finish starting services.

hold(1, 0.12); until("volume up", lambda: word(4) == 9)
hold(2, 0.12); hold(2, 0.12); until("volume down twice", lambda: word(4) == 7)
hold(3, 0.3); time.sleep(0.2)
if word(4) != 7: fail("both buttons together must not change volume")
hold(1, 2.4); until("held button repeats to the top", lambda: word(4) == 15)

# Home -> Apps -> Next (Wi-Fi Setup) -> Next (Sounds) -> Open.
tap_logical(190, 68); tap_logical(120, 110); tap_logical(120, 110); tap_logical(120, 164)
until("Sounds shows the volume bar", lambda: bar(15), 8)

tap_logical(60, 210)  # CHIME
until("chime starts", lambda: word(9) == 2 and word(8) == 1)
until("chime finishes", lambda: word(9) == 0 and word(3) & 1 == 0, 4)
frames = word(11)
if not 0.55 * 48000 <= frames <= 0.75 * 48000: fail(f"chime length {frames} frames")
if not 15000 <= word(10) <= 16384: fail(f"chime peak {word(10)} at full volume")

tap_logical(60, 138)  # "-" plays a click at the new level
until("on-screen minus", lambda: word(4) == 14 and bar(14))
until("click played", lambda: word(8) == 2 and word(9) == 0)

put(1, 1)  # Plug in headphones, then play a 440 Hz tone.
time.sleep(0.25)
tap_logical(60, 260)
until("tone routed to headphones", lambda: word(9) == 4 and word(3) == 3)
time.sleep(0.4)
samples = ring()
crossings = sum(1 for a, b in zip(samples, samples[1:]) if a < 0 <= b)
hz = crossings * 48000 / len(samples)
if not 420 <= hz <= 460: fail(f"tone measured at {hz:.0f} Hz")
expected = 16384 * GAIN[14] >> 15
if not 0.95 * expected <= max(samples) <= 1.02 * expected: fail(f"tone peak {max(samples)}, expected {expected}")
put(1, 0)
until("unplugging returns to the speaker", lambda: word(3) == 1)
tap_logical(170, 260)  # STOP
until("stop ends the tone early", lambda: word(9) == 0 and word(3) == 0, 2)
if word(11) >= 1.4 * 48000: fail("tone was not stopped early")
hold(2, 0.12); until("buttons work inside an app", lambda: word(4) == 13 and bar(13))
tap_logical(120, 368)  # HOME
time.sleep(0.5)
print("audio checks passed", file=sys.stderr)
'''

with TemporaryDirectory() as tmp:
    run.IMAGE = ROOT / "build/sel4/qemu"
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = run.ROOT / "build/sel4/qemu"
    # Use a throwaway disk so the test never changes the emulator's saved state.
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"audio test failed (viewer status {status})")
    print("Audio service verified: buttons, jack routing, playback, volume scaling")
