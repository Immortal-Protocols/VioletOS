"""The camera in QEMU: Home's Camera tile, the Camera service and the app.

QEMU's camera backend (targets/qemu/camera.rs) is a synthetic 640 x 480 BGGR
sensor: eight colour bars (white, yellow, cyan, green, magenta, red, blue,
black) and a white square that crosses them every four seconds. The app
develops each 2 x 2 block and turns the picture upright, so on screen the bars
become eight 80-pixel bands from y = 440 down, 480 pixels wide from x = 120.
The test opens Camera from Home, checks each band's colour, checks the square
moves, and goes back Home.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

SCRIPT = r'''#!/usr/bin/env python3
import mmap, os, re, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)

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
    print("camera test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
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
    v = struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
    return (v >> 16 & 255, v >> 8 & 255, v & 255)
def on_home():
    return pixel(5, 5) == (0x2a, 0x17, 0x40)
def band(k):
    # The mean colour of band k, away from its edges and from the square's path.
    pts = [pixel(x, 440 + k * 80 + 40) for x in range(130, 200, 7)]
    return tuple(sum(p[c] for p in pts) // len(pts) for c in range(3))

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
time.sleep(3)
if not on_home():
    tap(570, 276)  # the classic launcher's APPS button shows Home
until("Home app", on_home, 90)
tap(142, 1284)  # Home's Camera tile
until("camera frames", lambda: "camera: sensor 5153, 640 x 480" in everything(), 30)
until("the viewfinder", lambda: band(0)[1] > 120, 20)
time.sleep(3)  # auto-exposure and white balance settle
bands = [band(k) for k in range(8)]
print("bands:", bands, file=sys.stderr)
names = ["white", "yellow", "cyan", "green", "magenta", "red", "blue", "black"]
def bright(c): return c > 140
def dim(c): return c < 90
expect = {
    "white": lambda r, g, b: bright(r) and bright(g) and bright(b),
    "yellow": lambda r, g, b: bright(r) and bright(g) and dim(b),
    "cyan": lambda r, g, b: dim(r) and bright(g) and bright(b),
    "green": lambda r, g, b: dim(r) and bright(g) and dim(b),
    "magenta": lambda r, g, b: bright(r) and dim(g) and bright(b),
    "red": lambda r, g, b: bright(r) and dim(g) and dim(b),
    "blue": lambda r, g, b: dim(r) and dim(g) and bright(b),
    "black": lambda r, g, b: dim(r) and dim(g) and dim(b),
}
for name, colour in zip(names, bands):
    if not expect[name](*colour):
        fail(f"the {name} band shows {colour}")
# The square crosses the bands along the picture's middle column (x = 360):
# outside the white band, the rows it covers read white, and they move.
def square_rows():
    return [y for y in range(520, 1080, 8) if all(c > 150 for c in pixel(360, y))]
first = square_rows()
time.sleep(1.0)
second = square_rows()
if not first or not second or first == second:
    fail(f"the square did not move ({first[:3]} then {second[:3]})")
print("camera: bars in order, the square moves", file=sys.stderr)
tap(100, 60)  # Home
until("back on Home", on_home, 6)
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
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"camera test failed (viewer status {status})")
    print("Camera verified in QEMU: Home tile, colour bars in order, a moving picture")
