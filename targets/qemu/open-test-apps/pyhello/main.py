#!/bin/python3
"""A zero-build Python app: copy it into /apps and tap it."""
import collections, datetime, json, math, os, re, sys, threading, time, zlib

print("hello from python on violet", sys.version.split()[0], sys.platform)
counts = collections.Counter(re.findall(r"\w+", "the quick brown fox jumps over the lazy dog the end"))
print("most common:", counts.most_common(1))
print("json:", json.dumps({"pi": round(math.pi, 5), "list": [1, 2, 3]}, sort_keys=True))

results = []
lock = threading.Lock()
def work(n):
    s = sum(range(n * 10000))
    with lock:
        results.append(s)
threads = [threading.Thread(target=work, args=(i,)) for i in range(1, 5)]
for t in threads: t.start()
for t in threads: t.join()
print("threads:", sorted(results))

os.makedirs("/tmp/py", exist_ok=True)
with open("/tmp/py/note.txt", "w") as f:
    f.write("written by python")
print("file round trip:", open("/tmp/py/note.txt").read())
print("apps:", ",".join(sorted(os.listdir("/apps"))))
packed = zlib.compress(b"violet" * 100)
print("zlib:", len(packed) < 100, zlib.decompress(packed)[:6].decode())
start = time.monotonic(); time.sleep(0.05)
print("slept at least 50 ms:", time.monotonic() - start >= 0.05)
print("year:", datetime.datetime.now().year >= 2026)
print("python done")
