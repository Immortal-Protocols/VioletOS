"""Capture one complete, read-only Ellis board report from the Linux USB gadget."""
from pathlib import Path
import argparse
import os
import select
import termios
import time
import tty

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("port", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
fd = os.open(args.port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
settings = termios.tcgetattr(fd)
start, end = b"ELLIS-BOARD-BEGIN", b"ELLIS-BOARD-END"
try:
    tty.setraw(fd)
    data = bytearray()
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        if not select.select([fd], [], [], 1)[0]:
            continue
        chunk = os.read(fd, 65536)
        if not chunk:
            raise SystemExit("USB port disconnected")
        data.extend(chunk)
        begin = data.rfind(start)
        finish = data.find(end, begin) if begin >= 0 else -1
        if finish >= 0:
            with args.output.open("xb") as out:
                out.write(data[begin:finish + len(end)] + b"\n")
            break
        if len(data) > 1024 * 1024:
            raise SystemExit("Incomplete or unexpected board report")
    else:
        raise SystemExit("Timed out waiting for the board reporter")
finally:
    try:
        termios.tcsetattr(fd, termios.TCSANOW, settings)
    finally:
        os.close(fd)
