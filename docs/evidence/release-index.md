# Release Evidence Index

<!-- SPDX-License-Identifier: GPL-3.0-only -->

Index of release artifacts, evidence files, and claims policy for Nam-tor.
This file is the single entry point that maps every public release claim to
the evidence that supports it (or to the WITHHELD status that bounds it).

## 1. Release Artifacts

All release artifacts are produced by `.github/workflows/release.yml` at tag
time (`v*` tag push). The workflow is defined and committed; it has not yet
been executed against a published tag.

| Artifact | Format | Source | Status |
|---|---|---|---|
| Checksums | `namtor-<tag>-checksums.txt` (SHA256) | release.yml | Defined, not yet executed |
| Signature | `namtor-<tag>-checksums.txt.asc` (GPG) | release.yml, `secrets.GPG_PRIVATE_KEY` | Defined, not yet executed |
| SBOM | CycloneDX (cargo-cyclonedx, syft fallback) | release.yml | Defined, not yet executed |
| Provenance | SLSA Level 3 (slsa-framework/slsa-github-generator) | release.yml | Defined, not yet executed |

The workflow builds a matrix of three cells — Arch Linux (x86_64), Debian 13
trixie (amd64) and Fedora 41 (x86_64) — and runs lint, build, the shipped-
weights inference check, install, smoke, upgrade and uninstall in each,
aborting on any hard blocker.

This paragraph previously claimed "Arch, Fedora 40/41, Debian 12/13; x86_64
and aarch64". None of that beyond Arch x86_64 was ever in the matrix: until
2026-09-17 it had one cell, and every release shipped Arch packages only.
There is no aarch64 cell, no Fedora 40 and no Debian 12. Debian 12 cannot
build this tree at all — bookworm's cargo predates the v4 Cargo.lock, and
trixie's rustc 1.85.1 is below the 1.88 that `ort` requires, so the Debian
cell builds with a rustup toolchain. It does not push to a remote package
registry; downstream distribution (AUR, COPR, apt repo) is an owner-gated
manual step.

## 2. Evidence Index

Every evidence file referenced by a release claim is listed here with a
one-line description. Files that are defined but not yet present on disk are
marked `(defined)`.

### Readiness ledger

- `docs/evidence/status-audit.md` — honest readiness ledger; per-gate status
  (pass / partial / unmeasured / WITHHELD) with owner-gated inputs blocking
  each gate.

### CI and packaging

- `docs/evidence/ci-baseline.md` — CI security gates (G5.1 / G5.2 / G5.4 /
  G5.6 structural checks) and the baseline they enforce.
- `docs/evidence/install-manifest.json` — install tree manifest recorded by
  the disposable-container install step.
- `docs/evidence/package-matrix.json` — package matrix definition (distro,
  arch, build, install, smoke, upgrade, uninstall cells).
- `docs/evidence/pam-install-matrix.md` — PAM install test results across
  the matrix.
- `docs/evidence/rc-package-matrix.json` — RC package lifecycle results
  (build, install, upgrade, uninstall) per matrix cell.
- `docs/evidence/rc-package-matrix.md` — human-readable summary of the RC
  package matrix results.

### Packaging evidence

- `docs/evidence/packaging/install-layout.md` — install tree layout, paths,
  and ownership.
- `docs/evidence/packaging/distro-packages.md` — distro package build and
  test results (Arch, Fedora, Debian).
- `docs/evidence/packaging/pam-install.md` — transactional PAM install
  lifecycle evidence.
- `docs/evidence/packaging/systemd-hardening.md` — systemd unit hardening
  (service isolation, capabilities, restrictions).
- `docs/evidence/packaging/model-acquisition.md` — model weight acquisition
  and licence acceptance flow.
- `docs/evidence/packaging/diagnostics.md` — diagnostics bundle privacy and
  portability.
- `docs/evidence/packaging/signatures-sbom.md` — release signatures and SBOM
  generation evidence.
- `docs/evidence/packaging/reproducible-build.json` — reproducible build
  verification (hash, environment, flags).
- `docs/evidence/packaging/namtor.cyclonedx.json` — CycloneDX SBOM.
- `docs/evidence/packaging/namtor.spdx.json` — SPDX SBOM.
- `docs/evidence/packaging/namtor.provenance.json` — SLSA provenance
  attestation.

### G1 hardware-in-the-loop

- `docs/evidence/g1-selection.json` — camera auto-selection result
  (candidate enumeration, quirks match, selected node).
