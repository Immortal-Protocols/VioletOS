"""Counts while it runs, for targets/qemu/test_lifecycle.py: it goes Home at
once, ticks every half second until the lifecycle freezes it, and on the
first tick after a gap (it was frozen, then opened again) says how long the
gap was, goes Home again and ends, so that Cache is the only app behind
Home when Pressure needs the pool."""
import time


def sys_command(line):
    with open("/dev/violet/sys", "r+b", buffering=0) as system:
        system.write(line.encode() + b"\n")
        return system.read(4096).decode()


print("ticker: started", flush=True)
sys_command("home")
last, n = time.monotonic(), 0
while True:
    time.sleep(0.5)
    now = time.monotonic()
    n += 1
    if now - last > 3:
        print(f"ticker: thawed after {now - last:.0f} s", flush=True)
        sys_command("home")
        raise SystemExit(0)
    elif n % 2 == 0:
        print(f"ticker: tick {n}", flush=True)
    last = now
