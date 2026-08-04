# Nam-tor Implementation Roadmap

> **For agentic workers:** This is the milestone index, not an executable plan.
> Each milestone links to its own plan document. Use
> superpowers:subagent-driven-development or superpowers:executing-plans on the
> individual milestone plans, not on this file.

**Goal:** Build Nam-tor — IR face authentication for Linux meeting the acceptance
gates in `docs/superpowers/specs/2026-08-04-nam-tor-design.md` §12.

**Source spec:** `docs/superpowers/specs/2026-08-04-nam-tor-design.md`

---

## Why milestones are planned incrementally

M0 and M1 are fully planned and ready to execute. M2–M10 are scoped here with
deliverables, dependencies, and acceptance criteria, but their bite-sized task
plans are written **after their blocking measurements land**.

This is not deferral for convenience. Three open risks (spec §13) can invalidate
whole design sections:

| Risk | Question | Blocks |
|---|---|---|
| **R3** | Can the IR emitter be controlled at all? | M6 entirely; liveness approach |
| **R1** | Will UVC allow simultaneous IR + RGB streams? | M6 fusion design |
| **R2** | Does firmware AE hunt against the strobe? | M6 PAD weighting |

M1 answers all three in days. Task-level plans for M2+ written before then would
encode assumptions rather than facts.

**Rule:** when a milestone's dependencies are satisfied, write its plan using
superpowers:writing-plans, then execute it.

---

## Global Constraints

These apply to every task in every milestone plan. Values copied verbatim from
the spec.

**Language and toolchain**
- Rust for all shipped runtime code. Edition 2021 or later. Workspace layout per spec §15.
- Python + PyTorch for `ml/` only. Never shipped to users.
- GTK4 + libadwaita (Rust) for GNOME UI. QML + thin C++ `KCModule` for KDE.
- JavaScript for the optional GNOME Shell extension.

**Licensing**
- `namtor-hwdb`, `namtor-probe`, `quirks/`: **Apache-2.0**
- Everything else: **GPL-3.0**
- Every crate carries an SPDX header. CI asserts it.
- **No model weights are ever committed.** No dataset is ever committed.
- Every dataset's licence is verified before use and recorded in `ml/DATASETS.md`.

**PAM module — enforced in CI (spec §3.4)**
- `panic = "abort"`
- No ML dependencies, no ONNX, no image decoding libraries
- No dynamic linking beyond libc and libpam
- Hard timeout, default 5 s
- Fail-closed: every error path returns `PAM_AUTHINFO_UNAVAIL`, never `PAM_SUCCESS`
- Binary size budget: < 500 KB

**Security invariants — asserted by tests, not by review**
- No biometric data in logs at any level (spec §8.6)
- Worker cannot open files, network sockets, or unauthorised syscalls (spec §5.3)
- `SO_PEERCRED` verified on every socket connection (spec §3.2)
- Face auth refused when `PAM_RHOST` is set, regardless of config (spec §8.2)

**Process**
- TDD. Test first, watch it fail, minimal implementation, watch it pass, commit.
- Commit after every task.
- No task ships without an independently runnable test.

**Reference hardware** (spec §1.2) — Arch Linux, kernel 7.1.x, systemd 261,
GNOME/Wayland, Intel i9-13900H (AVX2 + AVX-VNNI), TPM 2.0, Sonix `3277:0018`
camera on USB `3-8`, IR node `/dev/video2`, IR metadata node `/dev/video3`.

---

## Milestones

### M0 — Measuring stick

**Plan:** `2026-08-04-nam-tor-m0-eval-harness.md` — **ready to execute**

Cargo workspace, `CaptureSource` abstraction, `FileReplay`, session recording
format, synthetic fixture generator, and the Python eval harness computing
FAR/FRR/DET, APCER/BPCER per attack type, and latency distributions.

**Why first:** nothing in this project is a claim until it is measured, and
`FileReplay` is what makes the whole system CI-testable without a camera and
reproducible by contributors.

