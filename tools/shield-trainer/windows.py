"""Sentinel records -> per-app `violet-log-v1` windows.

A record is one tab-separated line from the Sentinel (services/sentinel/mod.rs):
    t_ms pid app sha8 kind flags idle_s detail target
`WindowBuilder` keeps each app's recent records and the relay's own view of
connections, and formats a window exactly like `ShieldWindowBuilder` in
companion/mac/VioletAgent/Shield/ShieldWindows.swift.
"""
from __future__ import annotations

from dataclasses import dataclass, field

from schema import MAX_EVENTS, MAX_SOCKS_ONLY, TelemetryWindow, TraceEvent

# Only the kinds the student was trained on reach a window.
WINDOW_KINDS = {"EXEC", "OPEN_R", "OPEN_W", "SOCK_CONN", "SOCK_TX", "MEM_WX"}
FLAG_BG, FLAG_CROSS, FLAG_NEW, FLAG_DEV = 1, 2, 4, 16


@dataclass
class Record:
    t_ms: int
    pid: int
    app: str
    sha8: str
    kind: str
    flags: int
    idle_s: int
    detail: str
    target: str

    @staticmethod
    def parse(line: str) -> "Record | None":
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 9:
            return None
        try:
            return Record(int(parts[0]), int(parts[1]), parts[2], parts[3], parts[4], int(parts[5]), int(parts[6]), parts[7], parts[8])
        except ValueError:
            return None

    def as_dict(self):
        return self.__dict__.copy()


@dataclass
class AppTrack:
    pid: int
    name: str
    sha8: str
    records: list[Record] = field(default_factory=list)
    known_hosts: list[str] = field(default_factory=list)


def add_record(track: AppTrack, r: Record):
    """Keep the last 64 records, merging a send into the previous one to the same target."""
    last = track.records[-1] if track.records else None
    if r.kind == "SOCK_TX" and last is not None and last.kind == "SOCK_TX" and last.target == r.target \
            and last.detail.endswith("B") and r.detail.endswith("B") and last.detail[:-1].isdigit() and r.detail[:-1].isdigit():
        merged = Record(**{**last.__dict__, "detail": f"{int(last.detail[:-1]) + int(r.detail[:-1])}B", "flags": r.flags, "idle_s": r.idle_s})
        track.records[-1] = merged
    else:
        track.records.append(r)
    del track.records[:-64]


def window_for(track: AppTrack, socks_seen: set[str], unreported: list[tuple[str, int, int]]) -> TelemetryWindow:
    shown = [r for r in track.records if r.kind in WINDOW_KINDS]
    last = track.records[-1] if track.records else None
    base = shown[0].t_ms if shown else 0
    events = []
    for r in shown:
        seen = None
        if r.kind in ("SOCK_CONN", "SOCK_TX"):
            seen = r.target in socks_seen
        events.append(TraceEvent(max(0, r.t_ms - base), r.kind, r.detail, cross_app=bool(r.flags & FLAG_CROSS), socks5_seen=seen))
    flags = last.flags if last else 0
    return TelemetryWindow(
        app_path=f"/apps/{track.name}",
        sha_prefix=track.sha8,
        new_hash=bool(flags & FLAG_NEW),
        background=bool(flags & FLAG_BG),
        user_idle_sec=last.idle_s if last else 0,
        dev_push_recent=bool(flags & FLAG_DEV),
        known_hosts=track.known_hosts[:8],
        events=events,
        socks5_only_flows=unreported[-MAX_SOCKS_ONLY:],
    )


def windows_from_records(records: list[dict]):
    """Every window a companion would have scored while these records arrived
    (one per app per record that can change a verdict). Used for QEMU traces,
    where the relay saw every connection the guest reported."""
    tracks: dict[int, AppTrack] = {}
    out = []
    for d in records:
        r = Record(**d)
        t = tracks.get(r.pid)
        if t is None:
            t = tracks[r.pid] = AppTrack(r.pid, r.app, r.sha8)
        add_record(t, r)
        if r.kind in WINDOW_KINDS:
            seen = {x.target for x in t.records if x.target}
            out.append((f"/apps/{t.name}@{t.sha8}", window_for(t, seen, [])))
    return out