- `docs/evidence/g1-emitter.jsonl` — emitter discovery (MSXU unit, selector,
  mode, firmware-strobed confirmation).
- `docs/evidence/g1-s3-20x.jsonl` — S3 suspend/resume 20x cycle results
  (simulated; real S3 WITHHELD pending hardware access).
- `docs/evidence/g1-hardware-lifecycle.md` — G1 hardware lifecycle gate
  summary (selection, MSXU, teardown, hotplug, S3).
- `docs/evidence/g1-final.md` — G1 HIL gate suite summary (Markdown).
- `docs/evidence/g1-final.json` — G1 HIL gate suite summary (machine-readable).

### G2 through G4 (recognition and liveness)

- `docs/evidence/g2-latency-reliability.md` — G2 latency and reliability
  gate summary (median, p95, cold-start).
- `docs/evidence/g3-biometric-eval.md` — G3 biometric evaluation gate
  summary (FAR, FRR, DET curve, N, CIs).
- `docs/evidence/g4-presentation-attack.md` — G4 presentation attack
  detection gate summary (per-attack APCER, BPCER).
- `ml/reports/production-eval.md` — G2 to G4 evaluation protocol, metrics,
  and results (FAR, FRR, PAD error rates).
- `ml/reports/analytic-pad-calibration.md` — analytic PAD gate calibration
  (thresholds, cohort, error rates per §6.4).
- `ml/reports/learned-pad.md` — learned PAD (MiniFASNet) training, synthetic
  data regime, and deployment notes.
- `ml/reports/backend-ablation.md` — CPU backend ablation (FP32 vs INT8,
  AVX-VNNI, latency and accuracy).
- `docs/evidence/fusion-hil.jsonl` — RGB to IR fusion HIL results
  (extrinsics, alignment, score contribution).

### R5 normalisation

- `docs/evidence/r5-cohort.md` — z-norm cohort definition, licence status,
  and measured normalisation impact.

### Trust evidence

- `docs/evidence/trust/g5-security-proof.md` — G5 security proof: gate
  matrix, mutation testing, binary hashes, forbidden-data assertion receipt.
- `docs/evidence/trust/tpm2-lifecycle.md` — TPM2 sealing lifecycle (create,
  unseal, corruption, migration, cross-hardware WITHHELD).
- `docs/evidence/trust/fusion.md` — RGB-IR fusion status (extrinsics,
  alignment, geometric identity, WITHHELD).
- `docs/evidence/trust/znorm.md` — z-norm score normalisation status (cohort
  selection, calibration, WITHHELD).
- `docs/evidence/trust/pad-calibration.md` — analytic PAD calibration status
  (thresholds, attack corpus, WITHHELD).
- `docs/evidence/trust/learned-pad.md` — learned PAD (MiniFASNet) status
  (synthetic-only, deployment, production selection WITHHELD).
- `docs/evidence/trust/int8-eval.md` — INT8 vs FP32 evaluation status (Path
  B architectural decision, measurement WITHHELD).
- `docs/evidence/trust/backoff.md` — per-uid authentication backoff and
  rate limiting evidence.
- `docs/evidence/g5-tpm.md` — TPM2 sealing status (legacy reference;
  superseded by trust/tpm2-lifecycle.md).
- `docs/evidence/g5-fault-matrix.json` — security fault matrix (per-failure
  mode, detection, fail-closed behaviour).
- `docs/evidence/g5-rate-limit.json` — rate limiting measurements (lockout,
  backoff, per-uid bounds).

### G5 desktop end-to-end

- `docs/evidence/g5-desktop-e2e.md` — G5 desktop end-to-end gate summary
  (PAM, sandbox, fail-closed, log privacy, TPM).

### G6 install matrix

- `docs/evidence/g6-install-matrix.md` — G6 install matrix gate summary
  (distro, arch, build, install, smoke, upgrade, uninstall).

### G7 status reconciliation

- `docs/evidence/g7-status-reconciliation.md` — G7 status reconciliation
  gate summary (ledger consistency, claim fidelity, WITHHELD visibility).

### RC production gates

- `docs/evidence/rc-g2-g5.md` — RC production gates G2 to G5 summary.
- `docs/evidence/rc-g2-g5.json` — RC production gates G2 to G5
  machine-readable results.
- `docs/evidence/release-candidate.md` — release candidate evidence summary.

### Data collection

- `docs/evidence/collection-protocol.md` — consented collection protocol
  (ConsentRecord, per-session IDs, withdrawal, differential stats).

### Hardware compatibility

