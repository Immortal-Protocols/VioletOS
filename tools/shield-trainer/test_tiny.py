"""Checks the on-device scorer's checked-in files against each other (make test-unit).

    python3 tools/shield-trainer/test_tiny.py

Standard library only, well under a second:
- tiny.py (the reference) reproduces every window in fixtures/tiny_scorer.txt,
- runtime/open/scorer_model.rs is exactly what train_tiny.py generates from
  tiny_model.json (nobody edited the weights or thresholds by hand),
- the thresholds are ordered and the model says what it was trained on.
The Rust side of the same fixtures is `scorer_matches_python_reference`.
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import tiny  # noqa: E402
import train_tiny  # noqa: E402


def main():
    text = tiny.MODEL_JSON.read_text()
    sha = hashlib.sha256(text.encode()).hexdigest()
    q = json.loads(text)
    model = tiny.Model(q)
    assert 0 < model.t_warn <= model.t_contain, (model.t_warn, model.t_contain)
    assert "synthetic" in q["trained_on"]
    assert q["features"] == tiny.FEATURES
    generated = (HERE.parents[1] / "runtime/open/scorer_model.rs").read_text()
    assert generated == train_tiny.rust(q, sha), "runtime/open/scorer_model.rs is stale: run make shield-train-tiny"

    windows, actions = 0, [0, 0, 0, 0]
    w = tiny.Window()
    f: list[int] = []
    for line in (HERE / "fixtures/tiny_scorer.txt").read_text().splitlines():
        tag, _, rest = line.partition(" ")
        if tag == "#" and "model" in rest:
            assert rest.split()[-1] == sha[:8], "fixtures were written by another model"
        elif tag == "W":
            w = tiny.Window()
        elif tag == "R":
            r = rest.split("\t")
            assert len(r) == 9, r
            w.add_record(r[4], int(r[5]), int(r[6]), r[7], r[8])
        elif tag == "F":
            f = w.features()
            assert f == [int(v) for v in rest.split()], (windows, f)
            assert all(0 <= v <= 32 for v in f)
        elif tag == "L":
            assert model.logits(f) == [int(v) for v in rest.split()], windows
        elif tag == "A":
            action = tiny.decide(model, model.logits(f), f, True, w.facts)
            assert action == int(rest), windows
            assert tiny.decide(model, model.logits(f), f, False, w.facts) <= tiny.WARN, "never contained unless eligible"
            actions[action] += 1
            windows += 1
    assert windows >= 100 and all(actions), (windows, actions)

    # The record parser's edges agree with scorer_replay.rs `split_target`.
    e = tiny.event("SOCK_CONN", 0, "", "host:+80")
    assert e.bits & tiny.BIT_ODD_PORT and e.labels == 1
    assert tiny.event("SOCK_TX", 0, "12B", "").bytes == 12
    assert tiny.event("SOCK_TX", 0, "x", "a.test:443").bytes == 0
    assert tiny.event("SHIELD_HIT", 0, "deny 1 OPEN_W", "") is None
    assert tiny.event("EXEC", 0, "/bin/python3 /tmp/x.py", "").bits == tiny.BIT_TMP_EXEC
    assert tiny.event("EXEC", 0, "/apps/x/tmp/", "").bits == 0
    # The phone records 40 bytes of a command line: what is past that does not count.
    assert tiny.event("EXEC", 0, "/bin/python3 /apps/averyveryverylongname/main.py /tmp/x.py", "").bits == 0
    assert tiny.event("EXEC", 0, "/bin/python3 /apps/averyverylongnam/m.py /tmp/x.py", "").bits == 0
    assert tiny.event("OPEN_W", tiny.FLAG_CROSS, "/apps/home/" + "x" * 60, "").bits == tiny.BIT_SYSTEM

    # A full window gives up plain opens first, and the facts outlast the window.
    w = tiny.Window()
    w.add_record("OPEN_R", 6, 0, "/data/notes/keys.txt", "")
    for n in range(40):
        w.add_record("OPEN_R", 4, 0, f"/data/photosync/{n}", "")
    w.add_record("SOCK_CONN", 5, 0, "185.220.101.4:4444", "185.220.101.4:4444")
    w.add_record("SOCK_TX", 5, 0, "1048576B", "185.220.101.4:4444")
    f = w.features()
    assert (f[tiny.F_EVENTS], f[tiny.F_XREAD]) == (16, 1) and f[tiny.F_TX_AFTER] >= 17 and tiny.corroborated(f, w.facts)
    assert tiny.decide(model, model.logits(f), f, True, w.facts) == tiny.QUARANTINE
    w = tiny.Window()
    w.add_record("OPEN_R", 6, 0, "/data/notes/keys.txt", "")
    for n in range(16):
        w.add_record("SOCK_CONN", 5, 0, f"h{n}.test:443", f"h{n}.test:443")
    w.add_record("SOCK_TX", 5, 0, "1048576B", "185.220.101.4:4444")
    f = w.features()
    assert f[tiny.F_XREAD] == 0 and f[tiny.F_TX_AFTER] >= 17 and w.facts.tx_after == 1048576 and tiny.corroborated(f, w.facts)
    print(f"tiny scorer: {windows} fixture windows reproduced (model {sha[:8]}, warn {model.t_warn}, contain {model.t_contain})")


if __name__ == "__main__":
    main()
