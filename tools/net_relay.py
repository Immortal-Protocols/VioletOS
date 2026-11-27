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
they fail at once instead of reaching the Mac's resolver. The Mac companion's
private checkout-local discovery file enables its loopback SOCKS relay and
Agent pairing without setting environment variables.

A UDP slot (command 3) is a Mac UDP socket; datagrams cross the rings framed
as in net.rs. VIOLET_NET_UDP=0 refuses UDP, like a network that blocks it.
"""
from concurrent.futures import ThreadPoolExecutor
import json
import mmap
import os
from pathlib import Path
import socket
import struct
import stat
import sys
import threading
import time

LOCAL_COMPANION = Path(__file__).resolve().parents[1] / "build/local-companion/connection.json"
LOCAL_NAMES = {"agent-pair.violet", "agent-relay.violet", "sentinel.violet"}

PAGE = 0x49230000 - 0x40000000
SLOTS, TX, RX = 8, 8192, 16384
STRIDE = TX + RX
MAGIC = 0x3154454E
IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED = range(5)


def local_companion():
    """Read the same-user companion's loopback endpoints on each new connection."""
    try:
        fd = os.open(LOCAL_COMPANION, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd) as stream:
            info = os.fstat(stream.fileno())
            if info.st_uid != os.getuid() or not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077:
                return None
            config = json.loads(stream.read(4096))
        if not isinstance(config, dict) or not all(
            type(config.get(key)) is int and 0 < config[key] < 65536 for key in ("relayPort", "socksPort")
        ) or not isinstance(config.get("password"), str) or not 1 <= len(config["password"]) <= 255:
            return None
        pid = config.get("pid")
        if type(pid) is not int or pid <= 0:
            return None
        os.kill(pid, 0)
        return config
    except (OSError, ValueError, KeyError, TypeError):
        return None


