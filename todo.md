# Nam-tor — TODO

> Generated 2026-08-11 from a deep audit of docs, code, security, and usability.
> Sources: `docs/evidence/status-audit.md`, `docs/threat-model.md`, all crate sources,
> GNOME/CLI usability audit, security audit. Severity: **SEC** = security, **USA** = usability, **POL** = polish.

---

## A. Owner-gated inputs (block production gates, need human action)

| ID | Input | Blocks | Severity |
|----|-------|--------|----------|
| O1 | Root: install systemd units, `namtor` user, `namtor-auth` group | G1.1/G1.5 HIL, live daemon run | SEC+USA |
| O2 | Python onnxruntime binding (system Python 3.14 incompat) | R4 INT8-vs-FP32 eval, task 16 | SEC |
| O3 | Model selection + upstream licence acceptance | R4, production inference | SEC |
| O4 | R5 cohort dataset (licence-verified) | G3 FAR/FRR, z-norm calibration | SEC |
| O5 | Consented human subjects + attack props for PAD calibration | G4 APCER/BPCER | SEC |
| O6 | Second TPM-equipped machine | G5.5 cross-hardware unseal | SEC |
| O7 | Scripted reference-hardware S3 HIL run (20x) | G1.6, R10 emitter-survives-suspend | SEC+USA |
| O8 | Signing keys + distro packaging matrices | Signed packages | POL |

---

## B. Acceptance gates WITHHELD

Structural CI gates G5.1, G5.2, G5.4, G5.6 = passed. Everything else = WITHHELD.

| Gate | Target | Blocker | Severity |
|------|--------|---------|----------|
| G1.1 | Zero-config on known device | HIL (O1) | USA |
| G1.2 | Auto-probe ≥80% success | Only 1 device tested (O5/community) | USA |
| G1.3 | Specific message per §8.3 failure mode | Full fault-injection matrix on production path | USA+SEC |
| G1.4 | Zero interaction on MSXU path | HIL (O1) | USA |
| G1.5 | Emitter off ≤200ms after teardown | HIL luminance measurement (O1) | SEC |
| G1.6 | First auth after S3 succeeds 20/20 | R10 untested (O7) | SEC+USA |
| G2.1 | Median latency ≤500ms | No production-path measurement | USA |
| G2.2 | p95 ≤1000ms | Same | USA |
| G2.3 | First boot ≤120% steady-state | First-boot latency unmeasured | USA |
| G3.1 | FAR ≤1e-5 at FRR ≤3% | No lawful cohort dataset (O4) | SEC |
| G3.2 | Reported with N + CIs | No production eval report | SEC |
| G4.1 | APCER ≤5% per print/IR-print/replay | PAD uncalibrated, no attack corpus (O5) | SEC |
| G4.2 | Per attack type, never averaged | No production report | SEC |
| G4.3 | 3D mask documented as not evaluated | Documentation when G4 runs | SEC(doc) |
| G5.3 | All §8.3 conditions fail closed (production) | Full fault-injection on installed path (O1) | SEC |
| G5.5 | Templates unreadable off-device (TPM) | Single TPM machine (O6) | SEC |

---

## C. Security gaps (from code audit)

### SEC-1. Analytic PAD gates uncalibrated — HIGH
- `namtor-pad/src/calibration.rs:12-14` — no real attack corpus, G4.1 WITHHELD.
- `namtor-pad/src/radial.rs:10-14` — radial gate runs on conservative default, not calibrated (task 17).
- Hardcoded thresholds: `namtor-pad/src/lib.rs` (`min_energy_ratio=0.04`, `min_gradient_score=0.45`, etc.).
- **Impact**: anti-spoof not evidenced. Screen replay / print / IR-print APCER unknown.
- **Fix**: collect consented attack corpus (O5), calibrate thresholds, run G4.

### SEC-2. Learned PAD (MiniFASNet) synthetic-only — HIGH
- `namtor-worker/src/learned_pad.rs` — trained on physics-based synthetic data.
- `#[ignore]` tests for real model inference (`NAMTOR_HW_TEST=1`).
- **Impact**: real-attack performance unmeasured. Fail-closed (Uncertain→refuse) keeps it safe but live-face reject rate unknown.
- **Fix**: train on real device-disjoint attack data (O5), re-evaluate.

