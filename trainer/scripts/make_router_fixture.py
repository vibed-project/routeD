# SPDX-License-Identifier: Apache-2.0
"""Generate the tiny learned-router ONNX fixture used by routed-classify's
feature tests (ADR-0018).

The graph is the same shape the trainer exports (routed-features/1 in,
sigmoid logistic head out) with fixed weights chosen for easy assertions:

  quality    = sigmoid(features . w + b), w = [1, 0, ..., 0], b = 0
  confidence = 0.9 (constant)

With two task labels the feature length is 9, so:
  task == labels[0]  -> quality = sigmoid(1) ~= 0.731
  unknown task       -> quality = sigmoid(0) = 0.5

Usage:
  podman run --rm -v "$PWD:/src" docker.io/library/python:3.12-slim \
    sh -c "pip install -q onnx && python /src/trainer/scripts/make_router_fixture.py /src/crates/classify/tests/fixtures"
"""

import sys
from pathlib import Path

import onnx
from onnx import TensorProto, helper

N = 9  # feature_len(task_count=2) = 2 + 3 + 1 + 3


def build_model() -> onnx.ModelProto:
    features = helper.make_tensor_value_info("features", TensorProto.FLOAT, [1, N])
    outs = [
        helper.make_tensor_value_info("quality", TensorProto.FLOAT, [1]),
        helper.make_tensor_value_info("confidence", TensorProto.FLOAT, [1]),
    ]
    w = [0.0] * N
    w[0] = 1.0
    init = [
        helper.make_tensor("w", TensorProto.FLOAT, [N, 1], w),
        helper.make_tensor("b", TensorProto.FLOAT, [1], [0.0]),
        helper.make_tensor("conf_const", TensorProto.FLOAT, [1], [0.9]),
        helper.make_tensor("out_shape", TensorProto.INT64, [1], [1]),
    ]
    nodes = [
        helper.make_node("MatMul", ["features", "w"], ["z0"]),
        helper.make_node("Add", ["z0", "b"], ["z"]),
        helper.make_node("Sigmoid", ["z"], ["q2d"]),
        helper.make_node("Reshape", ["q2d", "out_shape"], ["quality"]),
        helper.make_node("Identity", ["conf_const"], ["confidence"]),
    ]
    graph = helper.make_graph(nodes, "routed_router_fixture", [features], outs, init)
    model = helper.make_model(
        graph, opset_imports=[helper.make_opsetid("", 17)], producer_name="routed-fixture"
    )
    model.ir_version = 8
    onnx.checker.check_model(model)
    return model


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    onnx.save(build_model(), out / "router.onnx")
    print(f"wrote {out}/router.onnx")


if __name__ == "__main__":
    main()
