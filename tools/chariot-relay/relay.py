#!/usr/bin/env python3
"""The Violet Agent relay (docs/agent-protocol.md): pairs a phone and a Mac
in a room and forwards their WebSocket messages. Standard library only, so it
runs as a Chariot agent app (see deploy.sh) or locally:

    python3 tools/chariot-relay/relay.py --port 8787

It never sees keys or plaintext; the two ends encrypt end to end.

With --stun PORT it also answers STUN binding requests (RFC 5389) on UDP, the
rendezvous the two ends use to learn their public addresses before they punch
a direct path. A Chariot carries HTTPS only, so there the ends use public STUN
servers instead; a relay on a host with UDP can be its own.
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
POLL = re.compile(r"^/v1/(send|recv)/([0-9a-f]{32})\?role=(phone|mac)&id=([0-9a-f]{8,32})$")
POLL_ONLY = False  # --poll-only: refuse WebSockets, as a Chariot's proxy does (tests)
WAIT = 8  # a Chariot's proxy ends requests after about 12 s
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


class PollConn:
    """A connection over plain HTTP, for proxies that refuse WebSockets (a
    Chariot's): POST /v1/send/ROOM?role=R&id=ID carries messages up, and a
    GET /v1/recv/... held for up to 8 s brings them down. Each message is a
    kind byte (1 text, 2 binary), a 4-byte big-endian length and the bytes.
    A new ID for a role is a new connection; one that stops polling for 45 s
    is gone."""

    def __init__(self, room, role, cid):
        self.room, self.role, self.id = room, role, cid
        self.queue = []
        self.cond = threading.Condition()
        self.closed = False
        self.seen = time.monotonic()
        self.waiting = 0

    def send(self, opcode, payload):
        with self.cond:
            if not self.closed:
                self.queue.append((opcode, payload))
                self.cond.notify_all()

    def close(self):
        with self.cond:
            self.closed = True
            self.cond.notify_all()

    def take(self):
        with self.cond:
            self.waiting += 1
            deadline = time.monotonic() + WAIT
            while not self.queue and not self.closed and time.monotonic() < deadline:
                self.cond.wait(deadline - time.monotonic())
            self.waiting -= 1
            self.seen = time.monotonic()
            out, size = [], 0
            while self.queue and size < MAX_MESSAGE:
                op, data = self.queue.pop(0)
                out.append(struct.pack("!BI", op, len(data)) + data)
                size += len(data)
            return b"".join(out), self.closed


def join(conn):
    """Put a connection in its room; tell both ends if the other is there."""
    with lock:
        slot = rooms.setdefault(conn.room, {})
        old = slot.get(conn.role)
        slot[conn.role] = conn
        other = slot.get("mac" if conn.role == "phone" else "phone")
        stats["connections"] += 1
    if old:
        old.close()
    if other and not other.closed:
        for c in (conn, other):
            c.send(1, b'{"peer":"up"}')
    return old


def leave(conn):
    with lock:
        slot = rooms.get(conn.room, {})
        peer = None
        if slot.get(conn.role) is conn:
            del slot[conn.role]
            peer = slot.get("mac" if conn.role == "phone" else "phone")
        if not slot:
            rooms.pop(conn.room, None)
    log(f"{conn.room[:8]} {conn.role} gone")
    if peer and not peer.closed:
        peer.send(1, b'{"peer":"down"}')


def forward(conn, data):
    peer = peer_of(conn)
    if peer and not peer.closed:
        peer.send(2, data)
        stats["forwarded"] += 1
        stats["bytes"] += len(data)


def reaper():
    while True:
        time.sleep(5)
        with lock:
            stale = [c for slot in rooms.values() for c in slot.values()
                     if isinstance(c, PollConn) and not c.waiting and time.monotonic() - c.seen > 45]
        for c in stale:
            c.close()
            leave(c)


VERBOSE = False


def poll(sock, kind, room, role, cid, body):
    if VERBOSE:
        log(f"{room[:8]} {role} {cid} {kind} {len(body)} B")
    with lock:
        conn = rooms.get(room, {}).get(role)
    if not isinstance(conn, PollConn) or conn.id != cid or conn.closed:
        conn = PollConn(room, role, cid)
        replaced = join(conn)
        log(f"{room[:8]} {role} polling" + (" (replacing)" if replaced else ""))
    conn.seen = time.monotonic()
    if kind == "send":
        at = 0
        while at + 5 <= len(body):
            op, n = struct.unpack_from("!BI", body, at)
            if op == 2:
                forward(conn, body[at + 5:at + 5 + n])
            at += 5 + n
        out = b""
    else:
        out, closed = conn.take()
        if closed and not out:
            sock.sendall(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return
    sock.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(out) + out)


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
        p = POLL.match(path)
        if p:
            body = head.split(b"\r\n\r\n", 1)[1]
            want = int(headers.get("content-length", "0") or 0)
            if want > MAX_MESSAGE * 2:
                return sock.close()
            while len(body) < want:
                chunk = sock.recv(65536)
                if not chunk:
                    return sock.close()
                body += chunk
            sock.settimeout(WAIT + 10)
            poll(sock, p.group(1), p.group(2), p.group(3), p.group(4), body)
            return sock.close()
        m = ROOM.match(path)
        key = None if POLL_ONLY else headers.get("sec-websocket-key")
        if method != "GET" or not m or not key or "websocket" not in headers.get("upgrade", "").lower():
            sock.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            return sock.close()
        accept = base64.b64encode(hashlib.sha1(key.encode() + GUID).digest())
        sock.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
    except (OSError, ValueError, EOFError):
        return sock.close()
    room, role = m.group(1), m.group(2)
    conn = Conn(sock, room, role)
    old = join(conn)
    log(f"{room[:8]} {role} connected from {addr[0]}" + (" (replacing)" if old else ""))
    try:
        for opcode, data in frames(sock):
            if opcode == 8:
                break
            if opcode == 9:
                conn.send(10, data)
                continue
            if opcode == 2:
                forward(conn, data)
    except (OSError, EOFError, ValueError):
        pass
    conn.close()
    leave(conn)


def stun_server(host, port):
    """Answer STUN binding requests with the sender's address (XOR-MAPPED-ADDRESS)."""
    u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    u.bind((host, port))
    log(f"STUN on udp {host}:{port}")
    while True:
        data, (ip, p) = u.recvfrom(2048)
        if len(data) < 20 or data[0:2] != b"\x00\x01" or data[4:8] != b"\x21\x12\xa4\x42":
            continue
        xip = struct.unpack(">I", socket.inet_aton(ip))[0] ^ 0x2112A442
        attr = struct.pack(">HHBBHI", 0x0020, 8, 0, 1, p ^ 0x2112, xip)
        u.sendto(struct.pack(">HH", 0x0101, len(attr)) + data[4:20] + attr, (ip, p))
        stats["stun"] = stats.get("stun", 0) + 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--host", default=os.environ.get("HOST", "0.0.0.0"))
    ap.add_argument("--port", type=int, default=int(os.environ.get("PORT", "8787")))
    ap.add_argument("--poll-only", action="store_true", help="refuse WebSockets, like a Chariot's proxy")
    ap.add_argument("--verbose", action="store_true", help="log every poll request")
    ap.add_argument("--stun", type=int, default=0, metavar="PORT", help="also answer STUN on this UDP port")
    args = ap.parse_args()
    global POLL_ONLY, VERBOSE
    POLL_ONLY, VERBOSE = args.poll_only, args.verbose
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((args.host, args.port))
    s.listen(64)
    log(f"Violet Agent relay on {args.host}:{args.port}")
    threading.Thread(target=reaper, daemon=True).start()
    if args.stun:
        threading.Thread(target=stun_server, args=(args.host, args.stun), daemon=True).start()
    while True:
        c, addr = s.accept()
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        threading.Thread(target=serve, args=(c, addr), daemon=True).start()


if __name__ == "__main__":
    main()
