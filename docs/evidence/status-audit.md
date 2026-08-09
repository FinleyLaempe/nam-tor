# Nam-tor — Status Audit

The authoritative readiness ledger. An independent reader should be able to
reconstruct the true state of every milestone, gate, and cross-cutting
dimension from this file alone. Where evidence exists it is linked; where it
does not, the entry says so explicitly and uses the vocabulary below rather
than claiming success.

**Status vocabulary**

| Term | Meaning |
|---|---|
| `implemented` | Code landed and committed; unit/integration tests green in CI. Does not imply a passed acceptance gate. |
| `partial` | Code landed for some sub-items; others remain stub, unmeasured, or absent. |
| `stub` | An interface exists but the production behaviour is not wired (e.g. file fallback standing in for TPM). |
| `unmeasured` | Code exists but no production-path measurement against the gate target has been published. |
| `passed` | Gate target met with published, reproducible evidence. No item in this audit carries `passed` for a G1-G5 production gate. |
| `failed` | Gate target measured and not met. |
| `WITHHELD` | Owner-gated or hardware-gated input missing; the claim is intentionally not made. The reason is recorded. |

**Rule:** no item is marked `passed` without linked evidence. `implemented` is a
code state, not a readiness claim. Anything not evidenced is `unmeasured` or
`WITHHELD`.

Audit date: 2026-08-08. Baseline commits: `98da82e` (Task 1, live-auth
baseline), `c0ebc9c` (Task 2, CI security gates), `4d8af1b` (release
candidate evidence). Inputs:
[`w1-auth.txt`](w1-auth.txt), [`ci-baseline.md`](ci-baseline.md),
[`AGENTS.md`](../../AGENTS.md),
[`../superpowers/plans/2026-08-04-nam-tor-roadmap.md`](../superpowers/plans/2026-08-04-nam-tor-roadmap.md),
[`.omo/plans/namtor-complete-product.md`](../../.omo/plans/namtor-complete-product.md)
(the 49-task plus 4-final-wave production plan).

---

## 1. Milestones M0-M10

