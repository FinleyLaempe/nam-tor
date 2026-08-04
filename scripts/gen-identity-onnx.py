#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# Generates fixtures/onnx/identity.onnx: a tiny ONNX identity model.
#
# This is a TEST ARTEFACT, not a model weight. It has one f32 input of dynamic
# shape [1, N] and one f32 output of the same shape, wired through an Identity
# op. Used by namtor-worker's OnnxCpu tests (Task 5) to verify the ort backend
# loads a model and reproduces its input as output. No face-recognition model
# is involved; the fixture is < 1 KB and carries no learned parameters.
#
# Run once after a fresh checkout:
#   python3 scripts/gen-identity-onnx.py
# The generated file is committed so CI does not need the `onnx` Python package.

import os
import sys

import onnx
from onnx import TensorProto, helper


def build_identity_model() -> onnx.ModelProto:
    x = helper.make_tensor_value_info("x", TensorProto.FLOAT, [1, "N"])
    y = helper.make_tensor_value_info("y", TensorProto.FLOAT, [1, "N"])
    node = helper.make_node("Identity", inputs=["x"], outputs=["y"], name="id")
    graph = helper.make_graph([node], "identity_graph", [x], [y])
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)])
    model.ir_version = 5
    onnx.checker.check_model(model)
    return model


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    out_dir = os.path.join(here, "..", "fixtures", "onnx")
    os.makedirs(out_dir, exist_ok=True)
    out_path = os.path.join(out_dir, "identity.onnx")
    model = build_identity_model()
    with open(out_path, "wb") as f:
        f.write(model.SerializeToString())
    size = os.path.getsize(out_path)
    print(f"wrote {out_path} ({size} bytes)")
    if size >= 1024:
        print("WARNING: fixture is >= 1 KB; expected a tiny test artefact", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
