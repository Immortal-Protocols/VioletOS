"""Train the on-device scorer and write the files the phone and the tests use.

    python3 tools/shield-trainer/train_tiny.py [--quick]      (make shield-train-tiny)

Standard library only: no torch, no download, about two minutes. It

1. generates randomized windows with scenarios.py (the same synthetic
   generator the companion's model is trained on: there is no real-world
   training data yet, see docs/violet-shield-poc.md),
2. turns each into Sentinel records and then into the 24 integer features of
   tiny.py (the reference for runtime/open/scorer.rs),
3. trains a 24 -> 12 -> 4 network in floating point, rounds it to int8
   weights, and from then on measures only the integer network,
4. picks the two thresholds on a calibration split it did not train on,
5. reports accuracy and false positives on a test split and on two scenario
   families it never saw, and
6. writes tools/shield-trainer/tiny_model.json, the generated
   runtime/open/scorer_model.rs, and tools/shield-trainer/fixtures/tiny_scorer.txt
   (windows with the features and logits the Rust scorer must reproduce).

All three outputs are checked in, so building an image never runs this.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import random
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import tiny  # noqa: E402
from scenarios import Gen  # noqa: E402
from schema import VERDICTS  # noqa: E402

HELD_OUT = ["attack_slow_drip_exfil", "hard_negative_user_upload"]
# The phone has no second view of the network, so it cannot see these.
RELAY_ONLY = {"attack_socks_mismatch"}
RELAY_ONLY_GENERATORS = {"attack_mismatch", "attack_unreported_sentinel_drop"}
V = {v: i for i, v in enumerate(VERDICTS)}


def records(window, rng, pid=100):
    """A synthetic window as the Sentinel would have reported it. Synthetic
    sends carry no target; the phone's do, so they get the last connection's.
    On the phone a process's window starts with its EXEC until 16 later events
    push that out, which the generator rarely shows: half the windows get one."""
    flags = (tiny.FLAG_BG if window.background else 0) | (tiny.FLAG_NEW if window.new_hash else 0) \
        | (tiny.FLAG_DEV if window.dev_push_recent else 0)
    name = window.app_path.rsplit("/", 1)[-1]
    target = ""
    out = []
    events = list(window.events)
    if rng.random() < 0.5 and not (events and events[0].kind == "EXEC"):
        line = f"/apps/{name}" if rng.random() < 0.6 else f"/bin/python3 /apps/{name}/main.py"
        lead = rng.choice([200, 600, 1600, 2600])
        events = [type(events[0])(0, "EXEC", line)] + [type(e)(e.dt_ms + lead, e.kind, e.detail, e.cross_app, e.socks5_seen) for e in events] \
            if events else events
    for ev in events:
        if ev.kind == "SOCK_CONN":
            target = ev.detail
        t = target if ev.kind in ("SOCK_CONN", "SOCK_TX") else ""
        out.append((ev.dt_ms, pid, name, window.sha_prefix, ev.kind, flags | (tiny.FLAG_CROSS if ev.cross_app else 0),
                    window.user_idle_sec, ev.detail, t))
    return out


def window(recs):
    w = tiny.Window()
    for r in recs:
        w.add_record(r[4], r[5], r[6], r[7], r[8])
    return w


def features(recs):
    return window(recs).features()


def dataset(seed: int, n_apps: int, held_out: bool = False):
    """(family, verdict index, records) for apps the way train.py builds them."""
    rng = random.Random(seed)
    gen = Gen(seed)
    out = []
    attacks = [a for a in Gen.ATTACKS if a not in RELAY_ONLY_GENERATORS]
    relay = []
    for _ in range(n_apps):
        app = gen.identity(new=rng.random() < 0.2)
        if held_out:
            s = gen.sample(rng.choice(HELD_OUT), app)
            out.append((s.family, V[s.verdict], records(s.window, rng)))
            continue
        for _ in range(rng.randint(1, 3)):
            s = gen.sample(rng.choice(Gen.BENIGN), app)
            recs = records(s.window, rng)
            out.append((s.family, V[s.verdict], recs))
            # The phone scores a window while it is still filling (a connection
            # before its first send, say); the generator only shows whole ones.
            if len(recs) > 2 and rng.random() < 0.5:
                out.append((s.family, V[s.verdict], recs[:rng.randint(1, len(recs) - 1)]))
        for _ in range(rng.randint(0, 2)):
            s = gen.sample(rng.choice(attacks), app)
            # The generator's exploits always run on an idle phone. On a real
            # one they can start the moment the app is tapped: half get that.
            if V[s.verdict] >= 2 and rng.random() < 0.5:
                s.window.user_idle_sec = rng.randint(0, 5)
            out.append((s.family, V[s.verdict], records(s.window, rng)))
        if rng.random() < 0.1:
            s = gen.sample(rng.choice(sorted(RELAY_ONLY_GENERATORS)), app)
            relay.append((s.family, V[s.verdict], records(s.window, rng)))
    return out, relay


def train(rows, hidden, epochs, seed):
    """Plain SGD with momentum on cross-entropy; inputs are features / 16."""
    rng = random.Random(seed)
    F, C = tiny.F, tiny.CLASSES
    w1 = [[rng.gauss(0, 0.3) for _ in range(F)] for _ in range(hidden)]
    b1 = [0.0] * hidden
    w2 = [[rng.gauss(0, 0.3) for _ in range(hidden)] for _ in range(C)]
    b2 = [0.0] * C
    counts = [sum(1 for _, y in rows if y == c) for c in range(C)]
    weight = [len(rows) / (C * max(1, n)) for n in counts]
    vw1 = [[0.0] * F for _ in range(hidden)]
    vb1 = [0.0] * hidden
    vw2 = [[0.0] * hidden for _ in range(C)]
    vb2 = [0.0] * C
    order = list(range(len(rows)))
    for epoch in range(epochs):
        rng.shuffle(order)
        lr = 0.02 * (0.5 ** (epoch / max(1, epochs / 3)))
        loss = 0.0
        for n in order:
            x, y = rows[n]
            pre = [b + sum(w * v for w, v in zip(row, x)) for row, b in zip(w1, b1)]
            h = [p if p > 0 else 0.0 for p in pre]
            z = [b + sum(w * v for w, v in zip(row, h)) for row, b in zip(w2, b2)]
            m = max(z)
            e = [math.exp(v - m) for v in z]
            total = sum(e)
            p = [v / total for v in e]
            loss -= math.log(max(p[y], 1e-12)) * weight[y]
            dz = [(p[c] - (1.0 if c == y else 0.0)) * weight[y] for c in range(C)]
            dh = [0.0] * hidden
            for c in range(C):
                g = dz[c]
                row, vrow = w2[c], vw2[c]
                for j in range(hidden):
                    dh[j] += g * row[j]
                    vrow[j] = 0.9 * vrow[j] - lr * (g * h[j] + 1e-4 * row[j])
                    row[j] += vrow[j]
                vb2[c] = 0.9 * vb2[c] - lr * g
                b2[c] += vb2[c]
            for j in range(hidden):
                if pre[j] <= 0:
                    continue
                g = dh[j]
                row, vrow = w1[j], vw1[j]
                for k in range(F):
                    vrow[k] = 0.9 * vrow[k] - lr * (g * x[k] + 1e-4 * row[k])
                    row[k] += vrow[k]
                vb1[j] = 0.9 * vb1[j] - lr * g
                b1[j] += vb1[j]
        print(f"  epoch {epoch + 1:2d}/{epochs} loss {loss / len(rows):.4f}", file=sys.stderr)
    return w1, b1, w2, b2


def quantize(w1, b1, w2, b2, feats):
    """int8 weights; the hidden layer is shifted to fit 0..127 on the training set."""
    a = 127 / max(abs(v) for row in w1 for v in row)
    q1 = [[round(v * a) for v in row] for row in w1]
    qb1 = [round(b * a * 16) for b in b1]
    top = max(max(0, bias + sum(w * x for w, x in zip(row, f))) for f in feats for row, bias in zip(q1, qb1))
    shift = 0
    while (top >> shift) > 127:
        shift += 1
    g = 16 * a / (1 << shift)
    c = 127 / max(abs(v) for row in w2 for v in row)
    q2 = [[round(v * c) for v in row] for row in w2]
    qb2 = [round(b * c * g) for b in b2]
    return {"w1": q1, "b1": qb1, "shift1": shift, "w2": q2, "b2": qb2}


def wilson(k: int, n: int, z: float = 1.96):
    """95% Wilson interval for k of n."""
    if n == 0:
        return 0.0, 1.0
    p = k / n
    d = 1 + z * z / n
    mid = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return max(0.0, mid - half), min(1.0, mid + half)


def measure(model, rows):
    """Per family: windows, right verdicts, warnings, and containments if the
    program were one the phone may contain (not from the boot image)."""
    by = {}
    for family, y, recs in rows:
        w = window(recs)
        f = w.features()
        logits = model.logits(f)
        d = by.setdefault(family, {"n": 0, "right": 0, "warn": 0, "contain": 0, "verdict": VERDICTS[y]})
        d["n"] += 1
        d["right"] += int(tiny.verdict(logits) == y)
        action = tiny.decide(model, logits, f, True, w.facts)
        d["warn"] += int(action >= tiny.WARN)
        d["contain"] += int(action >= tiny.QUARANTINE)
    return by


def summary(by):
    benign = [d for d in by.values() if d["verdict"] == "benign"]
    bad = [d for d in by.values() if d["verdict"] in ("exfil_or_c2", "exploit_or_persistence")]
    n_b, n_m = sum(d["n"] for d in benign), sum(d["n"] for d in bad)
    warn_b, contain_b = sum(d["warn"] for d in benign), sum(d["contain"] for d in benign)
    out = {
        "windows": sum(d["n"] for d in by.values()),
        "accuracy": round(sum(d["right"] for d in by.values()) / max(1, sum(d["n"] for d in by.values())), 4),
        "benign_windows": n_b,
        "benign_warned": warn_b,
        "benign_warn_rate_95ci": [round(v, 4) for v in wilson(warn_b, n_b)],
        "benign_contained": contain_b,
        "benign_contain_rate_95ci": [round(v, 4) for v in wilson(contain_b, n_b)],
        "malicious_windows": n_m,
        "malicious_warned": sum(d["warn"] for d in bad),
        "malicious_contained": sum(d["contain"] for d in bad),
        "families": by,
    }
    return out


def rust(model: dict, sha: str) -> str:
    def rows(m):
        return "\n".join("    [" + ", ".join(str(v) for v in row) + "]," for row in m)
    return f"""//! GENERATED by tools/shield-trainer/train_tiny.py: do not edit by hand.
