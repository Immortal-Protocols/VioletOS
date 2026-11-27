"""Host test of tools/net_relay.py (make test-unit): the emulator's side of the
Open Runtime's socket pages, against a file standing in for QEMU's RAM and
servers on this machine's loopback. No QEMU; a second or two.

It plays the Open Runtime's part of the layout (runtime/open/net.rs) and
checks that every slot works, that the relay keeps out of the Bluetooth pages
between the two runs of rings, and that a connect which does not answer delays
no other slot.
"""
import socket
import struct
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import net_relay
from net_relay import HEADER, HEADERS, NAME, RX, SLOTS, TX, rings

HOLE, HOLE_END = 0x40000, 0x43000


class App:
    """The Open Runtime's side of the pages."""

    def __init__(self, mem):
        self.mem = mem

    def word(self, slot, i):
        return struct.unpack_from("<I", self.mem, HEADERS + slot * HEADER + 4 * i)[0]

    def put(self, slot, i, v):
        struct.pack_into("<I", self.mem, HEADERS + slot * HEADER + 4 * i, v & 0xFFFFFFFF)

    def start(self, slot, command, ip=0, port=0, name=b""):
        at = HEADERS + slot * HEADER + 64
        self.mem[at:at + NAME] = name.ljust(NAME, b"\0")
        self.put(slot, 5, ip)
        self.put(slot, 6, port)
        # Its own ring words only (net.rs): the server empties the others
        # when it takes the command.
        for i in (7, 10, 11):
            self.put(slot, i, 0)
        self.put(slot, 3, net_relay.CONNECTING)
        self.put(slot, 4, 0)
        self.put(slot, 1, command)
        self.put(slot, 0, self.word(slot, 0) + 1)

    def state(self, slot):
        return net_relay.CONNECTING if self.word(slot, 0) != self.word(slot, 2) else self.word(slot, 3)

    def send(self, slot, data):
        head = self.word(slot, 7)
        for k, b in enumerate(data):
            self.mem[rings(slot) + (head + k) % TX] = b
        self.put(slot, 7, head + len(data))

    def read(self, slot):
        if self.word(slot, 0) != self.word(slot, 2):
            return b""  # not taken yet: the ring is the previous use's
        head, tail = self.word(slot, 9), self.word(slot, 10)
        data = bytes(self.mem[rings(slot) + TX + (tail + k) % RX] for k in range((head - tail) & 0xFFFFFFFF))
        self.put(slot, 10, head)
        return data


def until(what, check, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.002)
    raise AssertionError(what)


