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

Audit date: 2026-08-07. Baseline commits: `98da82e` (Task 1, live-auth
baseline), `c0ebc9c` (Task 2, CI security gates). Inputs:
[`w1-auth.txt`](w1-auth.txt), [`ci-baseline.md`](ci-baseline.md),
[`AGENTS.md`](../../AGENTS.md), [`../m3-r4-report.md`](../m3-r4-report.md),
[`../superpowers/plans/2026-08-04-nam-tor-roadmap.md`](../superpowers/plans/2026-08-04-nam-tor-roadmap.md).

---

## 1. Milestones M0-M10

| Milestone | Status | Basis / Evidence |
|---|---|---|
| **M0** — eval harness | `implemented` | `namtor_eval::score` shipped (`ScoreRecord`, `LatencyBreakdown`, `write_jsonl`, `read_jsonl`). 4 lib tests green. Commit history: M0 plan executed before M1. Harness is a foundation; no G2-G4 production run has been published through it yet, so it advances but does not pass G2-G4. |
| **M1** — hardware spike | `implemented` (reference hardware only) | `namtor-uvc` XU ioctls, `namtor-hwdb` descriptor parse, `namtor-probe`. R1/R3 RESOLVED PASS, R2 RESOLVED Tier 0 on the Sonix `3277:0018` reference device (`../m1-spike-report.md`, `quirks/devices/3277-0018.toml`). R10 (S3 emitter survival) WITHHELD below. HW-gated tests `#[ignore]`. |
| **M2** — detection + capture | `implemented` | `namtor_capture` (IrUvc/RgbUvc/CaptureSession), `namtor_camera` (emitter_ctrl, watchdog, resume, hotplug, selection, always_on). 38+40 lib tests green. See M2 shipped API in `AGENTS.md`. |
| **M3** — daemon + sandboxed worker | `implemented` | `namtor_daemon` (acl, nonce, listener, models, licence, lifecycle, supervisor, daemon), `namtor_worker` (infer, sandbox: seccomp + landlock). 26 M3 tasks committed (`735005d`). 55 daemon lib tests + 9 e2e green. R4 INT8-vs-FP32 WITHHELD (see §3). |
| **M4** — enrollment + template storage | `partial` | `namtor_store` (AES-256-GCM, per-uid, AAD binds uid+model_id, Zeroize) and `namtor_recog` (YuNet + SFace + alignment + cosine) shipped; CLI enroll/list/remove shipped (`6c6d3f8`, `8095605`). **TPM2 sealing is a stub** (file fallback; task 11). **Per-user z-norm/t-norm unmeasured** (task 16; no lawful cohort). **Extrinsic transform / RGB-IR fusion unmeasured** (task 17). |
| **M5** — PAM module | `implemented` (structural gates) | `namtor_pam` ships `pam_namtor.so` 457632 bytes < 500 KB budget, fail-closed, no ML deps, stdin keypress fallback, socket client drives Prepare→Verify→Release. G5.1 structural test green (`g5_structural.rs`). 19 e2e + 2 structural tests green. Production PAM-stack installation is owner-gated (WITHHELD, see §4). |
| **M6** — analytic PAD | `partial` | `namtor_pad` ships four analytic gates on `D = On − Off` (differential energy, gradient consistency, specular highlight, radial falloff). PAD is a veto, not a weighted term. 37-38 tests green. **All gates UNCALIBRATED**: thresholds are conservative defaults, not tuned against a real attack corpus. Radial gate explicitly labelled UNCALIBRATED in source (`w1-auth.txt` §"UNCALIBRATED LABEL"). Calibration is task 21. G4 production claim WITHHELD. |
| **M7** — desktop integration (GNOME) | `partial` | `namtor_gnome` GTK4/libadwaita enrollment wizard + settings app shipped (`bf13a4d`). Shows enrolled users, handles missing store. **KDE KCM not started** (task 27). **D-Bus `org.namtor.Manager` API not implemented** (task 25; current GNOME app talks to the store directly, which task 26 removes). GNOME marks 0 lib tests (UI not unit-tested). |
| **M8** — namtor-collect | `implemented` | `namtor_collect` consent-gated PAD data collection shipped (`3c29aaf`). ConsentRecord grant/revoke, per-session IDs for withdrawal, CollectionStore writes raw pairs + derived differential stats. 15 tests green. Production collection campaign (consented subjects, attack props) is owner-gated and has not run. |
| **M9** — trained PAD model | `partial` (synthetic-only) | MiniFASNet PAD model trained on physics-based synthetic strobe-differential data, 49 KB ONNX, ~80K params, 100% val accuracy on synthetic data (`f2be811`). Deployed to `var/lib/namtor/models/minifasnet_pad.onnx`. **Synthetic training data is explicitly non-production**; the model cannot be production-selected (task 22 requires consented subject/device-disjoint real data). NIR fine-tune recipes, full ablation table, and published eval report not produced. G3/G4 at full strength WITHHELD. |
| **M10** — remaining scope | `implemented` | M10 presence + depth + backend ablation implemented (tasks 29-31). Presence mode (`40c18a9`), `DepthUvc` depth path (`50ae1b5`), backend ablation (task 31). |

