"""Check that one open app cannot take the network from another, in QEMU.

Local only (make test-net; not part of CI). Servers run on this Mac's loopback
and the emulator's relay (tools/net_relay.py) is pointed at them, so nothing
leaves the machine. "nethog" (targets/qemu/net-test-apps) resolves 300 names,
connects to an address whose name the resolver has dropped, opens sockets
until it is refused, and leaves a connect to a host that never answers
hanging; "netpeer" must then fetch a page at once:

- the resolver's table is bounded and an address with no name goes nowhere;
- one app holds at most its share of the socket slots (slots.rs: 9 of 32 with the emulator's eight apps);
- a connect that does not answer delays no other app's sockets.

Then "netserve" checks listening sockets and IPv6 (net2): servers on the
phone's loopback (http.server with urllib, asyncio, which needs socketpair),
a refused loopback port, AF_INET6 sockets on ::1, on ::ffff:127.0.0.1 and on
the dual-stack wildcard, only IPv4 answers for a name (AAAA has none, so an
IPv6-only lookup finds nothing), ENETUNREACH for a real IPv6 address, UDP on
loopback with the sender's port, and a TCP listener and a bound UDP socket on
the wildcard that this Mac reaches through the relay (VIOLET_NET_LISTEN).

The host tests in `make test-unit` cover the same rules without QEMU; this
checks them end to end through the image.
"""

from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import re
import socket
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import net_relay
import run

# How long the host that does not answer keeps its caller waiting.
HOLE_SECONDS = 40

SCRIPT = r'''#!/usr/bin/env python3
import mmap, os, re, socket, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)

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
    print("net test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=60.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def line(pattern):
    found = re.search(pattern, everything())
    return found.groups() if found else None

until("Home did not start", lambda: "home: ready" in everything(), 90)
time.sleep(3)
shown = home_shown()
open_icon("nethog")
until("nethog did not resolve its names", lambda: line(r"nethog: names=(\d+) inside=(\w+)"), 120)
names, inside = line(r"nethog: names=(\d+) inside=(\w+)")
if (names, inside) != ("300", "True"):
    fail(f"300 names should get 300 addresses in 198.18.0.0/15, got {names} (inside={inside})")
until("nethog did not try the stale address", lambda: line(r"nethog: stale=(\d+) unknown=(\d+) badname=(\w+)"))
stale, unknown, badname = line(r"nethog: stale=(\d+) unknown=(\d+) badname=(\w+)")
if (stale, unknown, badname) != ("113", "113", "refused"):
    fail(f"an address with no name must be EHOSTUNREACH (113) and a bad name refused: stale={stale} unknown={unknown} badname={badname}")
until("nethog did not fill its share of the slots", lambda: line(r"nethog: held=(\d+) errno=(\d+)"), 120)
held, error = line(r"nethog: held=(\d+) errno=(\d+)")
if (held, error) != (os.environ["NET_SHARE"], "24"):
    fail(f"one app should hold {os.environ['NET_SHARE']} sockets and then get EMFILE (24), got held={held} errno={error}")
until("nethog did not start the hanging connect", lambda: "nethog: hanging started=True" in everything())
until("nethog did not go to the background", lambda: home_shown() > shown)
open_icon("netpeer")
until("netpeer did not finish while nethog's connect was hanging",
      lambda: line(r"netpeer: fetched (\d+) bytes and opened 2 more sockets in (\d+) ms"), 30)
if "nethog: hole=" in everything():
    fail("the hanging connect ended before netpeer ran; the test proved nothing")
size, took = line(r"netpeer: fetched (\d+) bytes and opened 2 more sockets in (\d+) ms")
if size != os.environ["NET_PAGE_BYTES"]:
    fail(f"netpeer fetched {size} bytes")
if int(took) > 5000:
    fail(f"netpeer needed {took} ms beside a connect that does not answer")
until("nethog's connect never timed out", lambda: line(r"nethog: hole=(\d+) after=\d+s still_held=(\d+)"), 90)
hole, still = line(r"nethog: hole=(\d+) after=\d+s still_held=(\d+)")
if (hole, still) != ("110", str(int(os.environ["NET_SHARE"]) - 1)):
    fail(f"the hanging connect should end in ETIMEDOUT (110) with one socket fewer still held, got hole={hole} still_held={still}")
print(f"netpeer fetched its page in {took} ms beside {held} held sockets and a hanging connect", file=sys.stderr)

# Listening sockets and IPv6.
until("netpeer did not go to the background", lambda: home_shown() > shown + 1, 30)
open_icon("netserve")
want = {
    "http": "served-on-the-phone", "refused": "errno111", "v6": "::1,::1,six", "mapped": "127.0.0.1,mapped,::1",
    "dual": "::,::1,dual", "families": "AF_INET", "v6lookup": "none", "global6": "errno101", "pair": "pair",
    "asyncio": "ASYNC", "udp": "dgram,127.0.0.1,True",
}
for name, value in want.items():
    until(f"netserve did not report {name}", lambda: line(rf"netserve: {name}=(\S+)"), 60)
    got = line(rf"netserve: {name}=(\S+)")[0]
    if got != value:
        fail(f"netserve {name}: wanted {value}, got {got}")
until("netserve did not listen on the network", lambda: line(r"netserve: listening=(\d+)"), 30)
mac_port = int(os.environ["NET_LISTEN_PORT"])
reply = b""
for attempt in range(50):
    try:
        with socket.create_connection(("127.0.0.1", mac_port), timeout=10) as conn:
            conn.sendall(b"hello-from-the-mac")
            while chunk := conn.recv(64):
                reply += chunk
        break
    except OSError:
        time.sleep(0.2)
if reply != b"phone:hello-from-the-mac":
    fail(f"the phone's listener answered {reply!r}")
until("netserve did not report lan_tcp", lambda: line(r"netserve: lan_tcp=(\S+)"), 30)
if line(r"netserve: lan_tcp=(\S+)")[0] != "hello-from-the-mac":
    fail("the listener read something else")
until("netserve did not bind UDP", lambda: line(r"netserve: udp_bound=(\d+)"), 30)
mac_udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
mac_udp.settimeout(1)
answer = None
for attempt in range(20):
    mac_udp.sendto(b"datagram-from-the-mac", ("127.0.0.1", mac_port))
    try:
        answer = mac_udp.recv(64)
        break
    except OSError:
        pass
if answer != b"phone:datagram-from-the-mac":
    fail(f"the phone's bound UDP socket answered {answer!r}")
until("netserve did not finish", lambda: "netserve: done=1" in everything(), 30)
print("netserve: loopback and network listeners, socketpair, asyncio, IPv6 sockets, AAAA fallback", file=sys.stderr)
'''

