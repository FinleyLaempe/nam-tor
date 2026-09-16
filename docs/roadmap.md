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

This is not deferral for convenience: task plans written against unmeasured
assumptions encode fiction rather than facts.

**Status as of 2026-08-04 — the blocking risks were measured directly on the
reference hardware, ahead of implementation:**

| Risk | Question | Status |
|---|---|---|
| **R3** | Can the IR emitter be controlled at all? | **RESOLVED PASS.** MSXU unit 4 selector 6 mode 2 — the firmware strobes its own emitter, 1:1 alternation, illuminated 81.9 vs clean 0.13. One `SET_CUR`, no elevated privilege. |
| **R1** | Will UVC allow simultaneous IR + RGB streams? | **RESOLVED PASS.** Both streams ran. IR falls 25 → 19.8 fps under contention; a fused pair costs ~101 ms rather than ~66 ms. |
| **R2** | Does firmware AE hunt against the strobe? | **RESOLVED for Tier 0.** Settles in ~15 pairs, affects illuminated frames only, first pair has the best SNR. **Open for Tier 1/2** devices where userspace drives the strobe. |
| **R11** | What does opening the camera stream cost? | **NEW, open.** ~677 ms fixed (upper bound, measured via subprocess). Threatens gate G2.1 and may make `PrepareForAuth` warming mandatory. Re-measure in-process in M2. |

**Consequence: M2 is unblocked and its plan can now be written.** The emitter
method, strobe behaviour, and fusion feasibility are all known facts recorded in
`quirks/devices/3277-0018.toml`. M6 remains gated on M2's real capture data for
PAD calibration, not on these risks.

**Rule:** when a milestone's dependencies are satisfied, write its plan using
superpowers:writing-plans, then execute it.

---

## Global Constraints

These apply to every task in every milestone plan. Values copied verbatim from
the spec.

**Language and toolchain**
- Rust for all shipped runtime code. Edition 2021 or later. Workspace layout per spec §15.
- Python + PyTorch for `ml/` only. Never shipped to users.
- GTK4 + libadwaita (Rust) for the single, desktop-neutral UI. (A QML + C++ `KCModule` KDE KCM shipped through 0.1.0 and was removed in the production-readiness program; see M7.)
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

**Status: implemented.** `namtor_eval::score` shipped; harness is a foundation, no G2-G4 production run published. See `docs/evidence/status-audit.md`.

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

**Status: implemented (reference hardware only).** R1/R3 RESOLVED PASS, R2 RESOLVED Tier 0 on Sonix `3277:0018`. R10 (S3) WITHHELD. HW-gated tests `#[ignore]`. See `docs/evidence/status-audit.md`.

**Plan:** `2026-08-04-nam-tor-m1-hardware-spike.md` — **ready to execute**

UVC extension unit ioctl bindings, safe probe harness, automatic emitter
discovery via luminance delta, UVCH metadata parsing, strobe sequencing with
torn-frame rejection, dual-stream bandwidth test, AE behaviour measurement.

Plus **Task 14, Tier 0 discovery** — asking the camera to strobe its own emitter,
which is what Windows Hello does. Run it before the Task 10 emitter search.

**Status:** R1, R2 and R3 were measured directly on the reference hardware on
2026-08-04, ahead of implementation. M1 is now **reproduction and codification**
of known-good results rather than open-ended discovery: turn the throwaway
measurement scripts into tested code, confirm the numbers, and extend
`quirks/devices/3277-0018.toml` where fields are still `untested`.

Still genuinely open in M1: whether the emitter mode survives suspend (R10), and
the in-process camera-open cost (R11).

**Dependencies:** M0 (frame types, recording format)
**Gates advanced:** none directly; M2 already unblocked by the measurements
**Output:** spike report, plus verification of quirks entry #1

---

### M2 — Detection and capture

**Status: implemented.** `namtor_capture`, `namtor_camera` (emitter_ctrl/watchdog/resume/hotplug/selection/always_on) shipped. See `docs/evidence/status-audit.md`.

