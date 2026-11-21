"""QEMU lab peer for the single HTTPS operation exposed by the radio service."""
from contextlib import contextmanager
import re
import socket
import struct
import subprocess
import threading

URL = "https://www.uuidgenerator.net/api/version4"
UUID4 = re.compile(rb"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-4[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}")
HOST = bytes.fromhex("0256494f4c54")
GUEST = bytes.fromhex("525400560001")
TYPE = bytes.fromhex("88b5")


def fetch_uuid():
    response = subprocess.run(
        ["curl", "--disable", "--silent", "--show-error", "--fail",
         "--max-time", "5", "--max-filesize", "128", "--header", "Cache-Control: no-cache", URL],
        capture_output=True, check=True, timeout=6,
    )
    return response.stdout.strip()


def serve(sock, fetch):
    with sock, sock.makefile("rb") as stream:
        try:
            while header := stream.read(4):
                if len(header) != 4:
                    return
                size, = struct.unpack("!I",header)
                if size > 65536:
                    return
                frame = stream.read(size)
                if len(frame) != size:
                    return
                if len(frame)<22 or frame[:18] != HOST + GUEST + TYPE + b"GET4":
                    continue
                try:
                    data = fetch()
                    ok = bool(UUID4.fullmatch(data))
                except (OSError, subprocess.SubprocessError):
                    data, ok = b"", False
                reply = GUEST + HOST + TYPE + b"UUID" + frame[18:22] + bytes([ok]) + (data if ok else b"")
                reply = reply.ljust(60, b"\0")
                sock.sendall(struct.pack("!I",len(reply)) + reply)
        except OSError:
            return


@contextmanager
def endpoint_peer(fetch=fetch_uuid):
    host, guest = socket.socketpair()
    thread = threading.Thread(target=serve, args=(host,fetch), daemon=True)
    thread.start()
    try:
        yield guest.fileno()
    finally:
        guest.close()
        thread.join(timeout=7)
