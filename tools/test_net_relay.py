"""A pending TCP connection must not block active sockets or survive slot reuse."""
import socket
import tempfile
import threading
import time
import unittest
from pathlib import Path

from net_relay import CONNECTING, IDLE, OPEN, STRIDE, TX, Relay


class RelayTest(unittest.TestCase):
    def wait_until(self, predicate):
        deadline = time.monotonic() + 3
        while not predicate():
            self.assertLess(time.monotonic(), deadline, "socket relay timed out")
            time.sleep(.005)

    def test_pending_connect_does_not_block_or_overwrite_a_reused_slot(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as server:
            server.bind(("127.0.0.1", 0))
            server.listen()
            server.settimeout(3)
            ram = Path(directory) / "ram"
            ram.write_bytes(bytes(4096 + 2 * STRIDE))
            relay = Relay(ram, offset=0, slots=2)
            release, waiting = threading.Event(), threading.Event()

            def connect(name, ip, port):
                if ip == 1:
                    waiting.set()
                    release.wait(3)
                return socket.create_connection(server.getsockname())

            def command(slot, operation, ip):
                relay.put(slot, 5, ip)
                relay.put(slot, 1, operation)
                relay.put(slot, 0, relay.word(slot, 0) + 1)

            relay.open_tcp = connect
            relay.start()
            try:
                command(0, 1, 1)
                self.assertTrue(waiting.wait(2))
                command(1, 1, 2)
                self.wait_until(lambda: relay.word(1, 3) == OPEN)
                fast, _ = server.accept()
                with fast:
                    fast.sendall(b"data")
                    self.wait_until(lambda: relay.word(1, 9) == 4)
                    base = 4096 + STRIDE + TX
                    self.assertEqual(relay.mem[base:base + 4], b"data")
                    self.assertEqual(relay.word(0, 3), CONNECTING)

                command(0, 2, 1)
                self.wait_until(lambda: relay.word(0, 3) == IDLE)
                command(0, 1, 3)
                self.wait_until(lambda: relay.word(0, 3) == OPEN)
                current, _ = server.accept()
                with current:
                    release.set()
                    stale, _ = server.accept()
                    with stale:
                        stale.settimeout(3)
                        self.assertEqual(stale.recv(1), b"")
                    current.sendall(b"new")
                    self.wait_until(lambda: relay.word(0, 9) == 3)
                    self.assertEqual(relay.mem[4096 + TX:4096 + TX + 3], b"new")
            finally:
                release.set()
                relay.stop()


if __name__ == "__main__":
    unittest.main()
