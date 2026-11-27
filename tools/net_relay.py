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
A port the app bound (word 6) is what it is told it has (word 12); the Mac
side takes any port (on loopback, the one VIOLET_NET_LISTEN maps it to), and
`forwarded` maps the one to the other for tests.

A listening slot (command 4) is a Mac listener on 127.0.0.1 (never the Mac's
other addresses), on the port VIOLET_NET_LISTEN maps the app's port to
({"8080": 18080}) or any; `forwarded` maps the app's port to it. Connections
wait there, at most the backlog (word 5), word 15 counting them, until an
accepting slot (command 5, word 5 the listener) of the same owner takes the
oldest. With a SOCKS relay set, listening fails with EOPNOTSUPP, as through
the phone's relay.

As Radio does, the relay empties its side of a slot's rings (words 8, 9, 12,
13, 15) when it takes a command, and refuses a "name" that is an IPv4 address
written out: the Open Runtime never sends one, and the Mac's resolver would
dial it as the address.

Nothing that can wait runs on the relay's own thread: each connect (and each
name a UDP slot sends to) is dialled on a thread of its own and its result
taken up on a later pass, so a host that does not answer delays no other slot.
tools/test_net_relay.py tests this; tools/test_net_layout.py checks the layout
against net.rs and sockets.c.
"""
import errno
import json
import mmap
import os
from pathlib import Path
import queue
import socket
import struct
import stat
import threading
import time

LOCAL_COMPANION = Path(__file__).resolve().parents[1] / "build/local-companion/connection.json"
LOCAL_NAMES = {"agent-pair.violet", "agent-relay.violet", "sentinel.violet", "shield-flows.violet"}

PAGE = 0x49230000 - 0x40000000
SLOTS, TX, RX = 32, 8192, 16384
STRIDE = TX + RX
MAGIC = 0x3254454E  # "NET2"
# Slot headers from page 1, 512 bytes each, the name from byte 64 of one.
HEADERS, HEADER, NAME = 0x1000, 512, 256
# Rings: slots 0-8 from RINGS_A, the rest from RINGS_B (the Bluetooth pages
# of tools/bluetooth_controller.py sit between the two runs).
RINGS_A, FIRST, RINGS_B = 0x5000, 9, 0x43000
SIZE = RINGS_B + (SLOTS - FIRST) * STRIDE
IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED = range(5)
CMD_CONNECT, CMD_CLOSE, CMD_UDP, CMD_LISTEN, CMD_ACCEPT = range(1, 6)
# What one listener keeps waiting (sockets.c LISTEN_BACKLOG).
LISTEN_BACKLOG = 4
# How long a UDP datagram waits for its name to resolve.
RESOLVE_SECONDS = 5


# Linux's numbers (the app's) for the errors a connect on this machine can end in.
LINUX_ERRNO = {"EPERM": 1, "ENOENT": 2, "EIO": 5, "EACCES": 13, "EINVAL": 22, "EOPNOTSUPP": 95, "EAFNOSUPPORT": 97,
               "EADDRNOTAVAIL": 99, "ENETDOWN": 100, "ENETUNREACH": 101, "ECONNABORTED": 103, "ECONNRESET": 104,
               "ETIMEDOUT": 110, "ECONNREFUSED": 111, "EHOSTDOWN": 112, "EHOSTUNREACH": 113}


class Refusal(OSError):
    """A connect the relay itself ends; its errno is already Linux's."""


def linux_errno(error):
    """The app's errno for an error of this machine's (macOS numbers differ)."""
    return LINUX_ERRNO.get(errno.errorcode.get(error.errno or 0, ""), 111)


def numeric(name):
    """The name is an address written out, in any form inet_aton reads."""
    try:
        socket.inet_aton(name)
        return True
    except (OSError, UnicodeError, ValueError):
        return False