### SEC-3. TPM2 file fallback — raw key in plaintext — HIGH (inherent, documented)
- `namtor-store/src/crypto.rs:590-591` — `FileBackend::seal` stores the raw 32-byte AES key in the sealed blob. No encryption, no KDF.
- Key file is `0600` owned by `namtor` user. If stolen + `.sealed` files copied → all templates decryptable offline.
- `tpm2` feature not enabled by default. Production uses `FileBackend`.
- **Impact**: disk theft or root compromise = full template exfiltration.
- **Fix**: enable real TPM2 sealing (task 11), or add password-derived key wrapping as a second layer (see section H below).

### SEC-4. z-norm cohort WITHHELD — hardcoded match threshold — HIGH
- `namtor-daemon/src/verify.rs:39` — `DEFAULT_MATCH_THRESHOLD = 0.45` (hardcoded fallback).
- `namtor-recog/src/znorm.rs` — `SyntheticCohort` is non-production bootstrap only.
- **Impact**: FAR/FRR at operating point not characterised. Global threshold may be too lenient or too strict per device.
- **Fix**: select licence-verified cohort dataset (O4), calibrate z-norm, replace hardcoded threshold.

### SEC-5. PollProgress D-Bus method unauthenticated — MEDIUM
- `namtor-daemon/src/manager/dbus.rs:752` — `poll_progress` takes no `peer` parameter, no authz.
- `namtor-daemon/src/manager/bus.rs:595-607` — no `conn`/`header` passed, peer identity never resolved.
- Session IDs are sequential (`sess3-N` from `AtomicU64`) — trivially guessable.
- **Impact**: any app on the system bus can poll any session ID, observe enrollment progress/timing of other users. Info leak + side-channel.
- **Fix**: add `peer: &PeerIdentity` parameter to `poll_progress`, call `authz_self(ENROLL_SELF, peer, session_uid)`.

### SEC-6. Worker sandbox: ioctl + mprotect surface — LOW-MEDIUM
- `namtor-worker/src/sandbox/seccomp.rs:39` — `ioctl` allowed broadly (large syscall surface).
- `namtor-worker/src/sandbox/seccomp.rs:31-32` — `mmap` + `mprotect` both allowed; `mprotect(PROT_EXEC)` not filtered.
- **Impact**: a vulnerability in an ioctl-reachable path or a W^X violation could escape.
- **Fix**: add seccomp arg filter denying `mprotect` with `PROT_EXEC` if ONNX runtime doesn't need it. Narrow `ioctl` allowlist if possible.

### SEC-7. Backoff lock window not persisted — LOW-MEDIUM
- `namtor-daemon/src/backoff.rs:19-23` — `backoff_until` is process-local `Instant`, not persisted. Attempt count IS persisted.
- **Impact**: daemon restart resets the active lock window to zero (though attempt count survives and next failure resumes the schedule). An attacker who can crash the daemon shortens the effective backoff.
- **Fix**: persist `backoff_until` as a monotonic timestamp alongside the attempt count.

### SEC-8. PeerIdentity.uid fragility — LOW
- `namtor-daemon/src/dbus.rs:387` — `authz_self` trusts `peer.uid` field. Currently only populated via trusted `resolve_peer_identity` / `from_creds`, but no type-level guarantee.
- **Impact**: a future code path that constructs `PeerIdentity` from untrusted data would bypass authz.
- **Fix**: make `PeerIdentity` fields private with a trusted constructor; document the invariant.

### SEC-9. S3 suspend/resume untested (R10) — SEC+USA
- No test exists for emitter survival across suspend. Lid-open is the most common interaction.
- **Impact**: emitter may not survive S3; first auth after resume may fail or run without emitter-off protection.
- **Fix**: scripted S3 HIL run (O7), verify emitter teardown + reconciliation on resume.

---

## D. Usability bugs (from GNOME/CLI audit)

