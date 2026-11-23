#!/bin/python3
"""Draw on the screen from Python: mmap /dev/violet/screen, present rows, read touches."""
import mmap, os, struct

W, H = 720, 1600
EVENT = struct.Struct("<QHHiii")
screen = os.open("/dev/violet/screen", os.O_RDWR)
events = os.open("/dev/violet/events", os.O_RDONLY)
fb = mmap.mmap(screen, W * H * 4)

def present(y, h):
    os.write(screen, struct.pack("<6I", 0, y, W, h, 0, 0))

def fill(x, y, w, h, color):
    row = struct.pack("<I", color) * w
    for yy in range(y, y + h):
        at = (yy * W + x) * 4
        fb[at:at + len(row)] = row

fill(0, 0, W, H, 0xff15102a)
fill(0, 0, W, 160, 0xff6a3fd1)
present(0, H)
print("pypaint: ready", flush=True)
taps = 0
while True:
    raw = os.read(events, EVENT.size)
    _, kind, _, x, y, value = EVENT.unpack(raw)
    if kind != 1:
        continue
    taps += 1
    print(f"pypaint: tap {taps} at {x},{y}", flush=True)
    if y < 160:
        with open("/dev/violet/sys", "w") as sysf:
            sysf.write("home\n")
        continue
    x0, y0 = max(0, min(W - 60, x - 30)), max(160, min(H - 60, y - 30))
    fill(x0, y0, 60, 60, 0xffffc14d)
    present(y0, 60)
