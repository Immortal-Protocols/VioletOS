"""Violet Agent's agent: Rose (companion/mac/roseos), run for the VioletOS phone.

Violet Agent for macOS starts this with Rose's Python environment:

    $ROSE_ENV/bin/python companion/mac/violet_rose/host.py --profile DIR [--port N]

It runs Rose's own server (rose.app.Server: listener, speaker, the Codex
conductor with her tasks, identity, transcripts and life) with three Violet
parts swapped in: the phone transport (the phone's microphone and player
over Violet Agent's link), VioletConductor (Violet's app tools) and no Android
phone. Rose's dashboard and HTTP API (/api/life ...) are served on
127.0.0.1:PORT. Rose keeps her state in the profile directory: her name and
voice (identity.json), her life and journal (rosie-life/), transcripts.

Frames on stdin/stdout (frames.py). Commands, Swift to Rose:
  {"cmd":"phone","online":bool}        the phone's link came or went
  {"cmd":"client","msg":{...}}         a playback report from the phone's player
  {"cmd":"text","text":"..."}          a typed turn (tests, the Mac window)
  {"cmd":"interrupt"}                  the owner tapped the phone to stop Rose speaking
  {"cmd":"rename","name":"..."}        the owner renamed the agent in the Mac window
  {"cmd":"answer","id":N,"result":{}}  Violet Agent's answer to a call
  {"cmd":"build_progress","task":N,"detail":"..."}
  {"cmd":"build_done","task":N,"status":"done|failed|cancelled","result":"..."}
Events, Rose to Swift: {"ev":"bus","msg":{...}} for every event on Rose's bus
(status, partial, user_turn, rose_turn, task, assistant_name, voice, ready,
error...), {"ev":"control","msg":{...}} for the phone's player,
{"ev":"call","id":N,"name":"open_app|list_apps|remove_app","args":{}},
{"ev":"build"|"build_update"|"build_cancel", ...}, {"ev":"dashboard","url":...}.
"""
from __future__ import annotations

import argparse
import asyncio
import itertools
import json
import os
import socket
import sys
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROSE = HERE.parent / "roseos" / "prototypes" / "voice-phone-demo"
sys.path.insert(0, str(ROSE))
sys.path.insert(0, str(HERE.parent))

from violet_rose import frames  # noqa: E402

frames.take_stdout()


QUIET = {"vad", "level", "stats"}


class Bridge:
    """Calls from Rose to Violet Agent, answered by an "answer" command."""

    def __init__(self):
        self._ids = itertools.count(1)
        self._waiting: dict[int, tuple[threading.Event, dict]] = {}
        self._lock = threading.Lock()

    def send(self, ev, **data):
        frames.event(ev=ev, **data)

    def call(self, name, args, timeout=20.0):
        cid = next(self._ids)
        done, box = threading.Event(), {}
        with self._lock:
            self._waiting[cid] = (done, box)
        frames.event(ev="call", id=cid, name=name, args=args)
        if not done.wait(timeout):
            with self._lock:
                self._waiting.pop(cid, None)
            return {"ok": False, "reason": "Violet Agent did not answer"}
        return box.get("result", {"ok": False})

    def answer(self, cid, result):
        with self._lock:
            waiting = self._waiting.pop(int(cid), None)
        if waiting:
            waiting[1]["result"] = result
            waiting[0].set()


