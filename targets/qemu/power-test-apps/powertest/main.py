"""Take a wake lock, then report Power's state four times a second for a
minute (each report is a request, which is when Power runs its governors),
and exit still holding the lock. targets/qemu/test_power.py watches."""
import os, time


def sys_command(line):
    fd = os.open("/dev/violet/sys", os.O_RDWR)
    try:
        os.write(fd, (line + "\n").encode())
        return os.read(fd, 4096).decode(errors="replace").strip()
    finally:
        os.close(fd)


print("powertest: before", sys_command("wakelocks").replace("\n", " | "), flush=True)
print("powertest: wakelock", sys_command("wakelock 1"), flush=True)
print("powertest: renew", sys_command("wakelock 1"), flush=True)
print("powertest: bad", sys_command("wakelock 2"), flush=True)
print("powertest: locks", sys_command("wakelocks").replace("\n", " | "), flush=True)
end = time.monotonic() + 60
while time.monotonic() < end:
    print("powertest: power", sys_command("power"), flush=True)
    time.sleep(0.25)
print("powertest: done", flush=True)
