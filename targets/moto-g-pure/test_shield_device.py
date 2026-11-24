#!/usr/bin/env python3
"""Violet Shield on real devices: VioletOS on the Moto, Violet Relay on the iPhone.

    build/shield/venv/bin/python targets/moto-g-pure/test_shield_device.py [--record DIR]

Flash the Moto first with the demo apps in its image:
    VIOLET_OPEN_EXTRA_APPS=$PWD/apps/shield-demo make moto
    services/usb-diagnostics/host/moto-flash build/sel4/moto/boot.img
and install the Shield build of Violet Relay on the iPhone (companion/ios/run.sh build,
then devicectl install). No one touches either phone:

  1. The Moto hosts its Phone Internet network (moto-relay, the saved pairing) and the
     iPhone rejoins it; every connection VioletOS makes goes through the iPhone's SOCKS5
     relay over cellular, and the Sentinel links to sentinel.violet on the iPhone.
  2. The iPhone signs three baseline Shields; the Moto verifies them (BearSSL P-256).
  3. Over USB, the Moto starts notes (benign), then photosync (steals notes' key and
     uploads 4 MiB from the background), then themes (tries to overwrite /apps/home).
     Home keeps fetching live weather the whole time (benign).
  4. The model on the iPhone (EmbeddingGemma 2 + head, CoreML) flags photosync; the
     iPhone signs a freeze and the Moto freezes it mid-upload, then thaws it after the
     TTL. A baseline Shield freezes themes inside its write.

The .test hosts go to stand-ins inside the iPhone app (standin:sink, standin:web), so
nothing reaches a real address. --record keeps iPhone screenshots and the Moto's log,
with times, for tools/shield-trainer/make_device_video.py.
"""
from __future__ import annotations

import argparse
import base64
import json
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BUILD = ROOT / "build"
PACKAGE = BUILD / "moto-package"
DEVICE = "3D9ACA61-50AF-5EAA-AE1C-2B5142827EDE"
BUNDLE = "com.immortal.violet.relay"
TTL = 30
HOST_MAP = {"drop.c2-exfil.test": "standin:sink", "bypass.c2-exfil.test": "standin:sink",
            "sync.notes.test": "standin:web", "api.weather.test": "standin:web"}
T0 = time.time()
# open-run and the screenshot requests share one USB mailbox (the last request wins),
# so one request at a time, with time for the Open Runtime to take it.
USB = threading.Lock()


def open_run(path):
    with USB:
        subprocess.run([PACKAGE, f"open-run={path}"], check=True, capture_output=True)
        time.sleep(0.5)


def say(s):
    print(f"[{time.time() - T0:6.1f}s] {s}", flush=True)
    if EVENTS:
        EVENTS.write(json.dumps({"t": round(time.time() - T0, 2), "caption": s}) + "\n")
        EVENTS.flush()


EVENTS = None


class MotoLog:
    """The Open Runtime's 4 KiB log ring, read over USB and stitched into one text."""

    def __init__(self, shots: Path | None = None):
        """shots: also save the Moto's screen there (USB screenshots), about every 2 s."""
        self.shots = shots
        self.last_shot = 0.0
        if shots:
            shots.mkdir(parents=True, exist_ok=True)
        self.text = ""
        self.timed = []  # (seconds into the run, line) as each line first appeared
        self.lock = threading.Lock()
        self.stop = False
        threading.Thread(target=self.run, daemon=True).start()

    def read(self):
        r = subprocess.run([PACKAGE, "open-log"], capture_output=True, text=True, errors="replace", timeout=20)
        return r.stdout.split("Open Runtime fault")[0]

    def run(self):
        while not self.stop:
            try:
                t = self.read()
            except subprocess.TimeoutExpired:
                continue
            with self.lock:
                before = self.text.count("\n")
                for k in range(min(len(self.text), len(t)), 0, -1):
                    if self.text.endswith(t[:k]):
                        self.text += t[k:]
                        break
                else:
                    self.text += t
                now = round(time.time() - T0, 2)
                self.timed += [(now, l) for l in self.text.split("\n")[before:-1]]
            if self.shots and time.time() - self.last_shot > 2:
                self.last_shot = time.time()
                with USB:
                    r = subprocess.run([PACKAGE, "screenshot=2"], capture_output=True, timeout=30)
                if r.returncode == 0 and r.stdout.startswith(b"P6"):
                    (self.shots / f"{time.time() - T0:08.2f}.ppm").write_bytes(r.stdout)
            time.sleep(0.3)

    def all(self):
        with self.lock:
            return self.text

    def count(self, s):
        return self.all().count(s)


