"""Rose as Violet Agent's agent, end to end in QEMU with the real Mac app.

The phone in QEMU pairs with Violet Agent for macOS through a local relay, as
in test_agent.py, but with the agent on: Rose (companion/mac/roseos, run by
companion/mac/violet_rose/host.py) with her Codex conductor. Typed turns stand
in for speech. Rose is asked for a Python app: she starts a build task, which
Violet Agent's builder carries out (Codex writes the app, Python goes to the
phone, then the app), and she answers a question while it builds. When the
build is done she announces it, and the app is installed on the phone.

Needs Codex signed in with ChatGPT, Rose's environment
(companion/mac/violet_rose/setup.sh) and the image test_agent.py uses:
    make test-rose
"""
from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import shutil
import socket
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

APP = ROOT / "build/mac-agent/Build/Products/Debug/Violet Agent.app/Contents/MacOS/Violet Agent"
PROFILE = "test-rose"
SUPPORT = Path.home() / f"Library/Application Support/Violet Agent {PROFILE}"
ASK = "Make me a coin flip app in Python with one big button."
CHAT = "While that builds, tell me your name in one sentence."

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, subprocess, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
APP, SUPPORT, PROFILE = os.environ["AGENT_APP"], os.environ["AGENT_SUPPORT"], os.environ["AGENT_PROFILE"]
PORT, STUN = os.environ["AGENT_PORT"], os.environ["AGENT_STUN"]
ASK, CHAT = os.environ["ROSE_ASK"], os.environ["ROSE_CHAT"]

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
    print("rose test failed:", why, "\n--- guest log ---\n" + everything()[-2500:] + "\n--- mac log ---\n" + mac_log()[-3500:], file=sys.stderr)
    if mac: mac.kill()
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.2)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)
def pixel(x, y):
    return struct.unpack_from("<I", screen, (y * 720 + x) * 4)[0]

# The guest's log is a 4 KB ring: read it often so no line is lost between checks.
import threading
def drain():
    while True:
        everything(); time.sleep(0.1)
threading.Thread(target=drain, daemon=True).start()
until("Home", lambda: "home: ready" in everything(), 60)
time.sleep(3)
open_icon("agent")
until("pairing code", lambda: re.search(r"agent: pairing code (\S+)", everything()), 60)
code = re.findall(r"agent: pairing code (\S+)", everything())[-1]
env = dict(os.environ, VIOLET_AGENT_PROFILE=PROFILE, VIOLET_AGENT_RELAY_MAP=f"http://violet-relay.test=http://127.0.0.1:{PORT}",
           VIOLET_AGENT_STUN=f"127.0.0.1:{STUN}")
mac = subprocess.Popen([APP, "--unpair", "--pair", code, "--say", ASK, "--say", CHAT, "--say-gap", "12"],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
until("paired", lambda: "agent: paired with a new Mac" in everything(), 60)
until("Rose ready", lambda: "Rose ready" in mac_log(), 180)
print("rose: Rose is the agent (" + re.findall(r"Rose's dashboard: (\S+)", mac_log())[-1] + ")", file=sys.stderr)
until("the request reached Rose", lambda: "user: " + ASK in mac_log(), 60)
until("a build task", lambda: re.search(r"build (\d+): ", mac_log()), 90)
task = re.findall(r"build (\d+): ", mac_log())[0]
print(f"rose: she started build task {task}", file=sys.stderr)
until("the question during the build", lambda: "user: " + CHAT in mac_log(), 90)
def answered():
    mlog = mac_log()
    return "agent: " in mlog[mlog.index("user: " + CHAT):]
until("an answer", answered, 90)
mlog = mac_log()
answer_at = mlog.index("agent: ", mlog.index("user: " + CHAT))
during = f"build {task} done" not in mlog[:answer_at]
print("rose: she answered " + ("while the app was still building" if during else "(it finished playing after the build)"), file=sys.stderr)
until("the build finished", lambda: re.search(rf"build {task} (done|failed|cancelled)", mac_log()), 420)
if not re.search(rf"build {task} done", mac_log()):
    fail("the build did not succeed")
if not re.search(r"agent: install \S+ -> ok", everything()):
    fail("no app was installed on the phone")
apps = [n for n in re.findall(r"agent: install (\S+) -> ok", everything()) if n != "python"]
print(f"rose: {', '.join(apps)} installed on the phone", file=sys.stderr)
# Rose announces a finished task with its result, word for word, when nobody is talking; heard in full.
result = re.findall(rf"build {task} done in \d+ s: (.*)", mac_log())[0]
until("the announcement", lambda: "agent: " + result + "\n" in mac_log(), 90)
print("rose: she announced it: " + result[:120], file=sys.stderr)
for line in everything().splitlines():
    if line.startswith("agent:") and ("install" in line or "speech" in line or "stop" in line):
        print("guest " + line[:160], file=sys.stderr)
mac.terminate(); mac.wait()
'''


def main():
    if not APP.exists():
        raise SystemExit(f"Build the Mac app first: {APP}")
    original = ROOT / "build/sel4/qemu"
    cpio = (original / "initfs.cpio").read_bytes()
    if b"violet-relay.test" not in cpio:
        raise SystemExit("Build the image as make test-agent does (make test-rose does)")
    subprocess.run([sys.executable, str(ROOT / "tools/agent_build.py"), "setup"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([sys.executable, str(ROOT / "tools/build_python_package.py")], check=True)
    with socket.socket() as t, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as u:
        t.bind(("127.0.0.1", 0)); u.bind(("127.0.0.1", 0))
        port, stun = t.getsockname()[1], u.getsockname()[1]
    relay = subprocess.Popen([sys.executable, str(ROOT / "tools/chariot-relay/relay.py"), "--host", "127.0.0.1",
                              "--port", str(port), "--stun", str(stun)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    shutil.rmtree(SUPPORT, ignore_errors=True)
    try:
        with TemporaryDirectory() as tmp:
            viewer = Path(tmp) / "input.py"
            viewer.write_text(home_grid.viewer(SCRIPT, cpio))
            viewer.chmod(0o755)
            image = Path(tmp) / "image"
            image.mkdir()
            for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
                (image / name).symlink_to(original / name)
            run.IMAGE = image
            os.environ.update(VIOLET_NET_HOSTS=json.dumps({"violet-relay.test": f"127.0.0.1:{port}",
                                                          "violet-relay.test:3478": f"127.0.0.1:{stun}"}),
                              AGENT_APP=str(APP), AGENT_SUPPORT=str(SUPPORT), AGENT_PROFILE=PROFILE,
                              AGENT_PORT=str(port), AGENT_STUN=str(stun), ROSE_ASK=ASK, ROSE_CHAT=CHAT)
            if run.main(str(viewer)) != 0:
                raise SystemExit("rose test failed")
    finally:
        relay.terminate()
    print("Rose verified: Violet Agent's agent is Rose; a typed request became her build task, the app was built and "
          "installed on the phone, she talked during the build and announced the result")


if __name__ == "__main__":
    main()
