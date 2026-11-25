"""Run unmodified Linux ARM programs under the Open Runtime in QEMU.

A scripted viewer taps each test program's icon on Home in turn (they live in
targets/qemu/open-test-apps and `make test-open` builds them into the image),
reading what it prints from the Open Runtime's log: Rust std (hello), C on musl
(chello), Go (gohello) and Python (pyhello). "canvas" (Rust) and "pypaint"
(Python) draw through /dev/violet/screen and answer taps; "canvas" also leaves
for Home through /dev/violet/sys and keeps its frame when reopened. "pytone"
streams PCM to /dev/violet/audio, checked at the emulated sound card.
Programs whose toolchain was not available when the image was built are
reported as skipped.
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
card = mmap.mmap(ram.fileno(), 9 * 4096, offset=0x49204000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
CHECKS = json.loads(os.environ["OPEN_CHECKS"])
APPS = json.loads(os.environ["OPEN_APPS"])
PRESENT = json.loads(os.environ["OPEN_PRESENT"])

def text():
    n = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:4096])
    if n <= len(data):
        return data[:n].decode(errors="replace")
    s = n % len(data)
    return (data[s:] + data[:s]).decode(errors="replace")
seen = ""
def everything():
    # The log is a 4 KiB ring; keep what scrolled past.
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("open test failed:", why, "\n--- log ---\n" + text(), file=sys.stderr)
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
def start(name):
    open_icon(name)
def console(name, timeout=60):
    n = home_shown()
    start(name)
    until(f"{name} finishes", lambda: f"{name} (pid" in everything() and "exited" in everything(), timeout)
    for line in CHECKS[name]:
        if line not in everything():
            fail(f"{name} did not print {line!r}")
    until(f"Home after {name}", lambda: home_shown() > n)  # a program that exits returns to Home
    print(f"{name}: passed", file=sys.stderr)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
until("Home", lambda: "home: ready" in everything(), 60)
time.sleep(3)  # Let Home finish starting services.
for name in ("hello", "chello", "gohello", "pyhello", "pynet", "gonet"):
    if name in PRESENT:
        console(name)

if "pytone" in PRESENT:
    n = home_shown()
    start("pytone")
    peak = frames = sound = 0
    deadline = time.monotonic() + 60
    while "pytone done" not in everything():
        if time.monotonic() > deadline:
            fail("pytone did not finish")
        peak, frames, sound = max(peak, card_word(10)), max(frames, card_word(11)), max(sound, card_word(9))
        time.sleep(0.01)
    # 12000 at volume 8 (gain 2920/32768) is about 1069.
    if sound != 6 or frames < 24000 or not 1000 <= peak <= 1100:
        fail(f"PCM stream: sound {sound}, frames {frames}, peak {peak}")
    until("Home after pytone", lambda: home_shown() > n)
    print("pytone: passed", file=sys.stderr)

if "pypaint" in PRESENT:
    start("pypaint")
    until("pypaint draws", lambda: "pypaint: ready" in everything(), 60)
    until("pypaint frame", lambda: pixel(10, 100) == 0xff6a3fd1 and pixel(10, 900) == 0xff15102a)
    tap(400, 1000)
    until("pypaint square", lambda: pixel(400, 1000) == 0xffffc14d)
    leave_to_home(lambda: tap(360, 60))
    print("pypaint: passed", file=sys.stderr)

start("canvas")
until("canvas draws", lambda: "canvas: drew gradient" in everything())
until("gradient on screen", lambda: pixel(10, 100) == 0xff2a7f62 and pixel(10, 1590) & 0xff0000 > 0xf00000)
tap(360, 800)
until("tap reaches canvas", lambda: "canvas: tap 1 at 360,800" in everything())
until("square drawn", lambda: pixel(360, 800) == 0xffffffff and pixel(360, 700) != 0xffffffff)
leave_to_home(lambda: tap(360, 60))  # canvas asks /dev/violet/sys to go Home
start("canvas")
until("canvas comes back with its frame", lambda: pixel(360, 800) == 0xffffffff)
print("canvas: passed", file=sys.stderr)
'''

CHECKS = {
    "hello": [
        "hello from rust std on violet",
        "sum of squares: 2870",
        "threads returned [0, 10, 20, 30], shared total 10",
        "file round trip: written by hello",
        "HOME=/data",
        "8 MiB allocation ok: true",
        "hello done",
        "exited with status 0",
    ],
    "chello": ["pthreads summed 15005000", "libm: 1.190020", "slept at least 20 ms: yes",
               "file round trip: written by c", "16 MiB malloc ok: 1", "c done"],
    "gohello": ["goroutines summed: 102018000", "gc cycles at least 1: true",
                "timer fired after at least 30ms: true ticks>0: true", "file round trip: written by go <nil>", "go done"],
    # TCP through the host relay to the test's web server, which stands in for example.com.
    "pynet": ["pynet: resolving example.com -> 198.18.", "pynet: HTTP 200, 84 bytes, title 'Violet test page'"],
    "gonet": ["gonet: example.com -> [198.18.", "gonet: http://example.com/ -> 200 OK, 84 bytes, title \"Violet test page\"",
              "gonet done"],
    "pyhello": ["threads: [49995000, 199990000, 449985000, 799980000]", "file round trip: written by python",
                "zlib: True violet", "slept at least 50 ms: True", "python done"],
}


def image_files(path):
    data, at, names = path.read_bytes(), 0, set()
    while data[at:at + 6] == b"070701":
        size, namesize = int(data[at + 54:at + 62], 16), int(data[at + 94:at + 102], 16)
        names.add(data[at + 110:at + 110 + namesize - 1].decode())
        at = (((at + 110 + namesize + 3) & ~3) + size + 3) & ~3
    return names


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make test-open builds it with the test programs).")
    viewer.write_text(home_grid.viewer(SCRIPT, (original / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    files = image_files(original / "initfs.cpio")
    python = "bin/python3" in files
    apps = [p.name for p in sorted((ROOT / "targets/qemu/open-test-apps").iterdir()) if p.is_dir()]
    present = [a for a in apps if f"apps/{a}" in files and (python or not a.startswith("py"))]
    for name in sorted(set(apps) - set(present)):
        print(f"skipping {name}: its toolchain was not available when the image was built", file=sys.stderr)
    if "hello" not in present or "canvas" not in present:
        raise SystemExit("The image lacks the Rust test programs; install the armv7-unknown-linux-musleabihf target.")
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    # A local web server stands in for example.com; the host relay sends connections there.
    import http.server, socketserver, threading
    page = b"<html><head><title>Violet test page</title></head><body>hello over tcp</body></html>"
    class Page(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)
        def log_message(self, *args):
            pass
    server = socketserver.TCPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    os.environ["VIOLET_NET_HOSTS"] = json.dumps({"example.com": f"127.0.0.1:{server.server_address[1]}"})
    os.environ.update(OPEN_CHECKS=json.dumps(CHECKS), OPEN_APPS=json.dumps(apps), OPEN_PRESENT=json.dumps(present))
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"open runtime test failed (viewer status {status})")
    print("Open Runtime verified:", ", ".join(present))