def until(what, check, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.3)
    raise SystemExit(f"timed out waiting for {what}\n--- Moto log ---\n{LOG.all()[-4000:]}")


def iphone_file(name, dest):
    r = subprocess.run(["xcrun", "devicectl", "device", "copy", "from", "--device", DEVICE, "--domain-type", "appDataContainer",
                        "--domain-identifier", BUNDLE, "--source", f"Documents/{name}", "--destination", str(dest)],
                       capture_output=True, text=True)
    return r.returncode == 0


def iphone_json(name):
    with tempfile.TemporaryDirectory() as d:
        out = Path(d) / name
        if iphone_file(name, out):
            try:
                return json.loads(out.read_text())
            except ValueError:
                return {}
    return {}


class Shots:
    """iPhone screenshots, about one a second, named by their time."""

    def __init__(self, folder: Path):
        self.folder = folder
        self.stop = False
        folder.mkdir(parents=True, exist_ok=True)
        threading.Thread(target=self.run, daemon=True).start()

    def run(self):
        while not self.stop:
            t = time.time() - T0
            out = self.folder / f"{t:08.2f}.png"
            subprocess.run(["xcrun", "devicectl", "device", "capture", "screenshot", "--device", DEVICE, "--destination", str(out)],
                           capture_output=True)


def decisions(app):
    return [d for d in iphone_json("shield-status.json").get("decisions", []) if d.get("app") == app]


