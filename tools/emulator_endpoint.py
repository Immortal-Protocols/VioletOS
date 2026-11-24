"""Transport for the emulated radio. TCP bytes pass through unchanged; the
guest performs the TLS handshake, validates the peer and parses HTTP."""

from contextlib import contextmanager
import socket
import struct
import threading

HOST = bytes.fromhex("0256494f4c54")
GUEST = bytes.fromhex("525400560001")
TYPE = bytes.fromhex("88b5")
PAYLOAD = 1024
FRAME = 24 + PAYLOAD


# Hosts the guest may name in a connect command. TLS and certificate checks
# stay in the guest; the relay only refuses other destinations.
ENDPOINTS = {"www.uuidgenerator.net"}


def connect_endpoint(host="www.uuidgenerator.net"):
    if host not in ENDPOINTS:
        raise OSError(f"endpoint {host!r} is not allowed")
    return socket.create_connection((host, 443), timeout=4)


def serve(sock, connect):
    remote = None
    with sock, sock.makefile("rb") as stream:
        try:
            while header := stream.read(4):
                if len(header) != 4:
                    return
                (size,) = struct.unpack("!I", header)
                if size > 65536:
                    return
                frame = stream.read(size)
                if len(frame) != size:
                    return
                if size != FRAME or frame[:14] != HOST + GUEST + TYPE:
                    continue
                command = frame[14]
                length = int.from_bytes(frame[20:22], "big")
                if length > PAYLOAD:
                    continue
                data = b""
                ok = False
                try:
                    if command in (1, 4):
                        if remote:
                            remote.close()
                            remote = None
                        if command == 1:
                            host = frame[24 : 24 + length].decode("ascii", "replace")
                            remote = connect(host) if host else connect()
                    elif command == 2 and remote:
                        remote.sendall(frame[24 : 24 + length])
                    elif command == 3 and remote:
                        data = remote.recv(length)
                    else:
                        raise OSError("inactive connection")
                    ok = True
                except OSError:
                    if remote:
                        remote.close()
                        remote = None
                reply = bytearray(FRAME)
                reply[:14] = GUEST + HOST + TYPE
                reply[14] = command
                reply[15] = int(ok)
                reply[16:20] = frame[16:20]
                reply[20:22] = len(data).to_bytes(2, "big")
                reply[24 : 24 + len(data)] = data
                sock.sendall(struct.pack("!I", FRAME) + reply)
        except OSError:
            pass
        finally:
            if remote:
                remote.close()


@contextmanager
def endpoint_peer(connect=connect_endpoint):
    host, guest = socket.socketpair()
    thread = threading.Thread(target=serve, args=(host, connect), daemon=True)
    thread.start()
    try:
        yield guest.fileno()
    finally:
        guest.close()
        thread.join(timeout=6)
