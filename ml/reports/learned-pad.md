# Learned Differential PAD — Task 22 Report

Spec §6.4 + §10.3 + §6.6. Two-stage presentation-attack detection: analytic
gates first (physics-based, in `namtor-pad`), learned MiniFASNet second
(trained on strobe differentials `D = On − Off`). Both stages are **vetoes**;
the learned model is never a weighted term.

## What landed

### `crates/namtor-worker/src/learned_pad.rs` (new, ~470 LOC incl. tests)

Public API:

| Item | Purpose |
|---|---|
| `LearnedPadBackend` trait | `load(&Path) -> Result<Self>`, `infer(&[f32]) -> Result<PadResult>`. Send + Sync. Fail-closed contract. |
| `MockLearnedPadBackend` | Deterministic test-only backend (mean-of-differential → probability → threshold). No ONNX, no weights. |
| `PadVerdict { Live, Spoof, Uncertain }` | Tri-state. `Uncertain` is fail-closed. |
| `PadResult { live_probability: f32, verdict: PadVerdict }` | Inference output. `PadResult::unavailable()` sentinel. `from_probability(p, live_min, spoof_max)` band classifier. |
| `AnalyticResult` | Local structural mirror of `namtor_pad::PadVerdict`. Only `live` drives the veto; per-gate scores are audit-only. Defined locally (not re-exported) so this module clippy-clean independently of task-21 calibration work in `namtor-pad`. |
| `VetoPolicy { live_min, spoof_max, learned_disabled }` | Thresholds + rollout bypass. `validate()` rejects `live_min <= spoof_max` and out-of-range. |
| `FinalVerdict { Live, AnalyticSpoof, LearnedSpoof, FailClosed }` | Combined verdict. Only `Live` proceeds. |
| `evaluate::<B>(analytic, learned, policy, backend) -> FinalVerdict` | The single pure decision function. |

Decision matrix:

```
analytic.live == false                   -> AnalyticSpoof   (learned NOT run)
analytic.live & policy.learned_disabled  -> Live            (analytic-only rollout)
analytic.live & learned.verdict == Live  -> Live
analytic.live & learned.verdict == Spoof -> LearnedSpoof
analytic.live & learned.verdict == Uncertain -> FailClosed  (fail-closed)
```

### `crates/namtor-worker/src/lib.rs`

Added `pub mod learned_pad;`.

### `ml/recipes/train_pad.py` (extended)

| Addition | Why |
|---|---|
| `subject_disjoint_split(subjects, n_subjects, val_subjects, test_subjects)` | Subject-disjoint partition. No subject appears in >1 split. Reproducible (seeded). |
| `apply_device_perturbation(X, subjects, n_subjects, seed)` | Per-subject gain/offset perturbation models the device axis. Applied before splitting so each subject's device signature is consistent across its samples. In the synthetic setup subject id ≡ device id, so the split is simultaneously subject- and device-disjoint. |
| `ModelCard` dataclass + `write_model_card()` | Deterministic JSON model card written next to the ONNX file (architecture, params, input shape, training-data provenance, `synthetic_only: true`, split sizes, val/test accuracy, onnx sha256). |
| `export_onnx()` now returns SHA-256 | For `namtor_proto::worker::ModelSpec.sha256` binding. |
| `--synthetic-only` flag (default True) | Refuses to train on non-synthetic data. `--allow-real-data` escape hatch is currently still refused (consent-gated collection §10.4 is a separate path). |
| `--n-subjects`, `--val-subjects`, `--test-subjects` CLI args | Split-axis sizing. Defaults 125 / 15 / 15. |
| `generate_dataset()` returns `(X, y, subject_ids)` | Each synthetic subject's samples drawn from a deterministic RNG stream keyed by `(subject_id, class)`. |
| Held-out test accuracy | Computed on the subject-disjoint test split (in addition to val). |

### Verification (run this task)

```
cargo test -p namtor-worker -- --test-threads=1
  → 48 lib tests pass (13 new in learned_pad, 35 pre-existing)
  → 5 + 6 + 3 + 3 integration tests pass
  → 1 doc-test ignored (onnx-feature)
cargo clippy -p namtor-worker -- -D warnings   → clean
cargo fmt -p namtor-worker -- --check          → clean
```

New `learned_pad` tests (13 total, ≥8 required):

