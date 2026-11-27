"""Stage 1 of the RFC's model plan, end to end on the Mac.

    build/shield/venv/bin/python tools/shield-trainer/train.py [--quick]

1. Generate randomized `violet-log-v1` windows (scenarios.py) plus any benign
   traces recorded from QEMU (build/shield/qemu-traces/*.jsonl, written by
   companion_sim.py --record).
2. Embed each window once with frozen EmbeddingGemma 2 (text-only, 270M,
   float32: the model card warns that float16 breaks it).
3. Give every window a per-app novelty score: the smallest cosine distance
   between its first 256 (Matryoshka) dimensions and that app's benign history.
4. Train the hybrid head on `[embedding 768 | side features 9]` with soft
   teacher targets (KL at temperature T) and focal BCE for tags, then fit a
   temperature on a calibration split.
5. Evaluate on an in-distribution test split and on two scenario families the
   head never saw, with ablations; write build/shield/report.json.
6. Export build/shield/VioletShieldEmbedder.mlpackage (int8 weights, float32
   compute), build/shield/VioletShieldHead.mlpackage, the tokenizer files and
   fixtures the Swift tests compare against.
"""
from __future__ import annotations

import argparse
import json
import math
import random
import shutil
import sys
import time
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
from scenarios import Gen, Sample, soft_label  # noqa: E402
from schema import ACTIONS, SEQ_LEN, TASK_PREFIX, THREAT_TAGS, VERDICTS, TelemetryWindow, TraceEvent  # noqa: E402

OUT = ROOT / "build/shield"
MODEL_ID = "google/embeddinggemma-2"
NOVELTY_DIM = 256
NO_HISTORY = 0.5  # novelty when an app has no benign history yet (ShieldChecker.swift agrees)
HELD_OUT = ["attack_slow_drip_exfil", "hard_negative_user_upload"]
SIDE_NAMES = ["min_cosine_dist", "is_bg", "is_new_hash", "dev_push", "log1p_user_idle", "cross_app_count",
              "socks5_mismatch_count", "wx_count", "log1p_tx_bytes"]


