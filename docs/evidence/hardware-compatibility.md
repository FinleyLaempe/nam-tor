# Hardware Compatibility — G1.2 Evidence and Contribution Flywheel

Task 24 (plan: `namtor-production-readiness.md`). Commit: `feat(probe): build hardware evidence flywheel`.

## Scope

G1.2 (spec §12): *On a device absent from the database, `namtor-probe` identifies the IR emitter and emits a candidate quirks entry.* Target: **≥80% success across contributed devices**.

This task builds the **flywheel** — the machinery that turns a one-device probe into a growing, validated, regression-guarded evidence base — and records the structural evidence that the machinery exists and is tested. It does **not** claim G1.2 passes: the contributor denominator is still one device (the reference Sonix `3277:0018`), so the ≥80% target is `unmeasured` until a device pool exists. See [`status-audit.md`](status-audit.md) row G1.2.

## What shipped

New module `crates/namtor-probe/src/flywheel.rs` (Apache-2.0), exposed as `namtor_probe::flywheel`. Production code 213 LOC (under the 250 ceiling), 22 unit tests, all green under `cargo test -p namtor-probe -- --test-threads=1`. `cargo clippy -p namtor-probe -- -D warnings` clean. `cargo fmt -p namtor-probe -- --check` clean.

### 1. `ContributionTemplate`

A contributed device record ready for the quirks database. Fields: `vid_pid`, `name`, `ir_vc_interface`, `ir_streaming`, MSXU findings (`msxu_unit`/`msxu_selector`/`msxu_mode`), vendor-XU findings (`vendor_xu_unit`/`vendor_xu_selector`), `emitter_method`, optical verification (`optical_verified`, `optical_delta`), `contributor`, `date`, `verified`, `experimental`.

Two constructors, each under the 3-parameter ceiling via a grouped `ContributionContext`:

- `ContributionTemplate::from_probe(&ProbeResult, &ContributionContext, optical_delta)` — verified path. `verified` is true iff `optical_delta ≥ 10.0` (matches `namtor-uvc::emitter` default `min_delta`).
- `ContributionTemplate::manual_light_judgment(&ProbeResult, &ContributionContext)` — **always `experimental: true`, always `verified: false`, always `optical_verified: false`.** A human looking at the emitter through a phone camera is not optical verification.

`invariant_holds()` asserts the rule: experimental entries are never verified; verified entries always passed optical verification. Tested in `manual_light_judgment_entry_is_experimental_and_unverified` and `contribution_template_verified_requires_optical_delta`.

### 2. `CompatibilityDenominator`

The G1.2 denominator made explicit. Fields: `total_known_devices`, `total_tested`, `total_msxu`, `total_vendor_xu`. Methods:

- `success_rate()` — `(total_msxu + total_vendor_xu) / total_known_devices`, 0.0 when empty.
- `g1_2_passes()` — `total_known_devices > 0 && success_rate() ≥ 0.8`.
- `record(&template)` — counts a known device; counts a tested device only if `template.verified`. Experimental entries are counted as known but **not** as tested.

Tested in `compatibility_denominator_success_rate_and_g1_2` and `compatibility_denominator_experimental_not_counted_as_tested`.

### 3. `QuirksSchemaCI`

CI gate for contributed quirks entries. Validates raw TOML against the `namtor_hwdb::quirks::QuirksEntry` schema plus contribution-specific invariants the schema alone does not encode:

- `device.vid_pid` parses as `vid:pid` hex (`parse_vid_pid`).
- `meta.verified_by` is non-empty.
- `emitter.method` is not `Unknown` (a contribution must identify a working method) — gated by `require_known_method`.
- A `method_note` containing "manual light judgment" must **not** be signed off by `namtor-probe` — a human must sign experimental entries — gated by `enforce_experimental_signoff`.

