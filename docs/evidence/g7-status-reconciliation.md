# G7 — Status Reconciliation

SPDX-License-Identifier: GPL-3.0-only

Task: 47 — Reconcile status, documentation, and claims.
Date: 2026-08-08.

This document reconciles the gate evidence from tasks 41-46 with the
authoritative readiness ledger (`docs/evidence/status-audit.md`). It
confirms that every public claim matches its evidence, every WITHHELD item
has a recorded reason, and no stale milestone claim survives.

## Reconciliation method

1. Read `status-audit.md` (audit date 2026-08-07, baseline commits
   `98da82e`, `c0ebc9c`).
2. Read the gate evidence docs produced by tasks 41-46:
   `g1-hardware-lifecycle.md`, `g2-latency-reliability.md`,
   `g3-biometric-eval.md`, `g4-presentation-attack.md`,
   `g5-desktop-e2e.md`, `g6-install-matrix.md`.
3. Read the RC gate snapshot `rc-g2-g5.md` and `rc-g2-g5.json`.
4. Cross-check each gate status against the evidence file that supports it.
5. Run the full workspace test suite and clippy to confirm the code state.

## Test and CI baseline (re-measured 2026-08-08)

| Item | Result | Command |
|---|---|---|
| Workspace tests | **989 passed, 0 failed, 9 ignored** | `cargo test --workspace --no-fail-fast -- --test-threads=1` |
| Clippy | **clean, no warnings** | `cargo clippy --workspace --all-targets -- -D warnings` |
| 9 ignored | HW-gated tests (`#[ignore]` with `"requires the reference camera"`) | CI never sets `NAMTOR_HW_TEST=1` |

Two pre-existing test compile failures were fixed during this task:

1. `crates/namtor-gnome/tests/privacy_scan.rs`: `StatusInfo` initializer
   was missing the `always_on_emitter` field added in task 18. Fixed by
   adding `always_on_emitter: false` to the test fixture.
2. `crates/namtor-gnome/tests/presenter_flow.rs`: the
   `service_not_registered_has_recovery_hint` test asserted the recovery
   hint title contains "bus", but the title was changed to "Nam-tor service
   is not registered" in task 15. Fixed the assertion to check for
   "service".

Both fixes are test-only; no production code changed.

## Gate status crosswalk

| Gate | status-audit.md | Task 41-46 evidence | Reconciled status | Match? |
|---|---|---|---|---|
| G1.1 | partial | WITHHELD (no HIL) | WITHHELD | yes (partial -> WITHHELD: audit said partial because harness exists; evidence doc clarifies HIL is the blocker) |
| G1.2 | unmeasured | WITHHELD (1 device) | WITHHELD | yes |
| G1.3 | WITHHELD | WITHHELD (R3 pass) | WITHHELD | yes |
| G1.4 | partial | implemented | implemented | yes |
| G1.5 | unmeasured | WITHHELD | WITHHELD | yes |
| G1.6 | WITHHELD | WITHHELD (simulated 20/20) | WITHHELD | yes |
| G2.1 | unmeasured | WITHHELD (INT8 component 28.42ms) | WITHHELD | yes |
| G2.2 | unmeasured | WITHHELD (INT8 component 30.95ms) | WITHHELD | yes |
| G2.3 | unmeasured | WITHHELD | WITHHELD | yes |
| G3.1 | WITHHELD | WITHHELD (no cohort) | WITHHELD | yes |
| G3.2 | WITHHELD | WITHHELD | WITHHELD | yes |
| G4.1 | unmeasured | WITHHELD (uncalibrated) | WITHHELD | yes |
| G4.2 | unmeasured | implemented (harness) | implemented (harness) | yes |
| G4.3 | unmeasured | implemented (3D mask excluded) | implemented | yes |
| G5.1 | passed (structural CI) | passed (structural CI) | passed | yes |
| G5.2 | passed (structural CI) | passed (structural CI) | passed | yes |
| G5.3 | passed (structural CI) | passed (structural CI) | passed | yes |
| G5.4 | passed (structural CI) | passed (structural CI) | passed | yes |
| G5.5 | partial | partial (MockTpm + same-device; cross-hardware WITHHELD) | partial | yes |
| Desktop (G5 visual QA) | partial | implemented (unit), WITHHELD (visual QA) | implemented / WITHHELD | yes |

