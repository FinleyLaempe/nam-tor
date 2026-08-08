# G1 — Hardware Lifecycle Gate Suite

SPDX-License-Identifier: GPL-3.0-only

Spec reference: §4.10 (camera topology, emitter control, suspend/resume), §12 (G1 acceptance gates).
Task: 41 — Run G1 hardware lifecycle suite.
Date: 2026-08-08.

The G1 gate covers automatic hardware operation: the system must select the
right camera, drive the emitter safely, recover from suspend, and never leak
the emitter past teardown. Most sub-gates require real hardware to measure.
Sub-gates that depend on the reference camera (Sonix 3277:0018 on the ASUS
ROG Flow X16 GV601VI) are WITHHELD. Code paths that support them are
implemented and tested with mocks.

## Hardware reference facts

| Fact | Value | Source |
|---|---|---|
| Device | Sonix `3277:0018` IR+RGB camera | `quirks/devices/3277-0018.toml` |
| USB bus | `3-8` (interface 2) | sysfs enumeration |
| IR stream | `/dev/video2`, L8 `{00000032-0002-...}`, 640x360@30 | R1 measurement |
| RGB stream | `/dev/video0` | R1 measurement |
| MSXU unit | 4, selectors 6+9 on interface 2 | R3 measurement |
| MSXU mode | selector 6 mode 2 = firmware-strobed emitter | R3 PASS |
| Emitter teardown budget | watchdog max_on_ms = 2000, systemd TimeoutStopSec=2 | `packaging/systemd/namtor-daemon.service` |

## R1 through R11 results

| Requirement | Description | Status | Evidence |
|---|---|---|---|
| R1 | Dual-stream (IR + RGB) simultaneous capture | **PASS** | `namtor_capture::session::CaptureSession` opens both sources; HIL confirmed on reference hardware. `../m1-spike-report.md`. |
| R2 | AE hunt (auto-exposure does not defeat strobe differential) | **PASS** (Tier 0) | Resolved on the Sonix reference device. AE converges before the strobe pair, so `D = On - Off` is stable. `../m1-spike-report.md`. |
| R3 | MSXU zero-prompt emitter control | **PASS** | Selector 6 mode 2 is firmware-strobed. `namtor-probe` verifies optically, not by asking the user. `g1-emitter.jsonl` lines 1-2. |
| R4 | INT8 vs FP32 production inference | **WITHHELD** | Python onnxruntime binding incompatible with system Python 3.14. `docs/evidence/trust/int8-eval.md` records the structural harness and synthetic-input SFace latency (median 28.42ms INT8). Owner-gated. |
| R5 | Z-norm normalisation cohort | **WITHHELD** | No licence-verified dataset selected. `docs/evidence/trust/znorm.md` records the calibration interface, DirCohort loader, and reproducibility hash. Owner-gated. |
| R6 | Strobe-differential liveness (analytic PAD) | **implemented** (uncalibrated) | Four gates on `D = On - Off`: differential energy, gradient consistency, specular highlight, radial falloff. Thresholds are conservative defaults, not tuned against real attacks. `ml/reports/analytic-pad-calibration.md`. |
| R7 | Dual-camera RGB+IR fusion | **stub** / **unmeasured** | `RgbUvc` capture source exists; enrollment-fitted extrinsics, SOF pairing, geometric identity check are implemented (task 28, `docs/evidence/trust/fusion.md`). No fusion HIL latency or protection evidence. |
| R8 | Depth corroboration (optional) | **partial** | `namtor_capture::depth` ships `DepthSource` trait, `DepthVetoResult`, `depth_pad_veto`. No depth sensor on reference hardware. `DEPTH_HIL_VALIDATED = false`. `docs/evidence/depth-status.md`. |
| R9 | Always-on emitter detection | **implemented** | `namtor_camera::always_on::detect_always_on` compares idle baseline vs emitter-off frames. Surfaces `ProtectionLevel` to UI. |
| R10 | Emitter survives suspend (S3) | **WITHHELD** | `ResumeMachine` + `Reconciler` code exists. Fake-event S3 20x test passes (`g1-s3-20x.jsonl`). Real S3 cycle never executed. Needs reference hardware + scripted suspend/resume. |
| R11 | Camera-open cost | **measured** (~677ms) | `measure_open_cost` subprocess upper bound ~677ms. Re-measured in-process in M2. Threatens G2.1 if cold-start is not amortised by warm lifecycle. |

## G1 sub-gate ledger

### G1.1 — Clean known-device enrollment

Status: **WITHHELD** (requires reference camera HIL).

