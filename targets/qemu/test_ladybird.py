"""The Ladybird browser (apps/open/ladybird) end to end in QEMU.

A small web server on the Mac stands in for two sites through the host relay
(VIOLET_NET_HOSTS): info.cern.ch, which the start page links to, and
typed.test. The test drives the app only through the touch screen and reads
its output from the guest's screen and the Open Runtime's log:

  1. Home opens Ladybird; the start page, parsed by Ladybird's HTML parser, draws.
  2. Tapping the start page's first link loads the "CERN" page over HTTP;
     its <style> sheet and table are applied (checked in the pixels).
  3. Tapping a relative link loads the second page from the right path.
  4. Back returns to the first page; dragging scrolls it.
  5. The address bar and on-screen keyboard load a typed URL.
  6. With the Mac online, example.com loads over HTTPS with a verified certificate.
  7. Home returns to the Home app.
"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import socket
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

FIRST = """<!doctype html>
<html><head><title>VioletOS Ladybird test</title>
<link rel=stylesheet href="style.css">
<style>
  body { background: #fffdf4; }
  .banner { background: #cc2266; color: white; padding: 12px; }
  table.grid td { border: 1px solid #ccc; }
  a.next { display: block; background: #00aa55; color: white; padding: 30px; text-decoration: none; }
  .tall { height: 40px; margin: 0 0 40px 0; }
</style></head>
<body>
<h1 class=banner>First page</h1>
<p>Parsed by <b>Ladybird</b> &amp; laid out on VioletOS.</p>
<p class=linked>Styled by a linked style sheet.</p>
<table class=grid><tr><td>1.</td><td>Narrow</td><td>A much wider cell with several words in it</td></tr></table>
<p><a class=next href="second.html?from=first">Go to the second page</a></p>
""" + "".join(f"<p class=tall>Paragraph {i} of the long page.</p>\n" for i in range(40)) + """
<p id=end style="background:#2255cc;color:white">The end of the first page.</p>
</body></html>
"""

SECOND = """<!doctype html><title>Second page</title>
<h1 style="background:#ee8800">Second page</h1><p>Reached by a relative link.</p>"""

LINKED_CSS = ".linked { background: #7744aa; color: white; }"

TYPED = """<!doctype html><title>Typed</title><h1>Typed address</h1>"""

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
APPS = json.loads(os.environ["OPEN_APPS"])
ONLINE = os.environ["OPEN_ONLINE"] == "1"

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
    print("ladybird test failed:", why, "\n--- log ---\n" + everything()[-4000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def touch(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
def release():
    os.write(1, bytes(6))
def row_y(index, count):
    h = 80 if count <= 13 else max(40, 1080 // count)
    return 260 + index * h + (h - 12) // 2
def tap(x, y, pause=.35):
    touch(x, y); time.sleep(.15); release(); time.sleep(pause)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0] & 0xffffff
def rows_with(color, y0=130, y1=1600, x=None):
    xs = range(0, 720, 8) if x is None else [x]
    return [y for y in range(y0, y1, 2) if any(pixel(px, y) == color for px in xs)]
def on_list():
    return pixel(60, 300) == 0x302a39 and pixel(5, 5) == 0x191620
def on_home():
    return pixel(5, 5) == 0x2a1740
def count(pattern):
    return len(re.findall(pattern, everything()))
def loaded(url, title):
    return re.search(r"ladybird: loaded " + re.escape(url) + r" status=200 .*title=" + re.escape(json.dumps(title)), everything())

KEYBOARD_TOP = 1600 - 5 * 96 - 16
ROWS = ["1234567890", "qwertyuiop", "asdfghjkl:", "zxcvbnm.-/"]
def key(k):
    special = {".com": 72, "_": 180, " ": 324, "back": 504, "go": 648}
    if k in special:
        tap(special[k], KEYBOARD_TOP + 8 + 4 * 96 + 48, .25)
        return
    row = next(i for i, r in enumerate(ROWS) if k in r)
    tap(ROWS[row].index(k) * 72 + 36, KEYBOARD_TOP + 8 + row * 96 + 48, .25)
def type_address(keys):
    tap(500, 63)
    until("address editing", lambda: "ladybird: editing address" in everything())
    for k in keys:
        key(k)
    key("go")

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
time.sleep(3)
if not on_home():
    tap(570, 276)  # the classic launcher's APPS button shows Home
    until("Home app", on_home, 90)
tap(360, 1141)  # Home's Programs tile
until("app list", on_list)
tap(360, row_y(APPS.index("ladybird"), len(APPS)))

# 1. The start page.
until("start page parsed", lambda: re.search(r"ladybird: loaded about:home status=200 elements=\d+ rules=7 links=\d+", everything()), 60)
print("start:", re.search(r"ladybird: starting.*", everything()).group(0), file=sys.stderr)
LINK = 0x1a55c4
until("start page links drawn", lambda: len(rows_with(LINK)) > 20, 20)
first_link = rows_with(LINK)[0]

# 2. A link to the first page, over HTTP.
tap(200, first_link + 12)
until("first page", lambda: loaded("http://info.cern.ch/hypertext/WWW/TheProject.html", "VioletOS Ladybird test"), 60)
until("first page drawn", lambda: len(rows_with(0xcc2266)) > 20 and len(rows_with(0x00aa55)) > 20, 20)
if not rows_with(0x7744aa):
    fail("the linked style sheet's background is not on screen")
if pixel(360, 1400) != 0xfffdf4:
    fail(f"page background is {pixel(360, 1400):06x}, not the style sheet's fffdf4")
print("first page:", re.findall(r"ladybird: loaded http://info.*", everything())[-1], file=sys.stderr)

# 3. A relative link.
green = rows_with(0x00aa55)
tap(360, green[len(green) // 2])
until("second page", lambda: loaded("http://info.cern.ch/hypertext/WWW/second.html?from=first", "Second page"), 60)
until("second page drawn", lambda: len(rows_with(0xee8800)) > 20, 20)

# 4. Back, then scroll by dragging.
before = count(r"ladybird: loaded http://info.cern.ch/hypertext/WWW/TheProject.html ")
tap(242, 63)
until("back to the first page", lambda: count(r"ladybird: loaded http://info.cern.ch/hypertext/WWW/TheProject.html ") > before, 60)
until("first page drawn again", lambda: len(rows_with(0xcc2266)) > 20, 20)
touch(360, 1400); time.sleep(.2)
for y in range(1360, 300, -60):
    touch(360, y); time.sleep(.05)
time.sleep(.2); release()
until("scrolled", lambda: re.search(r"ladybird: scrolled to (\d+)", everything()), 10)
scrolled = int(re.findall(r"ladybird: scrolled to (\d+)", everything())[-1])
if scrolled < 500:
    fail(f"dragging 1000 pixels scrolled only {scrolled}")
until("banner scrolled away", lambda: len(rows_with(0xcc2266)) == 0, 10)
print(f"scroll: dragged to {scrolled}", file=sys.stderr)

# 5. The address bar and keyboard.
type_address(list("http://typed.test/form"))
until("typed address", lambda: loaded("http://typed.test/form", "Typed"), 60)

# 6. HTTPS on the real web.
if ONLINE:
    type_address(list("example") + [".com"])
    until("example.com over HTTPS", lambda: loaded("https://example.com/", "Example Domain"), 90)
    print("https:", re.findall(r"ladybird: loaded https://example.com.*", everything())[-1], file=sys.stderr)
    # A large real page with linked style sheets. It depends on Wikipedia, so
    # it is reported, not required.
    type_address(list("en.m.wikipedia.org/wiki/html"))
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not re.search(r"ladybird: (loaded|failed) https://en(?:\.m)?\.wikipedia\.org/wiki/HTML", everything(), re.I):
        time.sleep(0.2)
    found = re.findall(r"ladybird: (?:loaded|failed) https://en(?:\.m)?\.wikipedia\.org/wiki/HTML.*", everything(), re.I)
    if found:
        print("wikipedia:", found[-1], file=sys.stderr)
    else:
        recent = [line for line in everything().splitlines() if "ladybird:" in line or "open" in line][-12:]
        print("wikipedia: no answer in 120 s; last log lines:\n  " + "\n  ".join(recent), file=sys.stderr)
else:
    print("https: skipped (the Mac is offline)", file=sys.stderr)

# 7. Home.
tap(100, 60)
until("back on Home", on_home, 10)
'''


class Site(BaseHTTPRequestHandler):
    pages = {
        "/hypertext/WWW/TheProject.html": FIRST,
        "/hypertext/WWW/second.html?from=first": SECOND,
        "/hypertext/WWW/style.css": LINKED_CSS,
        "/form": TYPED,
    }
    requests = []

    def do_GET(self):
        Site.requests.append((self.headers.get("Host"), self.path))
        body = self.pages.get(self.path)
        if body is None:
            self.send_response(404)
            body = "<title>Not found</title><h1>Not found</h1>"
        else:
            self.send_response(200)
        data = body.encode()
        kind = "text/css" if self.path.endswith(".css") else "text/html; charset=utf-8"
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


def online():
    try:
        socket.create_connection(("example.com", 443), timeout=5).close()
        return True
    except OSError:
        return False


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make qemu-image).")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Site)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    port = server.server_address[1]
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    folders = sorted(p for p in (ROOT / "apps/open").iterdir() if p.is_dir() and p.name != "home")
    apps = [p.name for p in folders if (p / "app.txt").exists()] + [p.name for p in folders if not (p / "app.txt").exists()]
    os.environ.update(
        OPEN_APPS=json.dumps(apps),
        OPEN_ONLINE="1" if online() else "0",
        VIOLET_NET_HOSTS=json.dumps({"info.cern.ch": f"127.0.0.1:{port}", "typed.test": f"127.0.0.1:{port}"}),
    )
    status = run.main(str(viewer))
    server.shutdown()
    if status != 0:
        raise SystemExit(f"ladybird test failed (viewer status {status})")
    expected = {("info.cern.ch", "/hypertext/WWW/TheProject.html"), ("info.cern.ch", "/hypertext/WWW/style.css"), ("info.cern.ch", "/hypertext/WWW/second.html?from=first"),
                ("typed.test", "/form")}
    missing = expected - set(Site.requests)
    if missing:
        raise SystemExit(f"the browser never requested {sorted(missing)}; it asked for {Site.requests}")
    print("Ladybird verified: start page, HTTP links, a linked style sheet, a relative link, back, scrolling, typed address"
          + (", HTTPS" if os.environ["OPEN_ONLINE"] == "1" else ""))
