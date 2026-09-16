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

Audit date: 2026-08-10 (updated for enrollment redesign certification, task 16; UI unification completion 2026-09-14).
Baseline commits: `98da82e` (Task 1, live-auth baseline), `c0ebc9c` (Task 2, CI
security gates), `4d8af1b` (release candidate evidence), `9630ee1` (enrollment
redesign packaging certification, task 14). Inputs:
[`w1-auth.txt`](w1-auth.txt), [`ci-baseline.md`](ci-baseline.md),
[`../roadmap.md`](../roadmap.md).

---

## 1. Milestones M0-M10

| Milestone | Status | Basis / Evidence |
|---|---|---|
| **M0** — eval harness | `implemented` | `namtor_eval::score` shipped (`ScoreRecord`, `LatencyBreakdown`, `write_jsonl`, `read_jsonl`). 4 lib tests green. Commit history: M0 plan executed before M1. Harness is a foundation; no G2-G4 production run has been published through it yet, so it advances but does not pass G2-G4. |
| **M1** — hardware spike | `implemented` (reference hardware only) | `namtor-uvc` XU ioctls, `namtor-hwdb` descriptor parse, `namtor-probe`. R1/R3 RESOLVED PASS, R2 RESOLVED Tier 0 on the Sonix `3277:0018` reference device (`../m1-spike-report.md`, `quirks/devices/3277-0018.toml`). R10 (S3 emitter survival) WITHHELD below. HW-gated tests `#[ignore]`. |
| **M2** — detection + capture | `implemented` | `namtor_capture` (IrUvc/RgbUvc/CaptureSession), `namtor_camera` (emitter_ctrl, watchdog, resume, hotplug, selection, always_on). 38+40 lib tests green. See M2 shipped API in the spec (`docs/superpowers/specs/2026-08-04-nam-tor-design.md`). |
| **M3** — daemon + sandboxed worker | `implemented` (complete) | `namtor_daemon` (acl, nonce, listener, models, licence, lifecycle, supervisor, daemon, manager), `namtor_worker` (infer, sandbox: seccomp + landlock). Daemon complete including system-bus `org.namtor.Manager` service (tasks 8-12). 55+ daemon lib tests + e2e green. Production daemon installation and HIL WITHHELD (see §4). |
| **M4** — enrollment + template storage | `implemented` (complete) | `namtor_store` (AES-256-GCM, per-uid, AAD binds uid+model_id, Zeroize) and `namtor_recog` (YuNet + SFace + alignment + cosine) shipped; CLI enroll/list/remove shipped; named profiles, ranked enrollment, temporal consensus shipped (tasks 14-15, 18). **TPM2 sealing complete** (task 26; software/reference TPM tested, cross-hardware WITHHELD). **Per-user z-norm/t-norm WITHHELD** (task 16; no lawful cohort). **RGB-IR fusion WITHHELD** (task 17; extrinsics unmeasured). |
| **M5** — PAM module | `implemented` (complete) | `namtor_pam` ships `pam_namtor.so` 457920 bytes < 500 KB budget, fail-closed, no ML deps, stdin keypress fallback, socket client drives Prepare→Verify→Release. G5.1 structural test green (`g5_structural.rs`). Production PAM-stack installation is owner-gated (WITHHELD, see §4). |
| **M6** — analytic PAD | `implemented` (complete, uncalibrated) | `namtor_pad` ships four analytic gates on `D = On − Off` (differential energy, gradient consistency, specular highlight, radial falloff). PAD is a veto, not a weighted term. 37+ tests green. **All gates UNCALIBRATED**: thresholds are conservative defaults, not tuned against a real attack corpus. Calibration WITHHELD (task 21; no attack corpus). G4 production claim WITHHELD. |
| **M7** — desktop integration (GNOME) | `implemented` (complete, redesigned) | `namtor_gnome` GTK4/libadwaita enrollment wizard + settings app shipped. Talks to the manager via D-Bus (task 26 moved it behind the bus). GNOME QA evidence under `docs/evidence/ui/gnome/`. **Enrollment redesign complete** (tasks 1-14): adaptive three-destination shell (Profiles, Authentication, System), continuous-enrollment NavigationView (Setup, Capture, Review, Stop, Resume, Save, Discard), Manager3 D-Bus contract, 83-test accessibility and privacy suite, desktop certification harness. Evidence under `docs/evidence/enrollment-redesign/`. Runtime HIL in a real GNOME session WITHHELD (task 15; no root, no camera, no AT-SPI). Screenshots WITHHELD (no installed app and running daemon; see `enrollment-redesign/16-visual-review.md`). |
| **M8** — namtor-collect | `implemented` (complete) | `namtor_collect` consent-gated PAD data collection shipped. ConsentRecord grant/revoke, per-session IDs for withdrawal, CollectionStore writes raw pairs + derived differential stats. 15+ tests green. Production collection campaign (consented subjects, attack props) is owner-gated and has not run. |
| **M9** — trained PAD model | `implemented` (complete, synthetic-only) | MiniFASNet PAD model trained on physics-based synthetic strobe-differential data, 49 KB ONNX, ~80K params, 100% val accuracy on synthetic data. Deployed to `var/lib/namtor/models/minifasnet_pad.onnx`. **Synthetic training data is explicitly non-production**; the model cannot be production-selected (requires consented subject/device-disjoint real data). G3/G4 at full strength WITHHELD. |
| **M10** — remaining scope | `implemented` | M10 presence + depth + backend ablation implemented (tasks 29-31). Presence mode, `DepthUvc` depth path, backend ablation. |
| **KDE KCM** | `implemented` (complete) | `namtor_kde` Rust bridge + QML KCM + C++ KCModule shim shipped (tasks 21-25). Async D-Bus bridge with generation-based stale-response handling, QAbstractListModel profile model, accessibility/i18n audit, desktop entry + AppStream metainfo. Runtime load in a real Plasma 6 session WITHHELD (no kcmshell6 / plasma-systemsettings on this host). Evidence: `docs/evidence/ui/kde/`. |
| **D-Bus `org.namtor.Manager`** | `implemented` (complete) | System-bus zbus service shipped (tasks 8-13). Methods, signals, polkit integration, peer identity, enrollment sessions, profile lifecycle backed by encrypted TemplateStore. **Manager3 continuous-enrollment contract** shipped (enrollment redesign tasks 2, 5, 6): Start, Stop, Resume, Save, Discard lifecycle on the system bus; Manager1 and Manager2 remain byte-for-byte stable. Evidence: `docs/evidence/manager-*.md`, `docs/evidence/dbus-contract.xml`, `docs/evidence/dbus-contract-v3.xml`, `enrollment-redesign/02-contract-pass.txt`, `enrollment-redesign/06-manager3-flow.jsonl`. Installed `gdbus introspect --system` WITHHELD (owner-run). |
| **Packaging** | `implemented` (complete, unsigned) | Packaging infrastructure complete (task 6): install manifest, distro packages (Arch/Fedora/Debian), systemd hardening, model acquisition, PAM install lifecycle, SBOM (CycloneDX + SPDX), provenance, reproducible build, signatures. **Desktop certification harness extended** (enrollment redesign task 14): 47 certification checks covering GNOME redesigned resources, Manager3 introspection, translation seam, executable linkage, build-output hygiene, core GTK-free invariant, exact-version dependency, and a round-trip mutation test. Evidence: `docs/evidence/packaging/`, `enrollment-redesign/14-package-matrix.json`, `enrollment-redesign/14-package-mutation.txt`. Signed packages WITHHELD (no signing keys). |

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
| **G1.5** emitter off ≤200 ms after teardown | HIL luminance check after 4 teardown paths | `WITHHELD` (measured 2026-09-17, budget not met) | **Now measured, and it does not pass.** `emitter_goes_dark_within_the_teardown_budget` (`crates/namtor-daemon/tests/enrollment_live.rs`, `#[ignore]`d, `NAMTOR_HW_TEST=1`) arms the reference camera's IR emitter, confirms in photons that the scene is lit (228–247/255), takes a clock reading, issues `force_off`, and timestamps every frame until the scene is sustainably dark (frame mean < 4.0/255 for the rest of a 1200 ms window; a *run*, not one frame, because mode 2 is a 1:1 strobe in which half of all frames read ~0.13/255 while the emitter is fully on). Five samples: **430.8, 434.6, 434.7, 434.9, 435.0 ms** — median **434.7 ms**, max **435.0 ms**, against the 200 ms budget. Sampling granularity **33.5 ms/frame** (GREY8 640x360 at a nominal 30 fps), timestamped at dequeue rather than exposure, so each value is an upper bound carrying about ±1 frame period; no digit below that is meaningful. **This is not a slow LED.** The `payload_off` write returned by 42.7 ms, and the device then delivered *no frames at all* for a further ~394 ms. The same MSXU mode switch blinds the stream for 390–393 ms on the way *in* (arming), so the blackout is a property of the mode change, not of the emitter. The instant the LED actually went dark is therefore bracketed only to **[42.7, 435.0] ms**, and this method cannot resolve it further because its only witness is the camera that goes blind. Closing G1.5 needs an observer that does not stall with the mode switch — a second camera watching the LED, or a photodiode. **Scope:** the `namtor_camera` `emitter_ctrl` / `watchdog` **library** path driven from a test process on the reference camera. It does **not** exercise the installed daemon's teardown path (`crates/namtor-daemon/src/bin/namtor-daemon/live_capture.rs` is untouched and unexercised), and it covers **one** teardown path, not the four this gate asks for. Emitter restoration was verified on every run: `GET_CUR` read back `payload_off` (`01 03 01 …`) and the scene measured 0.10–0.13/255 afterwards. |
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
| **UI (GNOME)** | `implemented` (redesigned) | `namtor_gnome` enrollment wizard + settings app shipped (M7). Talks to the manager via D-Bus (task 26). **Enrollment redesign complete** (tasks 1-14): adaptive three-destination shell, continuous-enrollment flow (Setup, Capture, Review, Stop, Resume, Save, Discard), accessible names and mnemonics on every actionable widget, rate-limited progress announcements, translatable strings via `tr()`, RTL and 200 percent text support, privacy scan across all view types (83 tests total). Visual review and mutation evidence: `enrollment-redesign/16-visual-review.md`, `enrollment-redesign/16-screenshots.json`, `enrollment-redesign/16-visual-mutation.md`. GNOME QA evidence under `docs/evidence/ui/gnome/`. Runtime HIL WITHHELD (task 15; no root, no camera, no AT-SPI). Screenshots WITHHELD (no installed app and running daemon). |
| **UI (KDE)** | `implemented` | Rust bridge + QML KCM + C++ KCModule shim shipped (tasks 21-25). Async D-Bus bridge, profile model, a11y/i18n audit. Runtime load in real Plasma 6 session WITHHELD (no kcmshell6). Evidence: `docs/evidence/ui/kde/`. |
| **UI unification (namtor-client + native KCM + GNOME polish)** | `implemented` (complete, software) | Shared `namtor-client` D-Bus client crate created; both GUIs (GNOME + KDE) rewired onto it with zero duplicate wire-type definitions. GNOME: 11 USA-* bugs from the bug ledger fixed with real behavioural tests (USA-0, USA-2, USA-3, USA-9/13, USA-10 explicitly fixed this plan; others already had behavioural coverage per Task 1's ledger); all source-string-scan tests eliminated (Task 14); gtk4/libadwaita bumped to 0.10/0.8. KDE: real `kcmutils_add_qml_kcm` + `K_PLUGIN_CLASS_WITH_JSON` wiring (builds correctly when ECM/KCMUtils present, untestable on this CI host which lacks those deps; QML module + abi-smoke build and pass locally: 3/3); metadata.json category fixed to `security-privacy`; KDE Face lock-screen authenticator provisioning implemented with lockout-safety check. **HIL screenshots (15 GNOME states + KDE KCM states): WITHHELD** (this and all other execution environments used for this plan lack a display server and camera; real screenshot capture requires `NAMTOR_HW_TEST=1` on reference hardware). No biometric or security gate is affected by this row. |
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
| Root: install systemd units, `namtor` user, `namtor-auth` group | Live daemon run, G1.1 HIL, **and all end-to-end enrollment HIL** | `WITHHELD: needs root install` — **highest-value unlock, see note below** |
| Python onnxruntime binding compatible with system Python | R4 INT8-vs-FP32, task 16 cohort tooling | `WITHHELD: python3 -c "import onnxruntime" fails (Python 3.14 incompat)` |
| Model selection + upstream licence acceptance for restricted models | R4, production inference | `WITHHELD: no restricted model bundled; redistributable models admit without acceptance record` |
| R5 cohort dataset (licence-verified) | G3, z-norm calibration, task 16 | `WITHHELD: no dataset selected` |
| Consented human subjects + attack props for PAD calibration | G4, tasks 21-22 | `WITHHELD: no collection campaign` |
| Second TPM-equipped machine | G5.5 cross-hardware unseal | `WITHHELD: single TPM machine available` |
| Scripted reference-hardware S3 HIL run (20x) | G1.6, R10 | `WITHHELD: no S3 cycle executed` |
| Signing keys + distro packaging matrices | Packages, task 6 | `WITHHELD: no signing keys` |

**Note on the root input (2026-09-16).** Everything else HIL needs is present
on the reference machine: the reference IR camera (Sonix `3277:0018`, IR at
`/dev/video2`, GREY8 640x360@30), a TPM2 device at `/dev/tpm0`, and the real
ONNX weights under `var/lib/namtor/models/`. The remaining blocker for an
end-to-end enrollment run is the IR emitter, which is armed through the UVC
XU and the quirks table from the daemon and therefore needs the root install.
Measured directly over 5 live frames with the emitter unarmed:
`mean=0.1/255, min=0, max=4` — an essentially black image, in which face
detection cannot succeed regardless of pipeline quality. So this single input
gates the entire G1/G2 HIL family, not just the gates listed above.

**Correction (2026-09-17).** The paragraph above is wrong about the emitter,
and is kept only so the correction is legible. Arming the IR emitter does
*not* need the root install: the quirks entry's measured
`[access] requires_root = false` holds, and the logind seat ACL on
`/dev/video2` gives the logged-in desktop user `rw`, so the XU write
succeeds from an ordinary test process. `crates/namtor-daemon/tests/hw_emitter/`
does exactly that, and both the live enrollment test and the G1.5 teardown
measurement now arm and disarm the emitter without privilege. What root still
gates is the *daemon* — its units, users and groups — and therefore the
production-path HIL runs, not emitter access itself.

---

## 5. Test and CI baseline

| Item | Status | Evidence |
|---|---|---|
| Workspace tests | 1622 passed, 0 failed | workspace `cargo test -- --test-threads=1`, 2026-09-16 |
| Integration tests (pam e2e + g5_structural) | passed | `w1-auth.txt` |
| UI test suite (`namtor-ui`) | 148 passed (36 shell + 34 presenter + 29 privacy + 10 guidance + …) | `cargo test -p namtor-ui` |
| `cargo fmt --all -- --check` | clean | workspace fmt check |
| `cargo clippy --workspace --all-targets -- -D warnings` | no warnings | workspace clippy |
| SPDX headers | enforced by `scripts/check-spdx.sh` | `ci-baseline.md` gate 6 |
| `cargo audit` | 0 vulnerabilities | gate 7; RUSTSEC-2026-0285 (rustls 0.23.41, TLS 1.3 handshake across encryption levels) fixed by bumping to 0.23.45 on 2026-09-16. rustls enters only as a build-dependency of `ort-sys` via `ureq`, so it never shipped in the auth path. `encoding` (unmaintained) and `chacha20` (yanked) remain tolerated warnings; `chacha20` is not in the compiled tree for this target. |
| PAM release `.so` size | 457920 < 512000 bytes | release build |

Both GitHub workflows are green as of 2026-09-16: `CI` (all four jobs, with
no `--skip` and no `--exclude` — the previously-skipped
`concurrent_connections_serialise_on_camera_lock` deadlock was resolved by
making the listener poll) and `release` (dry run, first success in the
workflow's history; the install/smoke, upgrade, uninstall and artifact steps
had never once executed before).

This is still a code and pipeline state, not a gate pass. Every WITHHELD gate
in section 2 remains withheld.

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
- No claim that the enrollment redesign is a production readiness pass. The
  redesign (tasks 1-14) is certified as complete software: the UI ships, 83
  tests pass, the design contract is followed, accessibility and privacy gates
  are closed, and packages certify. Screenshots are WITHHELD (no installed app
  and running daemon). The biometric accuracy gates (FAR, FRR, APCER, BPCER),
  TPM2 cross-hardware sealing, S3 reliability, z-norm calibration, and R4 INT8
  evaluation remain WITHHELD and are unchanged by the redesign. See
  `enrollment-redesign/16-visual-review.md` for the visual review.

The production plan (49 tasks plus a 4-task final verification wave F1-F4)
is the path to closing these gaps. As of this audit, all 49 implementation
tasks are complete. The Final Verification Wave (F1-F4) is in progress;
this ledger was updated as part of F1 (plan compliance and evidence audit)
and the enrollment redesign certification (task 16). Owner-gated inputs
(signing keys, hardware access, consented data, S3 cycles, second TPM)
remain WITHHELD and block the corresponding production gates.

The enrollment redesign plan comprises 16 tasks. Tasks 1-14 are complete
and committed. Task 15 (installed HIL) is WITHHELD (no root, no camera, no
AT-SPI). Task 16 (screenshot review and evidence reconciliation) is
certified by this update. The enrollment redesign is UI and software
completion, not a biometric accuracy gate pass.

For the current RC gate snapshot, see `docs/evidence/rc-g2-g5.md`.


---

## 7. Verification discipline

Two rules, both learned from defects found in this repository rather than
asserted in the abstract.

### A `FIXED` marker must cite a named test, or it does not count

`todo.md` carried an entry marked **FIXED** for USA-0b (the enrollment
"diversity" number meaning reservoir fill on the capture screen and true
pairwise cosine distance on the review screen). The bug was still live. The
claimed fix — "use `capture.diversity_percent()` in both places" — had never
been applied: `compute_progress` still read `snapshot.coverage_percent`, and
`diversity_percent` was still a private `fn` with exactly one caller, which is
structural proof the change never landed. The user-visible symptom stood
exactly as originally described: the capture screen climbed to 100% as the
100-slot reservoir filled, then review showed a single-digit number under the
same label.

It is now genuinely fixed, and the evidence is a named test:
`manager::worker::tests::capture_and_review_report_the_same_diversity_metric`,
which fails `100 != 3` against the pre-fix code.

The same rule caught a second instance: a hardware-gated test skipped with
"no real ONNX weights on this machine" when the weights were present all
along, at the documented repo-relative path. A skip whose stated reason is
false is worse than a failure, because it hides a runnable test.

### Nothing load-bearing may live only in an untracked file

`AGENTS.md`, `todo.md` and `HANDOFF.md` are deliberately gitignored
(`.gitignore`, "Internal agent / planning notes — kept locally, not
published", untracked in `9d82035` when the repo was prepared for public
publication). That decision stands.

The consequence is that anything written only there does not survive a fresh
clone — including, ironically, the USA-0b correction above, whose whole point
is that unverifiable claims rot. Load-bearing project state belongs in this
ledger, in `docs/architecture.md`, or in the plan under
`docs/superpowers/plans/`, all of which are tracked.
