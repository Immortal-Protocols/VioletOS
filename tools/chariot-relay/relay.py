#!/usr/bin/env python3
"""The Violet Agent relay (docs/agent-protocol.md): pairs a phone and a Mac
in a room and forwards their WebSocket messages. Standard library only, so it
runs as a Chariot agent app (see deploy.sh) or locally:

    python3 tools/chariot-relay/relay.py --port 8787

It never sees keys or plaintext; the two ends encrypt end to end.
"""
import argparse
import base64
import hashlib
import json
import os
import re
import socket
import struct
import sys
import threading
import time

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
ROOM = re.compile(r"^/v1/pipe/([0-9a-f]{32})\?role=(phone|mac)$")
IDLE = 60
MAX_MESSAGE = 1 << 20

rooms = {}  # room -> {"phone": Conn, "mac": Conn}
lock = threading.Lock()
stats = {"connections": 0, "forwarded": 0, "bytes": 0}


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


class Conn:
    def __init__(self, sock, room, role):
        self.sock, self.room, self.role = sock, room, role
        self.send_lock = threading.Lock()
        self.closed = False

    def send(self, opcode, payload):
        n = len(payload)
        if n < 126:
            head = struct.pack("!BB", 0x80 | opcode, n)
        elif n < 65536:
            head = struct.pack("!BBH", 0x80 | opcode, 126, n)
        else:
            head = struct.pack("!BBQ", 0x80 | opcode, 127, n)
        with self.send_lock:
            if self.closed:
                return
            try:
                self.sock.sendall(head + payload)
            except OSError:
                self.close()

    def close(self):
        if not self.closed:
            self.closed = True
            try:
                self.sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.sock.close()


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def frames(sock):
    """Yield (opcode, payload) for each complete message."""
    message, kind = b"", None
    while True:
        b0, b1 = read_exact(sock, 2)
        fin, opcode, masked, n = b0 & 0x80, b0 & 0x0F, b1 & 0x80, b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", read_exact(sock, 2))[0]
        elif n == 127:
            n = struct.unpack("!Q", read_exact(sock, 8))[0]
        if n > MAX_MESSAGE:
            raise EOFError
        mask = read_exact(sock, 4) if masked else None
        data = read_exact(sock, n)
        if mask:
            data = bytes(b ^ mask[i & 3] for i, b in enumerate(data))
        if opcode >= 8:
            yield opcode, data
            continue
        if opcode:
            kind = opcode
        message += data
        if len(message) > MAX_MESSAGE:
            raise EOFError
        if fin:
            yield kind, message
            message, kind = b"", None


def peer_of(conn):
    with lock:
        other = "mac" if conn.role == "phone" else "phone"
        return rooms.get(conn.room, {}).get(other)


def serve(sock, addr):
    sock.settimeout(IDLE)
    try:
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = sock.recv(4096)
            if not chunk or len(head) > 16384:
                return sock.close()
            head += chunk
        lines = head.split(b"\r\n\r\n")[0].decode("latin-1").split("\r\n")
        method, path = lines[0].split(" ")[:2]
        headers = {k.strip().lower(): v.strip() for k, v in (l.split(":", 1) for l in lines[1:] if ":" in l)}
        if path == "/health" or path == "/":
            body = json.dumps({"ok": True, "rooms": len(rooms), **stats}).encode()
            sock.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s" % (len(body), body))
            return sock.close()
        m = ROOM.match(path)
        key = headers.get("sec-websocket-key")
        if method != "GET" or not m or not key or "websocket" not in headers.get("upgrade", "").lower():
            sock.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return sock.close()
        accept = base64.b64encode(hashlib.sha1(key.encode() + GUID).digest())
        sock.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
    except (OSError, ValueError, EOFError):
        return sock.close()
    room, role = m.group(1), m.group(2)
    conn = Conn(sock, room, role)
    with lock:
        slot = rooms.setdefault(room, {})
        old = slot.get(role)
        slot[role] = conn
        other = slot.get("mac" if role == "phone" else "phone")
        stats["connections"] += 1
    if old:
        old.close()
    log(f"{room[:8]} {role} connected from {addr[0]}" + (" (replacing)" if old else ""))
    if other and not other.closed:
        for c in (conn, other):
            c.send(1, b'{"peer":"up"}')
    try:
        for opcode, data in frames(sock):
            if opcode == 8:
                break
            if opcode == 9:
                conn.send(10, data)
                continue
            if opcode == 2:
                peer = peer_of(conn)
                if peer and not peer.closed:
                    peer.send(2, data)
                    stats["forwarded"] += 1
                    stats["bytes"] += len(data)
    except (OSError, EOFError, ValueError):
        pass
    conn.close()
    with lock:
        slot = rooms.get(room, {})
        if slot.get(role) is conn:
            del slot[role]
            peer = slot.get("mac" if role == "phone" else "phone")
        else:
            peer = None
        if not slot:
            rooms.pop(room, None)
    log(f"{room[:8]} {role} gone")
    if peer and not peer.closed:
        peer.send(1, b'{"peer":"down"}')


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--host", default=os.environ.get("HOST", "0.0.0.0"))
    ap.add_argument("--port", type=int, default=int(os.environ.get("PORT", "8787")))
    args = ap.parse_args()
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((args.host, args.port))
    s.listen(64)
    log(f"Violet Agent relay on {args.host}:{args.port}")
    while True:
        c, addr = s.accept()
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        threading.Thread(target=serve, args=(c, addr), daemon=True).start()


if __name__ == "__main__":
    main()