### USA-0. PAM toggle: cannot enable any PAM service — OPEN
- **Symptom**: User flips a PAM service switch, it resets to disabled. "Authentication denied org.namtor.configure-pam" error.
- **Root cause 1** (fixed): Daemon used `system-bus-name` polkit subject — rejected because daemon (user `namtor`) is not a trusted caller for other identities' bus names. Fixed: switched to `unix-process` subject (pid-based, no trusted-caller restriction).
- **Root cause 2** (fixed): Presenter `set_pam_service_enabled` did not call `require_auth` → no client-side polkit prompt. Fixed: added `require_auth("org.namtor.configure-pam")`.
- **Root cause 3** (fixed in latest build): Client-side `PolkitAuth::check` used `system-bus-name` subject while daemon used `unix-process` — polkit may cache auth per subject type, causing the daemon's check to miss the client's cached auth. Fixed: both sides now use `unix-process` subject.
- **Still open**: The polkit `CheckAuthorization` call is blocking the GTK main thread (synchronous `zbus::blocking::Connection::call_method` from a switch handler). This may freeze the UI during the password dialog. If the polkit agent can't show the dialog (e.g. no polkit agent running, or the blocking call times out), the toggle fails silently. Needs testing after package reinstall.
- **Scroll-to-top**: `render_pam_services` rebuilds the entire list on every toggle → scroll resets to top. Needs scroll-position save/restore or incremental update.
- **Files**: `crates/namtor-daemon/src/manager/polkit.rs` (build_subject), `crates/namtor-gnome/src/polkit_auth.rs` (subject type), `crates/namtor-gnome/src/presenter.rs` (require_auth), `crates/namtor-gnome/src/components/pam_services.rs` (scroll reset)

### USA-0b. Diversity metric mismatch (100% during capture, 4% in review) — FIXED
- `crates/namtor-daemon/src/manager/worker.rs:474` — `compute_progress` used `snapshot.coverage_percent` (reservoir fill %) as `diversity_percent`, but `review_snapshot()` used actual pairwise cosine distance. Completely different metrics. Fixed: use `capture.diversity_percent()` in both places.

### USA-1. "Stats" button does nothing (no re-render) — FIXED
- `crates/namtor-gnome/src/app.rs:190-197` — `on_review` calls `review_existing_profile` but did NOT call `render_profiles` afterwards. Fixed: added re-render call.
- **Fix**: add `rc_review.borrow_mut().render_profiles(&rc_review);` after the presenter call.

### USA-2. Offline retry button is a no-op — HIGH
- `crates/namtor-gnome/src/app.rs:516-518` — `show_offline_all` passes `|| {}` as `on_retry`.
- **Impact**: user cannot recover from daemon-down without restarting app.
- **Fix**: implement `retry_connection` on `AppController` that re-runs `DbusManagerClient::connect()` and rebuilds.

### USA-3. Re-enroll creates duplicate instead of replacing — HIGH
- `crates/namtor-gnome/src/app.rs:178-189` — `on_reenroll` calls `start_enrollment_v3(name)` which has NO duplicate-name check (unlike v1 `start_enrollment`).
- **Impact**: clicking "Re-enroll" on "Home" creates a second "Home" or fails at commit.
- **Fix**: add duplicate-name check to `start_enrollment_v3`; wire `on_reenroll` through `request_replace`/`confirm_replace`.

### USA-4. `namtor-cli status` fabricates output — HIGH
- `crates/namtor-cli/src/bin/namtor-cli.rs:137-141` — constructs `CameraManager` with all-default, never queries daemon. Always prints "stream: closed / protection: full".
- **Fix**: connect to daemon `GetStatus`, build from reply; or remove the subcommand.

### USA-5. Save errors invisible in enrollment view — MEDIUM
- `crates/namtor-gnome/src/app.rs:495-509` — save failure sets `model.error` but `render_enrollment`'s Stopped branch doesn't render it. Error only shows in `render_profiles`.
- **Fix**: render `model.error` at top of `render_enrollment`, or transition to `Error` state on save failure.

### USA-6. No reconnect after daemon restart — MEDIUM
- `crates/namtor-gnome/src/dbus_client.rs:52-76` — `connect()` called once at build; no reconnect path.
- **Impact**: after daemon restart, every click fails until app restart.
- **Fix**: wrap client in `Rc<RefCell<Option<DbusManagerClient>>>`; auto-reconnect on `ServiceNotRegistered`/`Transport`.

