def command(line):
    with open("/dev/violet/sys", "r+b", buffering=0) as system:
        system.write(line)
        return system.read(4096).decode()

report = command(b"memory\n")
free_kib = int(report.split()[0].removeprefix("free="))
assert free_kib >= 32 * 1024 and " cache=" in report, report
print("memory test: allocating past the free pool")
allocation = bytearray((free_kib + 8192) * 1024)
for offset in range(0, len(allocation), 65536):
    allocation[offset] = 1
running = command(b"running\n").split()
assert "/apps/cache" not in running and "/apps/home" in running, running
print("memory test: foreground allocation survived")
