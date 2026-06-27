#!/usr/bin/env python3
"""Train the Synapse ONNX routing model.

The model predicts a single scalar, the *required quality* (0.0 to 1.0) for a
query. At inference the Rust gateway picks the cheapest capability-satisfying
model whose quality clears that bar (see crates/synapse-routing/src/strategy/
onnx.rs). Predicting a target instead of a model class keeps the model
decoupled from the model registry, so adding or removing models never
misaligns the output.

Two data sources:
  - bootstrap (default): synthetic queries labelled by distilling the existing
    heuristic routing signals. Ships a usable model before any traffic exists.
  - telemetry: real (features, achieved_min_quality) rows exported from
    ClickHouse once the gateway has logged routing outcomes (see
    clickhouse_schema.sql). This is where the model starts beating the
    heuristics.

The 8-feature input vector MUST stay byte-for-byte aligned with
`profile_to_features` in onnx.rs (same order, same normalization).

Usage:
    python train.py --out model.onnx                 # bootstrap
    python train.py --telemetry rows.csv --out model.onnx
"""

from __future__ import annotations

import argparse
import csv
import sys

import numpy as np
from skl2onnx import to_onnx
from skl2onnx.common.data_types import FloatTensorType
from sklearn.ensemble import GradientBoostingRegressor
from sklearn.metrics import mean_absolute_error
from sklearn.model_selection import train_test_split

# Feature layout, mirrors profile_to_features in onnx.rs. Do not reorder.
NUM_FEATURES = 8
FEATURE_NAMES = [
    "norm_input_tokens",  # estimated_input_tokens / 100_000
    "task_type",          # ordinal 0..5 (simple_qa..math)
    "complexity",         # ordinal 0..2 (low..high)
    "requires_tool_use",  # 0 or 1
    "vision",             # 0 or 1
    "long_context",       # 0 or 1
    "norm_message_count",  # message_count / 50
    "has_system_prompt",  # 0 or 1
]

# Distillation weights: how much each signal raises the required quality. These
# encode the same intuition as the threshold/score heuristics (harder, longer,
# tool/vision/code/math work needs stronger models). Tuned to land in [0, 1]
# after the sigmoid below.
_BIAS = -1.1
_W = np.array(
    [
        1.4,   # more input tokens -> needs a more capable model
        0.18,  # task_type ordinal: code/math (higher) lean harder
        0.9,   # complexity is the dominant signal
        0.35,  # tool use needs a competent model
        0.5,   # vision needs a capable multimodal model
        0.6,   # long context needs a strong long-context model
        0.4,   # multi-turn conversations are harder
        0.1,   # a system prompt slightly raises the bar
    ]
)


def _sigmoid(x: np.ndarray) -> np.ndarray:
    return 1.0 / (1.0 + np.exp(-x))


def synthesize(n: int, seed: int) -> tuple[np.ndarray, np.ndarray]:
    """Generate synthetic feature rows and distilled required-quality labels."""
    rng = np.random.default_rng(seed)
    x = np.zeros((n, NUM_FEATURES), dtype=np.float32)
    # norm_input_tokens: most queries are small, a long tail is large
    x[:, 0] = np.clip(rng.exponential(0.15, n), 0.0, 5.0)
    x[:, 1] = rng.integers(0, 6, n)  # task_type ordinal
    x[:, 2] = rng.integers(0, 3, n)  # complexity ordinal
    x[:, 3] = (rng.random(n) < 0.25).astype(np.float32)  # requires_tool_use
    x[:, 4] = (rng.random(n) < 0.10).astype(np.float32)  # vision
    x[:, 5] = (rng.random(n) < 0.15).astype(np.float32)  # long_context
    x[:, 6] = np.clip(rng.exponential(0.06, n), 0.0, 2.0)  # norm_message_count
    x[:, 7] = (rng.random(n) < 0.5).astype(np.float32)  # has_system_prompt

    # Normalize the ordinals into 0..1 so the linear combination is balanced
    norm = x.copy()
    norm[:, 1] = x[:, 1] / 5.0
    norm[:, 2] = x[:, 2] / 2.0

    logits = _BIAS + norm @ _W
    # Gaussian noise so the model learns a smooth function rather than the exact
    # formula, then squash to a probability-like required quality
    y = _sigmoid(logits + rng.normal(0.0, 0.25, n)).astype(np.float32)
    return x, y


def load_telemetry(path: str) -> tuple[np.ndarray, np.ndarray]:
    """Load (8 features, label) rows from a ClickHouse CSV export.

    Expected columns: the 8 FEATURE_NAMES in order, then `required_quality`
    (the lowest model quality that produced an accepted answer for the query).
    """
    rows, labels = [], []
    with open(path, newline="") as fh:
        reader = csv.DictReader(fh)
        for row in reader:
            rows.append([float(row[name]) for name in FEATURE_NAMES])
            labels.append(float(row["required_quality"]))
    if not rows:
        sys.exit(f"no telemetry rows found in {path}")
    return np.asarray(rows, dtype=np.float32), np.asarray(labels, dtype=np.float32)


def main() -> None:
    parser = argparse.ArgumentParser(description="Train the Synapse ONNX routing model")
    parser.add_argument("--out", default="model.onnx", help="output ONNX path")
    parser.add_argument("--telemetry", help="ClickHouse CSV export; omit to bootstrap synthetically")
    parser.add_argument("--samples", type=int, default=50_000, help="synthetic sample count")
    parser.add_argument("--seed", type=int, default=42, help="rng seed for reproducibility")
    args = parser.parse_args()

    if args.telemetry:
        x, y = load_telemetry(args.telemetry)
        print(f"loaded {len(x)} telemetry rows from {args.telemetry}")
    else:
        x, y = synthesize(args.samples, args.seed)
        print(f"synthesized {len(x)} bootstrap rows")

    x_train, x_test, y_train, y_test = train_test_split(x, y, test_size=0.2, random_state=args.seed)
    model = GradientBoostingRegressor(n_estimators=200, max_depth=3, learning_rate=0.1, random_state=args.seed)
    model.fit(x_train, y_train)

    mae = mean_absolute_error(y_test, np.clip(model.predict(x_test), 0.0, 1.0))
    print(f"holdout MAE: {mae:.4f}")

    # Export with a fixed [1, 8] f32 input and a single f32 output, matching the
    # Rust inference contract (one row in, one scalar out)
    onnx_model = to_onnx(
        model,
        initial_types=[("input", FloatTensorType([1, NUM_FEATURES]))],
        target_opset=17,
    )
    with open(args.out, "wb") as fh:
        fh.write(onnx_model.SerializeToString())
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
