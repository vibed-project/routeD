# SPDX-License-Identifier: Apache-2.0
"""Offline learned-router training (ADR-0018).

Joins the router's decision journal with accepted feedback, featurises with
the routed-features/1 layout (byte-for-byte the runtime's layout in
crates/classify/src/router_features.rs), fits a logistic model with plain
numpy, and exports the ONNX graph the runtime loads (`features` in,
`quality` + `confidence` out) plus a calibration report.

Inputs:
  --decisions  decisions.jsonl   (from --feedback-dir)
  --feedback   feedback.jsonl    (from --feedback-dir)
  --snapshot   snapshot.json     (routedctl validate --emit-snapshot, or the
                                  operator's fallback ConfigMap)
  --out        output directory  (model.onnx, calibration.json, report.json)

A record labels positive when feedback says success=true or rating >= 4.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper

FEATURES_VERSION = "routed-features/1"
COMPLEXITIES = ["low", "medium", "high"]


def read_jsonl(path: Path) -> list[dict]:
    out = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if line:
            out.append(json.loads(line))
    return out


def label_of(outcome: dict) -> int | None:
    if isinstance(outcome.get("success"), bool):
        return 1 if outcome["success"] else 0
    rating = outcome.get("rating")
    if isinstance(rating, (int, float)):
        return 1 if rating >= 4 else 0
    return None


def tier_features(tier: dict, task: str | None) -> list[float]:
    quality = tier["qualityByTask"].get(task, tier["qualityBaseline"]) if task else tier["qualityBaseline"]
    cost = tier["inputMicroEurPerMillion"] + tier["outputMicroEurPerMillion"]
    return [quality, math.log10(1 + cost), tier["latencyP50Ms"] / 1000.0]


def featurise(record: dict, snapshot: dict, task_labels: list[str]) -> list[float] | None:
    tier = snapshot["tiers"].get(record.get("selectedTier") or "")
    if tier is None:
        return None
    task = record.get("task")
    row = [1.0 if task == label else 0.0 for label in task_labels]
    row += [1.0 if record.get("complexity") == c else 0.0 for c in COMPLEXITIES]
    row.append(float(record.get("riskScore") or 0.0))
    row += tier_features(tier, task)
    return row


def fit_logistic(x: np.ndarray, y: np.ndarray, l2: float = 1e-3, epochs: int = 4000, lr: float = 0.1):
    n, d = x.shape
    w = np.zeros(d)
    b = 0.0
    for _ in range(epochs):
        z = x @ w + b
        p = 1.0 / (1.0 + np.exp(-z))
        grad_w = x.T @ (p - y) / n + l2 * w
        grad_b = float(np.mean(p - y))
        w -= lr * grad_w
        b -= lr * grad_b
    return w, b


def export_onnx(w: np.ndarray, b: float, confidence: float, path: Path) -> None:
    n = w.shape[0]
    features = helper.make_tensor_value_info("features", TensorProto.FLOAT, [1, n])
    outs = [
        helper.make_tensor_value_info("quality", TensorProto.FLOAT, [1]),
        helper.make_tensor_value_info("confidence", TensorProto.FLOAT, [1]),
    ]
    init = [
        helper.make_tensor("w", TensorProto.FLOAT, [n, 1], [float(v) for v in w]),
        helper.make_tensor("b", TensorProto.FLOAT, [1], [float(b)]),
        helper.make_tensor("conf_const", TensorProto.FLOAT, [1], [float(confidence)]),
        helper.make_tensor("out_shape", TensorProto.INT64, [1], [1]),
    ]
    nodes = [
        helper.make_node("MatMul", ["features", "w"], ["z0"]),
        helper.make_node("Add", ["z0", "b"], ["z"]),
        helper.make_node("Sigmoid", ["z"], ["q2d"]),
        helper.make_node("Reshape", ["q2d", "out_shape"], ["quality"]),
        helper.make_node("Identity", ["conf_const"], ["confidence"]),
    ]
    graph = helper.make_graph(nodes, "routed_learned_router", [features], outs, init)
    model = helper.make_model(
        graph, opset_imports=[helper.make_opsetid("", 17)], producer_name="routed-trainer"
    )
    model.ir_version = 8
    onnx.checker.check_model(model)
    onnx.save(model, path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--decisions", type=Path, required=True)
    ap.add_argument("--feedback", type=Path, required=True)
    ap.add_argument("--snapshot", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--min-samples", type=int, default=50)
    args = ap.parse_args()

    snapshot = json.loads(args.snapshot.read_text())
    profiles = snapshot.get("profiles", {})
    profile = profiles.get("default") or next(iter(profiles.values()), None)
    if profile is None:
        print("snapshot has no RouterProfile; task labels are required", file=sys.stderr)
        return 2
    task_labels = profile["taskLabels"]

    feedback_by_id: dict[str, int] = {}
    for f in read_jsonl(args.feedback):
        label = label_of(f.get("outcome") or {})
        if label is not None:
            feedback_by_id[f["decisionId"]] = label

    rows, labels = [], []
    for d in read_jsonl(args.decisions):
        if d.get("outcome") != "ROUTE" or d.get("mode") == "dry-run":
            continue
        y = feedback_by_id.get(d["decisionId"])
        if y is None:
            continue
        row = featurise(d, snapshot, task_labels)
        if row is not None:
            rows.append(row)
            labels.append(y)

    if len(rows) < args.min_samples:
        print(
            f"only {len(rows)} joined samples (< {args.min_samples}); refusing to train a model",
            file=sys.stderr,
        )
        return 3

    x = np.asarray(rows, dtype=np.float64)
    y = np.asarray(labels, dtype=np.float64)
    # Standardise for a stable fit, then fold the scaling back into the
    # exported weights so the runtime keeps feeding raw routed-features/1.
    mean = x.mean(axis=0)
    std = np.where(x.std(axis=0) > 1e-9, x.std(axis=0), 1.0)
    xs = (x - mean) / std
    # Chronological split: last 20% validates.
    cut = max(1, int(len(rows) * 0.8))
    ws, bs = fit_logistic(xs[:cut], y[:cut])
    w = ws / std
    b = float(bs - np.sum(ws * mean / std))
    val_p = 1.0 / (1.0 + np.exp(-(x[cut:] @ w + b)))
    accuracy = float(np.mean((val_p >= 0.5) == y[cut:])) if len(val_p) else 0.0

    args.out.mkdir(parents=True, exist_ok=True)
    export_onnx(w, b, accuracy, args.out / "model.onnx")
    # Calibration: per quality floor, the validation probability threshold at
    # which precision reaches the floor (identity when data is too thin).
    calibration = {}
    for floor in (0.5, 0.7, 0.75, 0.8, 0.9):
        calibration[f"{floor}"] = round(float(np.quantile(val_p, floor)) if len(val_p) else floor, 6)
    (args.out / "calibration.json").write_text(json.dumps(calibration, indent=1))
    report = {
        "featuresVersion": FEATURES_VERSION,
        "taskLabels": task_labels,
        "samples": len(rows),
        "positives": int(y.sum()),
        "validationAccuracy": round(accuracy, 4),
        "snapshotHash": snapshot.get("hash"),
    }
    (args.out / "report.json").write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))
    print(f"wrote {args.out}/model.onnx (pin it: sha256sum, then set learnedRouter.uri)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