### USA-7. CLI v3 enroll shows no live progress — MEDIUM
- `crates/namtor-cli/src/enroll_session.rs:1060-1065` — `next_progress` always returns `Ok(None)`.
- **Fix**: call the daemon's `PollProgress` method on a timer (like the GNOME app does).

### USA-8. System status "Restart Daemon" and "View Logs" are no-op stubs — MEDIUM
- `crates/namtor-gnome/src/components/system_status.rs:124-126, 139-141` — `connect_activated` closures are empty.
- **Fix**: wire to `systemctl restart namtor-daemon` (polkit-authorized) and `journalctl`; or set `set_activatable(false)`.

### USA-9. Diversity hits 100% with no "ready to save" signal — MEDIUM
- `crates/namtor-gnome/src/components/enrollment.rs:218-225` — progress bar fills to 100% but no visual cue to stop.
- **Fix**: when `diversity_percent >= 100` and `quality_bucket` is good, show "Enrollment complete — press Stop to review" and style the Stop button as suggested-action.

### USA-10. No rename UI — MEDIUM
- `crates/namtor-gnome/src/components/profile_list.rs` — no RenameCallback type or button.
- `presenter.rename_profile` exists and works.
- **Fix**: add a "Rename" button + `adw::EntryRow` or dialog; wire to `presenter.rename_profile`.

### USA-11. Polkit unavailable surfaces as generic "Authentication required" — MEDIUM
- `crates/namtor-gnome/src/polkit_auth.rs:50-85` — `Cancelled` variant never used; `Bus`/`Check` errors collapse to generic denial.
- `presenter.rs:80` — error only `eprintln!`'d, not shown to user.
- **Fix**: distinguish `Bus`/`Check` from `Denied`/`Cancelled` in `require_auth`; surface "Polkit service unavailable" vs "Authentication dismissed" vs "Authentication denied".

### USA-12. Recovery view renders title twice — LOW
- `crates/namtor-gnome/src/components/recovery.rs:31-48` — line 38 appends `hint.title`, line 44 appends `hint.title` again as "detail".
- **Fix**: drop the second label or render a preamble/`hint.steps` instead.

### USA-13. CLI `list` and `remove` bypass daemon — MEDIUM
- `crates/namtor-cli/src/list.rs`, `enroll.rs` — open encrypted store directly, require root/`namtor` user, bypass polkit and audit.
- **Fix**: add `--daemon` mode (default) using D-Bus `ListProfiles`/`DeleteProfile`; keep `--local` for recovery.

### USA-14. CLI `--old` enroll hardcodes `/dev/video2` and `3277:0018` — LOW
- `crates/namtor-cli/src/enroll.rs:372-384` — `--device` arg accepted but ignored.
- **Fix**: honor `--device`, probe quirks by actual VID:PID; or deprecate `--old`.

### USA-15. CLI SIGINT discards enrollment with no review — LOW
- `crates/namtor-cli/src/enroll_session.rs:894-913` — Ctrl+C calls `cancel` + prints "discarded", no review prompt.
- **Fix**: on SIGINT, call `stop` + `review`, print review summary, ask Save/Discard.

### USA-16. UI blocks on D-Bus calls (dispatch_async unused) — MEDIUM, latent
- `crates/namtor-gnome/src/app.rs:147-204` — all callbacks call synchronous presenter methods on the GTK main thread.
- `presenter.dispatch_async` exists but is never called.
- **Fix**: route mutations through `dispatch_async` + `glib::MainContext` idle callback.

---

## E. Stale documentation / comments (polish)

| File:Line | Issue |
|-----------|-------|
| `AGENTS.md` | Says "KDE KCM and D-Bus API not started" — both ARE shipped. Update. |
| `AGENTS.md` | Says "M10 not started" — M10 IS implemented (tasks 29-31). Update. |
| `namtor-daemon/src/dbus.rs:9` | "task 9 wiring" — bus service IS implemented. |
| `namtor-daemon/src/listener.rs:26-27` | "Task 17 implements this… Task 13 tests with a stub" — lifecycle IS implemented. |
| `namtor-daemon/src/manager/bus.rs:22` | "Proper polkit wiring is task 12" — SystemPolkit IS the wiring. |
| `namtor-kde/src/abi.rs:10-14` | "Task 22 NOT implemented here" — bridge.rs + namtor_kcm.cpp ARE implemented (1388 lines). |
| `namtor-probe/src/probe.rs:187` | "IrUvc (Task 12) will eventually" — IrUvc exists. |
| `namtor-probe/src/emit.rs:30,56-57` | Report template emits `TODO` fields for human fill-in (intentional). |
| `docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md` | Stale on M7/M10/KDE/D-Bus status vs status-audit. |