1. `test_analytic_spoof_vetoes_regardless_of_learned` — analytic veto short-circuits
2. `test_both_live_proceeds` — analytic live + learned live ⇒ Live
3. `test_learned_spoof_after_analytic_live_refuses` — analytic live + learned spoof ⇒ LearnedSpoof
4. `test_learned_uncertain_is_fail_closed` — analytic live + learned uncertain ⇒ FailClosed
5. `test_learned_disabled_uses_analytic_only` — rollout bypass
6. `test_learned_disabled_still_respects_analytic_veto` — bypass does not weaken analytic
7. `test_mock_backend_live_differential` — strong positive D ⇒ Live
8. `test_mock_backend_spoof_differential` — near-zero D ⇒ Spoof
9. `test_policy_validate_rejects_inverted_thresholds` — `live_min <= spoof_max` rejected
10. `test_pad_result_band_is_uncertain` — band `(spoof_max, live_min)` ⇒ Uncertain
11. `test_load_missing_model_is_not_found` — missing model ⇒ NotFound, no panic
12. `test_empty_differential_is_shape_error` — empty input ⇒ Shape error
13. `test_unavailable_is_uncertain_and_refuses` — sentinel refuses

## Fail-closed guarantees (asserted by tests, not review)

- Missing model file ⇒ `LearnedPadError::NotFound` (never panic). Caller maps to `PadResult::unavailable()` ⇒ `Uncertain` ⇒ `FailClosed`.
- Empty / wrong-shape differential ⇒ `LearnedPadError::Shape` (never panic, never `Uncertain`).
- Inverted thresholds ⇒ `VetoPolicy::validate()` rejects at construction.
- `Uncertain` from any source ⇒ `FinalVerdict::FailClosed` ⇒ authentication refused.
- No `unwrap` / `expect` in non-test code (project hard rule, holds).

## What this does NOT claim

- **No readiness claim.** This is a code-state landing, not a gate pass.
- **No real-data evaluation.** The model is trained on physics-based synthetic differentials only; APCER / BPCER on real attacks is unmeasured. The analytic PAD gates carry the load until collected data (§10.4 `namtor-collect`) is available.
- **No ONNX backend wired in `learned_pad.rs`.** The trait is in place; the `onnx`-feature production backend is deferred to the daemon integration task (the existing `infer_onnx` backend handles face embeddings; a parallel learned-PAD backend is a wiring step, not a new design).
- **No GUI exposure of thresholds** (spec hard rule). `VetoPolicy` is config-only.

## Files changed

```
crates/namtor-worker/Cargo.toml                 (no net dep change — namtor-pad dep added then removed in favour of local AnalyticResult)
crates/namtor-worker/src/lib.rs                 (+1 line: pub mod learned_pad;)
crates/namtor-worker/src/learned_pad.rs         (new, ~470 LOC)
ml/recipes/train_pad.py                         (extended: disjoint splits, model card, sha256, synthetic-only flag)
ml/reports/learned-pad.md                       (this file)
```

## Out of scope (owned by other tasks)

- `crates/namtor-pad/` — analytic PAD calibration (task 21). Not touched.
- Daemon wiring of the learned backend into `VerifyEngine` — separate integration task.
- Real-data evaluation and APCER/BPCER reporting — task 22 follow-on after §10.4 collection.

## Task 7 — Integration into the authentication path (2026-08-08)

### What landed

The learned PAD model is now enforced as a fail-closed veto in the daemon's
authentication path. Both analytic AND learned PAD must pass for
authentication to proceed.

#### `crates/namtor-worker/src/learned_pad.rs` — `OnnxCpuLearnedPad` backend

- New `OnnxCpuLearnedPad` struct (feature-gated behind `onnx`), implementing
  `LearnedPadBackend` via the `ort` crate (CPU-only ONNX Runtime, same
  Execution Provider as `OnnxCpu` for face recognition — no GPU EP, spec
  §5.3).
