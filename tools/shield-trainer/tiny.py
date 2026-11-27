"""Reference implementation of the on-device scorer (runtime/open/scorer.rs).

Integers only, standard library only. The Rust scorer must agree with this
file bit for bit: `train_tiny.py` writes fixtures from it, and the host test
`scorer_matches_python_reference` (make test-unit) replays them through the
Rust code.

A window is 16 recent events of one process, built from Sentinel records
(`t_ms pid app sha8 kind flags idle_s detail target`, services/sentinel).
When it is full, plain opens of the app's own files leave first, and a few
facts about the process are kept whatever leaves (`Window.facts`).
From it come 24 small integer features; a 24 -> 12 -> 4 integer network
turns them into four logits, one per verdict in schema.VERDICTS.
"""
from __future__ import annotations

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODEL_JSON = HERE / "tiny_model.json"

FEATURE_VERSION = 1
F = 24
H = 12
CLASSES = 4
WINDOW = 16

# Record kinds that enter a window, with the phone's EventKind numbers.
KINDS = {"EXEC": 1, "OPEN_R": 2, "SOCK_CONN": 3, "SOCK_TX": 4, "MEM_WX": 5, "OPEN_W": 10}
FLAG_BG, FLAG_CROSS, FLAG_NEW, FLAG_DEV, FLAG_UDP = 1, 2, 4, 16, 32
# Event bits.
BIT_IP, BIT_ODD_PORT, BIT_SYSTEM, BIT_TMP_EXEC = 1, 2, 4, 8

FEATURES = [
    "background", "new_hash", "dev_push", "idle_bits", "events", "cross_reads", "cross_writes", "system_writes",
    "wx_maps", "execs", "tmp_execs", "connects", "destinations", "ip_connects", "odd_port_connects", "tx_bits",
    "tx_after_cross_read_bits", "host_digits", "host_hyphens", "host_labels", "repeat_connects", "small_sends",
    "udp", "background_tx_bits",
]
assert len(FEATURES) == F
(F_BG, F_NEW, F_DEV, F_IDLE, F_EVENTS, F_XREAD, F_XWRITE, F_SYSW, F_WX, F_EXEC, F_TMPEXEC, F_CONN, F_DEST, F_IP,
 F_ODD, F_TX, F_TX_AFTER, F_DIGITS, F_HYPHENS, F_LABELS, F_REPEAT, F_SMALL, F_UDP, F_BGTX) = range(F)


def fnv(data: bytes, h: int = 0x811C9DC5) -> int:
    for b in data:
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h


def under(path: str, folder: str) -> bool:
    return path == folder or path.startswith(folder + "/")


class Event:
    __slots__ = ("kind", "flags", "bits", "dest", "bytes", "digits", "hyphens", "labels")

    def __init__(self, kind, flags, bits=0, dest=0, nbytes=0, digits=0, hyphens=0, labels=0):
        self.kind, self.flags, self.bits, self.dest, self.bytes = kind, flags, bits, dest, nbytes
        self.digits, self.hyphens, self.labels = digits, hyphens, labels


def event(kind: str, flags: int, detail: str, target: str) -> "Event | None":
    """One record as the scorer keeps it; None for kinds a window ignores."""
    k = KINDS.get(kind)
    if k is None:
        return None
    flags &= 0xFF
    if k in (3, 4):
        host, _, port = target.rpartition(":")
        if not (port.isascii() and port.isdigit() and len(port) <= 5 and int(port) <= 65535):
            host, port = target, "0"
        port = int(port)
        raw = host.encode()[:40].lower()
        dest = fnv(bytes([port & 255, port >> 8]), fnv(raw))
        bits = 0
        if raw and all(c in b"0123456789." for c in raw):
            bits |= BIT_IP
        if port not in (80, 443):
            bits |= BIT_ODD_PORT
        nbytes = 0
        if k == 4:
            digits = detail[:-1]
            nbytes = min(int(digits), 0xFFFFFFFF) if detail.endswith("B") and digits.isascii() and digits.isdigit() else 0
        return Event(k, flags, bits, dest, nbytes, min(15, sum(48 <= c <= 57 for c in raw)), min(15, raw.count(b"-")),
                     min(15, raw.count(b".") + 1))
    # The phone records 40 bytes of a path or command line.
    detail = detail.encode()[:40].decode(errors="ignore")
    bits = 0
    if k == 10 and (under(detail, "/bin") or under(detail, "/lib") or (flags & FLAG_CROSS and detail.startswith("/apps/"))):
        bits |= BIT_SYSTEM
    if k == 1 and any(detail.startswith(p) or (" " + p) in detail for p in ("/tmp/", "/data/")):
        bits |= BIT_TMP_EXEC
    return Event(k, flags, bits)


