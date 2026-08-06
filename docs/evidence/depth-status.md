# Depth capability path — status

Last updated: 2026-08-07
Owner: capture layer
Gate: G6 (depth corroboration) — `partial` until HIL

## What landed

`crates/namtor-capture/src/depth.rs` adds the capability-gated depth capture
path. Depth is an *optional* liveness channel: when present it corroborates the
strobe-differential IR PAD veto; when absent the IR path carries authentication
alone.

API surface:

- `DepthFormat { Depth16, DepthZ16, Unsupported }`
- `trait DepthSource: CaptureSource` — `depth_format()`, `depth_range_mm()`
- `DepthCapability { available, format, min_range_mm, max_range_mm }`
- `discover_depth(&SourceCaps) -> DepthCapability`
- `DepthQuality { Good, Marginal, Poor, NoDepth }`
- `assess_depth_quality(&[Frame]) -> DepthQuality`
- `DepthVetoResult { DepthPass, DepthFail(String), DepthUnsupported }`
- `depth_pad_veto(depth: &[Frame], ir: &[Frame]) -> DepthVetoResult`
- `ensure_ir_fallback(&DepthCapability) -> bool`
- `DepthCapability::production_ready() -> bool`
- `DepthFixture` (test-only replay fixtures)

## Fail-closed semantics

`depth_pad_veto` never returns `DepthPass` when:

- the depth slice is empty,
- no frame is in `Depth16` format,
- any depth frame fails `Frame::validate`,
- quality is `Poor` or `NoDepth`,
- or the IR corroboration slice is empty (depth-only auth is out of scope).

Empty/no-depth returns `DepthUnsupported` (fall back to IR); present-but-bad
returns `DepthFail` (veto).

## Production readiness gate

`DepthCapability::production_ready()` is a compile-time gate:
`DEPTH_HIL_VALIDATED = false`. It returns `false` for every capability until a
hardware-in-the-loop campaign records measured APCER per attack type for the
depth veto (spec §6.4, R10). `ensure_ir_fallback` returns `true` whenever
`production_ready` is `false`, so the auth path stays on the validated
strobe-differential IR path.

Flip `DEPTH_HIL_VALIDATED` to `true` only after the HIL evidence is recorded
in this directory.

## What is NOT claimed

- **No HIL evidence.** The depth veto has never run against a real depth
  sensor. APCER per attack type is unmeasured.
- **No depth sensor on reference hardware.** The ASUS ROG Flow X16 has no
  depth node; `discover_depth` returns `Unsupported` on this machine.
- **`DepthZ16` is structural only.** `PixFmt` has no `Z16` variant yet, so
  `discover_depth` only produces `Depth16`. The enum variant exists so
  downstream code can pattern-match exhaustively against future expansion.
- **Quality thresholds are heuristic.** `Good ≥ 50% valid pixels`,
  `Marginal ≥ 20%`, `Poor < 20%`. These are first-pass bands for replay
  fixtures, not calibrated PAD thresholds. Calibration requires the HIL
  campaign.

## Tests

9 tests in `depth::tests` (49 pass total in `namtor-capture`, 4 HW-gated
ignored):

- `discover_depth_finds_depth_format`
- `discover_depth_returns_unsupported_for_rgb_only`
- `assess_depth_quality_good_marginal_poor`
- `depth_pad_veto_fails_closed_on_unsupported`
- `depth_pad_veto_passes_on_valid_depth`
- `ir_fallback_preserves_auth_path`
- `production_ready_false_without_hil`
- `malformed_depth_fails_closed`
- `depth_source_trait_is_implementable`

CI: `cargo test -p namtor-capture -- --test-threads=1`,
`cargo clippy -p namtor-capture -- -D warnings`,
`cargo fmt -p namtor-capture -- --check` all green.

## Next steps to close the gate

1. Acquire a depth-bearing UVC device (Realsense-class) with a `DEPTH16` node.
2. Record a HIL attack dataset (live, print, screen replay, cutout) with the
   depth veto enabled.
3. Measure APCER per attack type; record in `docs/evidence/depth-hil.jsonl`.
4. Calibrate the `GOOD_VALID_FRAC` / `MARGINAL_VALID_FRAC` bands against the
   measured distribution.
5. Flip `DEPTH_HIL_VALIDATED` to `true` and update this status.
