"""A Mac stand-in for the iPhone companion's Violet Shield, for headless tests.

    build/shield/venv/bin/python tools/shield-trainer/companion_sim.py --port 1080 [--model] [--record trace.jsonl]

Speaks the companion's protocols exactly: SOCKS5 with username/password (the
relay QEMU's net_relay.py dials with VIOLET_NET_SOCKS), `sentinel.violet:7000`
answered locally, and signed `VSH1` Shields (vsh1.py, the dev key pinned in the
QEMU image). With --model it scores windows with the trained student in PyTorch
(the iPhone runs the CoreML export of the same weights); without it, Tier 0
only. --record writes every Sentinel record as JSON lines (train.py reads
build/shield/qemu-traces/*.jsonl as real benign traces). Status goes to
--status as JSON, like the app's Documents/shield-status.json.
"""
from __future__ import annotations

import argparse
import json
import socket
import struct
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import vsh1  # noqa: E402
from schema import ACTIONS, TASK_PREFIX, THREAT_TAGS, VERDICTS  # noqa: E402
from windows import WINDOW_KINDS, AppTrack, Record, add_record, window_for  # noqa: E402


class Companion:
    def __init__(self, args):
        self.args = args
        self.key = vsh1.load_key(args.key)
        self.lock = threading.RLock()
        self.tracks: dict[int, AppTrack] = {}
        self.flows: list[dict] = []
        self.reported: dict[str, float] = {}
        self.blocked: set[str] = set()
        self.decisions: list[dict] = []
        self.link = None
        self.link_up_since = None
        self.seq = int(time.time() * 1000)
        self.next_id = 5000
        self.pending: dict[int, str] = {}
        self.stats = {"events": 0, "windows_scored": 0, "shields_sent": 0, "shields_accepted": 0, "shields_rejected": 0, "heartbeat": ""}
        self.record_file = open(args.record, "a") if args.record else None
        self.host_map = json.loads(args.host_map) if args.host_map else {}
        self.contained: set[int] = set()
        self.dirty: dict[int, float] = {}
        self.model = None
        if args.model:
            self.model = StudentModel()

    # ---------------- SOCKS5 ----------------
    def serve(self):
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", self.args.port))
        srv.listen(32)
        print(f"companion_sim: SOCKS5 on 127.0.0.1:{self.args.port}", flush=True)
        threading.Thread(target=self.ticker, daemon=True).start()
        while True:
            c, _ = srv.accept()
            threading.Thread(target=self.session, args=(c,), daemon=True).start()

    @staticmethod
    def exact(c, n):
        b = b""
        while len(b) < n:
            d = c.recv(n - len(b))
            if not d:
                raise ConnectionError("closed")
            b += d
        return b

    def session(self, c):
        try:
            head = self.exact(c, 2)
            self.exact(c, head[1])
            c.sendall(b"\x05\x02")
            v, ul = self.exact(c, 2)
            user = self.exact(c, ul)
            pl = self.exact(c, 1)[0]
            pw = self.exact(c, pl)
            if pw.decode() != self.args.password:
                c.sendall(b"\x01\x01")
                return c.close()
            c.sendall(b"\x01\x00")
            ver, cmd, _, atyp = self.exact(c, 4)
            if atyp == 3:
                host = self.exact(c, self.exact(c, 1)[0]).decode()
            elif atyp == 1:
                host = socket.inet_ntoa(self.exact(c, 4))
            else:
                return c.close()
            port = struct.unpack(">H", self.exact(c, 2))[0]
            if host == "sentinel.violet":
                c.sendall(b"\x05\x00\x00\x01" + bytes(6))
                return self.sentinel(c)
            if host == "time.violet":
                c.sendall(b"\x05\x00\x00\x01" + bytes(6) + struct.pack(">I", int(time.time())))
                return c.close()
            target = f"{host.lower()}:{port}"
            with self.lock:
                if host.lower() in self.blocked or target in self.blocked:
                    c.sendall(b"\x05\x02\x00\x01" + bytes(6))
                    return c.close()
                flow = {"target": target, "started": time.time(), "up": 0, "down": 0, "sock": c, "flagged": False}
                self.flows.append(flow)
            dial = self.host_map.get(host.lower(), f"{host}:{port}")
            dh, _, dp = dial.rpartition(":")
            try:
                up = socket.create_connection((dh, int(dp)), timeout=8)
            except OSError:
                c.sendall(b"\x05\x05\x00\x01" + bytes(6))
                return c.close()
            c.sendall(b"\x05\x00\x00\x01" + bytes(6))
            self.pipe(c, up, flow)
        except (ConnectionError, OSError):
            try:
                c.close()
            except OSError:
                pass

    def pipe(self, a, b, flow):
        def one(src, dst, key):
            try:
                while True:
                    d = src.recv(65536)
                    if not d:
                        break
                    flow[key] += len(d)
                    dst.sendall(d)
            except OSError:
                pass
            for s in (src, dst):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
        threading.Thread(target=one, args=(b, a, "down"), daemon=True).start()
        one(a, b, "up")

    # ---------------- sentinel.violet ----------------
    def sentinel(self, c):
        with self.lock:
            self.link = c
            self.link_up_since = time.time()
        print("companion_sim: Sentinel linked", flush=True)
        buf = b""
        try:
            while True:
                d = c.recv(65536)
                if not d:
                    break
                buf += d
                while len(buf) >= 5:
                    kind, n = buf[0], struct.unpack("<I", buf[1:5])[0]
                    if len(buf) < 5 + n:
                        break
                    payload, buf = buf[5:5 + n], buf[5 + n:]
                    self.frame(kind, payload.decode(errors="replace"))
        except OSError:
            pass
        with self.lock:
            if self.link is c:
                self.link = None
                self.link_up_since = None
        print("companion_sim: Sentinel link closed", flush=True)

    def frame(self, kind, text):
        with self.lock:
            if kind == 0x01:
                self.stats["hello"] = text
                for m, v, t, wo, xo in vsh1.BASELINE:
                    self.send_shield(vsh1.OP_INSTALL, 3600, payload=vsh1.rule_payload(m, v, t, wo, xo), what=f"baseline {t}")
            elif kind == 0x02:
                for line in text.splitlines():
                    r = Record.parse(line)
                    if not r:
                        continue
                    self.stats["events"] += 1
                    if self.record_file:
                        self.record_file.write(json.dumps(r.as_dict()) + "\n")
                        self.record_file.flush()
                    t = self.tracks.get(r.pid) or AppTrack(r.pid, r.app, r.sha8)
                    self.tracks[r.pid] = t
                    add_record(t, r)
                    if r.kind == "SOCK_CONN":
                        self.reported[r.target.lower()] = time.time()
                    if r.kind in WINDOW_KINDS:
                        self.dirty.setdefault(r.pid, time.time())
                    if r.kind == "EXIT":
                        self.dirty.pop(r.pid, None)
                    if r.kind == "THAWED":
                        self.contained.discard(r.pid)
            elif kind == 0x03:
                self.stats["heartbeat"] = text
            elif kind == 0x04:
                f = text.split()
                what = self.pending.pop(int(f[1]), "shield") if len(f) > 1 and f[1].isdigit() else "shield"
                self.stats["shields_accepted" if f[0] == "ok" else "shields_rejected"] += 1
                self.note(f"VioletOS {'applied' if f[0] == 'ok' else 'rejected'} {what} {' '.join(f[2:]) if f[0] != 'ok' else ''}".strip())
            self.write_status()

    def note(self, s):
        print("companion_sim:", s, flush=True)
        if self.decisions:
            self.decisions[0]["note"] = (self.decisions[0].get("note", "") + "; " + s).strip("; ")

    def send_shield(self, op, ttl, pid=0, payload=b"", what=""):
        with self.lock:
            if not self.link:
                return
            self.seq = max(self.seq + 1, int(time.time() * 1000))
            self.next_id += 1
            env = vsh1.envelope(self.key, op, self.next_id, self.seq, ttl, pid=pid, payload=payload)
            self.pending[self.seq] = what
            self.link.sendall(bytes([0x10]) + struct.pack("<I", len(env)) + env)
            self.stats["shields_sent"] += 1

    # ---------------- scoring ----------------
    def ticker(self):
        while True:
            time.sleep(0.25)
            with self.lock:
                now = time.time()
                self.unreported(now)
                for pid, since in list(self.dirty.items()):
                    t = self.tracks[pid]
                    quiet = now - since > 0.3
                    if quiet or now - since > 1.0:
                        del self.dirty[pid]
                        self.evaluate(t)
                self.write_status()

    def evaluate(self, t: AppTrack):
        seen = {f["target"] for f in self.flows if time.time() - f["started"] < 180}
        w = window_for(t, seen, [])
        d = {"time": time.time(), "pid": t.pid, "app": t.name, "window": w.to_canonical_text()}
        for ev in w.events:
            if ev.kind == "OPEN_W" and (ev.detail.startswith(("/bin/", "/lib/")) or
                                         (ev.detail.startswith("/apps/") and ev.detail.split("/")[2] != t.name)):
                d.update(tier="tier0", verdict="exploit_or_persistence", p_malicious=1.0, action="freeze_app", tags=["persistence_write"])
                break
        else:
            if not self.model:
                return
            out = self.model.score(w)
            self.stats["windows_scored"] += 1
            d.update(tier="model", **out)
            if out["verdict_probs"][0] >= 0.9:
                for ev in w.events:
                    if ev.kind == "SOCK_CONN" and ev.detail not in t.known_hosts:
                        t.known_hosts.append(ev.detail)
        hosts = [ev.detail for ev in w.events if ev.kind == "SOCK_CONN" and ev.detail not in t.known_hosts]
        d["hosts"] = sorted(set(hosts))
        d["contained"] = False
        mal = d["p_malicious"] >= 0.9 and d["action"] in ("freeze_app", "block_host")
        if mal and t.pid not in self.contained:
            self.contained.add(t.pid)
            d["contained"] = True
            def cut(targets=[h.lower() for h in d["hosts"]]):
                for f in self.flows:
                    if f["target"] in targets:
                        try:
                            f["sock"].shutdown(socket.SHUT_RDWR)
                            f["sock"].close()
                        except OSError:
                            pass
            for h in d["hosts"]:
                self.blocked.add(h.lower())
                self.blocked.add(h.rsplit(":", 1)[0].lower())
            # Cut the live upload just after the freeze, as the app does.
            threading.Timer(1.0 if d["action"] == "freeze_app" and self.link else 0, cut).start()
            if d["action"] == "freeze_app":
                self.send_shield(vsh1.OP_FREEZE, self.args.ttl, pid=t.pid, what=f"freeze of {t.name}")
            for h in d["hosts"]:
                self.send_shield(vsh1.OP_INSTALL, self.args.ttl, payload=vsh1.rule_payload(vsh1.MATCH_HOST, vsh1.VERDICT_DENY, h.rsplit(":", 1)[0]),
                                 what=f"DENY {h}")
        print(f"companion_sim: {d['tier']} {t.name} pid {t.pid}: {d['verdict']} p_mal={d['p_malicious']:.2f} {d['action']}"
              f"{' CONTAINED' if d['contained'] else ''}", flush=True)
        self.decisions.insert(0, d)
        del self.decisions[30:]

    def unreported(self, now):
        if not self.link_up_since or now - self.link_up_since < 5:
            return
        for f in self.flows:
            if f["flagged"] or f["started"] < self.link_up_since + 2 or now - f["started"] < 3:
                continue
            f["flagged"] = True
            r = self.reported.get(f["target"])
            if r and abs(r - f["started"]) < 15:
                continue
            host = f["target"].rsplit(":", 1)[0]
            self.blocked.update({host, f["target"]})
            self.send_shield(vsh1.OP_INSTALL, self.args.ttl, payload=vsh1.rule_payload(vsh1.MATCH_HOST, vsh1.VERDICT_DENY, host), what=f"DENY {host}")
            self.decisions.insert(0, {"time": now, "pid": 0, "app": "unattributed", "tier": "relay", "verdict": "exfil_or_c2", "p_malicious": 1.0,
                                      "action": "block_host", "tags": ["socks_sentinel_mismatch"], "hosts": [f["target"]], "contained": True,
                                      "note": "relay saw a connection the Sentinel never reported"})
            print(f"companion_sim: UNREPORTED connection {f['target']} blocked", flush=True)

    def write_status(self):
        if not self.args.status:
            return
        s = dict(self.stats, link_up=self.link is not None, blocked=sorted(self.blocked),
                 decisions=[{k: v for k, v in d.items() if k != "window"} for d in self.decisions])
        Path(self.args.status).write_text(json.dumps(s, indent=1))