| Milestone | Status | Basis / Evidence |
|---|---|---|
| **M0** — eval harness | `implemented` | `namtor_eval::score` shipped (`ScoreRecord`, `LatencyBreakdown`, `write_jsonl`, `read_jsonl`). 4 lib tests green. Commit history: M0 plan executed before M1. Harness is a foundation; no G2-G4 production run has been published through it yet, so it advances but does not pass G2-G4. |
| **M1** — hardware spike | `implemented` (reference hardware only) | `namtor-uvc` XU ioctls, `namtor-hwdb` descriptor parse, `namtor-probe`. R1/R3 RESOLVED PASS, R2 RESOLVED Tier 0 on the Sonix `3277:0018` reference device (`../m1-spike-report.md`, `quirks/devices/3277-0018.toml`). R10 (S3 emitter survival) WITHHELD below. HW-gated tests `#[ignore]`. |
| **M2** — detection + capture | `implemented` | `namtor_capture` (IrUvc/RgbUvc/CaptureSession), `namtor_camera` (emitter_ctrl, watchdog, resume, hotplug, selection, always_on). 38+40 lib tests green. See M2 shipped API in `AGENTS.md`. |
| **M3** — daemon + sandboxed worker | `implemented` (complete) | `namtor_daemon` (acl, nonce, listener, models, licence, lifecycle, supervisor, daemon, manager), `namtor_worker` (infer, sandbox: seccomp + landlock). Daemon complete including system-bus `org.namtor.Manager` service (tasks 8-12). 55+ daemon lib tests + e2e green. Production daemon installation and HIL WITHHELD (see §4). |
| **M4** — enrollment + template storage | `implemented` (complete) | `namtor_store` (AES-256-GCM, per-uid, AAD binds uid+model_id, Zeroize) and `namtor_recog` (YuNet + SFace + alignment + cosine) shipped; CLI enroll/list/remove shipped; named profiles, ranked enrollment, temporal consensus shipped (tasks 14-15, 18). **TPM2 sealing complete** (task 26; software/reference TPM tested, cross-hardware WITHHELD). **Per-user z-norm/t-norm WITHHELD** (task 16; no lawful cohort). **RGB-IR fusion WITHHELD** (task 17; extrinsics unmeasured). |
| **M5** — PAM module | `implemented` (complete) | `namtor_pam` ships `pam_namtor.so` 457920 bytes < 500 KB budget, fail-closed, no ML deps, stdin keypress fallback, socket client drives Prepare→Verify→Release. G5.1 structural test green (`g5_structural.rs`). Production PAM-stack installation is owner-gated (WITHHELD, see §4). |
| **M6** — analytic PAD | `implemented` (complete, uncalibrated) | `namtor_pad` ships four analytic gates on `D = On − Off` (differential energy, gradient consistency, specular highlight, radial falloff). PAD is a veto, not a weighted term. 37+ tests green. **All gates UNCALIBRATED**: thresholds are conservative defaults, not tuned against a real attack corpus. Calibration WITHHELD (task 21; no attack corpus). G4 production claim WITHHELD. |
| **M7** — desktop integration (GNOME) | `implemented` (complete) | `namtor_gnome` GTK4/libadwaita enrollment wizard + settings app shipped. Talks to the manager via D-Bus (task 26 moved it behind the bus). GNOME QA evidence under `docs/evidence/ui/gnome/`. Runtime HIL in a real GNOME session WITHHELD. |
| **M8** — namtor-collect | `implemented` (complete) | `namtor_collect` consent-gated PAD data collection shipped. ConsentRecord grant/revoke, per-session IDs for withdrawal, CollectionStore writes raw pairs + derived differential stats. 15+ tests green. Production collection campaign (consented subjects, attack props) is owner-gated and has not run. |
| **M9** — trained PAD model | `implemented` (complete, synthetic-only) | MiniFASNet PAD model trained on physics-based synthetic strobe-differential data, 49 KB ONNX, ~80K params, 100% val accuracy on synthetic data. Deployed to `var/lib/namtor/models/minifasnet_pad.onnx`. **Synthetic training data is explicitly non-production**; the model cannot be production-selected (requires consented subject/device-disjoint real data). G3/G4 at full strength WITHHELD. |
| **M10** — remaining scope | `implemented` | M10 presence + depth + backend ablation implemented (tasks 29-31). Presence mode, `DepthUvc` depth path, backend ablation. |
| **KDE KCM** | `implemented` (complete) | `namtor_kde` Rust bridge + QML KCM + C++ KCModule shim shipped (tasks 21-25). Async D-Bus bridge with generation-based stale-response handling, QAbstractListModel profile model, accessibility/i18n audit, desktop entry + AppStream metainfo. Runtime load in a real Plasma 6 session WITHHELD (no kcmshell6 / plasma-systemsettings on this host). Evidence: `docs/evidence/ui/kde/`. |
| **D-Bus `org.namtor.Manager`** | `implemented` (complete) | System-bus zbus service shipped (tasks 8-13). Methods, signals, polkit integration, peer identity, enrollment sessions, profile lifecycle backed by encrypted TemplateStore. Installed `gdbus introspect --system` WITHHELD (owner-run). Evidence: `docs/evidence/manager-*.md`, `docs/evidence/dbus-contract.xml`. |
| **Packaging** | `implemented` (complete, unsigned) | Packaging infrastructure complete (task 6): install manifest, distro packages (Arch/Fedora/Debian), systemd hardening, model acquisition, PAM install lifecycle, SBOM (CycloneDX + SPDX), provenance, reproducible build, signatures. Signed packages WITHHELD (no signing keys). Evidence: `docs/evidence/packaging/`. |

---

## 2. Acceptance gates G1-G5

No G1-G4 production gate carries `passed` in this audit. Structural CI
gates G5.1, G5.2, G5.4, and G5.6 pass. All production gates (G1-G4 and
production-level G5) are `WITHHELD` pending hardware or consented data.

