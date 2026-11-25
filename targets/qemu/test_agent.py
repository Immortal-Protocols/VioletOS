"""The voice agent's pairing, direct path and durable installs, in QEMU with
the real Mac app.

A local relay (tools/chariot-relay/relay.py, with its STUN responder) stands
in for the Chariot; the guest reaches it as violet-relay.test through the host
relay. First boot: the agent shows its pairing code; Violet Agent for macOS
(built in build/mac-agent, run with a separate test identity and no speech)
pairs with it, moves to the pair's room, the two open a direct UDP path, and
the Mac installs a known-good program (the agent workspace's example, a
Stopwatch) over it. Second boot on the same disk, on a "network" that blocks
UDP (the host relay refuses UDP sockets), with the Mac app restarted: the
direct path fails, the pair carries on through the relay, the program is still
in /apps and in Programs and runs, and the pair reconnected without a new scan.
Then the Mac unpairs and the phone shows a new code.

The image leaves Python out, as the handset's does: the Mac also installs a
Python app (the workspace's Tally), sending the Python package first. With the
agent sent to the background, its microphone stream keeps reaching the Mac. On
the second boot nobody opens the agent: it starts behind Home by itself and
reconnects, and the Python app runs from the installed package.

Needs the image built with the test relay:
    VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=violet-relay.test:3478 make qemu-image
and the Mac app (xcodebuild, see companion/mac/README.md).
"""
from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import shutil
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import run

APP = ROOT / "build/mac-agent/Build/Products/Debug/Violet Agent.app/Contents/MacOS/Violet Agent"
EXAMPLE = ROOT / "build/agent-workspace/target/armv7-unknown-linux-musleabihf/agent/example"
TALLY = ROOT / "build/agent-workspace/target/python/tally"
SUPPORT = Path.home() / "Library/Application Support/Violet Agent test"

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, subprocess, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
# The emulated sound card (as in test_audio.py): word 12 bit 0 is set while the microphone runs.
card = mmap.mmap(ram.fileno(), 11 * 4096, offset=0x49204000 - 0x40000000, prot=mmap.PROT_READ)
def mic_on(): return struct.unpack_from("<I", card, 48)[0] & 1
STAGE = os.environ["AGENT_STAGE"]
APP, EXAMPLE, SUPPORT = os.environ["AGENT_APP"], os.environ["AGENT_EXAMPLE"], os.environ["AGENT_SUPPORT"]
TALLY = os.environ["AGENT_TALLY"]

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
def mac_log():
    try:
        return open(os.path.join(SUPPORT, "agent.log")).read()
    except OSError:
        return ""
mac = None
def fail(why):
    print("agent test failed:", why, "\n--- guest log ---\n" + everything()[-3000:] + "\n--- mac log ---\n" + mac_log()[-2000:], file=sys.stderr)
    if mac: mac.kill()
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.1)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]
def on_list():
    return pixel(60, 300) == 0xff302a39 and pixel(5, 5) == 0xff191620
def on_home():
    return pixel(5, 5) == 0xff2a1740
def open_list():
    if on_list():
        return
    if not on_home():
        tap(570, 276)
        until("Home app", on_home, 90)
    tap(360, 1141)  # Programs
    until("app list", on_list)
