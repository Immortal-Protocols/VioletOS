"""For targets/qemu/test_lifecycle.py: stays in front and forks a child that
works for 20 s, longer than the 15 s an app behind gets before it freezes. A
child of the app in front must keep running (lifecycle.rs `inherit`), so the
parent's wait ends; then it goes Home and ends."""
import os
import time

start = time.monotonic()
try:
    pid = os.fork()
except OSError as e:
    print(f"jobs: no fork ({e})", flush=True)
    raise SystemExit(1)
if pid == 0:
    while time.monotonic() - start < 20:
        time.sleep(0.5)
    os._exit(7)
_, status = os.waitpid(pid, 0)
print(f"jobs: child finished with {os.waitstatus_to_exitcode(status)} after {time.monotonic() - start:.0f} s", flush=True)
with open("/dev/violet/sys", "wb", buffering=0) as system:
    system.write(b"home\n")
