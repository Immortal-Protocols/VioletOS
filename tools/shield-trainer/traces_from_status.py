"""Rebuild Sentinel record traces from a companion status file.

    python3 tools/shield-trainer/traces_from_status.py OUT_DIR LABEL STATUS.json [STATUS.json ...]

`companion_sim.py --status` and targets/qemu/test_shield.py (build/shield/last-status.json)
keep every scored window as text. This turns the windows of each process back
into the record lines the Sentinel sent (`t_ms pid app sha8 kind flags idle_s
detail target`), one file per process: OUT_DIR/benign/LABEL-APP-N.log for the
demo's ordinary apps and OUT_DIR/attack/... for photosync and themes.

What is exact: the events, their order, their times relative to the first one,
paths, hosts, byte counts and the cross-app mark. What is rebuilt: an event's
background flag is that of the first window that showed it, a send's target is
the connection before it, and pids are renumbered. Records the window never
showed (EXIT, SHIELD_HIT, FROZEN) are not in the files.

Complete traces are better when you can run the emulator: `companion_sim.py
--record` writes every record, and targets/qemu/test_shield_enforce.py saves its
run as build/shield/qemu-traces/enforce-last.log.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ATTACK = {"photosync", "themes"}
LINE = re.compile(r"\[\+(\d+)ms\] (\S+) (.*?)((?: CROSS_APP=1)?)((?: SOCKS5_SEEN=[01])?)$")


def parse(text: str):
    head = text.splitlines()[0]
    fields = dict(part.split("=", 1) for part in head.split())
    events = []
    for line in text.splitlines()[2:]:
        m = LINE.match(line)
        if m:
            events.append((int(m.group(1)), m.group(2), m.group(3), bool(m.group(4))))
    flags = (1 if fields["BG"] == "1" else 0) | (4 if fields["NEW_HASH"] == "1" else 0) | (16 if fields["DEV_PUSH"] == "1" else 0)
    return fields["SHA"], flags, int(fields["USER_IDLE"].rstrip("s")), events


def trace(windows, pid: int, name: str):
    """Record lines for one process from its windows, oldest first."""
    out, shown, target = [], [], ""
    for sha, flags, idle, events in windows:
        for n, (t, kind, detail, cross) in enumerate(events):
            if n < len(shown):
                old = shown[n]
                if kind == "SOCK_TX" and old[1] == "SOCK_TX" and detail != old[2]:
                    more = int(detail[:-1]) - int(old[2][:-1])
                    if more > 0:
                        out.append((t, kind, f"{more}B", flags, idle, target, sha))
                    shown[n] = (t, kind, detail, cross)
                continue
            shown.append((t, kind, detail, cross))
            if kind == "SOCK_CONN":
                target = detail
            out.append((t, kind, detail, flags | (2 if cross else 0), idle, target if kind in ("SOCK_CONN", "SOCK_TX") else "", sha))
    return ["\t".join(str(v) for v in (1000 + t, pid, name, sha, kind, flags, idle, detail, tgt)) for t, kind, detail, flags, idle, tgt, sha in out]


def main():
    out, label, files = Path(sys.argv[1]), sys.argv[2], sys.argv[3:]
    seen, count = set(), {}
    for path in files:
        runs = {}
        for d in sorted(json.loads(Path(path).read_text()).get("decisions", []), key=lambda d: d["time"]):
            if d.get("window") and d.get("pid"):
                runs.setdefault((d["pid"], d["app"]), []).append(parse(d["window"]))
        for (pid, app), windows in sorted(runs.items()):
            lines = trace(windows, 100, app)
            body = "\n".join(lines) + "\n"
            if len(lines) < 2 or body in seen:
                continue
            seen.add(body)
            n = count[app] = count.get(app, 0) + 1
            folder = out / ("attack" if app in ATTACK else "benign")
            folder.mkdir(parents=True, exist_ok=True)
            (folder / f"{label}-{app}-{n}.log").write_text(body)
            print(f"{folder.name}/{label}-{app}-{n}.log: {len(lines)} records")


if __name__ == "__main__":
    main()