def start(row):
    h = 80 if len(rows) <= 13 else max(40, 1080 // len(rows))
    tap(360, 260 + row * h + (h - 12) // 2)
def launch_mac(*args):
    global mac
    env = dict(os.environ, VIOLET_AGENT_PROFILE="test", VIOLET_AGENT_RELAY_MAP="http://violet-relay.test=http://127.0.0.1:8787",
               VIOLET_AGENT_STUN="127.0.0.1:3478")
    mac = subprocess.Popen([APP, "--no-speech", *args], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
time.sleep(3)
rows = json.loads(os.environ["AGENT_ROWS"])
if STAGE == "first":
    open_list()
    start(rows.index("agent"))
    until("pairing code", lambda: re.search(r"agent: pairing code (\S+)", everything()), 60)
    code = re.findall(r"agent: pairing code (\S+)", everything())[-1]
    launch_mac("--unpair", "--pair", code, "--install", "example=" + EXAMPLE, "--install", "tally=" + TALLY, "--wait-direct")
    until("paired", lambda: "agent: paired with a new Mac" in everything(), 60)
    print("agent: paired through the relay", file=sys.stderr)
    until("direct path", lambda: "agent: direct path open to" in everything(), 60)
    print(re.findall(r"agent: direct path open to [^;\s]+", everything())[-1], file=sys.stderr)
    until("installed", lambda: re.search(r"agent: install example -> ok", everything()), 120)
    print(re.findall(r"agent: install example: .* arrived .*", everything())[-1], file=sys.stderr)
    print(re.findall(r"agent: install example -> .*", everything())[-1], file=sys.stderr)
    until("Mac sees the install", lambda: "test install example: ok" in mac_log(), 20)
    if "(via direct path" not in mac_log():
        fail("the install did not cross the direct path")
    print("agent: the program crossed the direct path", file=sys.stderr)
    # The Python app: Python goes first, as a package, then the script.
    until("python package", lambda: "agent: install python -> ok" in everything(), 180)
    print(re.findall(r"agent: install python: .* arrived .*", everything())[-1], file=sys.stderr)
    until("python app", lambda: "test install tally: ok" in mac_log(), 60)
    print("agent: Python installed as a package, then the Python app", file=sys.stderr)
    # In the background the agent keeps listening: the stream reaches the Mac.
    tap(100, 60)
    until("agent in the background", lambda: "the agent keeps listening in the background" in everything(), 10)
    until("back on Home", on_home, 10)
    time.sleep(3)
    if not mic_on():
        fail("the microphone stopped when the agent went to the background")
    print("agent: the microphone stayed on with the agent behind Home", file=sys.stderr)
    mac.terminate(); mac.wait()
    time.sleep(1)
    sys.exit(0)
# Second boot: the installs survived, and the agent starts behind Home by itself.
until("installed apps loaded", lambda: re.search(r"installed apps: 3 loaded", everything()), 30)
if "installed apps: package python" not in everything():
    fail("the Python package was not unpacked")
# Only Home is opened (APPS on the classic launcher); the agent must start by itself.
if not on_home():
    tap(570, 276)
    until("Home app", on_home, 90)
until("agent started in the background", lambda: "started the agent in the background" in everything(), 30)
launch_mac()
until("reconnected", lambda: mac_log().count("phone online") >= 2, 90)
print("agent: reconnected after the restart without a new scan", file=sys.stderr)
until("relay fallback", lambda: "agent: direct path not found" in everything(), 30)
print(re.findall(r"agent: direct path not found: .*", everything())[-1], file=sys.stderr)
tap(100, 60)  # Home
time.sleep(2)
open_list()
start(rows.index("example") if "example" in rows else len(rows))
until("the installed app runs", lambda: "stopwatch: starting at" in everything(), 60)
print("agent: the installed example runs from Programs after the restart", file=sys.stderr)
tap(100, 60)  # the app's Home button
until("back on Home", on_home, 10)
open_list()
start(rows.index("tally"))
time.sleep(8)
if "Traceback" in everything():
    fail("the Python app failed")
if pixel(120, 1000) & 0xffffff != 0xe68cc8:
    fail(f"the Python app's +1 button is not on screen ({pixel(120, 1000):#x})")
print("agent: the Python app runs from the installed Python after the restart", file=sys.stderr)
# Unpair from the Mac: the phone forgets it and shows a new code.
tap(100, 60)  # the app's Home button
until("back on Home", on_home, 10)
open_list()
start(rows.index("agent"))
time.sleep(3)
mac.terminate(); mac.wait()
launch_mac("--unpair-remote")
until("phone unpaired", lambda: "agent: unpaired" in everything(), 60)
mac.terminate(); mac.wait()
print("agent: unpairing from the Mac revoked the pair", file=sys.stderr)
'''


def listed_apps(cpio):
    """The shell's list: apps/.home's titled apps in order, then the rest sorted (as in test_open_apps)."""
    names, at = set(), 0
    while cpio[at:at + 6] == b"070701":
        size, namesize = int(cpio[at + 54:at + 62], 16), int(cpio[at + 94:at + 102], 16)
        name = cpio[at + 110:at + 110 + namesize - 1].decode()
        data = (at + 110 + namesize + 3) & ~3
        if name == "TRAILER!!!":
            break
        if name.startswith("apps/") and name.count("/") >= 1:
            names.add(name.split("/")[1])
        at = (data + size + 3) & ~3
    home = home_titles(cpio)
    return home + sorted(n for n in names if n not in home and not n.startswith(".") and n != "home")


def home_titles(cpio):
    at = 0
    while cpio[at:at + 6] == b"070701":
        size, namesize = int(cpio[at + 54:at + 62], 16), int(cpio[at + 94:at + 102], 16)
        name = cpio[at + 110:at + 110 + namesize - 1].decode()
        data = (at + 110 + namesize + 3) & ~3
        if name == "apps/.home":
            return [l.split("\t")[0] for l in cpio[data:data + size].decode().splitlines() if "\t" in l]
        at = (data + size + 3) & ~3
    return []


def free(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        return False
    except OSError:
        return True


def main():
    if not APP.exists():
        raise SystemExit(f"Build the Mac app first: {APP}")
    if not EXAMPLE.exists():
        subprocess.run([sys.executable, str(ROOT / "tools/agent_build.py"), "setup"], check=True)
    subprocess.run([str(ROOT / "build/agent-workspace/build"), "tally"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([sys.executable, str(ROOT / "tools/build_python_package.py")], check=True)
    if b"bin/python3\0" in (ROOT / "build/sel4/qemu/initfs.cpio").read_bytes():
        raise SystemExit("Build the image without Python: VIOLET_OPEN_NO_PYTHON=1 (make test-agent does)")
    original = ROOT / "build/sel4/qemu"
    cpio = (original / "initfs.cpio").read_bytes()
    if b"violet-relay.test" not in cpio or b"violet-relay.test:3478" not in cpio:
        raise SystemExit("Build the image with the test relay: VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=violet-relay.test:3478 make qemu-image")
    if not free(8787):
        raise SystemExit("Port 8787 is busy")
    relay = subprocess.Popen([sys.executable, str(ROOT / "tools/chariot-relay/relay.py"), "--host", "127.0.0.1", "--port", "8787", "--stun", "3478"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    shutil.rmtree(SUPPORT, ignore_errors=True)
    try:
        with TemporaryDirectory() as tmp:
            viewer = Path(tmp) / "input.py"
            viewer.write_text(SCRIPT)
            viewer.chmod(0o755)
            image = Path(tmp) / "image"
            image.mkdir()
            for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
                (image / name).symlink_to(original / name)
            run.IMAGE = image
            rows = listed_apps(cpio)
            # Installed apps are titled, so they follow the image's titled apps.
            titled = home_titles(cpio)
            rows_after = [r for r in rows if r in titled] + ["example", "tally"] + [r for r in rows if r not in titled]
            os.environ.update(VIOLET_NET_HOSTS=json.dumps({"violet-relay.test": "127.0.0.1:8787", "violet-relay.test:3478": "127.0.0.1:3478"}),
                              AGENT_APP=str(APP), AGENT_EXAMPLE=str(EXAMPLE), AGENT_SUPPORT=str(SUPPORT), AGENT_TALLY=str(TALLY))
            for stage, listed in (("first", rows), ("second", rows_after)):
                # The second boot's network blocks UDP: the pair falls back to the relay.
                os.environ.update(AGENT_STAGE=stage, AGENT_ROWS=json.dumps(listed), VIOLET_NET_UDP="0" if stage == "second" else "1")
                status = run.main(str(viewer))
                if status != 0:
                    raise SystemExit(f"agent test failed in the {stage} boot (viewer status {status})")
    finally:
        relay.terminate()
    print("Agent verified: QR pairing through the relay, a direct UDP path carrying an encrypted install that survives a restart, "
          "Python installed as a package with a Python app, listening from the background, starting at boot, "
          "the relay when UDP is blocked, reconnect, unpair")


if __name__ == "__main__":
    main()
