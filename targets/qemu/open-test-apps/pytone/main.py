#!/bin/python3
"""Synthesize a short chord in Python and stream it to /dev/violet/audio."""
import math, os, struct, time

RATE = 48000
audio = os.open("/dev/violet/audio", os.O_WRONLY)
frames = []
for n in range(RATE // 2):  # half a second: A major
    t = n / RATE
    v = sum(math.sin(2 * math.pi * f * t) for f in (440.0, 554.37, 659.25)) / 3
    s = int(v * 12000 * min(1.0, (RATE // 2 - n) / 2400))
    frames.append(struct.pack("<hh", s, s))
pcm = b"".join(frames)
start = time.monotonic()
sent = 0
while sent < len(pcm):
    sent += os.write(audio, pcm[sent:sent + 16384])
print(f"pytone: queued {len(pcm) // 4} frames in {time.monotonic() - start:.2f} s", flush=True)
time.sleep(0.6)
print("pytone done", flush=True)
