"""The framed stdio link between Violet Agent (Swift) and Rose (this process).

Each frame is a kind byte, a 4-byte big-endian length, then the bytes.
In (Swift to Rose): 1 JSON command, 3 microphone (16 kHz s16le mono).
Out (Rose to Swift): 1 JSON event, 2 speech (segment id u32 LE, then 24 kHz s16le mono).
"""
from __future__ import annotations

import json
import struct
import sys
import threading

JSON, SPEECH, MIC = 1, 2, 3

_lock = threading.Lock()
_out = sys.stdout.buffer


def take_stdout() -> None:
    """Keep library prints off the frame stream: everything printed goes to stderr."""
    sys.stdout = sys.stderr


_closed = False


def put(kind: int, data: bytes) -> None:
    """One frame to Violet Agent; nothing once it has gone (Rose is then stopping)."""
    global _closed
    with _lock:
        if _closed:
            return
        try:
            _out.write(struct.pack("!BI", kind, len(data)) + data)
            _out.flush()
        except (BrokenPipeError, OSError):
            _closed = True


def event(**e) -> None:
    put(JSON, json.dumps(e, default=str).encode())


def frames():
    """(kind, bytes) from stdin until it closes."""
    inp = sys.stdin.buffer
    while True:
        head = inp.read(5)
        if len(head) < 5:
            return
        kind, n = struct.unpack("!BI", head)
        body = inp.read(n)
        if len(body) < n:
            return
        yield kind, body
