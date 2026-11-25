"""Violet Agent's voice, on RoseOS's models and code: Rose's Listener (Silero
VAD, smart-turn v3, Nemotron streaming ASR, Parakeet final transcripts) hears
the phone's microphone, and Rose's Speaker (Phonon 31M) speaks the replies,
which the phone plays like Rose's browser player (duck, pause, resume, stop,
and a report every 100 ms of how far each segment played). Barge-in follows
Rose's conductor: her voice ducks at the first speech frame, pauses once
speech is clearly there, stops for real words or a stop word, and resumes for
a backchannel, a cough or her own voice coming back through the phone.

Run by Violet Agent (companion/mac) with the RoseOS voice environment:
    ROSE_VOICE=~/RoseOS-1/prototypes/voice-phone-demo \
        $ROSE_VOICE/.venv/bin/python companion/mac/voice/violet_voice.py

stdin and stdout carry frames: a kind byte, a 4-byte big-endian length, the
bytes. In: 1 JSON command, 3 microphone (16 kHz s16le mono). Out: 1 JSON
event, 2 speech (segment id u32 LE, then 24 kHz s16le mono).
Commands: {"cmd": "reply", "id": N, "text": "..."} adds words to reply N
(started on first use), {"cmd": "reply_end", "id": N}, {"cmd": "stop"},
{"cmd": "voice", "name": "zoey"}, {"cmd": "client", "msg": {...}} (the
phone's playback report), {"cmd": "agent_name", "name": "Violet"}.
Events: ready, partial, speech_start, turn (a committed user turn), barge
(duck/pause/resume/stop), control (for the phone's player), audible,
reply_done, error.
"""
from __future__ import annotations

import json
import os
import struct
import sys
import threading
import time
import traceback
from pathlib import Path

ROSE = Path(os.environ.get("ROSE_VOICE", Path.home() / "RoseOS-1/prototypes/voice-phone-demo")).expanduser()
sys.path.insert(0, str(ROSE))

import numpy as np  # noqa: E402

from rose.bus import Bus  # noqa: E402
from rose.conductor import (BACKCHANNELS, BARGE_BACKCHANNEL_S, BARGE_PAUSE_S, BARGE_VOICED_S,  # noqa: E402
                            STOP_WORDS, _bc_ish, _words, is_backchannel, is_hold)
from rose.listener import Listener  # noqa: E402
from rose.runtime import Runtime  # noqa: E402
from rose.speaker import Speaker  # noqa: E402

out_lock = threading.Lock()
stdout = sys.stdout.buffer
real_stdout = sys.stdout
sys.stdout = sys.stderr  # library prints must not reach the frame stream


def put(kind: int, data: bytes) -> None:
    with out_lock:
        stdout.write(struct.pack("!BI", kind, len(data)) + data)
        stdout.flush()


def event(**e) -> None:
    put(1, json.dumps(e).encode())


class PhoneTransport:
    """Rose's Transport, with the phone (through Violet Agent) as the player."""

    on_client = None

    def send_audio(self, seg: int, pcm: np.ndarray) -> None:
        s16 = (np.clip(np.asarray(pcm, np.float32), -1, 1) * 32767).astype("<i2").tobytes()
        put(2, struct.pack("<I", seg) + s16)

    def send_control(self, msg: dict) -> None:
        event(ev="control", msg=msg)