# ---------------------------------------------------------------- data
def build_dataset(seed: int, n_apps: int):
    rng = random.Random(seed)
    gen = Gen(seed)
    samples: list[Sample] = []
    history: dict[str, list[int]] = {}
    for _ in range(n_apps):
        app = gen.identity(new=rng.random() < 0.2)
        # Benign history the companion would have recorded for this app.
        h = 0 if app.new_hash else rng.choice([0, 2, 4, 8, 12, 16])
        for _ in range(h):
            fam = rng.choice(["benign_stream", "benign_local", "benign_stream"])
            s = gen.sample(fam, app)
            s.family = "history"
            history.setdefault(app.key, []).append(len(samples))
            samples.append(s)
        for _ in range(rng.randint(1, 3)):
            samples.append(gen.sample(rng.choice(Gen.BENIGN), app))
        for _ in range(rng.randint(0, 2)):
            samples.append(gen.sample(rng.choice(Gen.ATTACKS), app))
    # The unseen families, for evaluation only.
    for _ in range(max(40, n_apps // 5)):
        app = gen.identity()
        h = rng.choice([0, 4, 8])
        for _ in range(h):
            s = gen.sample("benign_stream", app)
            s.family = "history"
            history.setdefault(app.key, []).append(len(samples))
            samples.append(s)
        samples.append(gen.sample(rng.choice(HELD_OUT), app))
    return samples, history


def qemu_traces():
    """Benign windows recorded from the real Open Runtime in QEMU."""
    from windows import windows_from_records

    out = []
    # Only benign captures (companion_sim.py --record, saved as benign-*.jsonl; there is
    # no make target for it yet): demo runs include the attacks.
    for path in sorted((OUT / "qemu-traces").glob("benign-*.jsonl")):
        records = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
        for app_key, win in windows_from_records(records):
            out.append((app_key, win))
    return out


# ---------------------------------------------------------------- embedder
class Embedder(nn.Module):
    """EmbeddingGemma 2 text path, mean-pooled and normalized, as exported to CoreML."""

    def __init__(self, model):
        super().__init__()
        self.m = model

    def forward(self, ids, mask):
        add = (1.0 - mask.to(torch.float32))[:, None, None, :] * -1e9
        h = self.m.language_model(input_ids=ids, attention_mask={"full_attention": add, "sliding_attention": add}).last_hidden_state
        mf = mask.unsqueeze(-1).to(torch.float32)
        e = (h * mf).sum(1) / mf.sum(1).clamp(min=1)
        return F.normalize(e, dim=-1)


def load_embedder():
    from transformers import AutoConfig, AutoModel, AutoTokenizer

    cfg = AutoConfig.from_pretrained(MODEL_ID)
    cfg.vision_config = None
    cfg.audio_config = None
    model = AutoModel.from_pretrained(MODEL_ID, config=cfg, dtype=torch.float32, attn_implementation="eager").eval()
    return Embedder(model).eval(), AutoTokenizer.from_pretrained(MODEL_ID)


def tokenize(tok, texts):
    return tok([TASK_PREFIX + t for t in texts], padding="max_length", truncation=True, max_length=SEQ_LEN, return_tensors="pt")


def embed_all(emb, tok, texts, device):
    emb = emb.to(device)
    out, lengths = [], []
    for i in range(0, len(texts), 32):
        b = tokenize(tok, texts[i:i + 32])
        lengths += b["attention_mask"].sum(1).tolist()
        with torch.no_grad():
            out.append(emb(b["input_ids"].to(device), b["attention_mask"].to(device)).float().cpu())
        if i % 640 == 0:
            print(f"  embedded {i + len(texts[i:i + 32])}/{len(texts)}", flush=True)
    return torch.cat(out), lengths


def novelty(e: torch.Tensor, bank: torch.Tensor | None) -> float:
    if bank is None or len(bank) == 0:
        return NO_HISTORY
    q = F.normalize(e[:NOVELTY_DIM], dim=0)
    b = F.normalize(bank[:, :NOVELTY_DIM], dim=1)
    return float((1 - b @ q).clamp(min=0).min())


# ---------------------------------------------------------------- head
class Head(nn.Module):
    """2-layer MLP over [embedding | standardized side features]; outputs calibrated probabilities."""

    def __init__(self, hidden=128, side=9, use_side=True):
        super().__init__()
        self.use_side = use_side
        self.register_buffer("side_mean", torch.zeros(side))
        self.register_buffer("side_std", torch.ones(side))
        self.register_buffer("tau", torch.ones(1))
        self.body = nn.Sequential(nn.Linear(768 + side, hidden), nn.GELU(), nn.Dropout(0.1), nn.Linear(hidden, hidden), nn.GELU())
        self.verdict = nn.Linear(hidden, len(VERDICTS))
        self.action = nn.Linear(hidden, len(ACTIONS))
        self.tags = nn.Linear(hidden, len(THREAT_TAGS))

    def logits(self, emb, side):
        s = (side - self.side_mean) / self.side_std
        if not self.use_side:
            s = torch.zeros_like(s)
        h = self.body(torch.cat([emb, s], dim=-1))
        return self.verdict(h), self.action(h), self.tags(h)

    def forward(self, embedding, side_features):
        v, a, t = self.logits(embedding, side_features)
        return F.softmax(v / self.tau, -1), F.softmax(a / self.tau, -1), torch.sigmoid(t)


def focal_bce(logits, target, gamma=2.0):
    p = torch.sigmoid(logits)
    ce = F.binary_cross_entropy_with_logits(logits, target, reduction="none")
    pt = p * target + (1 - p) * (1 - target)
    return (ce * (1 - pt) ** gamma).mean()


def train_head(X, S, Yv, Ya, Yt, use_side=True, epochs=80, T=2.0, seed=0):
    torch.manual_seed(seed)
    head = Head(use_side=use_side)
    head.side_mean.copy_(S.mean(0))
    head.side_std.copy_(S.std(0).clamp(min=1e-3))
    opt = torch.optim.AdamW(head.parameters(), lr=2e-3, weight_decay=1e-2)
    n = len(X)
    for ep in range(epochs):
        head.train()
        perm = torch.randperm(n)
        for i in range(0, n, 128):
            idx = perm[i:i + 128]
            v, a, t = head.logits(X[idx], S[idx])
            loss = (T * T * F.kl_div(F.log_softmax(v / T, -1), Yv[idx], reduction="batchmean")
                    + 0.5 * T * T * F.kl_div(F.log_softmax(a / T, -1), Ya[idx], reduction="batchmean")
                    + 2.0 * focal_bce(t, Yt[idx]))
            opt.zero_grad()
            loss.backward()
            opt.step()
    head.eval()
    return head


def fit_temperature(head, X, S, y):
    with torch.no_grad():
        v, _, _ = head.logits(X, S)
    best, best_tau = 1e9, 1.0
    for tau in np.linspace(0.5, 4.0, 71):
        nll = float(F.cross_entropy(v / tau, y))
        if nll < best:
            best, best_tau = nll, float(tau)
    head.tau.fill_(best_tau)
    return best_tau


def metrics(head, X, S, y_true, mal_threshold=0.9):
    with torch.no_grad():
        pv, pa, pt = head(X, S)
    pred = pv.argmax(1)
    mal_true = y_true != 0
    p_mal = 1 - pv[:, 0]
    flagged = p_mal >= mal_threshold
    tp = int((flagged & mal_true).sum())
    fp = int((flagged & ~mal_true).sum())
    fn = int((~flagged & mal_true).sum())
    benign_n = int((~mal_true).sum())
    out = {
        "n": len(y_true),
        "accuracy_4class": float((pred == y_true).float().mean()),
        "auto_contain_threshold": mal_threshold,
        "precision_at_threshold": tp / (tp + fp) if tp + fp else None,
        "recall_at_threshold": tp / (tp + fn) if tp + fn else None,
        "false_positive_rate": fp / benign_n if benign_n else None,
        "auroc_malicious": auroc(p_mal.numpy(), mal_true.numpy()),
    }
    return out


def auroc(scores, labels):
    labels = labels.astype(bool)
    pos, neg = scores[labels], scores[~labels]
    if len(pos) == 0 or len(neg) == 0:
        return None
    order = np.argsort(np.concatenate([pos, neg]))
    ranks = np.empty(len(order))
    ranks[order] = np.arange(1, len(order) + 1)
    return float((ranks[: len(pos)].sum() - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg)))


def per_family(head, X, S, y, fams):
    with torch.no_grad():
        pv, _, _ = head(X, S)
    out = {}
    for f in sorted(set(fams)):
        idx = [i for i, g in enumerate(fams) if g == f]
        p_mal = (1 - pv[idx, 0])
        out[f] = {"n": len(idx), "mean_p_malicious": round(float(p_mal.mean()), 3),
                  "flagged_at_0.9": round(float((p_mal >= 0.9).float().mean()), 3),
                  "accuracy": round(float((pv[idx].argmax(1) == y[idx]).float().mean()), 3)}
    return out


# ---------------------------------------------------------------- export
def export(emb, tok, head, fixtures):
    import coremltools as ct
    import numpy as _np
    from coremltools.converters.mil import Builder as mb
    from coremltools.converters.mil.frontend.torch import ops as _ops

    # numpy 2 no longer turns one-element arrays into Python ints; coremltools 9 relies on it.
    orig = _ops._cast

    def _cast(context, node, dtype, dtype_name):
        x = _ops._get_inputs(context, node, expected=1)[0]
        if x.can_be_folded_to_const() and isinstance(x.val, _np.ndarray) and x.val.size == 1:
            context.add(mb.const(val=dtype(x.val.reshape(()).item()), name=node.name), node.name)
            return
        return orig(context, node, dtype, dtype_name)

    _ops._cast = _cast
    emb = emb.cpu().eval()
    b = tokenize(tok, ["APP=/apps/x SHA=00000000 NEW_HASH=0 BG=0 USER_IDLE=0s DEV_PUSH=0"])
    ids, mask = b["input_ids"].int(), b["attention_mask"].int()
    traced = torch.jit.trace(emb, (ids, mask))
    ml = ct.convert(traced, inputs=[ct.TensorType("input_ids", shape=(1, SEQ_LEN), dtype=np.int32),
                                    ct.TensorType("attention_mask", shape=(1, SEQ_LEN), dtype=np.int32)],
                    outputs=[ct.TensorType("embedding")], compute_precision=ct.precision.FLOAT32,
                    minimum_deployment_target=ct.target.iOS18, convert_to="mlprogram")
    cfg = ct.optimize.coreml.OptimizationConfig(global_config=ct.optimize.coreml.OpLinearQuantizerConfig(mode="linear_symmetric", dtype="int8"))
    ml_q = ct.optimize.coreml.linear_quantize_weights(ml, cfg)
    ml_q.short_description = "EmbeddingGemma 2 text encoder (270M), mean-pooled, L2-normalized; int8 weights, float32 compute."
    path = OUT / "VioletShieldEmbedder.mlpackage"
    if path.exists():
        shutil.rmtree(path)
    ml_q.save(str(path))

    head = head.eval()
    th = torch.jit.trace(head, (torch.zeros(1, 768), torch.zeros(1, 9)))
    mh = ct.convert(th, inputs=[ct.TensorType("embedding", shape=(1, 768)), ct.TensorType("side_features", shape=(1, 9))],
                    outputs=[ct.TensorType("verdict_probs"), ct.TensorType("action_probs"), ct.TensorType("tag_probs")],
                    compute_precision=ct.precision.FLOAT32, minimum_deployment_target=ct.target.iOS18, convert_to="mlprogram")
    mh.short_description = "Violet Shield hybrid decision head (Stage 1)."
    hp = OUT / "VioletShieldHead.mlpackage"
    if hp.exists():
        shutil.rmtree(hp)
    mh.save(str(hp))

    # Check the exported pair against PyTorch on the fixture windows.
    checks = []
    for fx in fixtures:
        b = tokenize(tok, [fx["text"]])
        e_ml = ml_q.predict({"input_ids": b["input_ids"].int().numpy(), "attention_mask": b["attention_mask"].int().numpy()})["embedding"]
        with torch.no_grad():
            e_pt = emb(b["input_ids"], b["attention_mask"]).numpy()
        cos = float((e_ml * e_pt).sum() / np.linalg.norm(e_ml) / np.linalg.norm(e_pt))
        out = mh.predict({"embedding": e_ml.astype(np.float32), "side_features": np.array([fx["side"]], dtype=np.float32)})
        fx["token_ids"] = b["input_ids"][0][: int(b["attention_mask"].sum())].tolist()
        fx["embedding_head"] = [round(float(x), 5) for x in e_ml[0][:8]]
        fx["verdict_probs"] = [round(float(x), 4) for x in out["verdict_probs"][0]]
        checks.append(cos)
    tok_dir = OUT / "ShieldTokenizer"
    tok_dir.mkdir(exist_ok=True)
    tok.save_pretrained(str(tok_dir))
    (OUT / "ShieldFixtures.json").write_text(json.dumps(fixtures, indent=1))
    return {"int8_vs_fp32_cosine_min": min(checks), "int8_vs_fp32_cosine_mean": sum(checks) / len(checks),
            "embedder_mb": round(sum(f.stat().st_size for f in path.rglob("*") if f.is_file()) / 1e6, 1),
            "head_kb": round(sum(f.stat().st_size for f in hp.rglob("*") if f.is_file()) / 1e3, 1)}


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="small dataset, no export")
    ap.add_argument("--apps", type=int, default=900)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    samples, history = build_dataset(args.seed, 120 if args.quick else args.apps)
    real = qemu_traces()
    print(f"{len(samples)} synthetic windows, {len(real)} QEMU windows", flush=True)

    rng = random.Random(args.seed)
    device = "mps" if torch.backends.mps.is_available() else "cpu"
    emb, tok = load_embedder()
    count = lambda t: len(tok(t)["input_ids"])
    texts = [s.window.fit_text(count) for s in samples] + [w.fit_text(count) for _, w in real]
    E, lengths = embed_all(emb, tok, texts, device)
    emb = emb.cpu()
    trunc = sum(1 for l in lengths if l >= SEQ_LEN)
    print(f"embedded in {time.time() - t0:.0f}s; tokens p50 {int(np.median(lengths))} p95 {int(np.percentile(lengths, 95))} max {max(lengths)}; cut at {SEQ_LEN} {trunc}")

    rows = []  # (index, family, side, verdict, action, tags)
    for i, s in enumerate(samples):
        if s.family == "history":
            continue
        bank_idx = [j for j in history.get(s.app.key, []) if j != i]
        bank = E[bank_idx] if bank_idx else None
        nov = novelty(E[i], bank)
        v, a, t = soft_label(s, rng)
        rows.append((i, s.family, s.window.to_side_features(nov), v, a, t, s.verdict))
    for k, (_, w) in enumerate(real):
        i = len(samples) + k
        from scenarios import Sample as _S, AppIdentity
        s = _S("benign_qemu_real", AppIdentity("qemu", w.sha_prefix, []), w, "benign", "allow", confidence=0.95)
        v, a, t = soft_label(s, rng)
        rows.append((i, "benign_qemu_real", w.to_side_features(NO_HISTORY), v, a, t, "benign"))

    def tensors(rs):
        X = E[[r[0] for r in rs]]
        S = torch.tensor([r[2] for r in rs], dtype=torch.float32)
        Yv = torch.tensor([r[3] for r in rs])
        Ya = torch.tensor([r[4] for r in rs])
        Yt = torch.tensor([r[5] for r in rs])
        y = torch.tensor([VERDICTS.index(r[6]) for r in rs])
        return X, S, Yv, Ya, Yt, y

    held = [r for r in rows if r[1] in HELD_OUT]
    rest = [r for r in rows if r[1] not in HELD_OUT]
    rng.shuffle(rest)
    n = len(rest)
    train, calib, test = rest[: int(n * 0.7)], rest[int(n * 0.7): int(n * 0.8)], rest[int(n * 0.8):]
    Xtr, Str, Yvtr, Yatr, Yttr, _ = tensors(train)
    Xc, Sc, _, _, _, yc = tensors(calib)
    Xte, Ste, _, _, _, yte = tensors(test)
    Xh, Sh, _, _, _, yh = tensors(held)

    head = train_head(Xtr, Str, Yvtr, Yatr, Yttr)
    tau = fit_temperature(head, Xc, Sc, yc)
    no_side = train_head(Xtr, Str, Yvtr, Yatr, Yttr, use_side=False)
    fit_temperature(no_side, Xc, Sc, yc)

    # Novelty alone, on attacks by apps that have a benign history.
    nov_scores, nov_labels = [], []
    for r in test + held:
        s = samples[r[0]] if r[0] < len(samples) else None
        if s is None or not history.get(s.app.key):
            continue
        nov_scores.append(r[2][0])
        nov_labels.append(r[6] != "benign")
    report = {
        "backbone": MODEL_ID + " (text-only, frozen, float32)",
        "head": "MLP 777->128->128 -> verdict(4) action(4) tags(8)",
        "teacher": "scenario generator with soft targets (no frontier model)",
        "windows": {"train": len(train), "calibration": len(calib), "test": len(test), "held_out_families": len(held),
                    "qemu_real": len(real)},
        "tokens": {"p50": int(np.median(lengths)), "p95": int(np.percentile(lengths, 95)), "max": int(max(lengths)),
                   "seq_len": SEQ_LEN, "cut_at_seq_len": trunc},
        "temperature": tau,
        "test_in_distribution": metrics(head, Xte, Ste, yte),
        "test_held_out_families": metrics(head, Xh, Sh, yh),
        "held_out_per_family": per_family(head, Xh, Sh, yh, [r[1] for r in held]),
        "test_per_family": per_family(head, Xte, Ste, yte, [r[1] for r in test]),
        "ablation_no_side_features": {"in_distribution": metrics(no_side, Xte, Ste, yte),
                                      "held_out_families": metrics(no_side, Xh, Sh, yh)},
        "novelty_alone_auroc": auroc(np.array(nov_scores), np.array(nov_labels)) if nov_scores else None,
        "novelty_alone_n": len(nov_scores),
    }
    if real:
        Xr, Sr, _, _, _, yr = tensors([r for r in rows if r[1] == "benign_qemu_real"])
        report["qemu_real_benign"] = metrics(head, Xr, Sr, yr)
    print(json.dumps(report["test_in_distribution"], indent=1))
    print(json.dumps(report["test_held_out_families"], indent=1))

    if not args.quick:
        fixtures = []
        picks = {}
        for r in test + held:
            picks.setdefault(r[1], r)
        for fam, r in sorted(picks.items()):
            s_text = texts[r[0]]
            fixtures.append({"family": fam, "text": s_text, "side": [float(x) for x in r[2]]})
        report["export"] = export(emb, tok, head, fixtures)
        torch.save(head.state_dict(), OUT / "head.pt")
    report["seconds"] = round(time.time() - t0)
    (OUT / "report.json").write_text(json.dumps(report, indent=1))
    print("report:", OUT / "report.json")


if __name__ == "__main__":
    main()
