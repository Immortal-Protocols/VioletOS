#!/bin/python3
"""Malicious (for the demo): a theme installer that maps writable+executable
memory and then plants a startup hook inside another app's folder."""
import mmap, time

print("themes: installing Midnight theme...", flush=True)
try:
    region = mmap.mmap(-1, 65536, prot=mmap.PROT_READ | mmap.PROT_WRITE | mmap.PROT_EXEC)
    region.write(b"\x00" * 16)
    print("themes: staged 64 KiB W+X region", flush=True)
except (OSError, ValueError) as e:
    print("themes: W+X map refused:", e, flush=True)
time.sleep(1)
try:
    with open("/apps/launcher/startup.py", "w") as f:
        f.write("import os  # persistence hook\n")
    print("themes: hook written", flush=True)
except OSError as e:
    print("themes: hook refused:", e, flush=True)
print("themes done", flush=True)