---

## 2. Acceptance gates G1-G5

No G1-G5 production gate carries `passed` in this audit. Targets are quoted
from spec §12.

| Gate | Target (spec §12) | Status | Basis / Evidence |
|---|---|---|---|
| **G1.1** zero-config on known device | clean-install enrollment, no file editing | `partial` | Selection logic exists (`namtor_camera::selection::select_camera`); reference quirks entry exists. **Live capture path hardcodes `/dev/video2`** (`crates/namtor-daemon/src/bin/namtor-daemon/live_capture.rs`); auto-selection is task 7. No clean-install HIL run published. |
| **G1.2** auto-probe on unknown device | ≥80% success across contributed devices | `unmeasured` | `namtor-probe` and tiered probing shipped. Contributor denominator does not exist (one device known). Cannot be measured without a device pool. |
| **G1.3** specific message per failure mode | fault-injection coverage of §8.3 table | `partial` | Distinct PAM/CLI reason codes shipped; PAD-veto detail is generic externally, detailed internally (`w1-auth.txt`). Full §8.3 fault-injection matrix not published. |
| **G1.4** zero interaction on MSXU | no prompt on MSXU path | `WITHHELD` (requires HIL) | MSXU tier code exists and is silent by construction. No HIL assertion run published. |
| **G1.5** emitter off ≤200 ms after teardown | HIL luminance check after 4 teardown paths | `unmeasured` (HIL) | Watchdog, RAII `EmitterGuard`, force-off helpers shipped. No HIL luminance measurement after end/stop/SIGKILL/resume published. |
| **G1.6** first auth after S3 succeeds 20/20 | scripted S3 cycle, 20 successes | `WITHHELD` | **R10 (emitter survives suspend) untested, needs real S3 cycle** (`AGENTS.md` hardware facts). Resume reconciler code exists; the 20x S3 HIL run has not been executed. |
| **G2.1** median latency ≤500 ms | eval report latency distribution | `unmeasured` | No production-path latency distribution published. R11 camera-open cost ~677 ms upper bound (subprocess) threatens this gate; re-measured in-process in M2 but not on the production auth path. |
| **G2.2** p95 ≤1000 ms | eval report | `unmeasured` | Same as G2.1. |
| **G2.3** first boot ≤120% steady-state | eval report | `unmeasured` | Warm/cold lifecycle state machine shipped (`namtor_daemon::lifecycle`); first-boot latency not measured on the production path. |
| **G3.1** FAR ≤1e-5 at FRR ≤3% | DET curve, ≥100k impostor comparisons | `WITHHELD` | No lawful cohort dataset selected (R5; `m3-r4-report.md`). No ≥100k impostor comparison run exists. At 100k comparisons zero false accepts gives a ~3e-5 95% upper bound, so even a clean run cannot be restated as a sub-1e-5 pass without larger N. |
| **G3.2** reported with N + CIs | eval report | `WITHHELD` | No production eval report published. |
| **G4.1** APCER ≤5% per print/IR-print/replay at BPCER ≤5% | per-attack APCER | `unmeasured` | Analytic gates exist but are UNCALIBRATED (M6). MiniFASNet PAD is synthetic-only (M9). No real print/IR-print/replay attack corpus evaluated. |
| **G4.2** per attack type, never averaged | report format | `unmeasured` | Eval harness enforces per-attack reporting by construction; no production report exists to apply it to. |
| **G4.3** 3D mask documented as not evaluated | report | `unmeasured` | No production report; 3D-mask exclusion will be stated in the report when G4 runs. |
| **G5.1** no ML/ONNX/image decoder in PAM | CI linker assertion | `passed` (structural, CI) | `g5_structural.rs` `no_ml_or_image_libs_linked` + `binary_size_under_budget` run in CI on the release cdylib. Mutation-tested (`ci-baseline.md` gate 2). 457632 < 512000 bytes. |
| **G5.2** worker cannot open files/network/syscalls | escape tests | `passed` (structural, CI) | seccomp allowlist/denylist disjoint test + landlock FS/net rules. Mutation-tested (`ci-baseline.md` gate 4: adding `socket` to allowlist fails the test). |
| **G5.3** all §8.3 conditions fail closed | fault injection | `passed` (structural CI gate) | PAM fails closed on every error path by construction; e2e tests cover Match/NoMatch/Unavailable/RateLimited/Refused driving Prepare→Verify→Release (`w1-auth.txt`). The fail-closed contract is asserted by tests on `master`. |
| **G5.4** no biometric data in logs | CI log scan | `passed` (structural, CI) | `e2e_no_biometric_data_in_logs` in namtor-daemon e2e. Mutation-tested (`ci-baseline.md` gate 5: adding `embedding=...` to a log fails the test). |
| **G5.5** templates unreadable off-device (TPM) | manual procedure, documented | `partial` | `namtor_store` AES-256-GCM with AAD binding uid+model_id, Zeroize on drop, file-fallback mode always reported. **Real TPM2 sealing is a stub** (task 11). Cross-hardware unseal procedure WITHHELD: requires a second TPM-equipped machine; absent that, evidence is limited to software-TPM + same-device unseal and the cross-hardware claim is not made. |

