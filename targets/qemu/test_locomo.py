"""Play a freshly generated Cool Jazz track in QEMU through the Mac companion.

Use the test relay build described in companion/mac/README.md. Pass the path to
the signed Locomo 0.1.14 .app. Locomo should be open on Cool Jazz with generation
ready. The test waits for a new track and checks the emulated sound device.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import home_grid
import run


VIEWER = r'''#!/usr/bin/env python3
import mmap, os, re, struct, subprocess, sys, time
from pathlib import Path

ram = open(sys.argv[1], "rb")
log = mmap.mmap(ram.fileno(), 4096, access=mmap.ACCESS_READ, offset=0x09220000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, access=mmap.ACCESS_READ, offset=0x39450000)
card = mmap.mmap(ram.fileno(), 4096, access=mmap.ACCESS_READ, offset=0x09204000)
support = Path.home() / ("Library/Application Support/Violet Agent " + os.environ["VIOLET_AGENT_PROFILE"])

def everything():
    n = struct.unpack_from("<I", log, 4)[0]
    data = log[8:4096]
    if n < len(data):
        return data[:n].decode(errors="replace")
    end = n % len(data)
    return (data[end:] + data[:end]).decode(errors="replace")

def mac_log():
    path = support / "agent.log"
    return path.read_text(errors="replace") if path.exists() else ""

def fail(reason):
    guest = re.sub(r"(agent: pairing code )\S+", r"\1[redacted]", everything())
    raise RuntimeError(f"{reason}\nGuest log:\n{guest}\nMac log:\n{mac_log()[-3000:]}")

def until(reason, check, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    fail(reason)

def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(0.15)
    os.write(1, bytes(6))
    time.sleep(0.35)

until("Home did not open", lambda: "home: ready" in everything(), 60)
time.sleep(2)
open_icon("agent")
code = until("No pairing code", lambda: re.search(r"agent: pairing code (\S+)", everything()), 30)
mac = subprocess.Popen([os.environ["VIOLET_TEST_MAC"], "--no-speech", "--pair", code[1],
                        "--locomo-app", os.environ["VIOLET_TEST_LOCOMO"]],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    until("Mac did not pair", lambda: "phone online" in mac_log(), 60)
    leave_to_home()
    open_icon("music")
    until("Music did not open", lambda: "music: starting" in everything())
    time.sleep(1)
    tap(580, 1528)
    until("Library did not draw", lambda: struct.unpack_from("<I", screen, (1500 * 720 + 580) * 4)[0] == 0xff191919)
    until("Mac library page did not arrive", lambda: "music: Mac library received" in everything())
    tap(580, 215)
    until("Cloud Songs filter did not select", lambda: struct.unpack_from("<I", screen, (187 * 720 + 580) * 4)[0] == 0xff191919)
    tap(140, 215)
    until("Mac library did not reload", lambda: everything().count("music: Mac library received") >= 2)
    tap(350, 455)
    until("Selected Mac library song did not transfer", lambda: "agent: Locomo track verified" in everything(), 60)
    tap(360, 1528)
    until("Generate did not draw", lambda: struct.unpack_from("<I", screen, (1500 * 720 + 360) * 4)[0] == 0xff191919)
    def generated_playing():
        title = re.findall(r"locomo: delivering (.+) \(\d+ MP3 bytes\)", mac_log())
        if len(title) < 2:
            return None
        return re.search(r"music: playing \d+ (" + re.escape(title[-1]) + r") \(0 BPM, Cool Jazz\)", everything())
    tap(360, 660)  # Open the song-length picker.
    tap(360, 455)  # Keep the desktop preference.
    tap(360, 1130)
    until("Mac did not receive the station request", lambda: "locomo: requested Cool Jazz song" in mac_log())
    print("QEMU requested a Cool Jazz track. Waiting for Locomo Music.", file=sys.stderr, flush=True)
    until("No generated track arrived", lambda: everything().count("agent: Locomo track verified") >= 2 or generated_playing(), 920)
    playing = until("The received track did not start", generated_playing)
    until("QEMU produced no audible PCM", lambda: struct.unpack_from("<I", card, 40)[0] > 100
          and struct.unpack_from("<I", card, 44)[0] >= 48_000, 30)
    print(f"QEMU played verified Locomo audio: {playing[1]}", file=sys.stderr, flush=True)
finally:
    mac.terminate()
    mac.wait(timeout=10)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("locomo", type=Path)
    args = parser.parse_args()
    binary = ROOT / "build/mac-agent/Build/Products/Debug/Violet Agent.app/Contents/MacOS/Violet Agent"
    if not binary.is_file() or not args.locomo.is_dir():
        parser.error("Build the Debug Mac companion and pass the Locomo app path.")
    original = run.IMAGE
    cpio = (original / "initfs.cpio").read_bytes()
    if b"http://violet-relay.test" not in cpio:
        parser.error("Build the QEMU image using VIOLET_AGENT_RELAY=http://violet-relay.test.")
    with TemporaryDirectory(prefix="violet-locomo-") as directory:
        work = Path(directory)
        viewer = work / "viewer"
        viewer.write_text(home_grid.viewer(VIEWER, cpio))
        viewer.chmod(0o700)
        image = work / "image"
        image.mkdir()
        for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
            (image / name).symlink_to(original / name)
        run.IMAGE = image
        relay = subprocess.Popen(
            [sys.executable, str(ROOT / "tools/chariot-relay/relay.py"), "--host", "127.0.0.1", "--port", "0"],
            stdout=subprocess.PIPE, text=True,
        )
        try:
            port = int(relay.stdout.readline().strip().rsplit(":", 1)[-1])
            os.environ.update(
                VIOLET_NET_HOSTS=json.dumps({"violet-relay.test": f"127.0.0.1:{port}"}),
                VIOLET_AGENT_RELAY_MAP=f"http://violet-relay.test=http://127.0.0.1:{port}",
                VIOLET_AGENT_PROFILE=work.name,
                VIOLET_AGENT_STUN="off",
                VIOLET_TEST_MAC=str(binary),
                VIOLET_TEST_LOCOMO=str(args.locomo.resolve()),
            )
            return run.main(str(viewer))
        finally:
            relay.terminate()
            relay.wait(timeout=10)


if __name__ == "__main__":
    sys.exit(main())
