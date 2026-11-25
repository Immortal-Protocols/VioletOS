import time

cache = bytearray(64 * 1024 * 1024)
for offset in range(0, len(cache), 65536):
    cache[offset] = 1
print("memory test: cache resident")
with open("/dev/violet/sys", "wb", buffering=0) as system:
    system.write(b"home\n")
while True:
    time.sleep(1)