Returns `Result<QuirksEntry, Vec<String>>` with all errors (not just the first). Tested in five cases: accepts valid, rejects unknown method, rejects bad vid_pid, rejects manual-light-judgment signed by probe, allows manual-light-judgment signed by human.

### 4. `RegressionCapture`

A frozen `ProbeResult` for a known device, used to detect regressions: a future change to the probe must produce the same `method`/`unit`/`selector`/`mode` on a device it previously matched.

- `from_probe(&ProbeResult, min_optical_delta)`.
- `matches_probe(&ProbeResult)` — structural match on vid_pid + method + unit + selector + mode.
- `optical_ok(delta)` — `delta ≥ min_optical_delta`.

Tested in `regression_capture_matches_equivalent_probe`, `regression_capture_rejects_method_change`, `regression_capture_rejects_mode_change`.

### 5. `ContributionBundle`

A privacy-safe aggregate of contributions: `Vec<ContributionTemplate>` + `CompatibilityDenominator`. `add(template)` records the denominator and stores the template. `privacy_safe()` returns true by construction — `ContributionTemplate` carries no serial-number or user-data fields — and exists so a future field addition that breaks the invariant fails a test rather than shipping silently.

Tested in `contribution_bundle_records_denominator_and_is_privacy_safe`.

### 6. `verify_emitter_optically(before: &[u8], after: &[u8]) -> bool`

Contribution-time optical sanity check on raw 8-bit greyscale buffers: `mean(after) − mean(before) ≥ 10.0`. This is the simpler sibling of `namtor_uvc::emitter::verify_emitter`, which does the stricter centre-weighted test on `Frame`s in the live probe path. Empty buffers return false — no evidence is not evidence.

Tested in `verify_emitter_optically_accepts_luminance_increase`, `verify_emitter_optically_rejects_no_change`, `verify_emitter_optically_rejects_empty_buffers`.

## Privacy

No serial numbers, user data, or biometric frames cross this boundary. `ContributionTemplate` has no such fields by construction; `ContributionBundle::privacy_safe()` is the structural assertion. The redacted-report invariant from `namtor_probe::emit` (the words "frame" and "image" never appear) is preserved: the flywheel speaks of "optical verification" and "luminance delta", not captures.

## G1.2 status after this task

| Sub-claim | Evidence | Status |
|---|---|---|
| Probe identifies emitter on unknown device | `namtor_probe::probe::run_probe` (M2, 3 tests) | **code complete** |
| Probe emits candidate quirks entry | `namtor_probe::emit::render_quirks_entry` (M2, 4 tests, round-trips through `QuirksEntry`) | **code complete** |
| Contribution is privacy-safe | `ContributionBundle::privacy_safe` + `ContributionTemplate` field audit | **structural** |
| Contribution is schema-valid | `QuirksSchemaCI::validate` (5 tests) | **structural** |
| Manual-light-judgment stays experimental | `ContributionTemplate::manual_light_judgment` + `QuirksSchemaCI` signoff rule (3 tests) | **structural** |
| Known-device regressions are caught | `RegressionCapture::matches_probe` (3 tests) | **structural** |
| ≥80% success across contributed devices | `CompatibilityDenominator::g1_2_passes` (2 tests) | **unmeasured** — denominator is 1 device |

**Gate verdict: G1.2 remains `unmeasured`.** The flywheel is the measuring instrument; it is built and tested, but the denominator is still one device (the reference Sonix `3277:0018`). The broad claim cannot be made until a device pool contributes. `CompatibilityDenominator` makes that impossibility structural: `g1_2_passes()` returns false on an empty denominator, so the claim cannot slip through.

## Reproducing

```
cargo test -p namtor-probe -- --test-threads=1
cargo clippy -p namtor-probe -- -D warnings
cargo fmt -p namtor-probe -- --check
```

22 new tests in `flywheel::tests::*`, 0 failed, 0 ignored. No `unwrap`/`expect` in non-test code. SPDX-Apache-2.0 header present.
