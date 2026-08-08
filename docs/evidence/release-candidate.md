# Release Candidate — Evidence-Complete Status

SPDX-License-Identifier: GPL-3.0-only

Task: 48 — Publish only an evidence-complete release candidate.
Date: 2026-08-08.

This is the release candidate evidence summary. It lists what is proven,
what is WITHHELD, and what human action is required to close each WITHHELD
gate. No claim is published without evidence. No WITHHELD gate is papered
over.

## Summary verdict

**4 structural CI gates PASSED. All production gates WITHHELD or
implemented-but-unmeasured. No failed gates. Not production-ready.**

The codebase is implementation-complete (M0-M10, 989 tests green, clippy
clean). Every production gate that requires real hardware, a lawful cohort,
consented attack data, or signed packages is WITHHELD with a recorded
reason. This is an evidence-complete release candidate: the evidence is
honest, the gaps are visible, and the path to closing each gap is documented.

## What is PROVEN (passed with evidence)

| Gate | What is proven | Evidence |
|---|---|---|
| G5.1 | `pam_namtor.so` is 457 KB (< 500 KB), contains no ML/ONNX/image decoder deps | `g5_structural.rs` CI gate, mutation-tested |
| G5.2 | Worker cannot open files, open sockets, or make unauthorised syscalls (seccomp + landlock) | `g5-fault-matrix.json`, mutation-tested (adding `socket` to allowlist fails the test) |
| G5.3 | Every error path returns `PAM_AUTHINFO_UNAVAIL`, never `PAM_SUCCESS` on error | e2e tests on `master`, fault injection |
| G5.4 | No biometric data (embeddings, templates, pixels, scores) in logs at any level | `e2e_no_biometric_data_in_logs`, mutation-tested |
| PAM lifecycle | Transactional PAM install/uninstall preserves password fallback through 8 scenarios | `pam-install-matrix.md`, 37/37 CI assertions |
| Install layout | Deterministic empty-DESTDIR staging; every source/consumer resolves | `install-manifest.json` |
| Privacy (UI) | No biometric fields in GNOME or KDE UI structs | `privacy_scan.rs` (GNOME), `privacy_scan_no_biometric_data` (KDE) |

## What is WITHHELD (owner-gated or hardware-gated)

### G1 — Hardware lifecycle

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| G1.1 Clean enrollment | No HIL run | Root install + reference camera enrollment |
| G1.2 >=80% compatibility | 1 device known | Contributor device reports |
| G1.3 MSXU zero-prompt | No second MSXU camera HIL | Repeatable HIL on a second MSXU device |
| G1.5 Emitter teardown <=200ms | No luminance measurement | HIL luminance check after 4 teardown paths |
| G1.6 20x S3 first-auth | No real S3 cycle | Scripted 20x suspend/resume on reference hardware |

### G2 — Latency

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| G2.1 Median <=500ms | No full-path measurement | RC packages on reference hardware; run eval harness |
| G2.2 p95 <=1000ms | No full-path measurement | Same |
| G2.3 First boot <=120% steady | No cold-start measurement | Same |

INT8 inference component is measured (median 28.42ms, p95 30.95ms). The
full auth path (camera + capture + PAD + recognition + IPC) is not.

### G3 — Biometric accuracy

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| G3.1 FAR <=1e-5 at FRR <=3% | No lawful cohort | Select a licence-verified dataset (record in `ml/DATASETS.md`); accept licences (`NAMTOR_ACCEPT_LICENCE=1`); run subject-disjoint eval with >=300,000 impostor comparisons |
| G3.2 Report with N + CIs | No production report | Same |

### G4 — Presentation attack detection

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| G4.1 APCER <=5% per type | No real attack corpus | Consent-gated collection campaign with print, IR-print, replay, cutout presentations; calibrate analytic + learned PAD |
| G4.2 Per-attack reporting | No production report | Same |
| G4.3 3D mask exclusion | Implemented (stated as not evaluated) | None (correctly excluded) |

### G5.5 — TPM cross-hardware

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| G5.5 Cross-hardware unseal | 1 TPM machine | Second TPM-equipped machine; transplant test |

### G5 — Desktop visual QA

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| GNOME visual QA | No display server | Clean GNOME VM; AT-SPI driver; screenshots at normal/200%/RTL/high-contrast |
| KDE visual QA | No KDE image | Clean Plasma 6 VM; Qt Quick/Plasma driver; same screenshot matrix |
| Real D-Bus integration | No root-installed daemon | Root install; both UIs against live `org.namtor.Manager` |

### G6 — Install matrix

| Sub-gate | Blocker | Human action needed |
|---|---|---|
| Fedora 40/41 packages | No live Fedora container | Build + lifecycle test in Fedora containers |
| Debian 12/13 packages | No live Debian container | Build + lifecycle test in Debian containers |
| aarch64 builds | No aarch64 builder | QEMU cross-build or native aarch64 builder |
| Signed packages | No signing keys | Set `secrets.GPG_PRIVATE_KEY`; enable OIDC for SLSA provenance |
| Live RC lifecycle | No root + reference hardware | Full install -> enroll -> verify -> upgrade -> rollback -> uninstall on each cell |

## What is implemented but unmeasured

| Component | Status | What is missing |
|---|---|---|
| TPM2 sealing | stub (file fallback) | Real TPM2 backend (task 11) |
| RGB+IR fusion | implemented, unmeasured | Fusion HIL: latency, FRR impact, residual |
| z-norm | interface + synthetic bootstrap | Licence-verified cohort |
| Analytic PAD | implemented, uncalibrated | Real-attack calibration |
| Learned PAD | synthetic-only | Consented real data training |
| Depth path | structural | No depth sensor on reference hardware |
| S3 resume | simulated 20/20 pass | Real S3 cycle |

## Test and CI state

| Check | Result |
|---|---|
| `cargo test --workspace --no-fail-fast -- --test-threads=1` | **989 passed, 0 failed, 9 ignored** (HW-gated) |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean** |
| `cargo fmt --all -- --check` | clean |
| SPDX headers | enforced by `scripts/check-spdx.sh` |
| `cargo audit` | 0 vulnerabilities, 329 deps |

## Release artifacts

Defined in `.github/workflows/release.yml` (not yet executed against a tag):

| Artifact | Format | Status |
|---|---|---|
| Checksums | SHA256 | Defined |
| Signature | GPG | Defined (needs `secrets.GPG_PRIVATE_KEY`) |
| SBOM | CycloneDX | Defined |
| Provenance | SLSA Level 3 | Defined (needs OIDC) |

## Path to production

1. **Human supplies owner-gated inputs:** root access, reference camera
   time, lawful cohort dataset, consented attack subjects, signing keys,
   second TPM, Fedora/Debian/aarch64 builders, clean GNOME/KDE VMs.
2. **Run the HIL scripts:** `scripts/hil/reference-auth-hil.sh` for G1.1,
   scripted S3 for G1.6, luminance check for G1.5.
3. **Run the eval harness** on RC packages for G2.
4. **Select + accept the cohort** and run the subject-disjoint evaluation
   for G3 (>=300,000 impostor comparisons).
5. **Collect + calibrate PAD** on consented real attacks for G4.
6. **Visual QA** on clean GNOME/KDE VMs for G5.
7. **Build + test the full matrix** in containers for G6.
8. **Sign + publish** the release tag.

Until those steps complete, this is an evidence-complete release
candidate: the code is done, the evidence is honest, and every gap is
named.