def rings(slot):
    """Where a slot's send ring starts; its receive ring follows."""
    return RINGS_A + slot * STRIDE if slot < FIRST else RINGS_B + (slot - FIRST) * STRIDE


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
    def __init__(self, ram_path, offset=PAGE):
        super().__init__(daemon=True)
        self.ram = open(ram_path, "r+b")
        self.mem = mmap.mmap(self.ram.fileno(), SIZE, offset=offset)
        self.socks = [None] * SLOTS
        self.udp = [False] * SLOTS
        # Sending was shut down (word 11 is the Open Runtime's to write).
        self.shut = [False] * SLOTS
        # Counts each slot's uses: a dial that finishes after its slot moved on is dropped.
        self.uses = [0] * SLOTS
        # Results of dials and lookups, taken up by the relay's own thread.
        self.finished = queue.Queue()
        # Per UDP slot: the addresses of names it sent to (None: no such name),
        # and the name being looked up with the time to stop waiting for it.
        self.udp_names = [{} for _ in range(SLOTS)]
        self.udp_waiting = [None] * SLOTS
        # Listening slots: (Mac listener, backlog, connections waiting, owner).
        self.listening = [None] * SLOTS
        # An app's bound or listening port -> the Mac's port, ("tcp"|"udp", port) keys.
        self.forwarded = {}
        self.listen_ports = {int(k): int(v) for k, v in json.loads(os.environ.get("VIOLET_NET_LISTEN", "{}")).items()}
        self.hosts = json.loads(os.environ.get("VIOLET_NET_HOSTS", "{}"))
        proxy = os.environ.get("VIOLET_NET_SOCKS")
        self.socks_proxy = None
        if proxy:
            h, _, p = proxy.rpartition(":")
            self.socks_proxy = (h or "127.0.0.1", int(p))
        self.socks_pass = os.environ.get("VIOLET_NET_SOCKS_PASS", "violet-dev")
        self.stopped = threading.Event()

    def word(self, slot, i):
        return struct.unpack_from("<I", self.mem, HEADERS + slot * HEADER + 4 * i)[0]

    def put(self, slot, i, v):
        struct.pack_into("<I", self.mem, HEADERS + slot * HEADER + 4 * i, v & 0xFFFFFFFF)

    def drop(self, slot):
        if self.listening[slot]:
            listener, _, waiting, _ = self.listening[slot]
            for conn in [listener] + [c for c, _ in waiting]:
                try:
                    conn.close()
                except OSError:
                    pass
            self.listening[slot] = None
        self.forwarded = {k: v for k, v in self.forwarded.items() if v[1] != slot}
        self.udp[slot] = False
        self.shut[slot] = False
        self.uses[slot] += 1
        self.udp_names[slot] = {}
        self.udp_waiting[slot] = None
        if self.socks[slot]:
            try:
                self.socks[slot].close()
            except OSError:
                pass
        self.socks[slot] = None

    @staticmethod
    def dial(address, timeout):
        """Open a TCP connection (tests replace this to stand in for the network)."""
        return socket.create_connection(address, timeout=timeout)

    def connect(self, slot):
        """Start a slot's connect on a thread of its own; `finish` takes the result."""
        at = HEADERS + slot * HEADER + 64
        name = bytes(self.mem[at:at + NAME]).split(b"\0")[0].decode(errors="replace")
        ip, port = self.word(slot, 5), self.word(slot, 6) & 0xFFFF
        threading.Thread(target=self.dial_slot, args=(slot, self.uses[slot], name, ip, port), daemon=True).start()

    def dial_slot(self, slot, use, name, ip, port):
        try:
            if numeric(name):
                raise Refusal(2, "an address is not a name")
            self.finished.put(("tcp", slot, use, self.route(name, ip, port), 0))
        except socket.gaierror:
            self.finished.put(("tcp", slot, use, None, 2))
        except socket.timeout:
            self.finished.put(("tcp", slot, use, None, 110))
        except Refusal as e:
            self.finished.put(("tcp", slot, use, None, e.errno or 111))
        except OSError as e:
            self.finished.put(("tcp", slot, use, None, linux_errno(e)))
        except (ValueError, UnicodeError):
            self.finished.put(("tcp", slot, use, None, 111))

    def route(self, name, ip, port):
        """The connection for a slot's destination: mapped, relayed or direct."""
        host = name or socket.inet_ntoa(struct.pack(">I", ip))
        # Explicit test mappings and SOCKS settings override desktop discovery.
        only_violet = os.environ.get("VIOLET_NET_SOCKS_SCOPE") == "violet"
        target = self.hosts.get(name) or self.hosts.get(f"{host}:{port}")
        local = local_companion() if not self.socks_proxy and not target else None
        if host == "shield-flows.violet" and (only_violet or self.hosts):
            raise Refusal(111, "partial proxy coverage")
        if local and host == "agent-relay.violet" and port == 80:
            sock = self.dial(("127.0.0.1", local["relayPort"]), 2)
        elif local and (not host.endswith(".violet") or host in LOCAL_NAMES):
            sock = self.socks_connect(host, port, bool(name), ("127.0.0.1", local["socksPort"]), local["password"])
        elif self.socks_proxy and (not only_violet or host.endswith(".violet")):
            sock = self.socks_connect(host, port, bool(name), self.socks_proxy, self.socks_pass)
        else:
            if target:
                host, _, mapped = target.partition(":")
                port = int(mapped or port)
            elif host.endswith(".violet"):
                raise Refusal(111, "no companion relay")
            sock = self.dial((host, port), 8)
        sock.setblocking(False)
        return sock

    def lookup(self, slot, use, name):
        try:
            address = socket.getaddrinfo(name, None, socket.AF_INET, socket.SOCK_DGRAM)[0][4][0]
        except (OSError, UnicodeError):
            address = None
        self.finished.put(("name", slot, use, name, address))

    def finish(self):
        """Take up the dials and lookups that have finished."""
        while True:
            try:
                kind, slot, use, result, error = self.finished.get_nowait()
            except queue.Empty:
                return
            if use != self.uses[slot]:
                # The slot was closed or reused meanwhile.
                if kind == "tcp" and result:
                    result.close()
            elif kind == "name":
                self.udp_names[slot][result] = error
            elif result:
                self.socks[slot] = result
                self.put(slot, 4, 0)
                self.put(slot, 3, OPEN)
            else:
                self.put(slot, 4, error)
                self.put(slot, 3, FAILED)

    # SOCKS5 reply codes as the errno the app sees.
    SOCKS_ERRNO = {1: 5, 2: 13, 3: 101, 4: 113, 5: 111, 6: 110, 7: 95, 8: 97}

    def socks_connect(self, host, port, by_name, proxy, password):
        sock = self.dial(proxy, 8)
        try:
            def exact(n):
                data = b""
                while len(data) < n:
                    chunk = sock.recv(n - len(data))
                    if not chunk:
                        raise Refusal(104, "relay closed")
                    data += chunk
                return data
            sock.sendall(b"\x05\x01\x02")
            if exact(2) != b"\x05\x02":
                raise Refusal(111, "relay refused auth method")
            user, pw = b"violet", password.encode()
            sock.sendall(bytes([1, len(user)]) + user + bytes([len(pw)]) + pw)
            if exact(2)[1] != 0:
                raise Refusal(13, "relay rejected credentials")
            if by_name:
                addr = b"\x03" + bytes([len(host)]) + host.encode()
            else:
                addr = b"\x01" + socket.inet_aton(host)
            sock.sendall(b"\x05\x01\x00" + addr + struct.pack(">H", port))
            head = exact(4)
            if head[1] != 0:
                code = self.SOCKS_ERRNO.get(head[1], 111)
                raise Refusal(code, f"relay reply {head[1]}")
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
        wanted = self.word(slot, 6) & 0xFFFF
        if wanted and ("udp", wanted) in self.forwarded:
            return self.fail(slot, 98)  # EADDRINUSE, as lwIP's udp_bind on a port a slot holds
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            # A port VIOLET_NET_LISTEN maps is the test's, on loopback.
            sock.bind(("127.0.0.1", self.listen_ports[wanted]) if wanted in self.listen_ports else ("0.0.0.0", 0))
        except OSError:
            sock.close()
            return self.fail(slot, 98)
        sock.setblocking(False)
        self.socks[slot] = sock
        self.udp[slot] = True
        if wanted:
            self.forwarded[("udp", wanted)] = (sock.getsockname()[1], slot)
        self.put(slot, 12, wanted or sock.getsockname()[1])
        self.put(slot, 13, struct.unpack(">I", socket.inet_aton(self.lan_address()))[0])
        self.put(slot, 4, 0)
        self.put(slot, 3, OPEN)

    def listen(self, slot):
        """A listening slot: a Mac listener on loopback for the app's port."""
        port, backlog = self.word(slot, 6) & 0xFFFF, min(max(self.word(slot, 5), 1), LISTEN_BACKLOG)
        if self.socks_proxy:
            return self.fail(slot, 95)  # EOPNOTSUPP: the relay carries outgoing connections only
        if not port or ("tcp", port) in self.forwarded:
            return self.fail(slot, 22 if not port else 98)
        try:
            listener = socket.create_server(("127.0.0.1", self.listen_ports.get(port, 0)), backlog=backlog)
        except OSError:
            return self.fail(slot, 98)  # EADDRINUSE
        listener.setblocking(False)
        self.listening[slot] = (listener, backlog, [], self.word(slot, 14))
        self.forwarded[("tcp", port)] = (listener.getsockname()[1], slot)
        self.put(slot, 12, port)
        self.put(slot, 13, struct.unpack(">I", socket.inet_aton(self.lan_address()))[0])
        self.put(slot, 15, 0)
        self.put(slot, 4, 0)
        self.put(slot, 3, OPEN)

    def accept(self, slot):
        """Give a slot the oldest connection waiting on the listener in word 5."""
        at = self.word(slot, 5)
        held = self.listening[at] if at < SLOTS else None
        if not held or held[3] != self.word(slot, 14):
            return self.fail(slot, 22)  # EINVAL: not that owner's listener
        if not held[2]:
            return self.fail(slot, 11)  # EAGAIN: none waiting
        conn, (host, port) = held[2].pop(0)
        self.put(at, 15, len(held[2]))
        conn.setblocking(False)
        self.socks[slot] = conn
        self.put(slot, 12, port)
        self.put(slot, 13, struct.unpack(">I", socket.inet_aton(host))[0])
        self.put(slot, 4, 0)
        self.put(slot, 3, OPEN)

    def take_waiting(self, slot):
        """Connections that reached a listening slot's Mac listener, up to its backlog."""
        listener, backlog, waiting, _ = self.listening[slot]
        while len(waiting) < backlog:
            try:
                waiting.append(listener.accept())
            except (BlockingIOError, OSError):
                break
        self.put(slot, 15, len(waiting))

    def fail(self, slot, error):
        self.put(slot, 4, error)
        self.put(slot, 3, FAILED)

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
        base = rings(slot)
        # A slot on a VIOLET_NET_LISTEN port listens on the Mac's loopback in
        # place of the network: what comes from loopback comes, as the phone
        # sees it, from the Mac's LAN address (never from the phone's own
        # 127.0.0.1), and an answer to that address goes back over loopback.
        lan = self.lan_address() if sock.getsockname()[0] == "127.0.0.1" else None
        head, tail = self.word(slot, 7), self.word(slot, 8)
        if self.superseded(slot):
            return
        if (head - tail) & 0xFFFFFFFF > TX:
            tail = head  # not a ring any more: drop what is queued
        while (head - tail) & 0xFFFFFFFF >= 2:
            body = struct.unpack("<H", self.ring(base, tail, 2, TX))[0]
            if body < 7 or (head - tail) & 0xFFFFFFFF < body + 2:
                if body < 7:
                    tail = head
                break
            f = self.ring(base, tail + 2, body, TX)
            ip, port, nl = struct.unpack("<IHB", f[:7])
            name = f[7:7 + nl].decode(errors="replace")
            host = name or socket.inet_ntoa(struct.pack(">I", ip))
            target = self.hosts.get(f"{host}:{port}")
            if name and numeric(name):
                host = None  # an address is not a name: dropped
            elif target:
                host, _, mapped = target.partition(":")
                port = int(mapped or port)
            elif name:
                # A name is looked up on a thread of its own; its datagram
                # waits at the head of the ring, and is dropped if no answer comes.
                known = self.udp_names[slot]
                if name not in known:
                    waiting = self.udp_waiting[slot]
                    if not waiting or waiting[0] != name:
                        self.udp_waiting[slot] = (name, time.monotonic() + RESOLVE_SECONDS)
                        threading.Thread(target=self.lookup, args=(slot, self.uses[slot], name), daemon=True).start()
                        break
                    if time.monotonic() < waiting[1]:
                        break
                    known[name] = None
                self.udp_waiting[slot] = None
                host = known[name]
                if len(known) > 64:
                    known.clear()
            tail = (tail + 2 + body) & 0xFFFFFFFF
            if host is None or 7 + nl > body:
                continue
            if lan and host == lan:
                host = "127.0.0.1"
            try:
                sock.sendto(f[7 + nl:], (host, port))
            except (OSError, UnicodeError):
                pass
        self.put(slot, 8, tail)
        rin, rout = self.word(slot, 9), self.word(slot, 10)
        while True:
            try:
                data, (h, p) = sock.recvfrom(2048)
            except (BlockingIOError, OSError):
                break
            if lan and h.startswith("127."):
                h = lan
            frame = struct.pack("<HIHB", len(data) + 7, struct.unpack(">I", socket.inet_aton(h))[0], p, 0) + data
            if RX - ((rin - rout) & 0xFFFFFFFF) < len(frame):
                continue  # the app is not reading: drop it
            for k in range(0, len(frame)):
                self.mem[base + TX + (rin + k) % RX] = frame[k]
            rin = (rin + len(frame)) & 0xFFFFFFFF
        self.put(slot, 9, rin)

    def superseded(self, slot):
        """A new command is waiting: the send ring may hold the next use's bytes."""
        return self.word(slot, 0) != self.word(slot, 2)

    def pump(self, slot):
        if self.listening[slot]:
            return self.take_waiting(slot)
        sock = self.socks[slot]
        if not sock or self.word(slot, 3) not in (OPEN, PEER_CLOSED):
            return
        if self.udp[slot]:
            return self.pump_udp(slot)
        base = rings(slot)
        head, tail = self.word(slot, 7), self.word(slot, 8)
        if self.superseded(slot):
            return
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
        if self.word(slot, 11) == 1 and head == tail and not self.shut[slot]:
            try:
                sock.shutdown(socket.SHUT_WR)
            except OSError:
                pass
            self.shut[slot] = True
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
        # The magic goes in last: the layout words are there when it is seen.
        struct.pack_into("<III", self.mem, 4, 3, SLOTS, TX)
        struct.pack_into("<I", self.mem, 16, RX)
        struct.pack_into("<I", self.mem, 0, MAGIC)
        while not self.stopped.is_set():
            self.finish()
            for slot in range(SLOTS):
                seq = self.word(slot, 0)
                if seq != self.word(slot, 2):
                    command = self.word(slot, 1)
                    self.drop(slot)
                    # This side's ring words start empty for the new use; the
                    # old connection is gone and can put nothing more in them.
                    for i in (8, 9, 12, 13, 15):
                        self.put(slot, i, 0)
                    if command == CMD_LISTEN:
                        self.listen(slot)
                    elif command == CMD_ACCEPT:
                        self.accept(slot)
                    elif command == 1:
                        # Taken at once; the state stays CONNECTING until the dial ends.
                        self.put(slot, 4, 0)
                        self.put(slot, 3, CONNECTING)
                        self.connect(slot)
                    elif command == 3:
                        self.udp_open(slot)
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
        self.finish()  # connections that were still being dialled
        self.mem.close()
        self.ram.close()
