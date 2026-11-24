"""RFC skeleton: Mac Teacher -> Student LoRA + Decision Head training & CoreML export.

Trains a compact local threat detector (ModernBERT-base ~150M or Qwen3-0.6B)
with a LoRA adapter + 3-head linear/MLP decision head distilled from a frontier
Teacher model's soft logits, then merges the LoRA weights and exports an
INT8/INT4 CoreML `.mlpackage` for Apple Neural Engine (ANE) execution inside
`companion/ios/`.

No cloud API key is shipped to or used by the iPhone at runtime.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from schema import ACTIONS, THREAT_TAGS, VERDICTS, TeacherLabel, sample_seed_windows


@dataclass
class TrainConfig:
    # Bidirectional encoder (~150M, ~100-160 MB CoreML ANE) is safest under iOS
    # background Jetsam; causal LM (~360M-600M) can also decode short rationales
    # in foreground when the user taps an alert.
    backbone_id: str = "answerdotai/ModernBERT-base"
    max_seq_len: int = 384
    lora_rank: int = 16
    lora_alpha: int = 32
    lora_targets: tuple[str, ...] = ("Wqkv", "Wo", "Wi")
    learning_rate: float = 2e-4
    distill_temperature: float = 2.0
    focal_gamma: float = 2.0
    epochs: int = 4
    quant_mode: str = "int8_ane"  # "int8_ane" | "int4_palettized"
    output_mlpackage: Path = Path("build/shield/VioletShieldStudent.mlpackage")


def distill_loss_formula_doc() -> str:
    """Loss combines soft Teacher distillation with rare-attack focal weighting:

        L = alpha * T^2 * KL(softmax(z_s / T) || p_teacher_verdict)
          + beta  * KL(softmax(a_s / T)       || p_teacher_action)
          + gamma * FocalBCE(sigmoid(t_s), p_teacher_tags)

    After training, fit scalar temperature `tau` on a held-out calibration split
    so `P(malicious) >= 0.90` corresponds to >= 90% empirical precision.
    """
    return distill_loss_formula_doc.__doc__ or ""


def load_active_learning_feedback(feedback_jsonl: Path) -> list[dict]:
    """Ingest user-labeled alerts (`Documents/shield-feedback.jsonl`) synced from
    the iPhone (`[Benign — Allow & Learn]` / `[Confirmed Threat]`) so the Teacher
    can re-verify hard negatives and refresh the LoRA adapter for your phone.
    """
    if not feedback_jsonl.exists():
        return []
    import json

    return [json.loads(line) for line in feedback_jsonl.read_text().splitlines() if line.strip()]


def train_and_export(cfg: TrainConfig, dataset: list[TeacherLabel]) -> Path:
    """End-to-end Mac Apple Silicon pipeline (RFC skeleton).

    1. Load `cfg.backbone_id` and attach LoRA adapters (`r=cfg.lora_rank`) +
       a 3-head decision head (`verdict_head`, `action_head`, `tag_head`) over
       the pooled/CLS/EOS hidden state.
    2. Train with KL soft-logit distillation + focal BCE on `dataset` (benign
       QEMU/handset traces + synthetic red-team attacks + iPhone feedback).
    3. Run temperature scaling on the validation set to calibrate decision thresholds.
    4. Merge LoRA deltas directly into base weights (`W' = W + (alpha/r) * B @ A`)
       for zero runtime LoRA overhead.
    5. Trace single-forward-pass `forward(input_ids, attention_mask) ->
       (verdict_probs, action_probs, tag_probs)` and export with `coremltools`
       targeting iOS Apple Neural Engine (`compute_precision=FLOAT16` + W8/W4).
    """
    assert len(VERDICTS) == 4 and len(ACTIONS) == 4 and len(THREAT_TAGS) == 8
    _ = dataset
    raise NotImplementedError(
        f"RFC skeleton: wire PyTorch/MPS or MLX training + coremltools export -> {cfg.output_mlpackage}"
    )


if __name__ == "__main__":
    # Smoke-check the canonical serializer on the built-in seed scenarios.
    for name, win in sample_seed_windows():
        print(f"=== {name} ===")
        print(win.to_canonical_text())
        print()
