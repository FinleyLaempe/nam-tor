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