def seed_identity(profile: Path, name: str | None, voice: str | None) -> None:
    """Rose's identity.json starts from what Violet Agent already called the agent."""
    path = profile / "identity.json"
    if path.exists() or not (name or voice):
        return
    data = {k: v for k, v in (("name", name), ("voice", voice)) if v}
    path.write_text(json.dumps(data, indent=1, sort_keys=True) + "\n")
    os.chmod(path, 0o600)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main() -> int:
    ap = argparse.ArgumentParser(description="Rose for the VioletOS phone")
    ap.add_argument("--profile", type=Path, required=True, help="Rose's state: identity, life, transcripts")
    ap.add_argument("--port", type=int, default=0, help="Rose's dashboard on 127.0.0.1 (0: any free port)")
    ap.add_argument("--name", help="the agent's name, for a new profile")
    ap.add_argument("--voice", help="the agent's voice, for a new profile")
    ap.add_argument("--phone-control", action="store_true", help="offer phone_task and look_at_screen")
    a = ap.parse_args()
    profile = a.profile.expanduser()
    profile.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.environ["ROSE_CONFIG_DIR"] = str(profile)
    os.environ.setdefault("ROSE_TRANSCRIPT_DIR", str(profile / "transcripts"))
    seed_identity(profile, a.name, a.voice)

    import traceback
    import rose.bus
    import rose.codex_conductor
    from rose import app as rose_app

    # Rose's conductor reports a failure in its event loop as a bare "Codex event handling failed": put each
    # distinct cause in rose.log (the emit runs inside the except block, so the exception is at hand).
    emit, causes = rose.bus.Bus.emit, set()

    def emit_with_cause(self, kind, **data):
        if kind == "error" and sys.exc_info()[0] is not None:
            cause = traceback.format_exc()
            if cause not in causes:
                causes.add(cause)
                print(f"[violet] {data.get('message')}:\n{cause}", file=sys.stderr, flush=True)
        return emit(self, kind, **data)
    rose.bus.Bus.emit = emit_with_cause
    from violet_rose.conductor import VioletConductor
    from violet_rose.transport import PhoneTransport

    bridge = Bridge()
    transport = PhoneTransport()

    class Conductor(VioletConductor):
        pass
    Conductor.bridge = bridge
    Conductor.phone_control = a.phone_control
    # Rose's server constructs rose.codex_conductor.CodexConductor when it loads: ours, with Violet's tools.
    rose.codex_conductor.CodexConductor = Conductor

    class VioletServer(rose_app.Server):
        def _make_transport(self):
            transport.on_mic = self.mic.mic
            transport.on_client = self._on_client
            return transport

    port = a.port or free_port()
    args = rose_app.parse_args(["--brain", "codex", "--phone", "fake", "--no-remote", "--port", str(port)])
    server = VioletServer(args)
    # Every bus event but the per-frame ones (voice activity, levels, stats) goes to Violet Agent.
    server.bus.subscribe(lambda msg: msg.get("kind") in QUIET or frames.event(ev="bus", msg=msg))

    def commands():
        for kind, body in frames.frames():
            if kind == frames.MIC:
                transport.mic(body)
                continue
            try:
                c = json.loads(body)
            except ValueError:
                continue
            cmd = c.get("cmd")
            conductor = server.conductor
            if cmd == "phone":
                transport.set_online(bool(c.get("online")))
            elif cmd == "client":
                transport.report(c.get("msg") or {})
            elif cmd == "text" and conductor is not None:
                conductor.inject_text_turn(str(c.get("text", "")))
            elif cmd == "interrupt" and conductor is not None:
                # The owner tapped the phone while Rose spoke: her own interrupt, as for a voice hold.
                conductor._background(conductor._interrupt_reply, "phone tap")
            elif cmd == "rename" and conductor is not None and conductor.identity is not None:
                result = conductor.identity.rename(str(c.get("name", "")))
                if result.get("ok"):
                    server.bus.emit("assistant_name", name=result["name"])
            elif cmd == "answer":
                bridge.answer(c.get("id"), c.get("result") or {})
            elif cmd == "build_progress" and conductor is not None:
                conductor.build_progress(c.get("task"), c.get("detail", ""))
            elif cmd == "build_done" and conductor is not None:
                conductor.build_done(c.get("task"), c.get("status", "failed"), c.get("result", ""))
        # Violet Agent closed the link: stop Rose.
        os.kill(os.getpid(), 15)

    threading.Thread(target=commands, name="violet-frames", daemon=True).start()
    servers = rose_app.build_servers(server, args, None)
    frames.event(ev="dashboard", url=f"http://127.0.0.1:{port}")
    print(f"[violet] Rose for the VioletOS phone; profile {profile}; dashboard http://127.0.0.1:{port}", flush=True)
    try:
        asyncio.run(rose_app.serve_all(servers, shutdown=server.shutdown))
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