def echo_server():
    """Echoes each connection's bytes back in upper case."""
    server = socket.create_server(("127.0.0.1", 0))

    def serve(conn):
        with conn:
            while data := conn.recv(4096):
                conn.sendall(data.upper())

    def accept():
        while True:
            try:
                conn, _ = server.accept()
            except OSError:
                return
            threading.Thread(target=serve, args=(conn,), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()
    return server


def main():
    server = echo_server()
    port = server.getsockname()[1]
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("127.0.0.1", 0))
    udp.settimeout(5)
    loopback = struct.unpack(">I", socket.inet_aton("127.0.0.1"))[0]

    # A destination that never answers: its dial blocks until the test lets go.
    release, dialling, late = threading.Event(), threading.Event(), []
    real_dial = net_relay.Relay.dial

    def dial(address, timeout):
        if address[0] == "nowhere.test":
            raise socket.gaierror(8, "no such name")
        if address[1] == 9:
            dialling.set()
            release.wait(30)
            if address[0] == "late.test":
                late.append(real_dial(("127.0.0.1", port), timeout))
                return late[-1]
            raise socket.timeout("timed out")
        return real_dial(address, timeout)

    net_relay.Relay.dial = staticmethod(dial)
    net_relay.RESOLVE_SECONDS = 0.3
    longest = ".".join(["a" * 63, "b" * 63, "c" * 63, "d" * 61]).encode()
    assert len(longest) == 253

    with tempfile.NamedTemporaryFile() as ram:
        ram.truncate(net_relay.SIZE)
        relay = net_relay.Relay(ram.name, offset=0)
        relay.hosts = {"echo.test": f"127.0.0.1:{port}", longest.decode(): f"127.0.0.1:{port}",
                       "hole.test": "hole.test:9", "late.test": "late.test:9", "nowhere.test": "nowhere.test:80"}
        relay.socks_proxy = None
        net_relay.local_companion = lambda: None
        app = App(relay.mem)
        relay.mem[HOLE:HOLE_END] = b"\xa5" * (HOLE_END - HOLE)
        relay.start()
        try:
            until("the relay's header", lambda: struct.unpack_from("<I", relay.mem, 0)[0] == 0x3254454E)
            assert struct.unpack_from("<IIII", relay.mem, 4) == (3, 32, 8192, 16384)
            assert rings(8) + TX + RX <= HOLE and rings(9) == HOLE_END and rings(31) + TX + RX == net_relay.SIZE

            # A connect that never answers is taken at once and holds nobody up.
            app.start(0, 1, name=b"hole.test", port=80)
            until("the dial starts", dialling.is_set)
            until("the command is taken", lambda: app.word(0, 0) == app.word(0, 2))
            assert app.word(0, 3) == net_relay.CONNECTING
            began = time.monotonic()
            # Every other slot connects (two by address) and echoes through its own rings.
            for slot in range(1, SLOTS):
                if slot < 3:
                    app.start(slot, 1, ip=loopback, port=port)
                else:
                    app.start(slot, 1, name=longest if slot == 3 else b"echo.test", port=80)
            for slot in range(1, SLOTS):
                until(f"slot {slot} opens", lambda: app.state(slot) == net_relay.OPEN)
            for round_ in range(3):
                for slot in range(1, SLOTS):
                    app.send(slot, b"slot %d round %d " % (slot, round_) + b"x" * 5000)
                for slot in range(1, SLOTS):
                    want = b"SLOT %d ROUND %d " % (slot, round_) + b"X" * 5000
                    got = b""
                    deadline = time.monotonic() + 5
                    while len(got) < len(want) and time.monotonic() < deadline:
                        got += app.read(slot)
                        time.sleep(0.001)
                    assert got == want, f"slot {slot} round {round_}: {len(got)} bytes"
            elapsed = time.monotonic() - began
            assert elapsed < 3, f"31 slots took {elapsed:.1f} s beside a connect that does not answer"
            assert app.state(0) == net_relay.CONNECTING
            assert bytes(relay.mem[HOLE:HOLE_END]) == b"\xa5" * (HOLE_END - HOLE), "the relay wrote into the Bluetooth pages"

            # A slot closed and reused while its dial is out: the late connection is dropped.
            app.start(4, 2)
            until("slot 4 closes", lambda: app.state(4) == net_relay.IDLE)
            dialling.clear()
            app.start(4, 1, name=b"late.test", port=80)
            until("the second dial starts", dialling.is_set)
            app.start(4, 2)
            until("slot 4 closes again", lambda: app.state(4) == net_relay.IDLE)
            app.start(4, 1, name=b"echo.test", port=80)
            until("slot 4 reopens", lambda: app.state(4) == net_relay.OPEN)
            mine = relay.socks[4]
            release.set()
            until("the black hole times out", lambda: app.state(0) == net_relay.FAILED)
            assert app.word(0, 4) == 110
            until("the late dial ends", lambda: late and late[0].fileno() == -1)
            assert relay.socks[4] is mine and app.state(4) == net_relay.OPEN
            app.send(4, b"still mine")
            until("slot 4 still echoes", lambda: app.read(4) == b"STILL MINE")

            # No such name; nothing listening.
            app.start(0, 1, name=b"nowhere.test", port=80)
            closed = socket.create_server(("127.0.0.1", 0))
            refused = closed.getsockname()[1]
            closed.close()
            app.start(5, 2)
            until("slot 5 closes", lambda: app.state(5) == net_relay.IDLE)
            app.start(5, 1, ip=loopback, port=refused)
            until("the unknown name fails", lambda: app.state(0) == net_relay.FAILED)
            until("the refused connect fails", lambda: app.state(5) == net_relay.FAILED)
            # The app's errno is Linux's whatever this machine calls it.
            assert app.word(0, 4) == 2 and app.word(5, 4) == 111

            # UDP: a datagram to an address, then one to a name that does not
            # resolve (dropped after the wait) with one behind it that must still go.
            app.start(6, 2)
            until("slot 6 closes", lambda: app.state(6) == net_relay.IDLE)
            app.start(6, 3)
            until("the UDP slot opens", lambda: app.state(6) == net_relay.OPEN)
            assert app.word(6, 12) != 0

            def datagram(ip, port, name, payload):
                body = struct.pack("<IHB", ip, port, len(name)) + name + payload
                return struct.pack("<H", len(body)) + body

            where = udp.getsockname()[1]
            app.send(6, datagram(loopback, where, b"", b"by address"))
            data, source = udp.recvfrom(2048)
            assert data == b"by address" and source[1] == app.word(6, 12)
            app.send(6, datagram(0, where, b"no-such-host.invalid", b"lost") + datagram(0, where, b"localhost", b"by name"))
            app.send(7, b"meanwhile")
            until("TCP moves while a UDP name resolves", lambda: app.read(7) == b"MEANWHILE")
            data, _ = udp.recvfrom(2048)
            assert data == b"by name"
            udp.sendto(b"reply", ("127.0.0.1", app.word(6, 12)))
            until("the reply arrives", lambda: app.word(6, 9) != 0)
            frame = app.read(6)
            assert struct.unpack_from("<HIHB", frame) == (12, loopback, where, 0) and frame[9:] == b"reply"

            # A slot reused before the relay has seen the close: nothing of the
            # old connection is in the new one's ring, whatever arrived meanwhile.
            for round_ in range(20):
                app.send(7, b"unread %d " % round_ + b"y" * 3000)
                until("the echo is waiting", lambda: app.word(7, 9) - app.word(7, 10) >= 3000)
                app.send(7, b"more on the way" * 100)
                app.start(7, 2)
                app.start(7, 1, name=b"echo.test", port=80)
                assert app.read(7) == b""
                app.send(7, b"new %d" % round_)
                until("slot 7 reopens", lambda: app.state(7) == net_relay.OPEN)
                got = b""
                deadline = time.monotonic() + 5
                while len(got) < len(b"NEW %d" % round_) and time.monotonic() < deadline:
                    got += app.read(7)
                assert got == b"NEW %d" % round_, f"round {round_}: the reused slot read {got[:40]!r}"
            # The UDP socket's port does not outlive it.
            app.start(6, 2)
            app.start(6, 1, name=b"echo.test", port=80)
            until("slot 6 reopens", lambda: app.state(6) == net_relay.OPEN)
            assert app.word(6, 12) == 0 and app.read(6) == b""

            # A "name" that is an address written out is refused, never dialled.
            dialled = []
            net_relay.Relay.dial = staticmethod(lambda address, timeout: (dialled.append(address), dial(address, timeout))[1])
            for k, form in enumerate([b"127.0.0.1", b"2130706433", b"0x7f000001", b"127.1", b"0177.0.0.1"]):
                app.start(8 + k, 2)
                app.start(8 + k, 1, name=form, port=port)
            for k in range(5):
                until("a numeric name fails", lambda: app.state(8 + k) == net_relay.FAILED)
                assert app.word(8 + k, 4) == 2
            assert not dialled, f"dialled {dialled}"
            app.start(6, 2)
            app.start(6, 3)
            until("the UDP slot reopens", lambda: app.state(6) == net_relay.OPEN)
            app.send(6, datagram(0, where, b"2130706433", b"to an address") + datagram(loopback, where, b"", b"after it"))
            data, _ = udp.recvfrom(2048)
            assert data == b"after it"

            # Listening: a Mac listener on loopback for the app's port, where
            # connections wait (word 15) until an accept of the same owner takes
            # the oldest, with its peer in words 12 and 13.
            def as_owner(slot, owner, command, ip=0, port=0):
                app.start(slot, 2)
                until(f"slot {slot} closes", lambda: app.state(slot) == net_relay.IDLE)
                app.put(slot, 14, owner)
                app.start(slot, command, ip=ip, port=port)

            as_owner(20, 7, net_relay.CMD_LISTEN, ip=2, port=8080)
            until("the listener opens", lambda: app.state(20) == net_relay.OPEN)
            assert app.word(20, 12) == 8080
            mac = relay.forwarded[("tcp", 8080)][0]
            first = socket.create_connection(("127.0.0.1", mac), timeout=5)
            second = socket.create_connection(("127.0.0.1", mac), timeout=5)
            until("two connections wait", lambda: app.word(20, 15) == 2)
            first.sendall(b"from the mac")
            as_owner(21, 8, net_relay.CMD_ACCEPT, ip=20)
            until("another owner's accept fails", lambda: app.state(21) == net_relay.FAILED)
            assert app.word(21, 4) == 22 and app.word(20, 15) == 2
            as_owner(21, 7, net_relay.CMD_ACCEPT, ip=20)
            until("the accept opens", lambda: app.state(21) == net_relay.OPEN)
            assert app.word(21, 12) == first.getsockname()[1] and app.word(21, 13) == loopback
            until("the bytes come", lambda: app.read(21) == b"from the mac")
            app.send(21, b"back")
            assert first.recv(16) == b"back"
            as_owner(22, 7, net_relay.CMD_ACCEPT, ip=20)
            until("the second is taken", lambda: app.state(22) == net_relay.OPEN)
            assert app.word(22, 12) == second.getsockname()[1] and app.word(20, 15) == 0
            as_owner(23, 7, net_relay.CMD_ACCEPT, ip=20)
            until("none waits", lambda: app.state(23) == net_relay.FAILED)
            assert app.word(23, 4) == 11
            as_owner(24, 7, net_relay.CMD_LISTEN, ip=1, port=8080)
            until("the port is taken", lambda: app.state(24) == net_relay.FAILED)
            assert app.word(24, 4) == 98
            # Closing the listener closes what waits, not what was accepted, and frees the port.
            third = socket.create_connection(("127.0.0.1", mac), timeout=5)
            until("one waits", lambda: app.word(20, 15) == 1)
            app.start(20, 2)
            until("the listener closes", lambda: app.state(20) == net_relay.IDLE)
            assert third.recv(16) == b"" and ("tcp", 8080) not in relay.forwarded
            app.send(22, b"still")
            assert second.recv(16) == b"still"
            for conn in (first, second, third):
                conn.close()
            # A UDP slot is told the port the app bound; datagrams to it come in.
            as_owner(24, 7, net_relay.CMD_UDP, port=5353)
            until("the bound UDP slot opens", lambda: app.state(24) == net_relay.OPEN)
            assert app.word(24, 12) == 5353
            udp.sendto(b"to the bound port", ("127.0.0.1", relay.forwarded[("udp", 5353)][0]))
            until("the datagram arrives", lambda: app.word(24, 9) != 0)
            frame = app.read(24)
            assert struct.unpack_from("<HIHB", frame)[1:3] == (loopback, udp.getsockname()[1]) and frame[9:] == b"to the bound port"
            # A port a slot holds is refused to another, as lwIP refuses it.
            as_owner(26, 8, net_relay.CMD_UDP, port=5353)
            until("the second bind fails", lambda: app.state(26) == net_relay.FAILED)
            assert app.word(26, 4) == 98
            # Through a SOCKS relay (the phone's), listening is refused.
            relay.socks_proxy = ("127.0.0.1", 9)
            as_owner(25, 7, net_relay.CMD_LISTEN, ip=1, port=9090)
            until("listening through the relay fails", lambda: app.state(25) == net_relay.FAILED)
            assert app.word(25, 4) == 95
            relay.socks_proxy = None
        finally:
            release.set()
            relay.stop()
            server.close()
            udp.close()
    print("net_relay.py: all tests passed")


if __name__ == "__main__":
    main()