- `docs/evidence/hardware-compatibility.md` — hardware flywheel: supported,
  quirks, and unsupported devices.
- `docs/evidence/depth-status.md` — depth capability status and path
  forward.

### Desktop integration

- `docs/evidence/dbus-contract.xml` — D-Bus API contract (Profile service).
- `docs/evidence/manager-contract.md` — manager wire contract
  reconciliation (methods, signals, version negotiation).
- `docs/evidence/manager-bus-runtime.md` — system-bus zbus service runtime
  evidence (introspection, ownership, shutdown).
- `docs/evidence/manager-store-integration.md` — manager profile lifecycle
  backed by encrypted TemplateStore.
- `docs/evidence/manager-enrollment.md` — manager transactional enrollment
  sessions.
- `docs/evidence/manager-polkit.md` — polkit peer identity and authorization
  evidence.
- `docs/evidence/manager-disconnect.md` — manager disconnect and
  cancellation fault handling.
- `docs/evidence/manager-fault-matrix.md` — manager fault matrix.
- **SUPERSEDED IN PART (2026-09-16):** the UI evidence below was recorded
  when the crate was `namtor-gnome` and the package was `namtor-gnome`, and
  its paths and prose still say "GNOME". It is left byte-identical because
  it records what the 0.1.0 release shipped. The crate is now
  `crates/namtor-ui` and the package is `namtor-ui` on all three distros;
  `namtor-gnome` is an obsoleted predecessor that a normal upgrade removes.
  The app itself, its binary `namtor-enroll-wizard` and its
  `org.namtor.Wizard` desktop/AppStream/icon ids are unchanged, so the
  behavioural findings below still hold — only the crate and package names
  moved. No claim that Nam-tor ships a GNOME-specific UI may be made from
  these files for any release after 0.1.0. See
  `docs/adr/single-desktop-ui.md`. The screenshot harness
  `scripts/hil/capture-ui-screenshots.sh` now writes to
  `docs/evidence/ui/real-run/ui/` rather than `.../real-run/gnome/`; the
  withheld placeholder manifest at `docs/evidence/ui/real-run/manifest.json`
  still names the old path and is left as-is until a real HIL run
  regenerates it.
- `docs/evidence/gnome-qa/report.md` — GNOME wizard and settings QA results.
- `docs/evidence/ui/gnome/enrollment.json` — GNOME enrollment AT-SPI
  scenarios.
- `docs/evidence/ui/gnome/profile-actions.json` — GNOME profile actions
  AT-SPI scenarios.
- `docs/evidence/ui/gnome/accessibility.md` — GNOME accessibility audit.
- `docs/evidence/ui/gnome/gnome-shell-qa.md` — GNOME Shell QA.
- `docs/evidence/ui/gnome/package-qa.md` — GNOME package QA.
- `docs/evidence/ui/gnome/review.json` — GNOME review scenarios.
- **SUPERSEDED (2026-09-16):** the KDE KCM evidence below records what the
  0.1.0 release shipped and is left byte-identical for that reason. The
  Plasma KCM (`crates/namtor-kde`, package `namtor-kde`) has since been
  removed; the project ships one desktop-neutral GTK4 UI. No claim about a
  KDE-native System Settings module may be made from these files for any
  release after 0.1.0. See `docs/adr/single-desktop-ui.md`. The same applies
  to `docs/evidence/public-release/15-kde-flow.json` and
  `15-kde-failures.json`.
- `docs/evidence/kde-qa/report.md` — KDE KCM QA results.
- `docs/evidence/ui/kde/build.md` — KDE KCM build evidence.
- `docs/evidence/ui/kde/bridge.json` — KDE async D-Bus bridge and profile
  model.
- `docs/evidence/ui/kde/enrollment.json` — KDE enrollment Qt Quick
  scenarios.
- `docs/evidence/ui/kde/profile-actions.json` — KDE profile management Qt
  Quick scenarios.
- `docs/evidence/ui/kde/package-qa.md` — KDE KCM package QA.

### Diagnostics and presence

- `docs/evidence/diagnostics.md` — diagnostics surface (status, logs,
  health checks).
- `docs/evidence/diagnostics-portability.md` — diagnostics portability
  (no hardcoded paths, device-independent).
- `docs/evidence/daemon-observability.md` — daemon structured logging and
  startup state evidence.
- `docs/evidence/protocol-lifecycle.md` — auth protocol lifecycle (Release
  semantics, peer close, capture/emitter release).
