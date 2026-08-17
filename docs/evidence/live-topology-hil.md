# Live Topology — Hardware-in-the-Loop (HIL) Evidence

**Status: WITHHELD** — no HIL measurement on real hardware in this task pass.
The topology correlation and node-pairing logic is unit-tested against a
faithful mirror of real sysfs layout (sibling interface dirs, VC/VS split,
hub-child isolation); no real `/dev/video*` open has been captured by an
owner in this pass.

## Scope

Task 1 preserves and validates the real-sysfs camera topology fixes in
`namtor-hwdb` (`nodes.rs`, `pipeline.rs`), `namtor-probe` (`describe.rs`),
and the `3277-0018.toml` quirk. The fixes correct two production defects:

1. **Sibling interface dirs.** Real Linux sysfs exposes USB interface
   directories (`<bus>-<port>:<cfg>.<iface>`) as *siblings* of the USB
   device directory under `/sys/bus/usb/devices/`, not as children of it.
   The original `correlate_nodes` walked the device dir's children and saw
   no interfaces on the reference camera.
2. **VideoControl vs VideoStreaming interfaces.** `uvcvideo` attaches
   `/dev/videoN` under the VideoControl (VC) interface dir; the quirk
   records the VideoStreaming (VS) interface for descriptor-level work
   (L8 GUID, XU enumeration). `pair()` must translate VS → VC when
   matching correlated nodes, or pick the wrong `/dev/videoN`.

## Unit evidence (CI, no hardware)

`cargo test -p namtor-hwdb --lib -- --test-threads=1` — 29 tests green,
including the new topology regressions:

- `nodes::correlates_nodes_from_sibling_interface_dirs_real_sysfs_layout` —
  proves `3-8:*` siblings are correlated while a hub child `3-8.1:1.0` and
  a hub entry `usb1` are excluded.
- `nodes::correlates_four_nodes_with_metadata_flags` — four nodes, metadata
  flags from the `name` file.
- `nodes::ignores_directories_that_are_not_interface_dirs` — stray siblings
  ignored.
- `pipeline::pair_picks_nodes_by_vc_interface_real_uvcvideo_topology` —
  end-to-end `classify` → `pair`: IR through VC2/VS3 selects `/dev/video2`,
  IR metadata `/dev/video3`, RGB through VC0/VS1 selects `/dev/video0`.
- `pipeline::quirks_source_is_authoritative_for_the_reference_device` —
  asserts `ir_vc_interface == Some(2)`, `rgb_vc_interface == Some(0)`,
  XUs enumerated on VC iface 2, MSXU first.
- `pipeline::pair_works_with_renumbered_nodes` — pairing by interface, not
  node index.
- `pipeline::pair_returns_none_when_ir_interface_missing_from_nodes` —
  missing mapping fails actionably (returns `None`, not a panic).

`cargo test -p namtor-probe --lib` — 27 tests green; `describe` now reads
the sibling-dir layout and still reports `IR interface: 3`, `/dev/video2`,
`/dev/video3`, `selectors [6, 9]`.

`cargo build -p namtor-hwdb -p namtor-probe -p namtor-camera` clean — the
new `ir_vc_interface`/`rgb_vc_interface` fields on `Classification` are
populated at every constructor (Quirks, FormatGuid, Heuristic) and consumed
only by `pair()`; all callers (`inventory.rs`, `describe.rs`) compile
unchanged.

## HIL harness (not run this pass)

The harness for a real HIL run consists of:

1. **Reference camera present** — Sonix `3277:0018` on USB `3-8`, IR at
   `/dev/video2`, RGB at `/dev/video0` (per the project's hardware facts).
2. **strace capture** — run `namtor-probe --describe` (or the daemon's
   inventory path) under `strace -f -e openat` and record the `/dev/video*`
   open syscalls. The capture must show `/dev/video2` (IR) and
   `/dev/video0` (RGB) opened, not the metadata nodes.
3. **Owner gate** — the run requires the reference camera attached and the
   `namtor` system user in the `video` group (M3 dependency); a non-owner
   agent must record WITHHELD.

## Residual risk

- The unit tests mirror real sysfs layout but cannot prove the live
  `/dev/video*` open. An owner HIL run is the gate.
- The fallback path (`ir_vc_interface == None`, FormatGuid/Heuristic)
  pairs by the VS interface, which is correct only when VC and VS numbers
  coincide. The reference camera has VC2/VS3 for IR and VC0/VS1 for RGB,
  so the fallback would mispair on it; the Quirks path is the authoritative
  one for this device and is covered by the regression above.