class Relay(threading.Thread):
    def __init__(self, ram_path, offset=PAGE, slots=SLOTS):
        super().__init__(daemon=True)
        self.slots = slots
        self.data_offset = (256 + slots * 128 + 4095) & ~4095
        self.ram = open(ram_path, "r+b")
        self.mem = mmap.mmap(self.ram.fileno(), self.data_offset + slots * STRIDE, offset=offset)
        self.socks = [None] * slots
        self.udp = [False] * slots
        self.connectors = ThreadPoolExecutor(max_workers=min(slots, 16), thread_name_prefix="violet-connect")
        self.pending = [None] * slots
        self.trace = os.environ.get("VIOLET_NET_TRACE") == "1"
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

    @staticmethod
    def discard_connection(future):
        if not future.cancelled() and future.exception() is None:
            future.result().close()

    def drop(self, slot):
        future = self.pending[slot]
        if future is not None:
            self.pending[slot] = None
            if not future.cancel():
                future.add_done_callback(self.discard_connection)
        self.udp[slot] = False
        if self.socks[slot]:
            try:
                self.socks[slot].close()
            except OSError:
                pass
        self.socks[slot] = None

    def open_tcp(self, name, ip, port):
        # Workers never touch guest memory. A slot can be closed and reused
        # while DNS, a companion handshake or a TCP connection is pending.
        host = name or socket.inet_ntoa(struct.pack(">I", ip))
        # Explicit test mappings and SOCKS settings override desktop discovery.
        only_violet = os.environ.get("VIOLET_NET_SOCKS_SCOPE") == "violet"
        target = self.hosts.get(name) or self.hosts.get(f"{host}:{port}")
        local = local_companion() if not self.socks_proxy and not target else None
        if local and host == "agent-relay.violet" and port == 80:
            sock = socket.create_connection(("127.0.0.1", local["relayPort"]), timeout=2)
        elif local and (not host.endswith(".violet") or host in LOCAL_NAMES):
            sock = self.socks_connect(host, port, bool(name), ("127.0.0.1", local["socksPort"]), local["password"])
        elif self.socks_proxy and (not only_violet or host.endswith(".violet")):
            sock = self.socks_connect(host, port, bool(name), self.socks_proxy, self.socks_pass)
        else:
            if target:
                host, _, mapped = target.partition(":")
                port = int(mapped or port)
            elif host.endswith(".violet"):
                raise ConnectionRefusedError(111, "no companion relay")
            sock = socket.create_connection((host, port), timeout=8)
        return sock

    def start_connect(self, slot):
        name = bytes(self.mem[256 + slot * 128 + 64:256 + slot * 128 + 128]).split(b"\0")[0].decode(errors="replace")
        self.pending[slot] = self.connectors.submit(self.open_tcp, name, self.word(slot, 5), self.word(slot, 6))

    def finish_connect(self, slot):
        future = self.pending[slot]
        if future is None or not future.done():
            return
        self.pending[slot] = None
        if self.trace:
            name = bytes(self.mem[256 + slot * 128 + 64:256 + slot * 128 + 128]).split(b"\0")[0].decode(errors="replace")
            host = name or socket.inet_ntoa(struct.pack(">I", self.word(slot, 5)))
            peer = f"{host}:{self.word(slot, 6)} on slot {slot}"
        try:
            sock = future.result()
            self.socks[slot] = sock
            sock.setblocking(False)
            local_ip, local_port = sock.getsockname()[:2]
            self.put(slot, 12, local_port)
            if sock.family == socket.AF_INET:
                self.put(slot, 13, int.from_bytes(socket.inet_aton(local_ip), "big"))
            self.put(slot, 4, 0)
            self.put(slot, 3, OPEN)
            if self.trace:
                print(f"socket relay connected {peer}", file=sys.stderr, flush=True)
        except (OSError, ValueError) as error:
            self.put(slot, 4, 2 if isinstance(error, socket.gaierror) else getattr(error, "errno", None) or 111)
            self.put(slot, 3, FAILED)
            if self.trace:
                print(f"socket relay failed {peer}: {error}", file=sys.stderr, flush=True)

    # SOCKS5 reply codes as the errno the app sees.
    SOCKS_ERRNO = {1: 5, 2: 13, 3: 101, 4: 113, 5: 111, 6: 110, 7: 95, 8: 97}

    def socks_connect(self, host, port, by_name, proxy, password):
        sock = socket.create_connection(proxy, timeout=8)
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
            user, pw = b"violet", password.encode()
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

    def udp_open(self, slot):
        if os.environ.get("VIOLET_NET_UDP") == "0" or self.socks_proxy:
            self.put(slot, 4, 101)
            self.put(slot, 3, FAILED)
            return
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("0.0.0.0", 0))
        sock.setblocking(False)
        self.socks[slot] = sock
        self.udp[slot] = True
        self.put(slot, 12, sock.getsockname()[1])
        self.put(slot, 13, struct.unpack(">I", socket.inet_aton(self.lan_address()))[0])
        self.put(slot, 4, 0)
        self.put(slot, 3, OPEN)

    @staticmethod
    def lan_address():
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            probe.connect(("192.0.2.1", 9))
            return probe.getsockname()[0]
        except OSError:
            return "127.0.0.1"
        finally:
            probe.close()

    def ring(self, base, at, n, size):
        """n bytes of a ring from position at."""
        out = b""
        while n:
            k = min(n, size - at % size)
            out += bytes(self.mem[base + at % size:base + at % size + k])
            at, n = at + k, n - k
        return out

    def pump_udp(self, slot):
        sock = self.socks[slot]
        base = self.data_offset + slot * STRIDE
        head, tail = self.word(slot, 7), self.word(slot, 8)
        while (head - tail) & 0xFFFFFFFF >= 2:
            body = struct.unpack("<H", self.ring(base, tail, 2, TX))[0]
            if body < 7 or (head - tail) & 0xFFFFFFFF < body + 2:
                if body < 7:
                    tail = head
                break
            f = self.ring(base, tail + 2, body, TX)
            tail = (tail + 2 + body) & 0xFFFFFFFF
            ip, port, nl = struct.unpack("<IHB", f[:7])
            name = f[7:7 + nl].decode(errors="replace")
            host = name or socket.inet_ntoa(struct.pack(">I", ip))
            target = self.hosts.get(f"{host}:{port}")
            if target:
                host, _, mapped = target.partition(":")
                port = int(mapped or port)
            try:
                sock.sendto(f[7 + nl:], (host, port))
            except OSError:
                pass
        self.put(slot, 8, tail)
        rin, rout = self.word(slot, 9), self.word(slot, 10)
        while True:
            try:
                data, (h, p) = sock.recvfrom(2048)
            except (BlockingIOError, OSError):
                break
            frame = struct.pack("<HIHB", len(data) + 7, struct.unpack(">I", socket.inet_aton(h))[0], p, 0) + data
            if RX - ((rin - rout) & 0xFFFFFFFF) < len(frame):
                continue  # the app is not reading: drop it
            for k in range(0, len(frame)):
                self.mem[base + TX + (rin + k) % RX] = frame[k]
            rin = (rin + len(frame)) & 0xFFFFFFFF
        self.put(slot, 9, rin)

    def pump(self, slot):
        sock = self.socks[slot]
        if not sock or self.word(slot, 3) not in (OPEN, PEER_CLOSED):
            return
        if self.udp[slot]:
            return self.pump_udp(slot)
        base = self.data_offset + slot * STRIDE
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
            for slot in range(self.slots):
                seq = self.word(slot, 0)
                if seq != self.word(slot, 2):
                    command = self.word(slot, 1)
                    self.drop(slot)
                    if command == 1:
                        self.put(slot, 3, CONNECTING)
                        self.start_connect(slot)
                    elif command == 3:
                        self.udp_open(slot)
                    else:
                        self.put(slot, 3, IDLE)
                    self.put(slot, 2, seq)
                self.finish_connect(slot)
                self.pump(slot)
            time.sleep(0.002)

    def stop(self):
        self.stopped.set()
        if self.is_alive():
            self.join(timeout=1)
        for slot in range(self.slots):
            self.drop(slot)
        self.connectors.shutdown(wait=False, cancel_futures=True)
        self.mem.close()
        self.ram.close()
