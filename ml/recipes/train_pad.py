#!/usr/bin/env python3
"""Train MiniFASNet PAD model on synthetic strobe-differential data.

Spec §6.4 + §10.3: MiniFASNet-class CNN trained on D = On − Off strobe
differentials. Without collected data, we generate physics-based synthetic
differentials that encode the same signals the analytic gates check:

Live (bonafide):
  - Strong positive D (emitter illuminates skin)
  - 1/r² radial falloff (nose nearer than ears)
  - Specular hotspot (compact bright region)
  - Subsurface scattering (smooth low-frequency gradient field)

Spoof:
  - Screen replay: near-zero D (panels emit ~nothing at 850 nm)
  - Print: weak/flat D (IR-transparent inks), no subsurface scattering
  - Cutout mask: uniform D, no radial falloff

The model is a small CNN (MiniFASNet-class, <100K params) that takes an
80×80 greyscale differential as input and outputs live/spoof logits.
Exported to ONNX for the namtor-worker inference path.

Usage:
    python3 ml/recipes/train_pad.py --epochs 50 --out ml/models/minifasnet_pad.onnx
"""

import argparse
import math
import os
import sys
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.utils.data import DataLoader, TensorDataset


# ---------------------------------------------------------------------------
# MiniFASNet architecture (§10.3: MiniFASNet-class, <100K params)
# ---------------------------------------------------------------------------

class DepthwiseSeparableConv(nn.Module):
    def __init__(self, in_ch, out_ch, stride=1):
        super().__init__()
        self.depthwise = nn.Conv2d(in_ch, in_ch, 3, stride=stride, padding=1, groups=in_ch, bias=False)
        self.pointwise = nn.Conv2d(in_ch, out_ch, 1, bias=False)
        self.bn = nn.BatchNorm2d(out_ch)

    def forward(self, x):
        x = self.depthwise(x)
        x = self.pointwise(x)
        x = self.bn(x)
        return F.relu6(x)


class MiniFASNet(nn.Module):
    """Small CNN for PAD on 80×80 strobe differentials. ~80K params."""

    def __init__(self, num_classes=2):
        super().__init__()
        self.features = nn.Sequential(
            nn.Conv2d(1, 16, 3, stride=2, padding=1, bias=False),
            nn.BatchNorm2d(16),
            nn.ReLU6(inplace=True),

            DepthwiseSeparableConv(16, 32, stride=2),
            DepthwiseSeparableConv(32, 32, stride=1),
            DepthwiseSeparableConv(32, 64, stride=2),
            DepthwiseSeparableConv(64, 64, stride=1),
            DepthwiseSeparableConv(64, 128, stride=2),
            DepthwiseSeparableConv(128, 128, stride=1),
        )
        self.gap = nn.AdaptiveAvgPool2d(1)
        self.classifier = nn.Linear(128, num_classes)

    def forward(self, x):
        x = self.features(x)
        x = self.gap(x)
        x = x.view(x.size(0), -1)
        x = self.classifier(x)
        return x


# ---------------------------------------------------------------------------
# Synthetic data generation (physics-based, spec §6.4)
# ---------------------------------------------------------------------------

def make_live_differential(size=80, rng=None):
    """Simulate a live face strobe differential: 1/r² falloff + specular + subsurface."""
    if rng is None:
        rng = np.random.default_rng()

    cx = size / 2 + rng.normal(0, 3)
    cy = size / 2 + rng.normal(0, 3)
    a = rng.uniform(3000, 8000)
    b = rng.uniform(3, 8)

    yy, xx = np.meshgrid(np.arange(size), np.arange(size), indexing='ij')
    r = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2)
    img = a / (r + b) ** 2

    # Specular hotspot
    spx, spy = cx + rng.normal(0, 5), cy + rng.normal(0, 5)
    sp_r = np.sqrt((xx - spx) ** 2 + (yy - spy) ** 2)
    img += rng.uniform(50, 150) * np.exp(-sp_r ** 2 / (2 * rng.uniform(2, 5) ** 2))

    # Subsurface scattering: smooth low-frequency modulation
    img += rng.uniform(-10, 10) * np.sin(xx * 0.1) * np.cos(yy * 0.1)

    # Noise
    img += rng.normal(0, 3, img.shape)

    return np.clip(img, 0, 255).astype(np.float32)


def make_screen_replay_differential(size=80, rng=None):
    """Screen replay: near-zero D (panels emit ~nothing at 850 nm)."""
    if rng is None:
        rng = np.random.default_rng()
    img = rng.normal(0, 2, (size, size))
    return np.clip(img, 0, 255).astype(np.float32)


def make_print_differential(size=80, rng=None):
    """Print: weak, flat D (IR-transparent inks), no subsurface scattering."""
    if rng is None:
        rng = np.random.default_rng()
    base = rng.uniform(2, 15)
    img = np.full((size, size), base, dtype=np.float32)
    img += rng.normal(0, 5, img.shape)
    return np.clip(img, 0, 255).astype(np.float32)


def make_cutout_differential(size=80, rng=None):
    """Cutout mask: uniform illumination, no radial falloff."""
    if rng is None:
        rng = np.random.default_rng()
    base = rng.uniform(20, 50)
    img = np.full((size, size), base, dtype=np.float32)
    # Slight vignetting (lens, not face geometry)
    cy, cx = size / 2, size / 2
    yy, xx = np.meshgrid(np.arange(size), np.arange(size), indexing='ij')
    r = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2)
    img *= 1 - 0.1 * (r / (size / 2)) ** 2
    img += rng.normal(0, 4, img.shape)
    return np.clip(img, 0, 255).astype(np.float32)