def plain(e: Event) -> bool:
    """An open of the app's own files that says nothing by itself."""
    return e.kind in (2, 10) and not e.flags & FLAG_CROSS and not e.bits & BIT_SYSTEM


def crossing(e: Event) -> bool:
    return e.kind == 2 and bool(e.flags & FLAG_CROSS)


class Facts:
    """What is remembered about a process whatever leaves its window."""
    __slots__ = ("crossed", "system_write", "tx_after")

    def __init__(self):
        self.crossed, self.system_write, self.tx_after = False, False, 0


class Window:
    def __init__(self):
        self.events: list[Event] = []
        self.flags = 0
        self.idle_s = 0
        self.carried = False
        self.facts = Facts()

    def add(self, ev: Event, idle_s: int):
        self.flags, self.idle_s = ev.flags, idle_s
        if crossing(ev):
            self.facts.crossed = True
        if ev.kind == 10 and ev.bits & BIT_SYSTEM:
            self.facts.system_write = True
        if ev.kind == 4 and self.facts.crossed:
            self.facts.tx_after = min(0xFFFFFFFF, self.facts.tx_after + ev.bytes)
        last = self.events[-1] if self.events else None
        if ev.kind == 4 and last is not None and last.kind == 4 and last.dest == ev.dest:
            last.bytes = min(0xFFFFFFFF, last.bytes + ev.bytes)
            last.flags = ev.flags
            return
        if len(self.events) == WINDOW:
            # A full window gives up its oldest plain open; with none, a new
            # plain open is not kept and anything else replaces the oldest.
            out = next((n for n, e in enumerate(self.events) if plain(e)), None)
            if out is None:
                if plain(ev):
                    return
                out = 0
            self.carried |= crossing(self.events.pop(out))
        self.events.append(ev)

    def add_record(self, kind: str, flags: int, idle_s: int, detail: str, target: str) -> bool:
        ev = event(kind, flags, detail, target)
        if ev is None:
            return False
        self.add(ev, idle_s)
        return True

    def features(self) -> list[int]:
        f = [0] * F
        ev = self.events
        f[F_BG] = int(bool(self.flags & FLAG_BG))
        f[F_NEW] = int(bool(self.flags & FLAG_NEW))
        f[F_DEV] = int(bool(self.flags & FLAG_DEV))
        f[F_IDLE] = min(15, self.idle_s.bit_length())
        f[F_EVENTS] = len(ev)
        tx = tx_after = tx_bg = 0
        crossed = self.carried
        dests: list[int] = []
        for n, e in enumerate(ev):
            if e.kind == 2 and e.flags & FLAG_CROSS:
                f[F_XREAD] += 1
                crossed = True
            elif e.kind == 10:
                f[F_XWRITE] += int(bool(e.flags & FLAG_CROSS))
                f[F_SYSW] += int(bool(e.bits & BIT_SYSTEM))
            elif e.kind == 5:
                f[F_WX] += 1
            elif e.kind == 1:
                f[F_EXEC] += 1
                f[F_TMPEXEC] += int(bool(e.bits & BIT_TMP_EXEC))
            elif e.kind == 3:
                f[F_CONN] += 1
                f[F_IP] += int(bool(e.bits & BIT_IP))
                f[F_ODD] += int(bool(e.bits & BIT_ODD_PORT))
                f[F_DIGITS] = max(f[F_DIGITS], e.digits)
                f[F_HYPHENS] = max(f[F_HYPHENS], e.hyphens)
                f[F_LABELS] = max(f[F_LABELS], e.labels)
                f[F_REPEAT] = max(f[F_REPEAT], sum(1 for o in ev[:n + 1] if o.kind == 3 and o.dest == e.dest))
                f[F_UDP] |= int(bool(e.flags & FLAG_UDP))
            elif e.kind == 4:
                tx = min(0xFFFFFFFF, tx + e.bytes)
                if crossed:
                    tx_after = min(0xFFFFFFFF, tx_after + e.bytes)
                if e.flags & FLAG_BG:
                    tx_bg = min(0xFFFFFFFF, tx_bg + e.bytes)
                f[F_SMALL] += int(e.bytes < 512)
            if e.kind in (3, 4) and e.dest not in dests:
                dests.append(e.dest)
        f[F_DEST] = len(dests)
        f[F_TX], f[F_TX_AFTER], f[F_BGTX] = tx.bit_length(), tx_after.bit_length(), tx_bg.bit_length()
        for n in (F_XREAD, F_XWRITE, F_SYSW, F_WX, F_EXEC, F_TMPEXEC, F_CONN, F_DEST, F_IP, F_ODD, F_REPEAT, F_SMALL):
            f[n] = min(15, f[n])
        return f