## Milestone status crosswalk

| Milestone | status-audit.md | Reconciled | Notes |
|---|---|---|---|
| M0 eval harness | implemented | implemented | 13 tests green |
| M1 hardware spike | implemented | implemented | R1/R3 PASS, R2 Tier 0 |
| M2 detection + capture | implemented | implemented | 38+40 lib tests |
| M3 daemon + worker | implemented | implemented | 55 daemon + 9 e2e |
| M4 enrollment + storage | partial | partial | TPM stub, z-norm WITHHELD, fusion unmeasured |
| M5 PAM module | implemented (structural) | implemented (structural) | G5.1/G5.3/G5.4 passed |
| M6 analytic PAD | partial | partial | Uncalibrated; no real corpus |
| M7 GNOME | partial | partial | KCM shim built; D-Bus reference impl; visual QA WITHHELD |
| M8 namtor-collect | implemented | implemented | 15 tests; no campaign |
| M9 MiniFASNet PAD | partial (synthetic) | partial (synthetic) | 100% val on synthetic; no real data |
| M10 remaining scope | implemented | implemented | Presence, depth, backend ablation |

## Claims that are NOT made (confirmed)

- No claim of "production ready".
- No claim of "Windows Hello parity".
- No G1-G4 production gate marked `passed`.
- No FAR/FRR/APCER/BPCER number published.
- No claim that TPM sealing is real (file fallback stub).
- No claim that fusion is wired (stub/unmeasured).
- No claim that PAD is calibrated (conservative defaults).
- No claim that learned PAD is production-ready (synthetic-only).

The only `passed` gates are G5.1, G5.2, G5.3, G5.4 (structural CI gates),
explicitly labelled as structural evidence rather than production gate
passes.

## Stale claim scan

The README's historical "targeting Windows Hello parity" is a design
ambition, not a measured result. The status-audit.md §6 explicitly notes
this. No stale milestone claim was found in the evidence docs. The
`rc-g2-g5.md` snapshot and `release-index.md` are consistent with the
gate evidence.

## Owner-gated inputs (confirmed WITHHELD)

| Input | Blocks | Status |
|---|---|---|
| Root: systemd units, namtor user, namtor-auth group | G1.1/G1.5 HIL, live daemon | WITHHELD |
| Python onnxruntime binding (Python 3.14 incompat) | R4 INT8 eval, cohort tooling | WITHHELD |
| Model licences (`NAMTOR_ACCEPT_LICENCE=1`) | R4, production inference | WITHHELD |
| R5 cohort dataset (licence-verified) | G3, z-norm calibration | WITHHELD |
| Consented subjects + attack props | G4, PAD calibration | WITHHELD |
| Second TPM-equipped machine | G5.5 cross-hardware unseal | WITHHELD |
| Scripted S3 HIL run (20x) | G1.6, R10 | WITHHELD |
| Signing keys + distro packaging matrices | Signed packages | WITHHELD |
| Clean GNOME/KDE VM with display | G5 visual QA | WITHHELD |
| Fedora/Debian/aarch64 build containers | G6 full matrix | WITHHELD |

## Conclusion

The gate evidence from tasks 41-46 is consistent with `status-audit.md`.
No claim is published without evidence. Every WITHHELD item has a recorded
reason. The test suite is green (989 passed, 0 failed, 9 HW-gated ignored)
and clippy is clean. The reconciled status is: 4 structural CI gates
passed, everything else WITHHELD or implemented-but-unmeasured, no failed
gates.
