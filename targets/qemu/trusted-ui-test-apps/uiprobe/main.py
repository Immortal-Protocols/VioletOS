#!/bin/python3
"""A hostile app for targets/qemu/test_trusted_ui.py: it paints its whole
surface red and presents it again and again (over the band, over prompts),
logs every event it is given, and tries the /dev/violet/sys commands an app
must not use. On the first boot (no passcode yet) it asks for the microphone
and Bluetooth and opens the camera: it has no manifest, so each is refused at
once with no prompt; on the second it posts a notification and locks the
phone when touched near the bottom."""
import mmap, os, struct, time

W, H = 720, 1600
RED = 0xffe01020
EVENT = struct.Struct("<QHHiii")
screen = os.open("/dev/violet/screen", os.O_RDWR)
events = os.open("/dev/violet/events", os.O_RDONLY | os.O_NONBLOCK)
fb = mmap.mmap(screen, W * H * 4)
fb[:] = struct.pack("<I", RED) * (W * H)


def present():
    os.write(screen, struct.pack("<6I", 0, 0, W, H, 0, 0))


def say(*words):
    print("uiprobe:", *words, flush=True)


def sys_cmd(line):
    fd = os.open("/dev/violet/sys", os.O_RDWR)
    try:
        os.write(fd, (line + "\n").encode())
        return os.read(fd, 4096).decode(errors="replace").strip()
    finally:
        os.close(fd)


def drain():
    """Log every event; the touch-ups it got (x, y)."""
    ups = []
    while True:
        try:
            raw = os.read(events, EVENT.size)
        except BlockingIOError:
            return ups
        if len(raw) < EVENT.size:
            return ups
        _, kind, _, x, y, value = EVENT.unpack(raw)
        if kind in (1, 2, 3):
            say("touch", kind, x, y)
            if kind == 3:
                ups.append((x, y))
        elif kind == 7:
            say("focus", value)


def ask(cap):
    """Ask for `cap` until answered, presenting all the while."""
    while True:
        answer = sys_cmd("consent " + cap)
        if answer != "pending":
            return answer
        drain()
        present()
        time.sleep(0.1)


present()
second = "set=1" in sys_cmd("passcode")
say("ready", "second" if second else "first")
# Commands an app may not give (C2-sys-injection).
say("bench", sys_cmd("bench press 360 800 1"))
say("set-relay", sys_cmd("set relay 10.0.0.1:1"))
say("notifications", sys_cmd("notifications"))
say("passcode-off", sys_cmd("passcode off"))
if not second:
    # Time for the test to look at the band over a red screen.
    end = time.monotonic() + 3
    while time.monotonic() < end:
        drain()
        present()
        time.sleep(0.1)
    # An app from the image with no manifest: policy answers at once, and
    # no prompt is raised (a user's answer would not be what decides).
    say("consent mic", ask("mic"))
    say("consent hci", ask("hci"))
    try:
        os.close(os.open("/dev/violet/camera", os.O_RDONLY))
        say("open camera ok")
    except OSError as e:
        say("open camera", e.errno)
else:
    say("notify", sys_cmd("notify secret code 4321"))
say("listening")
while True:
    for x, y in drain():
        if 600 <= y < 900:
            say("home", sys_cmd("home"))
        elif y >= 1450:
            say("lock", sys_cmd("lock"))
    present()
    time.sleep(0.2)