---

## F. Production plan — final verification wave

- All 49 implementation tasks: complete.
- **F1** (plan compliance + evidence audit): done (status-audit updated).
- **F2, F3, F4**: in progress — not confirmed complete. Check `.omo/plans/namtor-complete-product.md`.
- Enrollment redesign: tasks 1-14 complete; **task 15 (installed HIL) WITHHELD**; task 16 (screenshot review) certified.

---

## G. Threat model — residual risks

| Threat | Defence status |
|--------|---------------|
| Printed photo, IR-optimised | Moderate residual — uncalibrated |
| 2D cutout mask | Moderate residual — radial falloff uncalibrated |
| High-quality 3D mask | Out of scope, unevaluated (G4.3 documents exclusion) |
| Evil maid / physical access | Out of scope — Secure Boot disabled on reference HW |
| Root compromise | Out of scope by design — root can re-enroll |
| Coerced/unconscious auth | Out of scope by design — sudo/su opt-in |
| Template poisoning | Defences described; aging is opt-out; **aging impl status NOT verified in any audit gate** |

---

## H. TPM2 double-encryption design question

**User asked**: "What do you think about if once we got TPM we do it double with 0600 aes?"

**Answer**: Yes — this is a good defense-in-depth pattern and aligns with how LUKS, gnome-keyring, and fscrypt work. The design would be:

1. **TPM2 seals a 32-byte key** (the "outer key") — hardware-bound, non-exportable.
2. **The outer key encrypts the template container** via AES-256-GCM (same as now).
3. **Additionally, the key file itself is `0600`** and the daemon refuses to start if the key file is world-readable.

But "double with 0600 aes" can mean two different things:

### Option A: TPM2 as the ONLY key source, 0600 as file protection (recommended)
- TPM2 unseals the AES key at daemon start → key lives in `mlock`'d memory → encrypts/decrypts templates.
- Key file (`/var/lib/namtor/key`) is the TPM2-sealed blob, mode `0600`, owner `namtor`.
- If TPM2 is absent: fall back to a **password-derived key** (argon2id from a daemon-startup password or a root-readable secret). The `0600` file stores the salt + derived-key-wrapped master key.
- This is what the codebase already structurally supports (`BackendSelector`, `FileBackend`, `Tpm2Backend`). The gap is that `FileBackend` stores the raw key in plaintext — adding a KDF wrap would be the "double" layer.

### Option B: TPM2 + a second AES layer (belt-and-suspenders)
- TPM2 unseals outer key → outer key decrypts an inner key file → inner key encrypts templates.
- Two layers of AES-256-GCM. If TPM2 is compromised but the inner key file is `0600` and not yet decrypted, the inner layer holds.
- **Not recommended**: adds latency on every template read/write for marginal gain. If the attacker has root, they can read the decrypted inner key from process memory. The second layer only helps against disk theft where TPM2 unseal somehow worked off-device (which TPM2 already prevents).

### Recommendation
**Option A with a KDF-wrapped fallback.** When TPM2 is available, use it. When it's not, wrap the AES key with a key derived from a root-owned secret file (`0600`, not a user password — daemons can't prompt). This gives:
- Hardware binding where TPM2 exists (templates can't leave the machine).
- A software layer that's more than just "raw key in a 0600 file" where TPM2 is absent.
- No double-encryption latency.

The current `FileBackend::seal` storing the raw key is the weakest link. Wrapping it with `argon2id(secret_file, salt)` would make disk theft alone insufficient — the attacker needs both the `.sealed` templates AND the root-owned secret file.

**Severity**: SEC-3 in section C above. This is the highest-impact security gap that can be fixed with code alone (no owner-gated input needed, unlike PAD calibration or cohort datasets).
