# Optional CPU Backend Ablation — Task 31 Report

Spec §5.3 (CPU-only auth path) + threat model. This report covers the
**structural evaluation harness** for optional CPU backends (OpenVINO, R100)
landed in `crates/namtor-worker/src/backend_eval.rs`. It does NOT promote any
backend. **Status: WITHHELD.**

## What landed

### `crates/namtor-worker/src/backend_eval.rs` (new, ~330 LOC incl. tests)

Public API:

| Item | Purpose |
|---|---|
| `BackendKind` enum | `OnnxCpu` (default), `OpenVino` (`openvino` feature), `R100` (`r100` feature). No GPU variant. Feature-gated, OFF by default. |
| `BackendKind::available()` | Variants enabled in the current build. Default build → `[OnnxCpu]` only. |
| `BackendKind::as_str()` | Stable machine-readable label. |
| `BackendEvalConfig` | `backend`, `model_hashes: Vec<[u8;32]>`, `licence_accepted: bool`, `sandbox_parity: bool`. |
| `AblationResult` | `backend`, `latency_median_us`, `latency_p95_us`, `far_at_frr3`, `frr_at_far1e4`, `n_comparisons`, `arch`. |
| `AblationReport` | `baseline`, `candidate`, `winner`, `promote: bool`, `reason: &'static str`. |
| `run_ablation(config, baseline, candidate)` | The single decision function. Promotes ONLY if all 7 gates pass. |
| `check_no_gpu(backend)` | Defence-in-depth: always true (no GPU variant exists). |
| `check_arch_support(backend)` / `supports_arch(backend, arch)` | x86_64 + aarch64 matrix. OpenVINO = x86_64 only. |
| `check_licence(hashes)` | True iff at least one model hash is bound. |
| `check_sandbox_parity(backend)` | Statically true for `OnnxCpu`; false for optional backends (operator must verify + set `config.sandbox_parity`). |
| `TARGET_FAR_AT_FRR3` / `TARGET_FRR_AT_FAR1E4` / `MIN_COMPARISONS_FOR_ABLATION` | Predeclared constants. |

### `crates/namtor-worker/Cargo.toml`

Added `openvino = []` and `r100 = []` features, both OFF by default. Enabling
them ONLY makes the corresponding `BackendKind` variant available for ablation
reporting — it does not link a real backend, change the default, or relax the
sandbox.

### `crates/namtor-worker/src/lib.rs`

Added `pub mod backend_eval;`.

## Promotion contract (7 gates, in order)

`run_ablation` promotes a candidate over the baseline ONLY when ALL hold:

1. **No GPU** — `check_no_gpu(candidate)` (no GPU variant exists).
2. **Architecture support** — `supports_arch(candidate.backend, candidate.arch)`.
3. **Licence accepted** — `config.licence_accepted && check_licence(&config.model_hashes)`.
4. **Sandbox parity** — `check_sandbox_parity(candidate.backend) || config.sandbox_parity`.
5. **Statistical validity** — `candidate.n_comparisons >= MIN_COMPARISONS_FOR_ABLATION` (1000).
6. **Predeclared accuracy** — `far_at_frr3 ≤ 1e-5` AND `frr_at_far1e4 ≤ 3e-2`.
7. **Latency gain** — `latency_p95_us < baseline.latency_p95_us` (strict).

First failing gate → `promote = false`, `winner = baseline.backend`, `reason`
names the gate. This is the WITHHELD state.

## Tests (12)

| Test | Asserts |
|---|---|
| `no_gpu_provider_ever_loaded` | Every available `BackendKind` passes `check_no_gpu`. |
| `candidate_wins_on_accuracy_and_latency` | All gates pass → `promote = true`. |
| `candidate_loses_on_accuracy_keeps_baseline` | FAR exceeds target → no promote. |
| `candidate_loses_on_latency_keeps_baseline` | p95 not strictly lower → no promote. |
| `incomplete_arch_support_keeps_baseline` | Candidate measured on unsupported arch → no promote. |
| `sandbox_parity_required` | OnnxCpu promotes without operator flag (static parity true). |
| `licence_required_for_promotion` | Three licence-gate failure modes (no flag, no hashes, neither). |
| `default_build_uses_onnxcpu_only` | `available() == [OnnxCpu]` in default build. |
| `below_min_comparisons_withholds` | n < 1000 → no promote. |
| `candidate_loses_on_frr_keeps_baseline` | FRR exceeds target → no promote. |
| `check_licence_empty_rejected` | Empty hash list rejected; non-empty accepted. |
| `supports_arch_matrix` | OnnxCpu on x86_64/aarch64 true; riscv64 false. |

## WITHHELD status — why no backend is promoted

This report is **WITHHELD** per the project's evidence policy. No gate claim
is published unless evidenced. The harness is structural: it defines the
contract and the gates, but no real OpenVINO/R100 backend is linked, no
real ablation measurements have been taken, and no operator has verified
sandbox parity for any non-`OnnxCpu` backend. Specifically:

- **G1 (no GPU):** PASS structurally — `BackendKind` has no GPU variant;
  `check_no_gpu` is total over the enum.
- **G2 (arch support):** PARTIAL — matrix is declared but not measured on
  real OpenVINO/R100 builds. OpenVINO is x86_64-only by declaration.
- **G3 (licence):** WITHHELD — no optional-backend model licences accepted.
  `NAMTOR_ACCEPT_LICENCE` not set for OpenVINO/R100 weights.
- **G4 (sandbox parity):** WITHHELD — `check_sandbox_parity` returns false
  for every non-`OnnxCpu` backend by default. The operator must revise the
  seccomp+landlock filter for the candidate's syscall surface and verify
  parity before setting `config.sandbox_parity = true`. Not done.
- **G5 (accuracy):** UNMEASURED — no real ablation cohort. R5 background
  cohort is owner-gated WITHHELD (task 16). Synthetic-only measurements
  would not satisfy the predeclared accuracy gate.
- **G6 (latency):** UNMEASURED — no real p95 measurements on the reference
  hardware (i9-13900H) for any optional backend.
- **G7 (statistical validity):** UNMEASURED — no comparison runs.

## What this does NOT do

- Does NOT change the default backend (`OnnxCpu`).
- Does NOT link OpenVINO or R100 runtimes.
- Does NOT relax the seccomp/landlock sandbox.
- Does NOT add a GPU execution provider (spec §5.3, threat model).
- Does NOT expose any backend-selection knob in the GUI.
- Does NOT commit model weights or biometric data.

## What the human needs to un-WITHHOLD

1. **OpenVINO / R100 runtimes** installed and linkable behind their feature flags.
2. **Licence acceptance** for any optional-backend model weights
   (`NAMTOR_ACCEPT_LICENCE=1` + recorded `LicenceRecord`).
3. **Sandbox revision** for the candidate's syscall surface, verified by a
   seccomp/landlock parity test (the existing `namtor_worker::sandbox` tests
   must still pass with the candidate loaded).
4. **R5 background cohort** (owner-gated dataset, task 16) for the accuracy
   gate to be measurable rather than synthetic.
5. **Real p95 latency measurements** on the reference hardware under the
   auth-path workload (Prepare→Verify→Release, not a microbenchmark).

Until those land, the default `OnnxCpu` backend is the only promoted backend
and the ablation report for any optional backend is WITHHELD.
