"""An ordinary app beside nethog, for targets/qemu/test_net.py: it fetches a
page and opens two more connections, and reports how long that took."""
import socket, time, urllib.request

start = time.monotonic()
with urllib.request.urlopen("http://peer.test/", timeout=30) as reply:
    body = reply.read()
extra = [socket.create_connection(("peer.test", 80), timeout=30) for _ in range(2)]
elapsed = int((time.monotonic() - start) * 1000)
print(f"netpeer: fetched {len(body)} bytes and opened {len(extra)} more sockets in {elapsed} ms", flush=True)
