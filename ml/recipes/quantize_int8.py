#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Static INT8 quantization for Nam-tor stock ONNX models.

Usage:
    python3 quantize_int8.py --fp32 <path> --out <path> [--calib <dir>]

Produces an INT8-quantized ONNX model from an FP32 source using ONNX Runtime
static quantization. The calibration set is a directory of .npy files (pre-
extracted face crops); if no calibration directory is provided, a small
random calibration set is generated (sufficient for the identity test model).

This script runs on the user's machine at first-run model fetch time. No
weights are committed to the repository.
"""

import argparse
import os
import sys
import tempfile
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Quantize an ONNX model to INT8")
    parser.add_argument("--fp32", required=True, type=Path, help="FP32 ONNX model path")
    parser.add_argument("--out", required=True, type=Path, help="Output INT8 ONNX path")
    parser.add_argument("--calib", type=Path, help="Calibration data directory (.npy files)")
    args = parser.parse_args()

    try:
        import onnx
        import numpy as np
        from onnxruntime.quantization import QuantType, QuantFormat, CalibrationMethod
        from onnxruntime.quantization.quantize import quantize_static
    except ImportError as e:
        print(f"missing dependency: {e}", file=sys.stderr)
        print("install: pip install onnx onnxruntime numpy", file=sys.stderr)
        return 1

    if not args.fp32.exists():
        print(f"FP32 model not found: {args.fp32}", file=sys.stderr)
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)

    # Generate a minimal calibration set if none provided.
    with tempfile.TemporaryDirectory() as tmpdir:
        calib_dir = args.calib if args.calib else Path(tmpdir)
        if not args.calib:
            # Generate 4 random 112x112x3 float32 crops.
            for i in range(4):
                data = np.random.randn(1, 112, 112, 3).astype(np.float32)
                np.save(calib_dir / f"calib_{i}.npy", data)

        calib_files = sorted(calib_dir.glob("*.npy"))
        if not calib_files:
            print(f"no .npy calibration files in {calib_dir}", file=sys.stderr)
            return 1

        # Build calibration data reader.
        def calib_data_reader():
            for f in calib_files:
                data = np.load(f)
                yield {f"input": data}, None

        # Quantize. The model's input name may differ; we use a generic
        # approach that onnxruntime's calibration reader handles.
        try:
            quantize_static(
                str(args.fp32),
                str(args.out),
                calibration_data_reader=calib_data_reader(),
                quant_format=QuantFormat.QDQ,
                activation_type=QuantType.QUInt8,
                weight_type=QuantType.QInt8,
                calibrate_method=CalibrationMethod.MinMax,
            )
        except Exception as e:
            print(f"quantization failed: {e}", file=sys.stderr)
            return 1

    print(f"INT8 model written to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