**Scope:** `namtor-hwdb` (USB descriptor parsing, format-GUID classification,
node correlation), `namtor-probe` CLI with contribution flow, quirks database
schema and loader, production capture layer built on M1's findings.

**Key deliverables**
- Descriptor parser recovering the `MFVideoFormat_L8` signal V4L2 discards (spec §4.1)
- Classification order: quirks entry → format GUID → heuristic, with the fallback always reported as such
- `namtor-probe` emitting a candidate quirks entry plus the exact `gh pr create` invocation
- `IrUvc` and `RgbUvc` capture sources
- **Tiered probing** (spec §4.10.1): MSXU tier runs silently with no prompt; vendor XU tier keeps the consent gate
- **Emitter lifecycle** (spec §4.10.2): watchdog with a 2 s maximum on-time, `ExecStopPost=` teardown, start-up reconciliation forcing the emitter off before anything else
- **Resume handling** (spec §4.10.3): logind `PrepareForSleep` subscription, device reconciliation and emitter re-application on resume, requests queued rather than failed during reconciliation
- **Hotplug** (spec §4.10.4): udev rule on UVC add/remove, deterministic multi-camera selection order, selected device always reported in `namtor-cli status`
- **Always-on detection** (spec §4.10.5): record `method = "always_on"` and surface the reduced protection rather than assuming strobe works

**Dependencies:** M1 (emitter method, strobe timings must be known)
**Gates advanced:** G1.1, G1.2, G1.3, G1.4, G1.5, G1.6; retires R9, R10

**Note on G1.6:** the suspend/resume gate requires a scripted S3 cycle repeated
20 times with 20 successes. It cannot be satisfied by any test that does not
actually suspend the machine, so it is hardware-in-the-loop by construction.

---

### M3 — Daemon and sandboxed worker

**Status: implemented.** Daemon, worker, sandbox (seccomp+landlock), lifecycle, supervisor shipped. G5.2 structural gate passes. R4 INT8-vs-FP32 WITHHELD (blocked on human). See `docs/evidence/status-audit.md`.

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

**Status: implemented (complete).** `namtor_store` (AES-256-GCM) + `namtor_recog` (YuNet+SFace) + CLI enroll shipped. TPM2 sealing complete (task 26; software/reference TPM tested, cross-hardware WITHHELD). z-norm/t-norm WITHHELD (task 16; no lawful cohort). RGB↔IR fusion WITHHELD (task 17; extrinsics unmeasured). See `docs/evidence/status-audit.md`.

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

**Status: implemented (structural gates).** `pam_namtor.so` 457920 < 512000 bytes, fail-closed, no ML deps. G5.1 structural test passes. Production PAM-stack install owner-gated (WITHHELD). See `docs/evidence/status-audit.md`.

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

**Status: implemented (complete, uncalibrated).** Four analytic PAD gates shipped as a veto; all UNCALIBRATED (conservative defaults, not tuned against a real attack corpus; calibration is task 21). Fusion implemented but unmeasured (task 17). Temporal consensus / early exit shipped (task 18). G4 production claim WITHHELD. See `docs/evidence/status-audit.md` and the calibration note below.

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

#### M6 needs a calibration sub-task, not just implementation

Spec §6.4 describes the analytic PAD gates **qualitatively** — differential
energy, radial falloff regression, specular highlight detection — and gives no
numeric thresholds. That is correct and deliberate: the numbers must come from
real strobe data, which does not exist until M1 succeeds and real captures are
recorded. Writing constants into the spec beforehand would have been invention.

The consequence is that **M6 cannot be planned as pure implementation.** Its
plan must open with a calibration task that:

1. Records bona fide sessions across pose, distance, and ambient IR, plus attack
   sessions for each type in the §10.6 battery, using `namtor-collect`.
2. Computes the distribution of each analytic statistic separately for bona fide
   and attack presentations.
3. Chooses thresholds from the measured separation at the BPCER budget gate G4.1
   allows — not from intuition, and not from a single sample.
4. Records the chosen thresholds, the data they came from, and the measured
   APCER per attack type in the eval report.
