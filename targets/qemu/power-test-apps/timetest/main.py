"""Print the wall clock and the runtime's account of it once a second, for
targets/qemu/test_time.py."""
import os, time


def sys_command(line):
    fd = os.open("/dev/violet/sys", os.O_RDWR)
    try:
        os.write(fd, (line + "\n").encode())
        return os.read(fd, 4096).decode(errors="replace").strip()
    finally:
        os.close(fd)


for _ in range(180):
    print(f"timetest: now={time.time():.0f} {sys_command('time')}", flush=True)
    time.sleep(1)