| Gate | Target (spec §12) | Status | Basis / Evidence |
|---|---|---|---|
| **G1.1** zero-config on known device | clean-install enrollment, no file editing | `WITHHELD` (requires HIL) | Selection logic exists and auto-selects via quirks. Clean-install HIL run not published (needs root-installed daemon + camera). |
| **G1.2** auto-probe on unknown device | ≥80% success across contributed devices | `WITHHELD` | `namtor-probe` and tiered probing shipped. Contributor denominator does not exist (one device known). |
| **G1.3** specific message per failure mode | fault-injection coverage of §8.3 table | `WITHHELD` | Distinct PAM/CLI reason codes shipped. Full §8.3 fault-injection matrix not published against production path. |
| **G1.4** zero interaction on MSXU | no prompt on MSXU path | `WITHHELD` (requires HIL) | MSXU tier code is silent by construction. No HIL assertion run published. |
| **G1.5** emitter off ≤200 ms after teardown | HIL luminance check after 4 teardown paths | `WITHHELD` (requires HIL) | Watchdog, RAII `EmitterGuard`, force-off helpers shipped. No HIL luminance measurement published. |
| **G1.6** first auth after S3 succeeds 20/20 | scripted S3 cycle, 20 successes | `WITHHELD` | R10 (emitter survives suspend) untested, needs real S3 cycle. Resume reconciler code exists; 20x S3 HIL run not executed. |
| **G2.1** median latency ≤500 ms | eval report latency distribution | `WITHHELD` | No production-path latency distribution published. |
| **G2.2** p95 ≤1000 ms | eval report | `WITHHELD` | Same as G2.1. |
| **G2.3** first boot ≤120% steady-state | eval report | `WITHHELD` | Warm/cold lifecycle state machine shipped; first-boot latency not measured on the production path. |
| **G3.1** FAR ≤1e-5 at FRR ≤3% | DET curve, ≥100k impostor comparisons | `WITHHELD` | No lawful cohort dataset selected (R5). No ≥100k impostor comparison run exists. |
| **G3.2** reported with N + CIs | eval report | `WITHHELD` | No production eval report published. |
| **G4.1** APCER ≤5% per print/IR-print/replay at BPCER ≤5% | per-attack APCER | `WITHHELD` | Analytic gates exist but are UNCALIBRATED (M6). MiniFASNet PAD is synthetic-only (M9). No real attack corpus evaluated. |
| **G4.2** per attack type, never averaged | report format | `WITHHELD` | Eval harness enforces per-attack reporting by construction; no production report exists. |
| **G4.3** 3D mask documented as not evaluated | report | `WITHHELD` | No production report; 3D-mask exclusion stated when G4 runs. |
| **G5.1** no ML/ONNX/image decoder in PAM | CI linker assertion | `passed` (structural, CI) | `g5_structural.rs` `no_ml_or_image_libs_linked` + `binary_size_under_budget` run in CI on the release cdylib. Mutation-tested (`ci-baseline.md` gate 2). 457920 < 512000 bytes. |
| **G5.2** worker cannot open files/network/syscalls | escape tests | `passed` (structural, CI) | seccomp allowlist/denylist disjoint test + landlock FS/net rules. Mutation-tested (`ci-baseline.md` gate 4: adding `socket` to allowlist fails the test). |
| **G5.3** all §8.3 conditions fail closed | fault injection | `WITHHELD` (production gate) | PAM fails closed by construction; e2e tests cover Match/NoMatch/Unavailable/RateLimited/Refused. Structural CI coverage exists, but the production gate (full §8.3 fault-injection matrix on the installed path) is WITHHELD pending HIL. |
| **G5.4** no biometric data in logs | CI log scan | `passed` (structural, CI) | `e2e_no_biometric_data_in_logs` in namtor-daemon e2e. Mutation-tested (`ci-baseline.md` gate 5: adding `embedding=...` to a log fails the test). |
| **G5.5** templates unreadable off-device (TPM) | manual procedure, documented | `WITHHELD` | `namtor_store` AES-256-GCM with AAD binding uid+model_id, Zeroize on drop. TPM2 backend complete (task 26) with software/reference TPM tested. Cross-hardware unseal WITHHELD: requires a second TPM-equipped machine. |
| **G5.6** no unwrap/expect in non-test code | source scan | `passed` (structural, CI) | `g5_no_unwrap.rs` asserts no `unwrap()`/`expect()` in non-test code across the workspace. Mutation-tested (`ci-baseline.md`). |