- `infer()` preprocesses the strobe differential `D = On − Off`:
  1. Nearest-neighbor resize to 80×80 (the model's training input size).
  2. Clip to `[0, 255]` and normalize to `[0, 1]` (matching the training
     recipe `np.clip(img, 0, 255).astype(np.float32) / 255.0`).
  3. Run the ONNX session with shape `[1, 1, 80, 80]` (1-channel, matching
     the model's `nn.Conv2d(1, 16, ...)` first layer).
  4. Softmax over the 2-logit output → `live_probability = softmax[0]`
     (class 0 = live, class 1 = spoof).
  5. `PadResult::from_probability(p, live_min, spoof_max)` bands the
     probability into Live / Spoof / Uncertain.
- `LearnedPadBackend::infer` signature changed from `infer(&self, &[f32])` to
  `infer(&self, &[f32], w: u32, h: u32)` — the ONNX backend needs spatial
  dimensions for resizing. The mock backend ignores them.
- `evaluate()` simplified: the unused generic `_backend: &B` parameter was
  removed (it was never used; the function is pure). All 6 existing evaluate
  tests updated.
- 11 new ONNX backend tests (tests 14–24): load missing model, empty
  differential, resize upscale/downscale, softmax dominant live/spoof/equal,
  CPU-only provider, 2 hw-gated real-model tests.

#### `crates/namtor-daemon/src/verify.rs` — two-stage PAD veto

- `VerifyEngine` gained `learned_pad: Option<Box<dyn LearnedPadBackend>>` and
  `veto_policy: VetoPolicy` fields, with `with_learned_pad()` and
  `with_veto_policy()` builders.
- In `verify_pair()`, after the analytic gates pass, the learned PAD backend
  runs on the same strobe differential `D`:
  - `backend.infer(&d, w, h)` → `PadResult`
  - `evaluate(&analytic_result, &learned, &veto_policy)` → `FinalVerdict`
  - `FinalVerdict::Live` → proceed to face matching
  - `FinalVerdict::AnalyticSpoof` → `NoMatch("pad veto: analytic")`
  - `FinalVerdict::LearnedSpoof` → `NoMatch("pad veto: learned")`
  - `FinalVerdict::FailClosed` → `Unavailable("learned pad unavailable")`
  - Inference `Err` → `Unavailable("learned pad failed")`
- Model failure (load or inference) → `Unavailable`, NOT silent
  analytic-only fallback (spec §6.6).
- `external_reason()` already strips all `"pad veto: ..."` reasons to `None`
  for `NoMatch` — the learned veto ("pad veto: learned") is covered by the
  existing `starts_with("pad veto")` check.
- 11 new verify tests: both veto orders (analytic-first, learned-first),
  both pass, FailClosed → Unavailable, inference failure → Unavailable, load
  failure prevents construction, privacy boundary, unchanged matcher
  threshold, learned_disabled bypass, CPU-only provider, generic Unavailable
  reasons.

#### `crates/namtor-daemon/src/bin/namtor-daemon.rs` — startup wiring

- The PAD model path (`validated_paths[2]`, already validated by Task 6's
  5-gate pre-spawn validation) is loaded into `OnnxCpuLearnedPad::load()`
  at startup. If the load fails, the daemon exits with `FAILURE` — the
  learned PAD model is mandatory (fail-closed).
- The `VerifyEngine` is constructed with `.with_learned_pad(Box::new(learned_pad))`.

### Verification

```
cargo test -p namtor-worker --lib          → 73 passed, 3 ignored (hw-gated)
cargo test -p namtor-daemon --lib verify:: → 35 passed, 0 failed
cargo clippy -p namtor-worker -p namtor-daemon -- -D warnings → clean
cargo build -p namtor-daemon --bin namtor-daemon → compiles
```

### Evidence

- `docs/evidence/public-release/07-pad-veto-pass.json` — 16 gates, all pass
- `docs/evidence/public-release/07-pad-veto-failure.json` — 14 gates, all pass

### Key decisions

- **CPU-only, in-process**: `OnnxCpuLearnedPad` loads the model in the
  daemon process (same as `RecognitionPipeline` for YuNet/SFace), using the
  `ort` crate's CPU EP. The sandboxed worker process is not involved — the
  verify engine runs in the daemon. The CPU-only constraint (spec §5.3) is
  enforced by the session builder: `with_intra_threads(1)` and no GPU EP
  registration (the CPU EP is the `ort` default).
- **Trait signature change**: `infer()` now takes `(differential, w, h)`
  because the ONNX backend needs spatial dimensions for resizing. The mock
  backend ignores them. This is a backward-incompatible change to the
  `LearnedPadBackend` trait, but the trait is internal (no external callers).
- **`evaluate()` simplified**: the unused `_backend: &B` generic parameter
  was removed. The function is pure — it only uses the analytic result, the
  learned `PadResult`, and the `VetoPolicy`. The backend is not consulted
  during the decision (the caller already ran `infer()` to produce the
  `PadResult`).
- **FailClosed → Unavailable, not NoMatch**: when the learned model returns
  `Uncertain` (low confidence, unavailable), the outcome is
  `Unavailable("learned pad unavailable")`, not `NoMatch`. This is stronger
  than a veto — the system cannot authenticate because the liveness check
  is non-functional, not because the user was rejected. This matches the
  task requirement: "Model failure → Unavailable, NOT silent analytic-only
  fallback."
- **Privacy boundary**: learned PAD veto reasons ("pad veto: learned",
  "pad veto: analytic") are stripped to `None` by the existing
  `external_reason()` function, making them indistinguishable from a
  reason-less NoMatch. Unavailable reasons ("learned pad failed", "learned
  pad unavailable") are passed through but contain no gate names, scores, or
  biometric data — they are as generic as the existing "pad failed" and
  "inference failed" reasons.

## Model Card — MiniFASNet PAD (`minifasnet_pad`)

### Overview

| Field | Value |
|---|---|
| Model ID | `minifasnet_pad` |
| Version | 0.1.0 |
| Architecture | MiniFASNet (depthwise-separable CNN, GAP, linear classifier) |
| Parameters | ~80,000 (49 KB ONNX) |
| Input shape | `[1, 1, 80, 80]` (single-channel strobe differential `D = On − Off`) |
| Output shape | `[1, 2]` (class 0 = live, class 1 = spoof) |
| Licence | Apache-2.0 (self-trained; code reference: Silent-Face, Apache-2.0) |
| ONNX sha256 | `3f86c8e4bb1138098ed2f8dc1682c6e1e1d6c5fffcf65dd6955c0cd60579a155` |

### Training Recipe

- **Script**: `ml/recipes/train_pad.py`
- **Framework**: PyTorch 2.13.0+cu126, ONNX export opset 14
- **Hyperparameters**: 50 epochs, batch size 256, learning rate 1e-3, seed 42
- **Optimizer**: Adam
- **Loss**: Cross-entropy
- **Best-model selection**: highest validation accuracy (model state saved at best epoch)
- **Data augmentation**: per-subject gain/offset perturbation (device-axis modelling)
- **Subject-disjoint split**: 125 train / 15 val / 15 test subjects (no subject appears in more than one split)
- **Synthetic-only flag**: `--synthetic-only` (default True); `--allow-real-data` is currently refused regardless

### Dataset Description

The model is trained **entirely on physics-based synthetic strobe-differential
data** — no real biometric data, no licence-restricted dataset. The synthetic
data encodes the signals described in the Nam-tor design spec §6.4:

- **Live faces**: 1/r² inverse-square falloff, specular hotspot from 850 nm IR
  emitter, subsurface scattering simulation
- **Screen replay**: near-zero differential `D` (panels emit almost nothing at 850 nm)
- **Print attacks**: weak/flat differential
- **Cutout attacks**: uniform differential

Each synthetic subject's samples are drawn from a deterministic RNG stream
keyed by `(subject_id, class)`, ensuring reproducibility.

### Performance Metrics

- **Validation accuracy**: 100% (on synthetic validation split)
- **Held-out test accuracy**: 100% (on subject-disjoint synthetic test split)
- **APCER / BPCER on real attacks**: **UNMEASURED** — the model is trained on
  synthetic data only. The analytic PAD gates (§6.4) carry the liveness load
  until consent-gated real data collection (§10.4 `namtor-collect`) provides
  real attack samples for evaluation.

### Limitations

- **Synthetic-only training**: the 100% accuracy reflects the synthetic data
  distribution, not real-world attack diversity. Do not interpret this as a
  production readiness claim.
- **Fail-closed**: missing model, wrong shape, or uncertain inference →
  `Unavailable` (authentication refused, never silent fallback).
- **Veto, not weighted term**: the learned PAD is a hard veto. Both analytic
  AND learned PAD must pass for authentication to proceed.
- **No GUI threshold exposure**: `VetoPolicy` thresholds are config-only
  (spec hard rule).
