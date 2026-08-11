# Nam-tor — TODO

> Generated 2026-08-11 from a deep audit of docs, code, security, and usability.
> Sources: `docs/evidence/status-audit.md`, `docs/threat-model.md`, all crate sources,
> GNOME/CLI usability audit, security audit. Severity: **SEC** = security, **USA** = usability, **POL** = polish.
>
> Pruned 2026-08-12: code-fixable items (SEC-3/5/6/7/8, USA-0/2-16, section E stale-doc)
> resolved by the `namtor-todo-fix-and-gnome-redesign` plan. Owner-gated items below remain.

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

### SEC-4. z-norm cohort WITHHELD — hardcoded match threshold — HIGH
- `namtor-daemon/src/verify.rs:39` — `DEFAULT_MATCH_THRESHOLD = 0.45` (hardcoded fallback).
- `namtor-recog/src/znorm.rs` — `SyntheticCohort` is non-production bootstrap only.
- **Impact**: FAR/FRR at operating point not characterised. Global threshold may be too lenient or too strict per device.
- **Fix**: select licence-verified cohort dataset (O4), calibrate z-norm, replace hardcoded threshold.

### SEC-9. S3 suspend/resume untested (R10) — SEC+USA
- No test exists for emitter survival across suspend. Lid-open is the most common interaction.
- **Impact**: emitter may not survive S3; first auth after resume may fail or run without emitter-off protection.
- **Fix**: scripted S3 HIL run (O7), verify emitter teardown + reconciliation on resume.

---

## D. Usability bugs (resolved items retained for reference)

### USA-0b. Diversity metric mismatch (100% during capture, 4% in review) — FIXED
- `crates/namtor-daemon/src/manager/worker.rs:474` — `compute_progress` used `snapshot.coverage_percent` (reservoir fill %) as `diversity_percent`, but `review_snapshot()` used actual pairwise cosine distance. Completely different metrics. Fixed: use `capture.diversity_percent()` in both places.

### USA-1. "Stats" button does nothing (no re-render) — FIXED
- `crates/namtor-gnome/src/app.rs:190-197` — `on_review` calls `review_existing_profile` but did NOT call `render_profiles` afterwards. Fixed: added re-render call.

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

> Note: SEC-3 (argon2id key wrapping for FileBackend) was resolved by the
> `namtor-todo-fix-and-gnome-redesign` plan. The design discussion below is
> retained for context on the TPM2 + software fallback architecture.

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
- This is what the codebase already structurally supports (`BackendSelector`, `FileBackend`, `Tpm2Backend`). The gap was that `FileBackend` stored the raw key in plaintext — the argon2id KDF wrap (SEC-3, now resolved) is the "double" layer.

### Option B: TPM2 + a second AES layer (belt-and-suspenders)
- TPM2 unseals outer key → outer key decrypts an inner key file → inner key encrypts templates.
- Two layers of AES-256-GCM. If TPM2 is compromised but the inner key file is `0600` and not yet decrypted, the inner layer holds.
- **Not recommended**: adds latency on every template read/write for marginal gain. If the attacker has root, they can read the decrypted inner key from process memory. The second layer only helps against disk theft where TPM2 unseal somehow worked off-device (which TPM2 already prevents).

### Recommendation
**Option A with a KDF-wrapped fallback.** When TPM2 is available, use it. When it's not, wrap the AES key with a key derived from a root-owned secret file (`0600`, not a user password — daemons can't prompt). This gives:
- Hardware binding where TPM2 exists (templates can't leave the machine).
- A software layer that's more than just "raw key in a 0600 file" where TPM2 is absent.
- No double-encryption latency.

The `FileBackend::seal` argon2id wrap (SEC-3) makes disk theft alone insufficient — the attacker needs both the `.sealed` templates AND the `namtor`-owned secret file. This is defense-in-depth, not a full disk-theft mitigation; only TPM2 sealing defeats disk theft.