---

## 3. Cross-cutting dimensions (acceptance criteria)

| Dimension | Status | Basis / Evidence |
|---|---|---|
| **TPM** | `implemented` (software/reference TPM) | `namtor-store` TPM2 backend complete (task 26): production backend, versioned blob/migration, device binding, recovery policy, explicit fallback, zeroization. Software/reference TPM tested (create/reboot/unseal, corruption, reset/absence, migration). Cross-hardware G5.5 evidence WITHHELD (no second TPM machine). See `docs/evidence/trust/tpm2-lifecycle.md`. |
| **Fusion (RGB↔IR)** | `WITHHELD` | `RgbUvc` capture source exists; dual-stream R1 PASS on reference hardware. Enrollment-fitted extrinsic transform, SOF/timestamp pairing, geometric identity check, darkness policy implemented (task 17) but unmeasured. No fusion HIL or latency/protection evidence. See `docs/evidence/trust/fusion.md`. |
| **z-norm / score normalisation** | `WITHHELD` | `VerifyEngine` applies z-norm in code, but the normalisation cohort (R5) is not selected. No licence-verified dataset selected. No calibrated operating point. See `docs/evidence/trust/znorm.md`. |
| **PAD (analytic)** | `implemented` / `uncalibrated` | Four analytic gates on `D = On − Off` shipped as a veto. Thresholds are conservative defaults, not tuned against a real attack corpus. Calibration WITHHELD (task 21; no attack corpus). See `docs/evidence/trust/pad-calibration.md`. |
| **PAD (learned, MiniFASNet)** | `implemented` (synthetic-only) | 49 KB ONNX trained on physics-based synthetic strobe differentials, 100% val on synthetic. Cannot be production-selected: requires consented subject/device-disjoint real data. Deployed but not bound to a production operating point. See `docs/evidence/trust/learned-pad.md`. |
| **S3 / suspend-resume** | `WITHHELD` | R10 (emitter survives suspend) untested, needs a real S3 cycle. Resume reconciler + `ResumeMachine` code exists; the 20x S3 first-auth HIL run (G1.6) has not been executed. |
| **UI (GNOME)** | `implemented` | `namtor_gnome` enrollment wizard + settings app shipped (M7). Talks to the manager via D-Bus (task 26). GNOME QA evidence under `docs/evidence/ui/gnome/`. Runtime HIL WITHHELD. |
| **UI (KDE)** | `implemented` | Rust bridge + QML KCM + C++ KCModule shim shipped (tasks 21-25). Async D-Bus bridge, profile model, a11y/i18n audit. Runtime load in real Plasma 6 session WITHHELD (no kcmshell6). Evidence: `docs/evidence/ui/kde/`. |
| **D-Bus `org.namtor.Manager`** | `implemented` | System-bus zbus service shipped (tasks 8-13). Methods, signals, polkit, peer identity, enrollment sessions, profile lifecycle. Installed introspection WITHHELD (owner-run). |
| **Packages (Arch/Fedora/Debian, signed)** | `implemented` (unsigned) | Packaging infrastructure complete (task 6): install manifest, distro packages, systemd hardening, SBOM, provenance, reproducible build. Signed packages WITHHELD (no signing keys). Evidence: `docs/evidence/packaging/`. |
| **R4 INT8 vs FP32** | `WITHHELD` | Blocked on human action: Python onnxruntime binding compatibility, model licences, comparison set. Path B (INT8 on architectural grounds per §5.3) holds; measurement deferred. See `docs/evidence/trust/int8-eval.md`. |
| **R5 normalisation cohort** | `WITHHELD` | No licence-verified dataset selected. Decision is owner-gated. See `docs/evidence/r5-cohort.md`. |
| **Named per-user profiles** | `implemented` | implemented (task 14). `ProfileId`, multi-profile, rename/enable/delete. |
| **Unlimited ranked enrollment** | `implemented` | implemented (task 15). Bounded reservoir, Enrollment Quality, held-out Match Confidence, review-then-commit. |
| **Temporal consensus / early exit** | `implemented` | implemented (task 18). One-pair high-confidence exit, up-to-five-pair escalation, Balanced/Strict operating points. |
| **Presence mode** | `implemented` | implemented (task 29). |
| **Depth path** | `implemented` | implemented (task 30). No depth hardware on reference device; code path ships. |