- `docs/evidence/failure-contract.md` — safe failure context (typed external
  codes, PAM fail-closed, PAD veto indistinguishable from NoMatch).
- `docs/evidence/concurrency-flake.md` — camera-lock deadlock elimination
  evidence.
- `docs/evidence/enrollment-storage-faults.md` — enrollment ownership and
  atomicity fault evidence.
- `docs/evidence/live-topology-hil.md` — live camera topology HIL evidence.
- `docs/evidence/presence-hil.md` — presence mode HIL results.
- `docs/evidence/depth-status.md` — depth capability status and path
  forward.
- `docs/evidence/hardware-compatibility.md` — hardware flywheel: supported,
  quirks, and unsupported devices.
- `docs/evidence/collection-protocol.md` — consented collection protocol
  (ConsentRecord, per-session IDs, withdrawal, differential stats).
- `docs/evidence/profile-schema.json` — profile schema definition.
- `docs/evidence/enrollment-ranking.json` — enrollment ranking and
  reservoir configuration.
- `docs/evidence/decision-policy.json` — decision policy configuration.

## 3. Claims Policy

Every public release claim is bounded by the evidence, hardware, and data
that support it.

1. **Evidence-bound.** A claim is only published when the supporting
   evidence file exists and records a measured result. The
   `docs/evidence/status-audit.md` ledger is the source of truth for gate
   status.
2. **WITHHELD is visible.** Gates that failed or were not measured are
   reported as WITHHELD, not omitted. A WITHHELD gate is a limitation, not a
   gap to paper over.
3. **No parity or readiness claims without measurement.** "Windows Hello
   parity", "production ready", and similar claims do not ship without
   measured evidence at the required threshold. The absence of measurement
   means the claim is not made.
4. **Hardware scope is explicit.** All HIL results name the reference
   device (ASUS ROG Flow X16 GV601VI, Sonix `3277:0018`). Results do not
   generalise to other cameras without per-device evidence.
5. **Data scope is explicit.** PAD and recognition results name the dataset
   and its licence status. Synthetic-only training is stated as a
   limitation, not hidden.

## 4. Unsupported Risks (Explicitly WITHHELD)

The following risks are not covered by current evidence and are explicitly
stated as limitations in release notes and the status audit.

- **3D masks.** Not evaluated. The analytic PAD gates and MiniFASNet target
  2D replay and print; 3D mask attack performance is unknown.
- **Cross-hardware TPM.** WITHHELD. TPM2 sealing is a file fallback stub on
  the reference device; sealing on other TPMs or firmware versions is not
  measured.
- **Broad camera compatibility.** WITHHELD. The hardware flywheel lists one
  reference camera. Compatibility with other IR illuminators, UVC quirks,
  or emitter methods is not claimed.
- **FAR less than or equal to 1e-5.** WITHHELD. The recognition error rate
  at the 1e-5 operating point is not measured against a real cohort; the
  threshold is set by design, not by evaluation.

## 5. Release Notes Template

Every release tag ships notes in this shape.

```
# Nam-tor <version>

## Compatibility
- Model: <model id and version shipped>
- API: <D-Bus / PAM socket schema version>
- Schema: <profile / store schema version>

## Migration Guide
- Steps to upgrade from the previous release, including any schema
  migration, template re-enrollment, or config changes.

## Recovery Instructions
- How to fall back if auth fails (PAM deny/ignore toggle, cached
  credentials, emergency root access).

## Support Matrix
- Distro and architecture cells validated by the release matrix.
- Reference camera and required kernel / systemd versions.

## Known Limitations
- WITHHELD gates from the status audit, copied verbatim.
- Reference hardware scope.
- Dataset and training regime scope.
```

## 6. Signature Verification

Release signing is owner-gated, not automatic.

- **GPG signing** uses `secrets.GPG_PRIVATE_KEY` in the release workflow.
  If the secret is not set, signing is skipped with a warning and the
  release does not fail. An unsigned release is valid but carries no GPG
  attestation.
- **SLSA Level 3 provenance** is produced by
  `slsa-framework/slsa-github-generator` when the workflow runs with
  `id-token: write`. Provenance generation is owner-gated by the repository
  OIDC configuration.
- **Verification.** Consumers verify a signed release by checking the
  checksums file against the GPG signature, then checking each artifact
  against the checksums. An unsigned release can still be verified by
  checksum, but not by attestation.

Owners must set `secrets.GPG_PRIVATE_KEY` and enable OIDC for SLSA before a
release carries signed attestation.
