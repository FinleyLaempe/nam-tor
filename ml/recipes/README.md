# Nam-tor ML Recipes

Scripts that run on the user's machine at first-run or during development.
No model weights or datasets are committed to this repository.

## `quantize_int8.py`

Static INT8 quantization of stock ONNX models using ONNX Runtime. Produces
an INT8-quantized model from an FP32 source for the CPU EP (AVX-VNNI).

```bash
python3 quantize_int8.py --fp32 <model_fp32.onnx> --out <model_int8.onnx> [--calib <dir>]
```

Requires: `onnx`, `onnxruntime`, `numpy` (Python packages).