def generate_dataset(n_per_class=12500, size=80, seed=42):
    """Generate a balanced synthetic dataset: 25K live + 25K spoof (4 spoof types)."""
    rng = np.random.default_rng(seed)
    n_spoof_per_type = n_per_class // 3

    live = np.stack([make_live_differential(size, rng) for _ in range(n_per_class)])

    screen = np.stack([make_screen_replay_differential(size, rng) for _ in range(n_spoof_per_type)])
    prints = np.stack([make_print_differential(size, rng) for _ in range(n_spoof_per_type)])
    cutout = np.stack([make_cutout_differential(size, rng) for _ in range(n_spoof_per_type)])
    spoof = np.concatenate([screen, prints, cutout], axis=0)

    X = np.concatenate([live, spoof], axis=0).astype(np.float32) / 255.0
    X = X[:, np.newaxis, :, :]  # (N, 1, H, W)

    y_live = np.zeros(n_per_class, dtype=np.int64)
    y_spoof = np.ones(spoof.shape[0], dtype=np.int64)
    y = np.concatenate([y_live, y_spoof])

    # Shuffle
    perm = rng.permutation(len(X))
    X, y = X[perm], y[perm]
    return torch.from_numpy(X), torch.from_numpy(y)


# ---------------------------------------------------------------------------
# Training
# ---------------------------------------------------------------------------

def train_model(epochs=50, batch_size=256, lr=1e-3, device='cuda', seed=42):
    torch.manual_seed(seed)
    dev = torch.device(device if torch.cuda.is_available() else 'cpu')
    print(f"Device: {dev}")

    print("Generating synthetic dataset...")
    X, y = generate_dataset(seed=seed)
    print(f"Dataset: {X.shape}, {y.shape}, live={int((y==0).sum())}, spoof={int((y==1).sum())}")

    # Split 90/10
    n = len(X)
    n_train = int(n * 0.9)
    X_train, y_train = X[:n_train], y[:n_train]
    X_val, y_val = X[n_train:], y[n_train:]

    train_ds = TensorDataset(X_train, y_train)
    val_ds = TensorDataset(X_val, y_val)
    train_dl = DataLoader(train_ds, batch_size=batch_size, shuffle=True, num_workers=0)
    val_dl = DataLoader(val_ds, batch_size=batch_size, shuffle=False, num_workers=0)

    model = MiniFASNet(num_classes=2).to(dev)
    n_params = sum(p.numel() for p in model.parameters())
    print(f"Model: MiniFASNet, {n_params} params")

    optimizer = torch.optim.AdamW(model.parameters(), lr=lr, weight_decay=1e-4)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, T_max=epochs)
    criterion = nn.CrossEntropyLoss()

    best_val_acc = 0.0
    best_state = None

    for epoch in range(epochs):
        model.train()
        train_loss = 0.0
        train_correct = 0
        train_total = 0

        for xb, yb in train_dl:
            xb, yb = xb.to(dev), yb.to(dev)
            optimizer.zero_grad()
            out = model(xb)
            loss = criterion(out, yb)
            loss.backward()
            optimizer.step()

            train_loss += loss.item() * xb.size(0)
            train_correct += (out.argmax(1) == yb).sum().item()
            train_total += xb.size(0)

        scheduler.step()

        model.eval()
        val_loss = 0.0
        val_correct = 0
        val_total = 0
        with torch.no_grad():
            for xb, yb in val_dl:
                xb, yb = xb.to(dev), yb.to(dev)
                out = model(xb)
                loss = criterion(out, yb)
                val_loss += loss.item() * xb.size(0)
                val_correct += (out.argmax(1) == yb).sum().item()
                val_total += xb.size(0)

        train_acc = train_correct / train_total
        val_acc = val_correct / val_total
        print(f"Epoch {epoch+1:3d}/{epochs}: train_loss={train_loss/train_total:.4f} train_acc={train_acc:.4f} val_loss={val_loss/val_total:.4f} val_acc={val_acc:.4f}")

        if val_acc > best_val_acc:
            best_val_acc = val_acc
            best_state = {k: v.clone() for k, v in model.state_dict().items()}

    print(f"\nBest val accuracy: {best_val_acc:.4f}")
    model.load_state_dict(best_state)
    return model, best_val_acc


def export_onnx(model, path, size=80):
    model.eval()
    model_cpu = model.cpu()
    dummy = torch.randn(1, 1, size, size)
    torch.onnx.export(
        model_cpu,
        dummy,
        path,
        input_names=["input"],
        output_names=["logits"],
        dynamic_axes={"input": {0: "batch"}, "logits": {0: "batch"}},
        opset_version=14,
    )
    print(f"Exported ONNX model to {path}")


def main():
    parser = argparse.ArgumentParser(description="Train MiniFASNet PAD model")
    parser.add_argument("--epochs", type=int, default=50)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--lr", type=float, default=1e-3)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--device", type=str, default="cuda")
    parser.add_argument("--out", type=str, default="ml/models/minifasnet_pad.onnx")
    args = parser.parse_args()

    model, val_acc = train_model(
        epochs=args.epochs,
        batch_size=args.batch_size,
        lr=args.lr,
        device=args.device,
        seed=args.seed,
    )

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    export_onnx(model, str(out_path))

    print(f"\nDone. Val accuracy: {val_acc:.4f}")
    print(f"Model saved to: {out_path}")


if __name__ == "__main__":
    main()