5. Stores per-device threshold overrides in the quirks database where sensor
   response differs enough to matter.

Ordering: calibration precedes gate implementation, because a gate whose
threshold is guessed is not a gate. Budget real capture time for it — this is
the step where a rushed M6 quietly becomes Howdy's magic-number problem wearing
better architecture.

**If M1 returns TIER-0-PASS**, calibration gets easier: firmware-paired frames
remove torn-frame noise from every statistic, so the distributions separate more
cleanly and the thresholds are more stable across devices.

---

### M7 — Desktop integration

**Status: implemented (complete, redesigned).** `namtor_ui` (renamed from `namtor_gnome`) GTK4/libadwaita enrollment wizard + settings app shipped (redesigned to AdwPreferencesDialog idiom). A KDE KCM also shipped (tasks 21-25; Rust async D-Bus bridge, QML KCM, C++ KCModule shim) and was **removed** in the production-readiness program: the project consolidated on one desktop-neutral GTK4 UI, and the KCM's only unique logic (a `kscreenlockerrc` authenticator writer that was never wired into its QML) now lives in `namtor-pam-config::desktop_lockscreen`. D-Bus `org.namtor.Manager` system-bus service shipped (tasks 8-13; methods, signals, polkit, peer identity, enrollment sessions, profile lifecycle). Manager3 continuous-enrollment contract shipped (enrollment redesign tasks 2/5/6). Runtime HIL WITHHELD (no root, no camera, no AT-SPI). See `docs/evidence/status-audit.md`.

**Scope:** D-Bus system service, polkit actions, settings app and
enrollment wizard, GNOME Shell extension. (The KDE KCM was in scope
originally and has since been removed — see the status note above.)

**Key deliverables**
- `org.namtor.Manager` / `Device` / `Settings` per spec §9.1
- Enrollment wizard refusing completion on insufficient quality
- Named operating points only — raw thresholds never exposed in a GUI
- Device-bound / non-exportable statement shown at enrollment

**Dependencies:** M5
**Gates advanced:** G1.1, G1.3

---

### M8 — Community data flywheel

**Status: implemented.** `namtor_collect` consent-gated PAD data collection shipped (ConsentRecord, per-session withdrawal IDs, CollectionStore). Production collection campaign (consented subjects + attack props) owner-gated, has not run. See `docs/evidence/status-audit.md`.

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

**Status: partial (synthetic-only).** MiniFASNet PAD trained on physics-based synthetic strobe differentials, 100% val on synthetic; cannot be production-selected (task 22 needs consented real data). NIR fine-tune recipes, full ablation table, published eval report not produced. G3/G4 at full strength WITHHELD. See `docs/evidence/status-audit.md`.

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

**Status: implemented.** Presence mode, depth path (`DepthUvc`), backend ablation shipped (production-readiness plan tasks 29-31). See `docs/evidence/status-audit.md`.

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
| G1.4 zero interaction on MSXU devices | M2 |
| G1.5 emitter off after every teardown path | M2 |
| G1.6 first unlock after resume succeeds | M2 |
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
| ~~R1 dual-stream bandwidth~~ | **RESOLVED PASS 2026-08-04** — works, IR 25→19.8 fps under contention |
| R2 AE hunting vs strobe | **RESOLVED for Tier 0** — settles in ~15 pairs, affects illuminated frames only. Open for Tier 1/2 |
| ~~R3 emitter controllability~~ | **RESOLVED PASS 2026-08-04** — MSXU mode 2, firmware strobes itself |
| R4 INT8 embedding degradation | M3, measured by M0 |
| R5 normalisation cohort licensing | M4 |
| R6 PAD generalisation | M9, ongoing |
| R7 quirks contributions | M8, ongoing |
| R8 kernel uvcvideo changes | ongoing maintenance |
| R11 camera open costs ~677 ms | M2, re-measure in-process; may make `PrepareForAuth` warming mandatory |
| R9 always-on emitters | M2, per device |
| R10 XU state lost across S3 | M2 |
