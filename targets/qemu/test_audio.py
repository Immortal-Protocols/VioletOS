"""Drive the emulated sound card: volume buttons, headphone jack, playback and
the microphone.

A scripted viewer takes the place of the SDL window. It holds the virtual volume
buttons, plugs in headphones, opens the Sounds test program (targets/qemu/open-test-apps/sounds) and
taps its controls, feeds a tone
into the microphone ring, then checks the guest's sound card registers, the PCM
it wrote and the volume bar on screen.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import math, mmap, os, struct, sys, threading, time
ram = open(sys.argv[1], "r+b")
card = mmap.mmap(ram.fileno(), 11 * 4096, offset=0x49204000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000,
                   prot=mmap.PROT_READ)
ACCENT, PANEL = 0xffb39af0, 0xff363431  # the open apps' colours (apps/lib/violet/src/paper.rs)
GAIN = [0, 260, 368, 519, 734, 1036, 1464, 2067, 2920, 4125, 5827, 8231, 11626, 16422, 23197, 32767]

log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
def open_log():
    n = struct.unpack_from("<I", log, 4)[0]
    return bytes(log[8:8 + min(n, 4088)]).decode(errors="replace")
def word(i): return struct.unpack_from("<I", card, 4 * i)[0]
def put(i, v): struct.pack_into("<I", card, 4 * i, v)
def fail(why):
    print("audio test failed:", why, [word(i) for i in range(17)], file=sys.stderr)
    print(open_log()[-1500:], hex(pixel(60, 300)), hex(pixel(5, 5)), file=sys.stderr)
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
def hold(bit, seconds):
    put(0, bit); time.sleep(seconds); put(0, 0); time.sleep(0.1)
def pixel(x, y): return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
def bar(level):
    # Sounds draws fifteen volume segments, (720 - 80) // 15 = 42 px apart from x=40, at y 410-450.
    return all(pixel(40 + 42 * i + 18, 430) == (ACCENT if i < level else PANEL) for i in range(15))
def ring():
    size, end = word(7), word(5)
    base = 4096
    return [struct.unpack_from("<h", card, base + 4 * ((end - n) % size))[0] for n in range(4096, 0, -1)]

def microphone():
    # A 1 kHz tone at -12 dBFS, delivered at 16 kHz in real time while the guest listens.
    start, sent = time.monotonic(), 0
    while True:
        due = int((time.monotonic() - start) * 16000)
        if word(12) & 1:
            written = word(13)
            for n in range(sent, due):
                struct.pack_into("<h", card, 9 * 4096 + 2 * (written % 4096),
                                 int(8192 * math.sin(2 * math.pi * 1000 * n / 16000)))
                written += 1
            put(13, written)
        sent = due
        time.sleep(0.004)
threading.Thread(target=microphone, daemon=True).start()
def mic_mode(): return word(15) & 0xff

until("sound card ready", lambda: word(2) == 0x31445541, 20)
if word(12) & 1: fail("the microphone must start off")
put(1, 0)
until("default volume", lambda: word(4) == 8)
time.sleep(1.5)  # Let Home finish starting services.

hold(1, 0.12); until("volume up", lambda: word(4) == 9)
hold(2, 0.12); hold(2, 0.12); until("volume down twice", lambda: word(4) == 7)
hold(3, 0.3); time.sleep(0.2)
if word(4) != 7: fail("both buttons together must not change volume")
hold(1, 2.4); until("held button repeats to the top", lambda: word(4) == 15)

# Home -> Sounds (a test program: make test-audio builds it into the image).
until("Home app", lambda: "home: ready" in open_log(), 90)
time.sleep(1)
open_icon("sounds")
until("Sounds shows the volume bar", lambda: bar(15), 60)

before = word(11)
tap(190, 780)  # Chime
# A slow runner can miss the 0.6 s chime mid-play (the tap alone takes 0.5 s), so a
# chime that already finished since the tap counts as started too.
until("chime starts", lambda: word(8) == 1 and (word(9) == 2 or (word(9) == 0 and word(11) not in (0, before))))
until("chime finishes", lambda: word(9) == 0 and word(3) & 1 == 0, 4)
frames = word(11)
if not 0.55 * 48000 <= frames <= 0.75 * 48000: fail(f"chime length {frames} frames")
if not 15000 <= word(10) <= 16384: fail(f"chime peak {word(10)} at full volume")

tap(190, 530)  # "-" plays a click at the new level
until("on-screen minus", lambda: word(4) == 14 and bar(14))
until("click played", lambda: word(8) == 2 and word(9) == 0)

put(1, 1)  # Plug in headphones, then play a 440 Hz tone.
time.sleep(0.25)
tap(190, 930)  # Tone
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
at_tap = word(11)
tap(530, 930)  # Stop
until("stop ends the tone early", lambda: word(9) == 0 and word(3) == 0, 2)
# The tone is 3 s (144000 frames). STOP must end it promptly after the tap and
# before it would have ended on its own, however long the steps before took.
if word(11) - at_tap >= 0.8 * 48000 or word(11) >= 144000 - 480:
    fail(f"tone was not stopped early: {at_tap} frames at the tap, {word(11)} when it stopped")
hold(2, 0.12); until("buttons work inside an app", lambda: word(4) == 13 and bar(13))

tap(190, 1210)  # Record
until("recording starts", lambda: mic_mode() == 2 and word(12) & 1)
until("the meter shows the tone", lambda: 75 <= word(16) <= 85, 2)
until("recording stops at four seconds", lambda: mic_mode() == 0 and word(15) >> 16 == 400, 7)
if word(12) & 1: fail("the microphone stayed on after recording")
if not word(15) & 0x100: fail("no recording reported")
tap(530, 1210)  # Play
until("the recording plays", lambda: word(9) == 5)
time.sleep(0.4)
samples = ring()
crossings = sum(1 for a, b in zip(samples, samples[1:]) if a < 0 <= b)
hz = crossings * 48000 / len(samples)
if not 980 <= hz <= 1020: fail(f"recording played back at {hz:.0f} Hz")
expected = 16384 * GAIN[13] >> 15
if not 0.85 * expected <= max(samples) <= 1.02 * expected:
    fail(f"recording peak {max(samples)}, expected about {expected}")
until("the recording finishes", lambda: word(9) == 0, 6)
tap(190, 1210)  # Record again, then leave the app while it records.
until("second recording starts", lambda: mic_mode() == 2)
tap(100, 60)  # Home
until("leaving the app turns the microphone off", lambda: mic_mode() == 0 and not word(12) & 1, 2)
time.sleep(0.5)
print("audio checks passed", file=sys.stderr)
'''

with TemporaryDirectory() as tmp:
    run.IMAGE = ROOT / "build/sel4/qemu"
    viewer = Path(tmp) / "input.py"
    original = run.ROOT / "build/sel4/qemu"
    viewer.write_text(home_grid.viewer(SCRIPT, (original / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    # Use a throwaway disk so the test never changes the emulator's saved state.
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"audio test failed (viewer status {status})")
    print("Audio service verified: buttons, jack routing, playback, volume scaling, microphone")
