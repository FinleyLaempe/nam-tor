<!-- SPDX-License-Identifier: GPL-3.0-only -->

# M1 Hardware Spike — Report

Date: 2026-08-04
Machine: ASUS ROG Flow X16 GV601VI, Arch Linux, kernel 7.1.4
Camera: Sonix 3277:0018, USB 3-8
Branch: `master` (commits `feat(uvc): …` through `docs: M1 spike findings …`)

This milestone retired the three design risks that could have invalidated the
Nam-tor architecture (R1, R2, R3) and produced quirks database entry #1 for
the reference camera. R1/R2/R3 were already resolved PASS on the reference
machine before this run (see `HANDOFF.md`); M1 codifies them as executable
Rust in the new `namtor-uvc` crate and reproduces the measurements via the
`scripts/spike-*.sh` harness.

## Risk verdicts

| Risk | Question | Verdict | Codified by |
|---|---|---|---|
| **R3** | Is the IR emitter software-controllable? | **PASS** — MSXU mode 2 | `namtor-uvc::mode` + `namtor-spike modes` |
| **R1** | Can IR + RGB stream concurrently? | **PASS** — both streams exit 0 | `scripts/spike-dual-stream.sh` |
| **R2** | Does firmware AE hunt against the strobe? | **PASS for Tier 0** | `scripts/spike-ae-hunting.sh` |
| **R9** | Emitter idle baseline | `idle_dark` | `scripts/spike-emitter-idle.sh` |
| **R10** | Emitter mode survives suspend? | **OPEN** — needs an S3 cycle at the keyboard | (not in M1 scope) |
| **R11** | Cold stream-open latency | **OPEN** — ~677 ms upper bound (subprocess); re-measure in-process in M2 | (not in M1 scope) |

### R3 — PASS (MSXU mode 2, Tier 0)

The reference camera's IR VideoControl interface (USB interface 2) carries an
MSXU (unit 4, GUID `0f3f95dc-2632-4c4e-92c9-a04782f43bc8`) advertising
`bmControls = 20 01` — selectors 6 and 9. Selector 6 is a 9-byte mode
control: byte 1 (`0x03`) is a capability mask, byte 2 is the mode field
(current `1`, max `3`). Writing mode `2` (`01 03 02 00 00 00 00 00 00`)
makes the **firmware strobe the emitter itself** in strict 1:1 alternation,
illuminated mean 81.9 vs clean 0.13. Modes 0, 1, 3 are inert; selector 9 is
unrelated to the emitter.

This is the Windows Hello mechanism: the daemon sets one mode control and the
device tags alternating illuminated/clean IR frames. Torn-frame handling
(spec §4.4) is unnecessary and risk R2 is void for this device.

`namtor-uvc::mode::measure_alternation` classifies alternation by **frame
parity**, not a global brightness threshold — measured reference data shows
the illuminated phase decaying 217.6 → 45.3 under auto-exposure while clean
frames hold at 0.0, which would defeat a threshold derived from the burst
mean. The shape check (sign-flip count) rejects monotonic AE ramps.

### R1 — PASS (dual-stream)

Concurrent IR (640×360 GREY) + RGB (1280×720 MJPG) capture succeeds: both
`v4l2-ctl` streams exit 0. Contention is real: IR falls from ~25 fps solo to
19.8 fps concurrent, RGB runs at 12 fps with one dropped buffer. A fused
strobe pair therefore costs ~101 ms during fusion vs ~66 ms IR-only. Fusion
is available and not free. Dual-camera fusion (spec §6.5) uses concurrent
streams.

### R2 — PASS for Tier 0

Because Tier 0 (MSXU mode 2) has the firmware strobe the emitter with no
userspace toggle, there is no userspace toggle for AE to fight. The measured
AE decay acts on illuminated frames only (217.6 → 45.3, settling after ~15
pairs / ~1 s); clean frames stay at 0.0, so pairing and the differential
remain valid while settling. The first pair has the best SNR (217.2); the
daemon must not wait for settling. Per-pair normalisation is required.

R2 remains **open for Tier 1/2 devices** where userspace drives the strobe —
no such device is in scope for M1.

## Hardware-gated tests

Tests that require the physical camera are marked `#[ignore]` in
`namtor-uvc::device` and are **not** run by CI. CI never sets
`NAMTOR_HW_TEST`. They compile against the real device path and are executed
on the reference machine by the developer:

```bash
NAMTOR_HW_TEST=1 cargo test -p namtor-uvc -- --ignored --test-threads=1
```

Currently ignored (2):

- `device::tests::reference_camera_ir_node_opens` — opens `/dev/video2`.
- `device::tests::xu_query_returns_either_data_or_a_clean_error_never_a_hang` —
  probes MSXU unit 4 selectors 6 and 9; EPERM here would be a finding, not a
  failure. Already confirmed: succeeds as an ordinary desktop user, no root,
  no `CAP_SYS_ADMIN` (recorded in `quirks/devices/3277-0018.toml` `[access]`).

## What M1 codified

The `namtor-uvc` crate (Apache-2.0) is the executable form of this spike:

| Module | Codifies |
|---|---|
| `xu` | `UvcXuControlQuery` ABI, `Query` codes, `UVCIOC_CTRL_QUERY` ioctl binding, `CtrlInfo` GET_INFO decoding |
| `probe` | `SafeXu` enforcing the six probe-safety rules (spec §4.3): read-before-write, restore-on-failure, GET_LEN/GET_INFO honoured, no-SET-skipped, `Restorer` RAII |
| `descriptors` | sysfs descriptor parsing recovering the L8 IR format GUID (spec §4.1); multi-sensor interface filtering; selector bitmap decoding |
| `metadata` | `V4L2_META_FMT_UVC` payload-header parsing (ns, SOF, PTS, EOF, error) for torn-frame pairing |
| `strobe` | `StrobeSequencer` + `classify` torn-frame rejection |
| `emitter` | `luma_stats` + `verify_emitter` — optical emitter verification by luminance delta (replaces asking a human) |
| `discover` | `discover_emitter` Tier 1/2 search driver with advertised-selector ordering |
| `device` | `V4l2Device` live transport |
| `mode` | `ModeControl` + `find_mode_controls` + `measure_alternation` — Tier 0 device-driven illumination |

`namtor-spike` binary exposes `descriptors`, `modes` (Tier 0), and `emitter`
(Tier 1/2) subcommands.

## Quirks entry

`quirks/devices/3277-0018.toml` records every measured value. A header
comment in that file maps each field to the `namtor-uvc` module or spike
script that codifies it. The entry is the first in the Apache-2.0 quirks
database (`quirks/README.md`).

## Design changes required

None. R3 PASS via Tier 0 (MSXU mode 2) confirms the design holds; R1 PASS
keeps dual-camera fusion concurrent; R2 is void for Tier 0 on this device.
The two open risks (R10 suspend survival, R11 in-process stream-open
latency) are deferred to on-device and M2 work respectively and do not
block M2 planning.
