"""Rose's audio transport for the VioletOS phone.

The phone streams its microphone to Violet Agent and plays Rose's speech as
Rose's browser player does (rose/web/audio.js): it obeys duck, unduck, pause,
resume, stop and end, and reports played, stopped and idle. Violet Agent
carries both over its encrypted link and passes them here as frames, so this
is Rose's Transport (rose/transport.py) with the phone at the far end. While
the phone is away, speech counts as played at once and a stop as stopped, as
Rose's BrowserTransport does with no browser, so a turn never waits for it.

A phone can answer a stop late (its runtime pauses while it installs a large
package). Rose's speaker gives up after 0.5 s and speaks on, and a "stopped"
arriving after that would cut off the reply she started since: the phone's
player handles messages in order, so a stop only ever stopped audio sent
before it. A report answering a stop that newer speech followed is dropped.
"""
from __future__ import annotations

import struct
import threading
from collections import deque
from typing import Callable

import numpy as np

from . import frames

FRAME = 512  # 32 ms at 16 kHz, the listener's frame


class PhoneTransport:
    def __init__(self) -> None:
        self.on_mic: Callable[[np.ndarray], None] | None = None
        self.on_client: Callable[[dict], None] | None = None
        self.online = False
        self._lock = threading.Lock()
        self._pending = np.zeros(0, dtype=np.float32)
        self._stops: deque[dict] = deque()  # stops sent, oldest first: {"followed": speech sent after it}

    # ------------------------------------------------------------ from Rose
    def send_audio(self, seg: int, pcm: np.ndarray) -> None:
        if not self.online:
            self._report({"type": "played", "seg": seg, "samples": int(len(pcm)), "done": True, "dropped": True})
            return
        with self._lock:
            for stop in self._stops:
                stop["followed"] = True
        samples = np.clip(np.asarray(pcm, dtype=np.float32), -1.0, 1.0)
        frames.put(frames.SPEECH, struct.pack("<I", seg & 0xFFFFFFFF) + (samples * 32767.0).astype("<i2").tobytes())

    def send_control(self, msg: dict) -> None:
        if not self.online:
            if msg.get("type") == "stop":
                self._report({"type": "stopped", "seg": -1, "samples": 0, "dropped": True})
            return
        if msg.get("type") == "stop":
            with self._lock:
                self._stops.append({"followed": False})
        frames.event(ev="control", msg=msg)

    def stop(self) -> None:
        self.set_online(False)

    # ------------------------------------------------------------ from the phone
    def mic(self, body: bytes) -> None:
        """16 kHz s16le from the phone, cut into the listener's 512-sample frames."""
        if self.on_mic is None:
            return
        pcm = np.frombuffer(body[: len(body) // 2 * 2], dtype="<i2").astype(np.float32) / 32768.0
        with self._lock:
            pending = np.concatenate([self._pending, pcm])
            whole = len(pending) // FRAME * FRAME
            ready, self._pending = pending[:whole], pending[whole:]
        for i in range(0, whole, FRAME):
            self.on_mic(ready[i:i + FRAME])

    def report(self, msg: dict) -> None:
        """A playback report (played, stopped, idle) from the phone's player."""
        if msg.get("type") == "stopped":
            with self._lock:
                stop = self._stops.popleft() if self._stops else None
            if stop is not None and stop["followed"]:
                return  # a late answer to a stop that newer speech followed (see above)
        self._report(msg)

    def set_online(self, online: bool) -> None:
        was, self.online = self.online, online
        with self._lock:
            self._stops.clear()
        if was and not online:
            # As Rose's BrowserTransport does when its browser goes: whatever was playing stopped.
            self._report({"type": "stopped", "seg": -1, "samples": 0, "reason": "disconnected"})

    def _report(self, msg: dict) -> None:
        if self.on_client is not None:
            try:
                self.on_client(msg)
            except Exception:  # a report must never take down the link
                pass