class Model:
    def __init__(self, d: dict):
        assert d["feature_version"] == FEATURE_VERSION and len(d["w1"]) == H and len(d["w1"][0]) == F
        self.w1, self.b1, self.shift1 = d["w1"], d["b1"], d["shift1"]
        self.w2, self.b2 = d["w2"], d["b2"]
        self.t_warn, self.t_contain = d["t_warn"], d["t_contain"]
        self.sha = d.get("sha", "")

    @staticmethod
    def load(path: Path = MODEL_JSON) -> "Model":
        return Model(json.loads(Path(path).read_text()))

    def logits(self, f: list[int]) -> list[int]:
        hidden = []
        for row, bias in zip(self.w1, self.b1):
            acc = bias + sum(w * x for w, x in zip(row, f))
            hidden.append(min(127, max(0, acc) >> self.shift1))
        return [bias + sum(w * h for w, h in zip(row, hidden)) for row, bias in zip(self.w2, self.b2)]


def verdict(logits: list[int]) -> int:
    """Index of the largest logit (the first one on a tie)."""
    return max(range(CLASSES), key=lambda n: (logits[n], -n))


def margin(logits: list[int]) -> int:
    """How far the two malicious verdicts (exfil_or_c2, exploit_or_persistence)
    are ahead of benign and suspicious. "Suspicious" alone calls for nothing
    on the phone: telling a new host from a bad one takes the companion."""
    return max(logits[2], logits[3]) - max(logits[0], logits[1])


def corroborated(f: list[int], facts: Facts) -> bool:
    """A second, deterministic sign: a write to another app's program or a
    system file, or 64 KiB or more sent after reading another app's data by
    an app now in the background. Taken from the facts kept about the process
    (`Window.facts`), not from the window, so filling the window does not
    remove it. The model alone never contains anything."""
    return facts.system_write or (f[F_BG] == 1 and facts.tx_after >= 64 * 1024)


NONE, WARN, QUARANTINE, FREEZE = 0, 1, 2, 3


def decide(model: Model, logits: list[int], f: list[int], eligible: bool, facts: Facts) -> int:
    if margin(logits) < model.t_warn:
        return NONE
    if not eligible or margin(logits) < model.t_contain or not corroborated(f, facts):
        return WARN
    return FREEZE if verdict(logits) == 3 and facts.system_write else QUARANTINE