**Dependencies:** none
**Gates advanced:** foundation for G2, G3, G4; R4 becomes measurable

---

### M1 — Hardware spike

**Plan:** `2026-08-04-nam-tor-m1-hardware-spike.md` — **ready to execute**

UVC extension unit ioctl bindings, safe probe harness, automatic emitter
discovery via luminance delta, UVCH metadata parsing, strobe sequencing with
torn-frame rejection, dual-stream bandwidth test, AE behaviour measurement.

**Why immediately after M0:** retires R1, R2, R3 — the only unknowns that can
invalidate the design. Days, not weeks.

**Dependencies:** M0 (frame types, recording format)
**Gates advanced:** none directly; unblocks M2 and M6
**Output:** spike report + quirks database entry #1 for the reference camera

---

### M2 — Detection and capture

**Scope:** `namtor-hwdb` (USB descriptor parsing, format-GUID classification,
node correlation), `namtor-probe` CLI with contribution flow, quirks database
schema and loader, production capture layer built on M1's findings.

**Key deliverables**
- Descriptor parser recovering the `MFVideoFormat_L8` signal V4L2 discards (spec §4.1)
- Classification order: quirks entry → format GUID → heuristic, with the fallback always reported as such
- `namtor-probe` emitting a candidate quirks entry plus the exact `gh pr create` invocation
- `IrUvc` and `RgbUvc` capture sources

**Dependencies:** M1 (emitter method, strobe timings must be known)
**Gates advanced:** G1.1, G1.2, G1.3

---

### M3 — Daemon and sandboxed worker

**Scope:** `namtor-daemon`, `namtor-worker`, socket protocol, seccomp + landlock
sandbox, ONNX CPU inference with stock downloaded weights, model fetcher with
upstream licence acceptance.

**Key deliverables**
- Four-process topology per spec §3
- Socket ACL: `SO_PEERCRED`, uid 0 or `namtor-auth` group, nonce-bound replies
- Worker sandbox with escape tests
- `InferenceBackend` trait, `OnnxCpu` implementation, INT8 path
- Warm models / cold camera lifecycle (spec §5.5)

**Dependencies:** M2 (capture), M0 (eval harness to validate INT8 — resolves R4)
**Gates advanced:** G5.1, G5.2; R4, R5 resolved

---

### M4 — Enrollment and template storage

**Scope:** `namtor-store`, TPM2 sealing, AEAD template containers, enrollment
pipeline with quality filtering, per-user score normalisation, cross-sensor
transform fitting.

**Key deliverables**
- 25-frame quality-filtered enrollment, 8 diverse templates retained (spec §6.6)
- Per-user z-norm/t-norm calibration against background cohort
- TPM2-sealed AES-256-GCM containers, AAD binding uid + model_id
- Graceful fallback to root-only encrypted files, mode always reported
- `mlock`, `MADV_DONTDUMP`, zeroisation
- Extrinsic transform fitted from enrollment landmarks, no user configuration

**Dependencies:** M3
**Gates advanced:** G3 (Tier 1 accuracy levers), G5.5

---

### M5 — PAM integration

**Scope:** `namtor-pam`, socket client, failure modes, instant keypress fallback,
rate limiting, audit logging, PAM stack drop-ins.

**Key deliverables**
- All spec §8.3 failure modes with distinct messages and journal reason codes
- PAD veto and match failure indistinguishable to the user, distinguished in the journal
- Keypress aborts face auth and passes the keystroke through to the password prompt
- `PAM_RHOST` refusal, not user-configurable
- Per-uid exponential backoff with lockout

**Dependencies:** M4
**Gates advanced:** G1.3, G5.1, G5.3, G5.4

---

### M6 — Liveness, fusion, early exit

**Scope:** analytic PAD gates, dual-camera fusion, cross-sensor geometric
consistency, multi-frame consensus with temporal coherence, early exit.

**Key deliverables**
- Analytic gates: differential energy, spatial gradient consistency, specular
  highlight detection, radial falloff regression (spec §6.4)
