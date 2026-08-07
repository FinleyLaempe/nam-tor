# G1 — Hardware-in-the-Loop Gate Suite

Spec reference: §4.10 (camera topology selection, emitter control, suspend/resume).
Date: 2026-08-06.

The G1 gate covers automatic hardware operation: the system must select the right
camera, drive the emitter safely, and recover from suspend without leaking the
emitter or losing the auth path. Most of this gate needs real hardware to measure,
so the sub-gates that depend on the reference camera (Sonix 3277:0018) are
WITHHELD. The code paths that support them are implemented and tested with mocks.

## G1.1 — Clean known-device enrollment

Status: **WITHHELD** (requires reference camera HIL).

The harness is ready. `namtor-cli enroll` drives the enrollment flow against the
daemon's capture pipeline, and `namtor-probe` verifies device selection against
the auto-detected topology (see `g1-selection.json`). Eleven inventory tests and
five pairing tests cover the deterministic selection logic on fake topologies.
What is missing is a single real enrollment run on the reference camera, captured
as evidence. CI cannot substitute for that.

## G1.2 — Novel-device probe sampling / ≥80% compatibility

Status: **WITHHELD**.

One device is known (Sonix 3277:0018). A broad ≥80% compatibility claim cannot be
made from a denominator of one. The contributor flywheel in
`crates/namtor-probe/src/flywheel.rs` is built and ready to accept community
hardware reports; each report widens the denominator. Until there are enough
reported devices, the claim stays narrowed and the gate stays WITHHELD.

## G1.3 — MSXU zero-prompt

Status: **WITHHELD** (requires MSXU camera HIL).

R3 PASS on the reference hardware: selector 6 mode 2 on the Sonix unit is a
firmware-strobed emitter, and the probe verifies it optically rather than by
asking the user whether a light came on (§4.10.1). The zero-prompt contract is
implemented in `emitter_ctrl`, with the MSXU path gated silent and the vendor-XU
path gated behind consent (`g1-emitter.jsonl` lines 1-2). A repeatable HIL
measurement on a second MSXU camera is needed to lift the WITHHELD.

## G1.4 — Failure messages

Status: **implemented**.

`namtor-cli status` and `namtor-cli diagnostics` produce actionable,
distro-specific error codes. The output is privacy-safe: no biometric data is
logged at any level, asserted by a test. Evidence: `docs/evidence/diagnostics.md`.

## G1.5 — Emitter teardown (≤200ms)

Status: **WITHHELD** (requires HIL measurement).

The enforcement scaffolding is in place. The watchdog enforces
`max_on_ms = 2000` and forces the emitter off after expiry
(`g1-emitter.jsonl` line 3). Startup runs `force_off` before any `apply_on`
(line 4), and the RAII `Drop` restores `payload_off` when the controller is
armed (line 5). The systemd unit sets `TimeoutStopSec=2` with an `ExecStopPost`
binary that fires even after SIGKILL to force the emitter off (line 7). The
≤200ms teardown target itself needs a real measurement on the reference camera.

## G1.6 — 20× S3 first-auth

Status: **WITHHELD** (requires real S3 cycles).

The logic is implemented and exercised with fake events. `HotplugState` plus the
`Reconciler` and `ReconcileGuard` cover remove→add cycling, suspend→resume
rediscovery, stale-node invalidation after renumber, missing-camera fallback,
and bounded queue overflow (fail-closed). The emitter is forced off on suspend
and device removal via `EmitterGuard` Drop. Fake-event tests pass 20/20 on the
simulated path (`g1-s3-20x.jsonl`). The real 20× S3 run needs the reference
camera and real suspend/resume cycles, which CI cannot provide.

## Contributor denominator

The G1.2 ≥80% compatibility claim stays narrowed and WITHHELD until the
contributor flywheel has collected enough device reports to support the
denominator. One known device is not enough. Community reports land in
`quirks/` and widen both the denominator and the claim.

## Summary table

| Sub-gate | Status | Evidence |
|---|---|---|
| G1.1 Clean known-device enrollment | WITHHELD | `g1-selection.json` (harness ready, 11 inventory + 5 pairing tests) |
| G1.2 Novel-device probe / ≥80% | WITHHELD | `crates/namtor-probe/src/flywheel.rs`; one device known |
| G1.3 MSXU zero-prompt | WITHHELD (R3 pass) | `g1-emitter.jsonl` lines 1-2; R3 PASS on reference hardware |
| G1.4 Failure messages | implemented | `docs/evidence/diagnostics.md` |
| G1.5 Emitter teardown ≤200ms | WITHHELD | `g1-emitter.jsonl` lines 3-5, 7; `packaging/systemd/namtor-daemon.service` |
| G1.6 20× S3 first-auth | WITHHELD (simulated pass) | `g1-s3-20x.jsonl`; 20/20 fake-event tests pass |
| Contributor denominator | WITHHELD | 1 known device, ≥80% claim needs more |