PAGE = b"<html><title>Violet net test</title>" + b"x" * 4000 + b"</html>"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(PAGE)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(PAGE)

    def log_message(self, *args):
        pass


def sink():
    """Accepts connections and keeps them open, saying nothing."""
    server = socket.create_server(("127.0.0.1", 0), backlog=64)
    kept = []

    def accept():
        while True:
            try:
                kept.append(server.accept()[0])
            except OSError:
                return

    threading.Thread(target=accept, daemon=True).start()
    return server


web = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=web.serve_forever, daemon=True).start()
held = sink()
real_dial = net_relay.Relay.dial


def dial(address, timeout):
    # hole.test: a host that does not answer, for longer than the test needs.
    if address[1] == 9:
        time.sleep(HOLE_SECONDS)
        raise socket.timeout("timed out")
    return real_dial(address, timeout)


net_relay.Relay.dial = staticmethod(dial)
os.environ["VIOLET_NET_HOSTS"] = json.dumps({
    "peer.test": f"127.0.0.1:{web.server_address[1]}",
    "sink.test": f"127.0.0.1:{held.getsockname()[1]}",
    "hole.test": "127.0.0.1:9",
})
os.environ["NET_PAGE_BYTES"] = str(len(PAGE))


def share():
    """One app's share of the slots (slots.rs share()), from the sources."""
    slots = (ROOT / "runtime/open/slots.rs").read_text()
    const = lambda name: int(re.search(rf"pub const {name}: usize = (\d+);", slots).group(1))
    apps = max(int(n) for n in re.findall(r"pub const MAX_APPS: usize = (\d+);", (ROOT / "runtime/open/mod.rs").read_text()))
    spare = max(0, min(const("SLOTS"), 32) - const("RESERVED") - apps * const("APP_FLOOR"))
    return min(const("APP_FLOOR") + (spare + 1) // 2, const("APP_MAX"))


os.environ["NET_SHARE"] = str(share())
# The port the Mac reaches netserve's network listener (and UDP socket) on.
with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    free = probe.getsockname()[1]
os.environ["NET_LISTEN_PORT"] = str(free)
os.environ["VIOLET_NET_LISTEN"] = json.dumps({"8090": free})

with TemporaryDirectory(prefix="violet-net-") as directory:
    source = run.IMAGE
    cpio = (source / "initfs.cpio").read_bytes()
    listed = home_grid.apps(cpio)
    if not {"nethog", "netpeer", "netserve"} <= set(listed):
        raise SystemExit("The image lacks the net test apps; run make test-net, which builds them in.")
    image = Path(directory)
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(source / name)
    viewer = image / "viewer"
    viewer.write_text(home_grid.viewer(SCRIPT, cpio))
    viewer.chmod(0o755)
    run.IMAGE = image
    run.machine = partial(run.machine, image=image)
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"net test failed (viewer status {status})")
    print("Sockets verified: bounded resolver, per-app slot share, no head-of-line blocking, listening and IPv6")