//! Run `make shield-train-tiny` to regenerate; the weights come from
//! tools/shield-trainer/tiny_model.json (sha256 {sha}).
//!
//! Trained on synthetic windows only (docs/violet-shield-poc.md).
pub const FEATURE_VERSION: u32 = {tiny.FEATURE_VERSION};
pub const F: usize = {tiny.F};
pub const H: usize = {tiny.H};
/// First four bytes of the model file's SHA-256, for telemetry.
pub const MODEL_ID: u32 = 0x{sha[:8]};
pub const SHIFT1: u32 = {model['shift1']};
/// Warn when a malicious verdict leads benign and suspicious by this much.
pub const T_WARN: i32 = {model['t_warn']};
/// Contain (with a second sign) when it leads by this much.
pub const T_CONTAIN: i32 = {model['t_contain']};
pub static W1: [[i8; F]; H] = [
{rows(model['w1'])}
];
pub static B1: [i32; H] = [{", ".join(str(v) for v in model['b1'])}];
pub static W2: [[i8; H]; 4] = [
{rows(model['w2'])}
];
pub static B2: [i32; 4] = [{", ".join(str(v) for v in model['b2'])}];
"""


def fixtures(model, rows, limit=120):
    """Windows with the features and logits the Rust scorer must reproduce."""
    rng = random.Random(7)
    picked = rng.sample(rows, min(limit, len(rows)))
    lines = [f"# feature_version {tiny.FEATURE_VERSION} model {model.sha[:8]}",
             "# W family / R record (tab separated) / F features / L logits / A action when eligible"]
    for family, _, recs in picked:
        lines.append(f"W {family}")
        for r in recs:
            lines.append("R " + "\t".join(str(v) for v in r))
        w = window(recs)
        f = w.features()
        logits = model.logits(f)
        lines.append("F " + " ".join(str(v) for v in f))
        lines.append("L " + " ".join(str(v) for v in logits))
        lines.append(f"A {tiny.decide(model, logits, f, True, w.facts)}")
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="a small run to check the script (does not write the model)")
    ap.add_argument("--report", default=str(ROOT / "build/shield/tiny-report.json"))
    args = ap.parse_args()
    n_train, epochs = (150, 4) if args.quick else (1500, 30)
    train_rows, _ = dataset(11, n_train)
    calib_rows, _ = dataset(12, n_train // 2)
    test_rows, relay_rows = dataset(13, n_train // 2)
    held_rows, _ = dataset(14, max(60, n_train // 5), held_out=True)
    print(f"windows: train {len(train_rows)} calibration {len(calib_rows)} test {len(test_rows)} unseen {len(held_rows)}", file=sys.stderr)
    feats = [features(r) for _, _, r in train_rows]
    w = train([([v / 16 for v in f], y) for f, (_, y, _) in zip(feats, train_rows)], tiny.H, epochs, 5)
    q = quantize(*w, feats)
    q.update(feature_version=tiny.FEATURE_VERSION, t_warn=0, t_contain=0)
    model = tiny.Model(q)
    # Thresholds from the calibration split, measured on the integer network.
    benign = sorted(tiny.margin(model.logits(features(r))) for _, y, r in calib_rows if y == 0)
    # Warn: above 99.9% of the benign calibration windows. Contain: above all
    # of them, with a quarter of the spread again as slack.
    q["t_warn"] = max(1, benign[min(len(benign) - 1, int(len(benign) * 0.999))] + 1)
    spread = benign[-1] - benign[len(benign) // 2]
    q["t_contain"] = max(q["t_warn"], benign[-1] + 1 + spread // 4)
    q["trained_on"] = "synthetic windows from tools/shield-trainer/scenarios.py (no real-world data)"
    q["features"] = tiny.FEATURES
    text = json.dumps(q, indent=1, sort_keys=True) + "\n"
    sha = hashlib.sha256(text.encode()).hexdigest()
    q["sha"] = sha
    model = tiny.Model(q)
    report = {
        "model_sha256": sha, "t_warn": q["t_warn"], "t_contain": q["t_contain"], "shift1": q["shift1"],
        "data": "synthetic (scenarios.py); seeds 11 train, 12 calibration, 13 test, 14 unseen families",
        "calibration": summary(measure(model, calib_rows)),
        "test": summary(measure(model, test_rows)),
        "unseen_families": summary(measure(model, held_rows)),
        "relay_only_not_visible_on_device": summary(measure(model, relay_rows)),
    }
    for split in ("calibration", "test", "unseen_families", "relay_only_not_visible_on_device"):
        s = report[split]
        print(f"{split}: {s['windows']} windows, verdict accuracy {s['accuracy']:.3f}; benign warned {s['benign_warned']}/{s['benign_windows']}"
              f" (95% CI {s['benign_warn_rate_95ci'][0]:.3f}-{s['benign_warn_rate_95ci'][1]:.3f}), benign contained {s['benign_contained']}/{s['benign_windows']}"
              f" (upper bound {s['benign_contain_rate_95ci'][1]:.3f}); malicious warned {s['malicious_warned']}/{s['malicious_windows']},"
              f" contained {s['malicious_contained']}/{s['malicious_windows']}", file=sys.stderr)
        for family, d in sorted(s["families"].items()):
            print(f"    {family:28s} n={d['n']:4d} right={d['right']:4d} warn={d['warn']:4d} contain={d['contain']:4d}", file=sys.stderr)
    Path(args.report).parent.mkdir(parents=True, exist_ok=True)
    Path(args.report).write_text(json.dumps(report, indent=1) + "\n")
    if args.quick:
        print("quick run: model files not written", file=sys.stderr)
        return
    tiny.MODEL_JSON.write_text(text)
    (ROOT / "runtime/open/scorer_model.rs").write_text(rust(q, sha))
    (HERE / "fixtures").mkdir(exist_ok=True)
    (HERE / "fixtures/tiny_scorer.txt").write_text(fixtures(model, test_rows + held_rows))
    (HERE / "fixtures/tiny_report.json").write_text(json.dumps(report, indent=1) + "\n")
    print(f"wrote tiny_model.json ({sha[:8]}), runtime/open/scorer_model.rs, fixtures/tiny_scorer.txt", file=sys.stderr)


if __name__ == "__main__":
    main()