The harness is ready. `namtor-cli enroll` drives the enrollment flow against
the daemon's capture pipeline. `namtor-probe` verifies device selection
against the auto-detected topology (`g1-selection.json`). Eleven inventory
tests and five pairing tests cover the deterministic selection logic on fake
topologies. The live capture path still hardcodes `/dev/video2` in
`crates/namtor-daemon/src/bin/namtor-daemon/live_capture.rs`; auto-selection
wiring is task 7 territory (completed in the plan but not HIL-verified). A
single real enrollment run on the reference camera, captured as evidence, is
what is missing. CI cannot substitute for that.

### G1.2 — Novel-device probe sampling (>=80% compatibility)

Status: **WITHHELD**.

One device is known (Sonix 3277:0018). A broad >=80% compatibility claim
cannot be made from a denominator of one. The contributor flywheel in
`crates/namtor-probe/src/flywheel.rs` is built and ready to accept community
hardware reports; each report widens the denominator.
`CompatibilityDenominator::g1_2_passes()` returns false on an empty
denominator, so the claim cannot slip through. See
`docs/evidence/hardware-compatibility.md`.

### G1.3 — MSXU zero-prompt

Status: **WITHHELD** (requires MSXU camera HIL for a repeatable measurement).

R3 PASS on the reference hardware: selector 6 mode 2 on the Sonix unit is a
firmware-strobed emitter, and the probe verifies it optically rather than by
asking the user whether a light came on. The zero-prompt contract is
implemented in `emitter_ctrl`, with the MSXU path gated silent and the
vendor-XU path gated behind consent. A repeatable HIL measurement on a
second MSXU camera is needed to lift the WITHHELD.

### G1.4 — Failure messages (per-mode)

Status: **implemented**.

`namtor-cli status` and `namtor-cli diagnostics` produce actionable,
distro-specific error codes. The output is privacy-safe: no biometric data
is logged at any level, asserted by a test. Full §8.3 fault-injection matrix
is `partial` (distinct PAM/CLI reason codes shipped; PAD-veto detail is
generic externally, detailed internally). Evidence: `diagnostics.md`,
`failure-contract.md`.

### G1.5 — Emitter teardown (<=200ms)

Status: **WITHHELD** (requires HIL luminance measurement).

The enforcement scaffolding is in place. The watchdog enforces
`max_on_ms = 2000` and forces the emitter off after expiry. Startup runs
`force_off` before any `apply_on`. The RAII `Drop` restores `payload_off`
when the controller is armed. The systemd unit sets `TimeoutStopSec=2` with
an `ExecStopPost` binary that fires even after SIGKILL to force the emitter
off. The <=200ms teardown target itself needs a real luminance measurement
on the reference camera after four teardown paths (end, stop, SIGKILL,
suspend).

### G1.6 — 20x S3 first-auth

Status: **WITHHELD** (requires real S3 cycles).

The logic is implemented and exercised with fake events. `HotplugState`
plus the `Reconciler` and `ReconcileGuard` cover remove-to-add cycling,
suspend-to-resume rediscovery, stale-node invalidation after renumber,
missing-camera fallback, and bounded queue overflow (fail-closed). The
emitter is forced off on suspend and device removal via `EmitterGuard` Drop.
Fake-event tests pass 20/20 on the simulated path (`g1-s3-20x.jsonl`). The
real 20x S3 run needs the reference camera and real suspend/resume cycles,
which CI cannot provide. This is the R10 blocker.

## Summary table

| Sub-gate | Status | Evidence |
|---|---|---|
| G1.1 Clean known-device enrollment | WITHHELD | `g1-selection.json` (harness ready, 11 inventory + 5 pairing tests) |
| G1.2 Novel-device probe / >=80% | WITHHELD | `crates/namtor-probe/src/flywheel.rs`; one device known |
| G1.3 MSXU zero-prompt | WITHHELD (R3 pass on reference) | `g1-emitter.jsonl` lines 1-2; R3 PASS |
| G1.4 Failure messages | implemented | `diagnostics.md`, `failure-contract.md` |
| G1.5 Emitter teardown <=200ms | WITHHELD | `g1-emitter.jsonl` lines 3-5, 7; `packaging/systemd/namtor-daemon.service` |
| G1.6 20x S3 first-auth | WITHHELD (simulated 20/20 pass) | `g1-s3-20x.jsonl`; R10 blocker |
| Contributor denominator | WITHHELD | 1 known device, >=80% claim needs more |

No G1 sub-gate is `passed`. Two are `implemented` (G1.4 failure messages,
plus the simulated S3 path). The rest are WITHHELD pending real hardware
access. The code paths are tested with mocks; the measurements are not.
