"""RFC skeleton: Mac Teacher -> Student Hybrid Head + Optional LoRA pipeline.

Recommended base: **EmbeddingGemma 2 (270M text+code slice: 130M transformer +
140M embedder, 8K context, 768-dim Matryoshka embeddings)** paired with:
  - **Per-app 256-dim Matryoshka cosine novelty** (unsupervised zero-day detector)
  - **Explicit 9-dim structured side-features** (`schema.py:to_side_features`)
  - **2-layer Hybrid MLP decision head** (`768 + 9 -> verdict, action, threat_tags`)

Two-stage rollout:
  - **Stage 1 (`frozen_head`):** Freeze EmbeddingGemma 2 on iPhone; train only the
    <500 KB Hybrid MLP head in seconds from Teacher soft logits.
  - **Stage 2 (`lora_distill`):** Optionally attach a light LoRA (`r=8..16`) to the
    130M transformer with a cosine-anchor regularizer so subtle evasion/typosquat
    cues sharpen without distorting the base Matryoshka novelty space.

No cloud API key is shipped to or used by the iPhone at runtime.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from schema import ACTIONS, THREAT_TAGS, VERDICTS, TeacherLabel, sample_seed_windows


@dataclass
class TrainConfig:
    # EmbeddingGemma 2 modular text+code slice (130M transformer + 140M embedder,
    # ~150-250 MB quantized on iPhone ANE, native code/URL/path understanding).
    backbone_id: str = "google/embeddinggemma-2-270m-text-code"
    stage: str = "frozen_head"  # "frozen_head" (Stage 1) | "lora_distill" (Stage 2)
    max_seq_len: int = 512
    embedding_dim: int = 768
    matryoshka_novelty_dim: int = 256
    side_feature_dim: int = 9
    head_hidden_dim: int = 256
    lora_rank: int = 12
    lora_alpha: int = 24
    cosine_anchor_weight: float = 0.25  # preserves Matryoshka novelty geometry in Stage 2
    learning_rate: float = 3e-4
    distill_temperature: float = 2.0
    focal_gamma: float = 2.0
    epochs: int = 6
    quant_mode: str = "int8_ane"  # "int8_ane" | "int4_palettized"
    output_embedder: Path = Path("build/shield/EmbeddingGemma2_TextCode.mlpackage")
    output_head: Path = Path("build/shield/VioletShieldHead.mlpackage")


def distill_loss_formula_doc() -> str:
    """Loss combines soft Teacher distillation, rare-attack focal weighting, and
    (in Stage 2) a cosine anchor preserving EmbeddingGemma 2's novelty geometry:

        h = concat([emb_768, side_features_9])
        L = alpha * T^2 * KL(softmax(z_s(h) / T) || p_teacher_verdict)
          + beta  * KL(softmax(a_s(h) / T)       || p_teacher_action)
          + gamma * FocalBCE(sigmoid(t_s(h)), p_teacher_tags)
          + lambda_anchor * (1 - cos(emb_lora[:256], emb_base[:256]))  # Stage 2 only

    After training, fit scalar temperature `tau` on a held-out calibration split
    so `P(malicious) >= 0.90` corresponds to >= 90% empirical precision.
    """
    return distill_loss_formula_doc.__doc__ or ""


def load_active_learning_feedback(feedback_jsonl: Path) -> list[dict]:
    """Ingest user-labeled alerts (`Documents/shield-feedback.jsonl`) synced from
    the iPhone (`[Benign — Allow & Learn]` / `[Confirmed Threat]`) so the Teacher
    can re-verify hard negatives and refresh the <500 KB head (or Stage 2 LoRA).
    """
    if not feedback_jsonl.exists():
        return []
    import json

    return [json.loads(line) for line in feedback_jsonl.read_text().splitlines() if line.strip()]


def train_and_export(cfg: TrainConfig, dataset: list[TeacherLabel]) -> tuple[Path, Path]:
    """End-to-end Mac Apple Silicon pipeline (RFC skeleton).

    Stage 1 (`frozen_head`):
      1. Cache 768-dim embeddings from frozen EmbeddingGemma 2 (270M text+code)
         over all QEMU/handset benign traces, red-team attacks, and iPhone feedback.
      2. Compute per-app 256-dim Matryoshka cosine novelty + 9-dim side-features.
      3. Train the 2-layer Hybrid MLP head (`768 + 9 -> verdict, action, tags`)
         with KL soft-logit + focal BCE loss, then temperature-calibrate.
      4. Export the <500 KB head (`cfg.output_head`) independently of the stock
         quantized embedder (`cfg.output_embedder`).

    Stage 2 (`lora_distill`):
      1. Attach light LoRA (`r=cfg.lora_rank`) to the 130M transformer layers.
      2. Jointly fine-tune LoRA + Hybrid MLP head with `cosine_anchor_weight`.
      3. Merge LoRA (`W' = W + (alpha/r) * B @ A`) and re-export quantized ANE pkg.
    """
    assert len(VERDICTS) == 4 and len(ACTIONS) == 4 and len(THREAT_TAGS) == 8
    if cfg.stage != "frozen_head":
        raise NotImplementedError("Stage 2 (LoRA) is not built yet; see docs/violet-shield-poc.md")
    # Stage 1 is implemented in train.py (its own generator stands in for `dataset`).
    import train

    train.main()
    return train.OUT / "VioletShieldEmbedder.mlpackage", train.OUT / "VioletShieldHead.mlpackage"


if __name__ == "__main__":
    # Smoke-check the canonical serializer and 9-dim side-feature extractor.
    for name, win in sample_seed_windows():
        print(f"=== {name} ===")
        print(win.to_canonical_text())
        print("SIDE_FEATURES:", [round(x, 3) for x in win.to_side_features()])
        print()