---

## 3. Cross-cutting dimensions (acceptance criteria)

| Dimension | Status | Basis / Evidence |
|---|---|---|
| **TPM** | `stub` | `namtor-store` has TPM2 *intent* with an explicit file fallback. `crates/namtor-store/src/crypto.rs:74-80` is the seam. Real TPM2 backend, device-bound no-PCR sealed AES key, version/migration is task 11. Cross-hardware G5.5 evidence WITHHELD (no second TPM machine). |
| **Fusion (RGB↔IR)** | `stub` / `unmeasured` | `RgbUvc` capture source exists; dual-stream R1 PASS on reference hardware. Enrollment-fitted extrinsic transform, SOF/timestamp pairing, geometric identity check, darkness policy are task 17. `VerifyEngine` runs IR-only PAD + match today. No fusion HIL or latency/protection evidence. |
| **z-norm / score normalisation** | `unmeasured` | `VerifyEngine` applies z-norm in code, but the normalisation cohort (R5) is not selected. `m3-r4-report.md` records "Cohort: WITHHELD — blocked on owner-gated dataset selection". No licence-verified dataset → no calibrated operating point. Synthetic bootstrap is explicitly non-production. |
| **PAD (analytic)** | `implemented` / `uncalibrated` | Four analytic gates on `D = On − Off` shipped as a veto. Thresholds are conservative defaults, not tuned against a real attack corpus. Radial gate labelled UNCALIBRATED in source. Calibration is task 21. |
| **PAD (learned, MiniFASNet)** | `partial` (synthetic-only) | 49 KB ONNX trained on physics-based synthetic strobe differentials, 100% val on synthetic. Cannot be production-selected: task 22 requires consented subject/device-disjoint real data. Deployed to `var/lib/namtor/models/` but not bound to a production operating point. |
| **S3 / suspend-resume** | `WITHHELD` | R10 (emitter survives suspend) untested, needs a real S3 cycle. Resume reconciler + `ResumeMachine` code exists; the 20x S3 first-auth HIL run (G1.6) has not been executed. |
| **UI (GNOME)** | `partial` | `namtor_gnome` enrollment wizard + settings app shipped (M7). Talks to the store directly; task 26 moves it behind D-Bus. 0 lib tests. |
| **UI (KDE)** | `partial` | Rust bridge + QML template shipped (task 27); KCM does not load on KDE image — C++ KCModule shim not built [~] |
| **D-Bus `org.namtor.Manager`** | `partial` | reference impl shipped (task 25, commit eb2ce0a); in-memory, not yet wired to zbus |
| **Packages (Arch/Fedora/Debian, signed)** | `partial` | packaging infrastructure defined (task 6); no live packages built — owner-gated [~] |
| **R4 INT8 vs FP32** | `WITHHELD` | Blocked on human action: Python onnxruntime binding incompatible with system Python (AGENTS.md), model licences not accepted, comparison set not placed. `m3-r4-report.md` carries a placeholder delta table. Path B (INT8 on architectural grounds per §5.3) holds; measurement deferred, not abandoned. |
| **R5 normalisation cohort** | `WITHHELD` | No licence-verified dataset selected. Candidates listed in `m3-r4-report.md` (CASIA NIR-VIS 2.0, LAMP-HQ, BUAA-VisNir, Oulu-CASIA NIR-VIS). Decision is owner-gated. |
| **Named per-user profiles** | `implemented` | implemented (task 14, commit b11d33c). `ProfileId`, multi-profile, rename/enable/delete. |
| **Unlimited ranked enrollment** | `implemented` | implemented (task 15, commit 1b01043). Bounded reservoir, Enrollment Quality, held-out Match Confidence, review-then-commit. |
| **Temporal consensus / early exit** | `implemented` | implemented (task 18, commit 92d8d31). One-pair high-confidence exit, up-to-five-pair escalation, Balanced/Strict operating points. |
| **Presence mode** | `implemented` | implemented (task 29, commit 40c18a9). |
| **Depth path** | `implemented` | implemented (task 30, commit 50ae1b5). No depth hardware on reference device; code path ships. |