class Voice:
    def __init__(self):
        self.bus = Bus()
        self.runtime = Runtime()
        self.transport = PhoneTransport()
        self.listener = Listener(self.runtime, self.bus, self.on_listen)
        self.speaker = Speaker(self.runtime, self.bus, self.transport)
        self.speaker.on_audible = lambda a: event(ev="audible", audible=a)
        self.replies: dict[int, object] = {}
        self.current: int | None = None
        self.barge: dict | None = None
        self.lock = threading.RLock()
        self.user_active = False
        self.name = "Violet"

    def load(self, voice: str) -> None:
        t = time.monotonic()
        self.listener.load()
        self.speaker.load(voice)
        self.listener.start()
        threading.Thread(target=self.tick, daemon=True).start()
        event(ev="ready", voice=self.speaker.voice, voices=sorted(self.speaker.voices), load_s=round(time.monotonic() - t, 1))

    # ------------------------------------------------------------- speaking
    def reply(self, rid: int, text: str) -> None:
        with self.lock:
            utt = self.replies.get(rid)
            if utt is None:
                utt = self.speaker.utterance(rid, None)
                self.replies[rid] = utt
                self.current = rid
                threading.Thread(target=self.watch, args=(rid, utt), daemon=True).start()
            utt.add(text)

    def reply_end(self, rid: int) -> None:
        with self.lock:
            utt = self.replies.get(rid)
            if utt is not None:
                utt.finish()

    def watch(self, rid: int, utt) -> None:
        utt.done.wait()
        with self.lock:
            self.replies.pop(rid, None)
            if self.current == rid:
                self.current = None
        try:
            heard = utt.heard()
        except Exception:
            heard = ""
        event(ev="reply_done", id=rid, interrupted=bool(getattr(utt, "interrupted", False)), heard=heard)

    def stop(self, reason: str) -> None:
        self.barge = None if self.barge is None else {**self.barge, "stopped": True}
        self.speaker.stop()
        event(ev="barge", stage="stop", reason=reason)

    def resume(self, reason: str) -> None:
        self.barge = None
        self.speaker.resume()
        event(ev="barge", stage="resume", reason=reason)

    def audible(self) -> bool:
        return self.speaker.audible

    def is_echo(self, text: str) -> bool:
        """Her own recent speech coming back through the phone's microphone."""
        with self.lock:
            utts = list(self.replies.values())
        heard = " ".join(u.heard() for u in utts if hasattr(u, "heard"))
        recent = set(_words(heard)[-14:])
        words = [w for w in _words(text) if w not in BACKCHANNELS]
        return len(words) >= 2 and sum(w in recent for w in words) / len(words) >= 0.8

    # ------------------------------------------------------------ listening
    def on_listen(self, ev) -> None:
        try:
            self.listen(ev)
        except Exception:
            traceback.print_exc()

    def listen(self, ev) -> None:
        if ev.kind in ("speech_start", "speech_resume", "partial"):
            self.user_active = True
        elif ev.kind in ("turn_end", "noise"):
            self.user_active = False
        if ev.kind == "speech_start":
            event(ev="speech_start")
            if self.audible() and self.barge is None:
                self.barge = {"t": time.monotonic(), "stopped": False, "voiced": 0.0, "last": time.monotonic(), "text": ""}
                self.speaker.duck()
                event(ev="barge", stage="duck", reason="user started speaking")
        elif ev.kind == "partial":
            event(ev="partial", text=ev.text)
            b = self.barge
            if b is not None and not b["stopped"]:
                b["text"] = ev.text
                words = _words(ev.text)
                if not words or self.is_echo(ev.text):
                    pass
                elif STOP_WORDS & set(words):
                    self.stop("stop word")
                elif not _bc_ish(ev.text) and not b.get("backchannel"):
                    self.stop("user is talking")
        elif ev.kind == "tentative":
            if self.barge is not None and not self.barge["stopped"]:
                self.barge["backchannel"] = is_backchannel(ev.text)
        elif ev.kind == "noise":
            if self.barge is not None and not self.barge["stopped"]:
                self.resume("noise")
        elif ev.kind == "turn_end":
            text = (ev.text or "").strip()
            b = self.barge
            barging = b is not None and not b["stopped"]
            if barging and self.is_echo(text):
                return self.resume(f"echo: {text}")
            if barging and is_backchannel(text):
                return self.resume(f"backchannel: {text}")
            if not barging and self.audible() and (is_backchannel(text) or self.is_echo(text)):
                return
            if barging or (is_hold(text) and self.audible()):
                self.stop("hold request" if is_hold(text) else "user turn")
            if not text:
                return
            if is_hold(text):
                event(ev="turn", text=text, hold=True, listen=ev.data)
                return
            if self.audible():
                self.stop("new user turn")
            event(ev="turn", text=text, hold=False, listen=ev.data)

    def tick(self) -> None:
        while True:
            time.sleep(0.05)
            b = self.barge
            if b is None:
                continue
            now = time.monotonic()
            if b["stopped"]:
                if now - b["t"] > 1.0 and not self.audible():
                    self.barge = None
                continue
            dt, b["last"] = now - b["last"], now
            if getattr(self.listener, "in_speech", False) and getattr(self.listener, "vad_p", 1.0) >= 0.5:
                b["voiced"] += dt
            bc = b.get("backchannel") or is_backchannel(b["text"]) or (b["text"] and _bc_ish(b["text"]))
            limit = BARGE_BACKCHANNEL_S if bc else BARGE_VOICED_S
            if b["voiced"] >= limit and not self.is_echo(b["text"]):
                self.stop(f"{round(b['voiced'] * 1000)} ms of speech")
            elif b["voiced"] >= BARGE_PAUSE_S and not b.get("paused"):
                b["paused"] = True
                self.speaker.pause()
                event(ev="barge", stage="pause", reason="speech continues")
            elif not self.user_active and now - b["t"] >= 1.5:
                self.resume("no speech")


def main() -> int:
    voice_name = os.environ.get("VIOLET_VOICE", "zoey")
    v = Voice()
    try:
        v.load(voice_name)
    except Exception as e:
        event(ev="error", message=f"could not load the RoseOS voice models: {e}")
        traceback.print_exc()
        return 1
    inp = sys.stdin.buffer
    while True:
        head = inp.read(5)
        if len(head) < 5:
            return 0
        kind, n = struct.unpack("!BI", head)
        data = inp.read(n)
        if kind == 3:
            v.listener.push(np.frombuffer(data, "<i2").astype(np.float32) / 32768.0)
            continue
        try:
            c = json.loads(data)
            cmd = c.get("cmd")
            if cmd == "reply":
                v.reply(int(c["id"]), c.get("text", ""))
            elif cmd == "reply_end":
                v.reply_end(int(c["id"]))
            elif cmd == "stop":
                v.stop(c.get("reason", "stopped"))
            elif cmd == "voice":
                v.speaker.set_voice(c["name"])
                event(ev="voice", voice=v.speaker.voice)
            elif cmd == "client":
                v.speaker.on_client(c.get("msg", {}))
            elif cmd == "agent_name":
                v.name = c.get("name", "Violet")
        except Exception as e:
            event(ev="error", message=str(e))
            traceback.print_exc()


if __name__ == "__main__":
    sys.exit(main())