- PAD as a veto gate, never a weighted term (spec §6.8)
- Darkness policy: fusion adds confidence, never subtracts
- Early exit at one pair on high confidence; escalation to five pairs

**Dependencies:** M5, and **M1 results** — R1, R2, R3 all shape this milestone
**Gates advanced:** G2, G4

---

### M7 — Desktop integration

**Scope:** D-Bus system service, polkit actions, GNOME settings app and
enrollment wizard, KDE KCM, GNOME Shell extension.

**Key deliverables**
- `org.namtor.Manager` / `Device` / `Settings` per spec §9.1
- Enrollment wizard refusing completion on insufficient quality
- Named operating points only — raw thresholds never exposed in a GUI
- Device-bound / non-exportable statement shown at enrollment

**Dependencies:** M5
**Gates advanced:** G1.1, G1.3

---

### M8 — Community data flywheel

**Scope:** `namtor-collect` with consent flow, quirks contribution pipeline,
contributor documentation.

**Key deliverables**
- Local-only by default; explicit informed consent at collection time
- Derived features preferred over raw face images where sufficient
- Per-session identifiers enabling withdrawal and deletion

**Dependencies:** M6
**Gates advanced:** R6, R7 mitigation; produces M9's data

---

### M9 — Trained models and published evidence

**Scope:** NIR fine-tune recipes, learned PAD training, full ablation table,
published eval report.

**Key deliverables**
- BN recalibration → paired VIS↔NIR distillation → pseudo-NIR augmentation (spec §10.2)
- MiniFASNet-class PAD trained on differential images
- Complete ablation table (spec §10.6)
- Eval report with sample sizes and confidence intervals; no rate published without its N

**Dependencies:** M8 (data), M0 (harness)
**Gates advanced:** G3, G4 at full strength

---

### M10 — Remaining scope

**Scope:** presence mode, depth-capable sensor path (`DepthUvc`), additional
backbones (R100), optional OpenVINO EP.

**Key deliverables**
- Long-lived IR-only capture session, lock-on-absence via logind
- Opt-in only, persistent session indicator
- Depth path for Realsense-class hardware (untestable on reference hardware)

**Dependencies:** M9
**Gates advanced:** completes spec scope

---

## Dependency graph

```
M0 ──┬─→ M1 ──→ M2 ──→ M3 ──→ M4 ──→ M5 ──┬─→ M6 ──→ M8 ──→ M9 ──→ M10
     │                  ↑                  │        ↑
     └──────────────────┘                  └─→ M7 ──┘
                (R4 via eval harness)
```

M7 depends only on M5 and can run in parallel with M6.

---

## Acceptance gate coverage

Every gate in spec §12 maps to at least one milestone. Verified in this table so
no gate is orphaned.

| Gate | Milestones |
|---|---|
| G1.1 zero-config on known device | M2, M7 |
| G1.2 automatic probe on unknown device | M2 |
| G1.3 specific message per failure mode | M2, M5, M7 |
| G2.1–G2.3 latency | M6, measured by M0 harness |
| G3.1–G3.2 FAR/FRR | M4, M9, measured by M0 harness |
| G4.1–G4.3 APCER/BPCER | M6, M9, measured by M0 harness |
| G5.1 no ML in PAM | M5 |
| G5.2 worker sandbox | M3 |
| G5.3 fail-closed | M5 |
| G5.4 no biometrics in logs | M5 |
| G5.5 templates unreadable off-device | M4 |

---

## Risk retirement schedule

| Risk | Retired by |
|---|---|
| R1 dual-stream bandwidth | M1 |
| R2 AE hunting vs strobe | M1 |
| R3 emitter controllability | M1 |
| R4 INT8 embedding degradation | M3, measured by M0 |
| R5 normalisation cohort licensing | M4 |
| R6 PAD generalisation | M9, ongoing |
| R7 quirks contributions | M8, ongoing |
| R8 kernel uvcvideo changes | ongoing maintenance |