---

## 4. Owner-gated inputs (WITHHELD until provided)

These are inputs only the project owner can supply. Each blocks one or more
gates and is recorded as `WITHHELD: <reason>` rather than fake-success.

| Input | Blocks | Status |
|---|---|---|
| Root: install systemd units, `namtor` user, `namtor-auth` group, setuid `namtor-worker-spawn` | Live daemon run, G1.1/G1.5 HIL | `WITHHELD: needs root install` |
| Python onnxruntime binding compatible with system Python | R4 INT8-vs-FP32, task 16 cohort tooling | `WITHHELD: python3 -c "import onnxruntime" fails (Python 3.14 incompat)` |
| Model selection + upstream licence acceptance (`NAMTOR_ACCEPT_LICENCE=1`) | R4, production inference | `WITHHELD: licences not accepted` |
| R5 cohort dataset (licence-verified) | G3, z-norm calibration, task 16 | `WITHHELD: no dataset selected` |
| Consented human subjects + attack props for PAD calibration | G4, tasks 21-22 | `WITHHELD: no collection campaign` |
| Second TPM-equipped machine | G5.5 cross-hardware unseal | `WITHHELD: single TPM machine available` |
| Scripted reference-hardware S3 HIL run (20x) | G1.6, R10 | `WITHHELD: no S3 cycle executed` |
| Signing keys + distro packaging matrices | Packages, task 6 | `WITHHELD: no signing keys` |

---

## 5. Test and CI baseline

| Item | Status | Evidence |
|---|---|---|
| Workspace lib tests | 374 passed, 0 failed, 7 ignored (HW-gated) | `w1-auth.txt` §"VERIFICATION OUTPUT" |
| Integration tests (pam e2e + g5_structural) | 19 + 2 passed | `w1-auth.txt` |
| `cargo fmt --all -- --check` | clean | `w1-auth.txt` |
| `cargo clippy --workspace --all-targets -- -D warnings` | no warnings | `w1-auth.txt` |
| SPDX headers | enforced by `scripts/check-spdx.sh` | `ci-baseline.md` gate 6 |
| `cargo audit` | 0 vulnerabilities, 329 deps | `ci-baseline.md` gate 7 |
| PAM release `.so` size | 457632 < 512000 bytes | `ci-baseline.md` |
| Pre-existing flake | `concurrent_connections_serialise_on_camera_lock` deadlocks under parallel executor; CI skips it | `w1-auth.txt`, `ci-baseline.md` |

CI is green on the mock path (no camera, no weights, no root). This is a code
state, not a gate pass.

---

## 6. What this audit does not claim

- No claim of "production ready".
- No claim of "Windows Hello parity". The README's historical "targeting
  Windows Hello parity" is a design ambition, not a measured result; no G1-G5
  production gate has passed.
- No G1-G5 gate marked `passed` except G5.1/G5.2/G5.4 structural CI gates
  (linker assertion, sandbox escape test, log scan), which are explicitly
  labelled as structural CI evidence rather than production gate passes.
- No FAR/FRR/APCER/BPCER number is published. The eval harness exists but no
  production run has been executed against a lawful cohort and attack corpus.
- No claim that TPM sealing is real. It is a file-fallback stub.
- No claim that fusion is wired. It is stub/unmeasured.
- No claim that PAD is calibrated. Analytic gates run on conservative
  defaults; the learned PAD model is synthetic-only.

The production-readiness plan (`.omo/plans/namtor-production-readiness.md`)
is the path to closing these gaps. As of this audit, 35 of 39 tasks are
complete (all implementation tasks 1-35). 4 tasks (6, 27, 32, 35) are marked
[~] as owner-gated (signing keys, KDE dev env, tag/CI execution). The Final
Verification Wave (F1-F4) is in progress.

This ledger was updated post-Wave-9 to reflect the committed state. For the
current RC gate snapshot, see `docs/evidence/rc-g2-g5.md`.
