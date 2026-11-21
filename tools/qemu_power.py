"""Translate the emulated power controller's registers into QEMU power states."""

import json
import socket
import time


class Monitor:
    def __init__(self, path, guest):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(3)
        deadline = time.monotonic() + 3
        while True:
            try:
                self.socket.connect(str(path))
                break
            except (ConnectionRefusedError, FileNotFoundError):
                if guest.poll() is not None or time.monotonic() > deadline:
                    raise
                time.sleep(0.02)
        self.stream = self.socket.makefile("rb")
        if "QMP" not in json.loads(self.stream.readline()):
            raise RuntimeError("missing QMP greeting")
        self.command("qmp_capabilities")

    def command(self, name):
        self.socket.sendall(json.dumps({"execute": name}).encode() + b"\n")
        while True:
            reply = json.loads(self.stream.readline())
            if "return" in reply:
                return reply["return"]
            if "error" in reply:
                raise RuntimeError(reply["error"])

    def close(self):
        self.stream.close()
        self.socket.close()