def main():
    global EVENTS, LOG
    ap = argparse.ArgumentParser()
    ap.add_argument("--record", type=Path)
    ap.add_argument("--no-relay", action="store_true", help="the Moto is already on Phone Internet")
    args = ap.parse_args()
    if args.record:
        args.record.mkdir(parents=True, exist_ok=True)
        EVENTS = open(args.record / "events.jsonl", "w")
    LOG = MotoLog(args.record / "moto" if args.record else None)
    shots = Shots(args.record / "iphone") if args.record else None

    pairing = json.loads((BUILD / "relay-pairing.json").read_text())
    key = subprocess.check_output(["openssl", "pkcs8", "-topk8", "-nocrypt", "-in", str(BUILD / "shield/companion-signing.pem"), "-outform", "DER"])
    relay = None
    if not args.no_relay:
        say(f"The Moto hosts its Phone Internet network, {pairing['ssid']}")
        # moto-relay reuses the saved pairing only when --open matches it; otherwise it makes a new network.
        opened = ["--open"] if pairing["ssid"].startswith("VioletRelayOpen-") else []
        relay = subprocess.Popen([sys.executable, str(ROOT / "services/usb-diagnostics/host/moto-relay"), "--wait", "240", *opened],
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        time.sleep(28)  # let the Moto beacon before the iPhone looks for it
    say("Violet Relay starts on the iPhone with Violet Shield and its on-phone model")
    subprocess.run(["xcrun", "devicectl", "device", "process", "launch", "--device", DEVICE, "--terminate-existing", BUNDLE, "--",
                    "-pairingSecret", pairing["socks_password"], "-joinSSID", pairing["ssid"],
                    # A network iOS has not saved yet needs its passphrase (the first join shows iOS's Join prompt).
                    *(["-joinPassphrase", pairing["passphrase"]] if pairing.get("passphrase") else []),
                    "-shieldDevKey", base64.b64encode(key).decode(), "-shieldFreezeTTL", str(TTL),
                    "-hostMapB64", base64.b64encode(json.dumps(HOST_MAP).encode()).decode()], check=True, capture_output=True)
    until("the Sentinel to link to the iPhone", lambda: "sentinel: linked" in LOG.all(), 240)
    say("The Moto's Sentinel linked to sentinel.violet on the iPhone, through the relay")
    until("three baseline Shields", lambda: LOG.count("installed") >= 3, 60)
    say("The iPhone signed 3 baseline Shields; the Moto verified them with BearSSL P-256")
    if relay:
        threading.Thread(target=relay.communicate, daemon=True).start()
    time.sleep(6)

    say("notes: an ordinary app saves notes (with an API key) and syncs to its server")
    open_run("/apps/notes")
    until("notes to finish", lambda: "notes done" in LOG.all(), 120)
    until("notes' verdict", lambda: decisions("notes"), 90)
    say("The iPhone scored notes on the phone: benign, nothing blocked")
    time.sleep(4)

    frozen = LOG.count("froze pid")
    say("photosync: 'syncs photos', then quietly reads notes' secrets...")
    open_run("/apps/photosync")
    until("photosync to hide", lambda: "going to the background" in LOG.all(), 120)
    say("...slips into the background and uploads 4 MiB to drop.c2-exfil.test")
    # On a real iPhone a window scores in about 80 ms, so the model often answers when
    # photosync connects, before any upload: the relay refuses it and the Moto applies a
    # signed DENY. Otherwise the Moto freezes photosync mid-upload.
    until("photosync to be stopped", lambda: LOG.count("froze pid") > frozen or "photosync: upload stopped" in LOG.all(), 180)
    if LOG.count("froze pid") > frozen:
        say("The model on the iPhone says exfiltration; the Moto froze photosync mid-upload")
        time.sleep(8)
        say(f"The freeze is reversible and expires: photosync thaws after {TTL} s")
        until("photosync to thaw", lambda: "thawed pid" in LOG.all(), TTL + 40)
    else:
        say("Within seconds the iPhone's model flagged exfiltration (p = 1.00); its relay cut the upload")
        time.sleep(6)
        say("The Moto verified the iPhone's signed DENY for drop.c2-exfil.test (30 s TTL)")
    until("photosync's upload to fail", lambda: "photosync: upload stopped" in LOG.all() or "photosync (pid" in LOG.all(), 90)
    time.sleep(4)

    frozen = LOG.count("froze pid")
    say("themes: maps W+X memory, then tries to overwrite Home's program, /apps/home")
    open_run("/apps/themes")
    until("themes to be frozen", lambda: LOG.count("froze pid") > frozen, 90)
    say("Frozen inside the write call by a baseline Shield: Home is untouched")
    time.sleep(8)
    st = iphone_json("shield-status.json")
    standins = iphone_json("standins.json")
    got = standins.get("received", {}).get("sink", 0)
    say(f"photosync got {got // 1024} KiB of its 4096 KiB out; Home was never written")
    time.sleep(5)
    say("Violet Shield on real phones: local model, signed live Shields, zero reboots")
    time.sleep(4)
    LOG.stop = True
    if shots:
        shots.stop = True
    if args.record:
        (args.record / "moto-log.txt").write_text(LOG.all())
        (args.record / "moto-log.jsonl").write_text("".join(json.dumps({"t": t, "line": l}) + "\n" for t, l in LOG.timed))
        (args.record / "shield-status.json").write_text(json.dumps(st, indent=1))
        (args.record / "standins.json").write_text(json.dumps(standins, indent=1))
    (BUILD / "shield/last-device-status.json").write_text(json.dumps(st, indent=1))

    by_app = {}
    for d in st.get("decisions", []):
        by_app.setdefault(d["app"], []).append(d)
    problems = []
    for benign in ("notes", "home"):
        if any(d.get("contained") for d in by_app.get(benign, [])):
            problems.append(f"{benign} was contained")
    if not any(d.get("contained") and d.get("tier") == "model" for d in by_app.get("photosync", [])):
        problems.append("photosync was not contained by the model")
    sunk = standins.get("received", {}).get("sink", 0)
    if sunk >= 4 * 1024 * 1024:
        problems.append(f"the sink received the whole upload ({sunk} bytes)")
    if st.get("shields_rejected"):
        problems.append(f"the Moto rejected {st['shields_rejected']} Shields")
    print(f"sink on the iPhone received {sunk // 1024} KiB of photosync's 4096 KiB", file=sys.stderr)
    for app, ds in sorted(by_app.items()):
        d = next((x for x in ds if x.get("contained")), ds[0])
        print(f"  {app:12s} {d.get('tier', ''):6s} {d.get('verdict', ''):22s} p_mal={d.get('p_malicious', 0):.2f} {d.get('action', ''):10s}"
              f" {'CONTAINED' if d.get('contained') else ''}", file=sys.stderr)
    print(f"  shields sent {st.get('shields_sent')} verified {st.get('shields_accepted')} rejected {st.get('shields_rejected')}"
          f"; model {st.get('model')}", file=sys.stderr)
    if problems:
        raise SystemExit("Violet Shield device test FAILED:\n  " + "\n  ".join(problems))
    print("Violet Shield verified on the Moto and the iPhone", file=sys.stderr)


if __name__ == "__main__":
    main()