---

## 4. Owner-gated inputs (WITHHELD until provided)

These are inputs only the project owner can supply. Each blocks one or more
gates and is recorded as `WITHHELD: <reason>` rather than fake-success.

| Input | Blocks | Status |
|---|---|---|
| Root: install systemd units, `namtor` user, `namtor-auth` group | Live daemon run, G1.1/G1.5 HIL | `WITHHELD: needs root install` |
| Python onnxruntime binding compatible with system Python | R4 INT8-vs-FP32, task 16 cohort tooling | `WITHHELD: python3 -c "import onnxruntime" fails (Python 3.14 incompat)` |
| Model selection + upstream licence acceptance for restricted models | R4, production inference | `WITHHELD: no restricted model bundled; redistributable models admit without acceptance record` |
| R5 cohort dataset (licence-verified) | G3, z-norm calibration, task 16 | `WITHHELD: no dataset selected` |
| Consented human subjects + attack props for PAD calibration | G4, tasks 21-22 | `WITHHELD: no collection campaign` |
| Second TPM-equipped machine | G5.5 cross-hardware unseal | `WITHHELD: single TPM machine available` |
| Scripted reference-hardware S3 HIL run (20x) | G1.6, R10 | `WITHHELD: no S3 cycle executed` |
| Signing keys + distro packaging matrices | Packages, task 6 | `WITHHELD: no signing keys` |

---

## 5. Test and CI baseline

| Item | Status | Evidence |
|---|---|---|
| Workspace lib tests | 989 passed, 0 failed, 9 ignored (HW-gated) | workspace `cargo test` |
| Integration tests (pam e2e + g5_structural) | 19 + 2 passed | `w1-auth.txt` |
| `cargo fmt --all -- --check` | clean | workspace fmt check |
| `cargo clippy --workspace --all-targets -- -D warnings` | no warnings | workspace clippy |
| SPDX headers | enforced by `scripts/check-spdx.sh` | `ci-baseline.md` gate 6 |
| `cargo audit` | 0 vulnerabilities, 329 deps | `ci-baseline.md` gate 7 |
| PAM release `.so` size | 457920 < 512000 bytes | release build |

CI is green on the mock path (no camera, no weights, no root). This is a code
state, not a gate pass.

---

## 6. What this audit does not claim

- No claim of "production ready".
- No claim of "Windows Hello parity". The README's historical "targeting
  Windows Hello parity" is a design ambition, not a measured result; no G1-G5
  production gate has passed.
- No G1-G5 production gate marked `passed` except G5.1/G5.2/G5.4/G5.6
  structural CI gates (linker assertion, sandbox escape test, log scan,
  no-unwrap source scan), which are explicitly labelled as structural CI
  evidence rather than production gate passes.
- No FAR/FRR/APCER/BPCER number is published. The eval harness exists but no
  production run has been executed against a lawful cohort and attack corpus.
- No claim that TPM sealing is production-proven. Software/reference TPM
  tested; cross-hardware transplant WITHHELD.
- No claim that fusion is measured. Code shipped, HIL unmeasured.
- No claim that PAD is calibrated. Analytic gates run on conservative
  defaults; the learned PAD model is synthetic-only.

The production plan
([`.omo/plans/namtor-complete-product.md`](../../.omo/plans/namtor-complete-product.md))
is the path to closing these gaps. It comprises 49 tasks plus a 4-task final
verification wave (F1-F4). As of this audit, all 49 implementation tasks are
complete. The Final Verification Wave (F1-F4) is in progress; this ledger
was updated as part of F1 (plan compliance and evidence audit). Owner-gated
inputs (signing keys, hardware access, consented data, S3 cycles, second
TPM) remain WITHHELD and block the corresponding production gates.

For the current RC gate snapshot, see `docs/evidence/rc-g2-g5.md`.

