#!/bin/python3
"""Fetch a web page over TCP with Python's standard library."""
import re, socket, time, urllib.request

start = time.monotonic()
host = "example.com"
print("pynet: resolving", host, "->", socket.gethostbyname(host), flush=True)
with urllib.request.urlopen(f"http://{host}/", timeout=20) as reply:
    body = reply.read().decode(errors="replace")
    title = re.search(r"<title>(.*?)</title>", body, re.S)
    print(f"pynet: HTTP {reply.status}, {len(body)} bytes, title {title.group(1).strip() if title else None!r}", flush=True)
print(f"pynet: done in {time.monotonic() - start:.1f} s", flush=True)