class StudentModel:
    """The trained student in PyTorch (same weights the CoreML export carries)."""

    def __init__(self):
        import torch
        from train import Head, load_embedder, novelty  # noqa: F401
        self.torch = torch
        self.emb, self.tok = load_embedder()
        self.head = Head()
        self.head.load_state_dict(torch.load(ROOT / "build/shield/head.pt"))
        self.head.eval()

    def score(self, w):
        torch = self.torch
        count = lambda s: len(self.tok(s)["input_ids"])
        text = w.fit_text(count)
        from train import tokenize
        b = tokenize(self.tok, [text])
        t0 = time.time()
        with torch.no_grad():
            e = self.emb(b["input_ids"], b["attention_mask"])
            side = torch.tensor([w.to_side_features(0.5)], dtype=torch.float32)
            pv, pa, pt = self.head(e, side)
        pv, pa, pt = pv[0].tolist(), pa[0].tolist(), pt[0].tolist()
        return {"verdict": VERDICTS[max(range(4), key=lambda i: pv[i])], "p_malicious": 1 - pv[0], "verdict_probs": pv,
                "action": ACTIONS[max(range(4), key=lambda i: pa[i])], "tags": [t for t, p in zip(THREAT_TAGS, pt) if p >= 0.5],
                "latency_ms": int((time.time() - t0) * 1000)}


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=1080)
    ap.add_argument("--password", default="violet-dev")
    ap.add_argument("--key", default=str(ROOT / "build/shield/companion-signing.pem"))
    ap.add_argument("--model", action="store_true")
    ap.add_argument("--record")
    ap.add_argument("--status")
    ap.add_argument("--ttl", type=int, default=30)
    ap.add_argument("--host-map", default="")
    Companion(ap.parse_args()).serve()
