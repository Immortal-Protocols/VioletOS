"""Serve the Open Runtime's socket pages from the Mac, in QEMU.

On the handset, Radio serves these pages with lwIP (targets/moto-g-pure/ip/
sockets.c). In the emulator this thread does it with the Mac's own sockets:
each slot's CONNECT opens a host TCP connection, by name or address, and bytes
move between it and the slot's rings. The layout is in runtime/open/net.rs.
VIOLET_NET_HOSTS maps names to "host:port" (for tests), e.g.
{"example.com": "127.0.0.1:8088"}.

VIOLET_NET_SOCKS="127.0.0.1:1080" sends every connection through a SOCKS5
relay instead (RFC 1928/1929, user "violet", password VIOLET_NET_SOCKS_PASS),
the way the handset's Phone Internet mode goes through the iPhone companion.
Names ending in ".violet" are the companion's own endpoints; without a relay
they fail at once instead of reaching the Mac's resolver.
"""
import json
import mmap
import os
import socket
import struct
import threading
import time

PAGE = 0x49230000 - 0x40000000
SLOTS, TX, RX = 8, 8192, 16384
STRIDE = TX + RX
MAGIC = 0x3154454E
IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED = range(5)


class Relay(threading.Thread):
    def __init__(self, ram_path):
        super().__init__(daemon=True)
        self.ram = open(ram_path, "r+b")
        self.mem = mmap.mmap(self.ram.fileno(), 4096 + SLOTS * STRIDE, offset=PAGE)
        self.socks = [None] * SLOTS
        self.closing = [False] * SLOTS
        self.hosts = json.loads(os.environ.get("VIOLET_NET_HOSTS", "{}"))
        proxy = os.environ.get("VIOLET_NET_SOCKS")
        self.socks_proxy = None
        if proxy:
            h, _, p = proxy.rpartition(":")
            self.socks_proxy = (h or "127.0.0.1", int(p))
        self.socks_pass = os.environ.get("VIOLET_NET_SOCKS_PASS", "violet-dev")
        self.stopped = threading.Event()

    def word(self, slot, i):
        return struct.unpack_from("<I", self.mem, 256 + slot * 128 + 4 * i)[0]

    def put(self, slot, i, v):
        struct.pack_into("<I", self.mem, 256 + slot * 128 + 4 * i, v & 0xFFFFFFFF)

    def drop(self, slot):
        if self.socks[slot]:
            try:
                self.socks[slot].close()
            except OSError:
                pass
        self.socks[slot] = None

    def connect(self, slot):
        name = bytes(self.mem[256 + slot * 128 + 64:256 + slot * 128 + 128]).split(b"\0")[0].decode()
        ip, port = self.word(slot, 5), self.word(slot, 6)
        host = name or socket.inet_ntoa(struct.pack(">I", ip))
        try:
            # VIOLET_NET_SOCKS_SCOPE=violet: only the companion's own .violet endpoints use the relay.
            only_violet = os.environ.get("VIOLET_NET_SOCKS_SCOPE") == "violet"
            if self.socks_proxy and (not only_violet or host.endswith(".violet")):
                sock = self.socks_connect(host, port, bool(name))
            else:
                if host.endswith(".violet") and not self.socks_proxy:
                    raise ConnectionRefusedError(111, "no companion relay")
                target = self.hosts.get(name) or self.hosts.get(f"{host}:{port}")
                if target:
                    host, _, mapped = target.partition(":")
                    port = int(mapped or port)
                sock = socket.create_connection((host, port), timeout=8)
            sock.setblocking(False)
            self.socks[slot] = sock
            self.put(slot, 4, 0)
            self.put(slot, 3, OPEN)
        except socket.gaierror:
            self.put(slot, 4, 2)
            self.put(slot, 3, FAILED)
        except OSError as e:
            self.put(slot, 4, e.errno or 111)
            self.put(slot, 3, FAILED)

    # SOCKS5 reply codes as the errno the app sees.
    SOCKS_ERRNO = {1: 5, 2: 13, 3: 101, 4: 113, 5: 111, 6: 110, 7: 95, 8: 97}

    def socks_connect(self, host, port, by_name):
        sock = socket.create_connection(self.socks_proxy, timeout=8)
        try:
            def exact(n):
                data = b""
                while len(data) < n:
                    chunk = sock.recv(n - len(data))
                    if not chunk:
                        raise ConnectionResetError(104, "relay closed")
                    data += chunk
                return data
            sock.sendall(b"\x05\x01\x02")
            if exact(2) != b"\x05\x02":
                raise ConnectionRefusedError(111, "relay refused auth method")
            user, pw = b"violet", self.socks_pass.encode()
            sock.sendall(bytes([1, len(user)]) + user + bytes([len(pw)]) + pw)
            if exact(2)[1] != 0:
                raise PermissionError(13, "relay rejected credentials")
            if by_name:
                addr = b"\x03" + bytes([len(host)]) + host.encode()
            else:
                addr = b"\x01" + socket.inet_aton(host)
            sock.sendall(b"\x05\x01\x00" + addr + struct.pack(">H", port))
            head = exact(4)
            if head[1] != 0:
                code = self.SOCKS_ERRNO.get(head[1], 111)
                raise OSError(code, f"relay reply {head[1]}")
            exact({1: 4, 4: 16}.get(head[3], 0) + 2 if head[3] != 3 else exact(1)[0] + 2)
            return sock
        except BaseException:
            sock.close()
            raise

    def pump(self, slot):
        sock = self.socks[slot]
        if not sock or self.word(slot, 3) not in (OPEN, PEER_CLOSED):
            return
        base = 4096 + slot * STRIDE
        head, tail = self.word(slot, 7), self.word(slot, 8)
        while head != tail:
            at = tail % TX
            n = min((head - tail) & 0xFFFFFFFF, TX - at)
            try:
                sent = sock.send(bytes(self.mem[base + at:base + at + n]))
            except BlockingIOError:
                break
            except OSError:
                self.put(slot, 4, 104)
                self.put(slot, 3, FAILED)
                return
            tail = (tail + sent) & 0xFFFFFFFF
            if sent < n:
                break
        self.put(slot, 8, tail)
        if self.word(slot, 11) == 1 and head == tail:
            try:
                sock.shutdown(socket.SHUT_WR)
            except OSError:
                pass
            self.put(slot, 11, 2)
        if self.word(slot, 3) != OPEN:
            return
        rin, rout = self.word(slot, 9), self.word(slot, 10)
        while (rin - rout) & 0xFFFFFFFF < RX:
            at = rin % RX
            room = min(RX - ((rin - rout) & 0xFFFFFFFF), RX - at)
            try:
                data = sock.recv(room)
            except BlockingIOError:
                break
            except OSError:
                self.put(slot, 4, 104)
                self.put(slot, 3, FAILED)
                break
            if not data:
                self.put(slot, 9, rin)
                self.put(slot, 3, PEER_CLOSED)
                return
            self.mem[base + TX + at:base + TX + at + len(data)] = data
            rin = (rin + len(data)) & 0xFFFFFFFF
        self.put(slot, 9, rin)

    def run(self):
        struct.pack_into("<II", self.mem, 0, MAGIC, 3)
        while not self.stopped.is_set():
            for slot in range(SLOTS):
                seq = self.word(slot, 0)
                if seq != self.word(slot, 2):
                    command = self.word(slot, 1)
                    self.drop(slot)
                    if command == 1:
                        self.put(slot, 3, CONNECTING)
                        self.connect(slot)
                    else:
                        self.put(slot, 3, IDLE)
                    self.put(slot, 2, seq)
                self.pump(slot)
            time.sleep(0.002)

    def stop(self):
        self.stopped.set()
        if self.is_alive():
            self.join(timeout=1)
        for slot in range(SLOTS):
            self.drop(slot)
        self.mem.close()
        self.ram.close()
