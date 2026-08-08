# Manager Enrollment Capture (Task 11)

**Date:** 2026-08-08
**Commit:** `feat(manager): run transactional enrollment`
**Status:** implemented and tested (unit); production capture wiring (real camera + ONNX pipeline) is owner-gated.

## Summary

Wired real enrollment capture into `ManagerService` sessions. Previously
`commit_profile` persisted a `ProfileRecord` with an empty template list (task
10 scaffolding). This task populates the templates from a bounded,
quality-filtered, diversity-selected candidate reservoir driven by a
`CaptureSource` + `EmbedSource`.

## What changed

### New module: `crates/namtor-daemon/src/manager/session.rs`

- `EnrollmentCapture` — per-uid session state owning an optional
  `CaptureSource`, an optional `EmbedSource`, and a fixed-size candidate
  reservoir (default 100). Phases: `Capturing → Finished | Cancelled`.
- `EmbedSource` trait — abstracts "frame → (embedding, coarse quality)" so
  tests run without ONNX model weights. `MockEmbedSource` is a deterministic
  LCG-from-frame-bytes impl (mirrors `namtor_worker::MockBackend`); production
  wraps `namtor_recog::RecognitionPipeline` (detect → align → embed).
- `pump()` — pulls one frame, validates it, embeds it, offers the candidate to
  the reservoir (evict-lowest-quality-when-full), and returns a coarse
  `ProgressSnapshot` (frames_captured counter, QualityLabel, coverage_percent).
- `pump_until_exhausted()` — drains the source up to a 512-frame budget;
  no-op when no capture is configured (backward compat for store/bus tests).
- `finish()` — farthest-first diversity selection of ≤8 templates from the
  reservoir, zeroizes every held raw buffer, transitions to `Finished`.
- `cancel()` — zeroizes the reservoir, stops the capture source, transitions to
  `Cancelled`. Idempotent.
- `Drop` — zeroizes residual buffers and stops the capture source (defensive).
- Zeroize: raw frame buffers and embeddings are zeroized on evict, finish,
  cancel, and drop. No raw frame is persisted (only template vectors + quality).

### Modified: `crates/namtor-daemon/src/dbus.rs`

- Replaced the stub `Session` struct with `EnrollmentCapture`.
- `ManagerService` gains `capture_factory: Mutex<Option<CaptureFactory>>` and
  `embed_factory: Mutex<Option<EmbedFactory>>`.
- New constructor `new_with_capture(polkit, store, capture_factory, embed_factory)`.
  `new` / `new_with_store` retain no-capture behavior (bus/authz tests).
- `start_enrollment` — authorization + busy check, then opens a fresh
  `CaptureSource` + `EmbedSource` via the factories and stores an
  `EnrollmentCapture`. Arms the emitter.
- New internal method `pump_enrollment(session_id)` — drives one
  capture→embed→reservoir step and queues an `EnrollmentProgress` signal.
  Not a wire method (the frozen contract exposes progress only as a signal).
- `commit_profile` — calls `pump_until_exhausted` (auto-finalize for the
  start→commit path without explicit pumps), then `finish`, then writes the
  populated templates to the store via `create_profile`. Disarms the emitter,
  emits `EnrollmentCompleted` + `ProfileChanged`.
- `cancel_enrollment` — calls `EnrollmentCapture::cancel` (zeroize + stop
  capture), removes the session, disarms the emitter.
- `handle_disconnect` — for each live session owned by the disconnected peer,
  calls `cancel`, removes it, disarms the emitter.
- `review_profile` — quality now derived from stored templates' quality scores
  via `coarse_quality_for` (mean → `QualityLabel`), not just the `enabled`
  flag. `needs_reenroll` when disabled, empty gallery, or Low quality. No
  score or threshold crosses the surface — only the coarse bucket.

### Privacy

- `EnrollmentProgress` carries `frames_captured` (counter), `quality`
  (`QualityLabel`), `coverage_percent` (u32 0-100). No embedding, score,
  threshold, template, pixel, key, or nonce.
- `ReviewOutcome` carries `profile_id`, `needs_reenroll` (bool), `quality`
  (`QualityLabel`). No score or threshold.
- `progress_signal_carries_no_biometrics` and `no_biometrics_in_api_structs`
  tests assert the privacy invariant over serialized JSON.

## Tests (248 daemon lib tests green, 13 new)

New `manager::session` tests (8):
- `pump_pulls_frames_and_populates_reservoir`
- `finish_selects_diverse_templates_and_zeroizes`
- `finish_with_too_few_candidates_yields_empty_templates`
- `cancel_zeroizes_and_stops_capture`
- `pump_after_cancel_errors`
- `reservoir_evicts_lowest_quality_when_full`
- `snapshot_reports_coarse_progress_without_biometrics`
- `pump_without_capture_errors`

New `dbus` tests (13):
- `start_progress_review_finish_full_lifecycle` — start → pump → progress
  signal → commit → review (full replay lifecycle).
- `commit_writes_real_templates_from_reservoir` — templates are non-empty,
  ≤8, first is anchor.
- `busy_rejects_second_session_for_same_uid` — `SessionBusy`.
- `cancel_during_capturing_zeroizes_and_disarms` — emitter off, no profile, no
  `EnrollmentCompleted` signal.
- `cancel_after_finish_not_possible_via_wire` — committed profile survives
  post-commit cancel.
- `cancel_unknown_session_is_noop`.
- `disconnect_during_enrollment_cancels_and_disarms` — session removed, emitter
  off, no profile.
- `removal_during_enrollment_preserves_old_profile` — commit profile, start
  re-enroll, cancel → old profile intact.
- `daemon_restart_preserves_committed_profile_loses_in_progress` — committed
  profile survives `drop(svc)` + new instance; in-progress session lost.
- `emitter_off_on_cancel_with_capture` — status reflects disarmed emitter.
- `progress_signal_carries_no_biometrics` — JSON scan.
- `review_reflects_template_quality_not_enabled_flag` — high-quality templates
  → `High` label (not derived from `enabled`).
- `no_capture_factory_commit_writes_empty_templates` — backward compat.

Existing 31 dbus tests retained green (test `svc()` helper upgraded to
`new_with_capture` with mock factories).

## What is NOT done (owner-gated)

- **Real ONNX embedder in the manager path:** `EmbedSource` is abstracted;
  `MockEmbedSource` is wired in tests. Production needs a `PipelineEmbedSource`
  wrapping `RecognitionPipeline::new(yunet_path, sface_path, ...)` with model
  paths from `var/lib/namtor/models/`. The daemon binary must construct the
  factories and pass them to `new_with_capture`. This is a wiring task in
  `bin/namtor-daemon.rs` / `live_capture.rs`, not a logic change.
- **Background pump loop:** `pump_enrollment` is synchronous. The bus service
  (zbus async, `bus.rs` — frozen) does not call it on a timer. Production needs
  a background task or the daemon main loop to drive pumping and emit progress
  signals during enrollment. The method exists; the caller wiring is deferred
  to avoid modifying `bus.rs`.
- **Live camera HIL:** not testable without the reference camera + root.

## Test discipline

`cargo test -p namtor-daemon --lib -- --test-threads=1` — 248 passed, 1
ignored (HW-gated), 0 failed. The `--test-threads=1` flag is for pre-existing
flakes in `acl`/`licence` (shared `/tmp`/env state), not this task's tests.
