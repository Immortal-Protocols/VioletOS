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
in /apps with an icon on Home and runs, and the pair reconnected without a new
scan. Settings › Agent Apps then deletes it, and the Mac unpairs and the phone
shows a new code.

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
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

APP = ROOT / "build/mac-agent/Build/Products/Debug/Violet Agent.app/Contents/MacOS/Violet Agent"
EXAMPLE = ROOT / "build/agent-workspace/target/armv7-unknown-linux-musleabihf/agent/example"
SUPPORT = Path.home() / "Library/Application Support/Violet Agent test"

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, subprocess, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
STAGE = os.environ["AGENT_STAGE"]
APP, EXAMPLE, SUPPORT = os.environ["AGENT_APP"], os.environ["AGENT_EXAMPLE"], os.environ["AGENT_SUPPORT"]

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
def launch_mac(*args):
    global mac
    env = dict(os.environ, VIOLET_AGENT_PROFILE="test", VIOLET_AGENT_RELAY_MAP="http://violet-relay.test=http://127.0.0.1:8787",
               VIOLET_AGENT_STUN="127.0.0.1:3478")
    mac = subprocess.Popen([APP, "--no-speech", *args], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

until("Home", lambda: "home: ready" in everything(), 60)
time.sleep(3)
open_icon("agent")
if STAGE == "first":
    until("pairing code", lambda: re.search(r"agent: pairing code (\S+)", everything()), 60)
    code = re.findall(r"agent: pairing code (\S+)", everything())[-1]
    launch_mac("--unpair", "--pair", code, "--install", "example=" + EXAMPLE, "--wait-direct")
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
    mac.terminate(); mac.wait()
    time.sleep(1)
    sys.exit(0)
# Second boot: the install survived, and the pair reconnects by itself.
until("installed apps loaded", lambda: re.search(r"installed apps: 1 loaded", everything()), 30)
launch_mac()
until("reconnected", lambda: mac_log().count("phone online") >= 2, 90)
print("agent: reconnected after the restart without a new scan", file=sys.stderr)
until("relay fallback", lambda: "agent: direct path not found" in everything(), 30)
print(re.findall(r"agent: direct path not found: .*", everything())[-1], file=sys.stderr)
leave_to_home()
time.sleep(1)
# An installed app gets an icon after the built-in ones.
open_icon("example")
until("the installed app runs", lambda: "stopwatch: starting at" in everything(), 60)
print("agent: the installed example runs from its Home icon after the restart", file=sys.stderr)
leave_to_home()
# Settings › Agent Apps deletes it (Delete, then Confirm).
open_icon("settings")
until("Settings", lambda: "settings: ready" in everything(), 60)
time.sleep(1)
tap(360, 250 + 4 * 124 + 54)
until("Agent Apps", lambda: "settings: AgentApps" in everything())
tap(582, 250 + 62)
tap(582, 250 + 62)
until("deleted", lambda: "settings: uninstall example -> ok" in everything(), 15)
print("agent: Settings deleted the installed example", file=sys.stderr)
tap(100, 60)
time.sleep(.5)
leave_to_home()
# Unpair from the Mac: the phone forgets it and shows a new code.
open_icon("agent")
time.sleep(3)
mac.terminate(); mac.wait()
launch_mac("--unpair-remote")
until("phone unpaired", lambda: "agent: unpaired" in everything(), 60)
mac.terminate(); mac.wait()
print("agent: unpairing from the Mac revoked the pair", file=sys.stderr)
'''


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
            viewer.write_text(home_grid.viewer(SCRIPT, cpio))
            viewer.chmod(0o755)
            # An installed app's icon follows the built-in apps.
            icons = json.loads(os.environ["HOME_ICONS"])
            names = home_grid.apps(cpio) + ["example"]
            icons["example"] = home_grid.icon(len(names) - 1, len(names))
            os.environ["HOME_ICONS"] = json.dumps(icons)
            image = Path(tmp) / "image"
            image.mkdir()
            for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
                (image / name).symlink_to(original / name)
            run.IMAGE = image
            os.environ.update(VIOLET_NET_HOSTS=json.dumps({"violet-relay.test": "127.0.0.1:8787", "violet-relay.test:3478": "127.0.0.1:3478"}),
                              AGENT_APP=str(APP), AGENT_EXAMPLE=str(EXAMPLE), AGENT_SUPPORT=str(SUPPORT))
            for stage in ("first", "second"):
                # The second boot's network blocks UDP: the pair falls back to the relay.
                os.environ.update(AGENT_STAGE=stage, VIOLET_NET_UDP="0" if stage == "second" else "1")
                status = run.main(str(viewer))
                if status != 0:
                    raise SystemExit(f"agent test failed in the {stage} boot (viewer status {status})")
    finally:
        relay.terminate()
    print("Agent verified: QR pairing through the relay, a direct UDP path carrying an encrypted install that survives a restart, "
          "the relay when UDP is blocked, a Home icon, deleting it from Settings, reconnect, unpair")


if __name__ == "__main__":
    main()
