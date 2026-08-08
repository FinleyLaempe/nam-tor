# Enrollment-Fitted RGB↔IR Fusion Evidence

**Gate:** G2 — anti-spoof (dual-camera cross-sensor consistency)
**Status:** `partial` — fusion engine, extrinsics storage, and verify-engine wiring implemented and unit-tested; live dual-camera HIL WITHHELD.
**Date:** 2026-08-08
**Task:** 28 — Finish enrollment-fitted RGB↔IR fusion
**Commit:** `feat(recog): enrollment-fitted RGB-IR fusion`

## What shipped

### Fusion engine (`namtor-daemon/src/fusion.rs`)

Cross-sensor RGB↔IR fusion per spec §6.5. Three independent checks, none
of which Windows Hello performs:

1. **IR embedding** against the IR gallery (verify engine's existing path).
2. **RGB embedding** against the RGB gallery (verify engine's existing path).
3. **Cross-sensor geometric + embedding consistency** — owned by the fusion
   engine. The sensors sit ~2 cm apart on one bezel, so a real face produces
   landmark correspondences under a stable transform between views.

**Components:**

| Component | Purpose |
|---|---|
| `Affine2D` | 2D affine transform (6 floats), fitted from paired RGB→IR landmark correspondences |
| `EnrollmentTransform` | Affine + residual stats + device identity; fitted at enrollment, stored with profile |
| `FusionEngine` | Pure evaluator: combines IR + RGB embeddings under the darkness policy |
| `geometric_identity_check` | Verifies RGB face center, mapped through the transform, lands within tolerance of IR face center |
| `pair_frames` | SOF/timestamp pairing with torn-pair rejection |
| `FusionOutcome` | `Fused` / `IrOnly` / `Rejected` — serialises no biometric data |

**Darkness policy (spec §6.5):** RGB is useless in darkness, so the engine
falls back to IR-only with `reduced_protection = true`. Fusion may only add
confidence, never subtract it — the bar is never lowered to compensate.

### Extrinsics storage (`namtor-store/src/template.rs`)

`ExtrinsicsRecord` is a plain-data serialisable struct (affine `[f32; 6]`,
residual stats, device identity) stored with the enrollment record:

- `EnrollmentMeta.extrinsics: Option<ExtrinsicsRecord>` — the legacy
  single-record path, used by the verify engine.
- `ProfileRecord.extrinsics: Option<ExtrinsicsRecord>` — the multi-profile
  path, used by the enrollment wizard.

Both fields use `#[serde(default)]` for backward compatibility — old
containers deserialize with `extrinsics = None`.

Round-trip conversions between `ExtrinsicsRecord` (store) and
`EnrollmentTransform` (fusion engine) are tested in
`fusion::tests::extrinsics_record_roundtrip`.

### Verify engine wiring (`namtor-daemon/src/verify.rs`)

The verify engine now loads extrinsics from the enrollment record and
applies cross-sensor fusion after the IR match:

1. **Load:** `VerifyEngine::verify()` reads `record.meta.extrinsics` and
   builds a `FusionEngine` if present. A pre-configured engine
   (`with_fusion`) takes precedence (for testing).
2. **IR pipeline:** `verify_pair()` runs the existing IR path (strobe
   differential → PAD → detect → align → embed → gallery match → z-norm →
   threshold).
3. **Fusion veto:** If the IR outcome is `Match` and fusion context is
   available, `apply_fusion()` runs:
   - **Geometric identity check:** maps the RGB face box through the
     enrollment transform and compares to the IR face box. `Reject` →
     `NoMatch` (split spoof: RGB face not where the transform predicts).
     `Invalid` → IR-only fallback (degenerate transform, fail closed by
     not fusing).
   - **Cross-modal embedding consistency:** cosine similarity between IR
     and RGB embeddings. Below `cross_modal_threshold` (default 0.30) →
     `NoMatch` (sensors see different faces).
   - **Fused / IrOnly** → IR `Match` stands.

Fusion may only veto a match, never rescue a non-match. `NoMatch` /
`Unavailable` / `Refused` / `RateLimited` outcomes pass through unchanged.

### Test coverage

**Fusion module tests (14, `fusion::tests`):**

| Test | Verifies |
|---|---|
| `sof_pairing_accepts_within_tolerance` | Paired frames within 33 ms gap accepted |
| `sof_pairing_rejects_too_far_apart` | Paired frames > tolerance rejected |
| `sof_pairing_rejects_torn_pairs` | Missing SOF on either sensor rejected |
| `geometric_identity_accepts_colocated_face` | Co-located face accepted |
| `geometric_identity_rejects_shifted_face` | Shifted face rejected |
| `geometric_identity_invalid_for_degenerate_transform_or_bad_box` | Degenerate transform / bad box → Invalid |
| `darkness_policy_falls_back_to_ir_only` | Darkness → IrOnly(Darkness, reduced_protection) |
| `missing_rgb_embedding_falls_back_to_ir_only` | No RGB → IrOnly(NoRgbFace) |
| `matching_embeddings_produce_fused_outcome` | Identical embeddings → Fused(consistency ≈ 1.0) |
| `divergent_embeddings_are_rejected` | Orthogonal embeddings → Rejected(CrossModalMismatch) |
| `transform_invalidation_on_high_residual` | High residual → invalidation |
| `device_identity_mismatch_invalidates` | Device mismatch → transform invalid |
| `fit_recovers_transform_and_rejects_outlier` | Least-squares fit recovers known transform, rejects outlier |
| `fit_rejects_insufficient_correspondences` | < 3 correspondences → error |
| `affine_apply_and_degenerate` | Affine2D apply + degeneracy check |
| `fusion_config_serialises_roundtrip` | Config serialisation round-trip |
| `extrinsics_record_roundtrip` | ExtrinsicsRecord ↔ EnrollmentTransform preserves all fields |
| `fusion_outcome_serialises_no_biometric_data` | No biometric material in serialised outcome |

**Verify engine fusion integration tests (8, `verify::tests`):**

| Test | Verifies |
|---|---|
| `fusion_accepts_genuine_pair` | IR Match + RGB agrees → Match stands |
| `fusion_rejects_split_spoof` | IR Match + RGB disagrees → NoMatch |
| `fusion_rejects_geometric_mismatch` | IR Match + RGB face shifted → NoMatch |
| `fusion_falls_back_when_no_rgb` | IR Match + no RGB data → Match (IR-only) |
| `fusion_falls_back_when_no_extrinsics` | IR Match + no extrinsics → Match (IR-only) |
| `fusion_cannot_rescue_nomatch` | IR NoMatch + RGB present → still NoMatch |
| `fusion_preserves_unavailable` | IR Unavailable + fusion → still Unavailable |
| `fusion_improves_over_single_spectrum` | Split spoof: IR-only Match → fused NoMatch |

The `fusion_improves_over_single_spectrum` test is the key proof: an IR-only
path returns `Match` for a face that passes the gallery, but the fused path
catches the cross-modal disagreement (IR face ≠ RGB face) and downgrades to
`NoMatch`.

## What is WITHHELD

| Item | Reason |
|---|---|
| **Live dual-camera HIL** | Requires reference camera + paired RGB+IR capture. No camera in this session. |
| **Enrollment-time extrinsics fitting** | The `EnrollmentTransform::fit` API is tested with synthetic correspondences. Real fitting requires paired RGB+IR enrollment captures, which needs camera hardware. |
| **RGB capture source in verify path** | The verify engine's `verify()` passes `rgb: None` to `verify_pair` (no RGB capture source plumbed through daemon.rs/dispatch.rs yet). The fusion logic is fully wired and unit-tested via `apply_fusion`; the RGB capture plumbing is a daemon integration task. |
| **Measured FAR/FRR improvement** | Requires a dataset of paired RGB+IR spoof attacks. Synthetic tests prove the logic; measured error rates need the R5 cohort dataset (task 29). |

## Architecture notes

- **Late fusion:** The spec §6.5 describes fusion at the score/embedding
  level (late fusion), not pixel-level early fusion. The implementation
  matches: each sensor runs its own detect→align→embed pipeline, then the
  fusion engine combines embeddings + geometric position.
- **No biometric data in serialised state:** `FusionOutcome`,
  `FusionConfig`, and `ExtrinsicsRecord` carry only scalar geometry and
  enum tags — no embeddings, frames, or pixels. Verified by
  `fusion_outcome_serialises_no_biometric_data`.
- **CPU-only:** The fusion engine is pure arithmetic (cosine similarity,
  affine apply, L2 residual). No GPU, no ML inference in the fusion path.
- **Backward compatibility:** `extrinsics` field is `#[serde(default)]` on
  both `EnrollmentMeta` and `ProfileRecord`. Old containers load with
  `None` — the verify engine falls back to IR-only.
