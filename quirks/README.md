<!-- SPDX-License-Identifier: Apache-2.0 -->

# Nam-tor device quirks database

**Licensed Apache-2.0**, deliberately, so any project can consume it — Howdy, other desktop projects, distribution tooling, and ideally the kernel. IR camera detection is a Linux-wide problem, and this database is the piece most likely to outlive Nam-tor itself.

## What this is

Microsoft ships a per-device driver metadata store, which is a large part of why Windows Hello "just works" across certified hardware. Linux has no equivalent. This database is the community substitute.

**It only works if contributing is frictionless**, which is why `namtor-probe` is a first-class deliverable rather than a developer utility.

## Contributing an entry

```bash
namtor-probe --contribute
```

This detects your IR sensor, probes for emitter control, verifies it **optically** — measuring the luminance change rather than asking whether a light came on, which is an unreasonable question about invisible 850 nm emission — and prints the exact `gh pr create` command to submit the result.

A report from a camera that **doesn't** work is just as valuable as one that does. Negative entries stop the next person repeating the search.

## What is recorded

Hardware facts only:

- USB vendor and product identifiers
- Interface numbers and format GUIDs
- Extension unit GUIDs, unit IDs, control selectors, and payloads
- Strobe timing parameters and measured device behaviour

**No personal data. No images. No biometric information of any kind.**

## Schema

One TOML file per device under `devices/`, named `VID-PID.toml`.

| Section | Contents |
|---|---|
| `[device]` | Identifiers, human-readable name, host machine, UVC version |
| `[nodes]` | Interface numbers for IR and colour sensors, format GUID, metadata availability |
| `[emitter]` | Control method, GUID, VideoControl interface, unit, selector, payloads |
| `[emitter.fallback]` | Secondary path if the primary proves inert |
| `[emitter.idle]` | Behaviour with no control applied — see below |
| `[emitter.lifecycle]` | Maximum continuous on-time |
| `[bandwidth]` | Whether concurrent IR + colour streaming is permitted |
| `[exposure]` | Whether firmware auto-exposure hunts against the strobe |
| `[meta]` | Who verified it, when, on which kernel |

### `emitter.method` values

| Value | Meaning | Probing tier |
|---|---|---|
| `msxu_mode` | MSXU mode control; **firmware strobes the emitter itself**, as it does for Windows Hello | **Tier 0 — silent, no prompt** |
| `msxu` | MSXU direct emitter toggle, userspace-driven strobe | **Tier 1 — silent, no prompt** |
| `vendor_xu` | Vendor-specific extension unit | Tier 2 — requires consent |
| `always_on` | Emitter energised by streaming; no off control | — |
| `firmware_strobed` | Device strobes unprompted, with no control set at all | — |
| `unknown` | Nothing found | — |

`msxu_mode` and `firmware_strobed` are both firmware-driven, and the distinction is worth keeping: `firmware_strobed` alternates with **no control set**, while `msxu_mode` alternates **only after a mode is selected** and reverts when it is not. The latter is what the reference camera does, and it is why the daemon must set the mode per capture session rather than once at install.

`always_on` matters: strobe differential is impossible on such devices, so liveness falls back to non-differential signals and gate G4 is evaluated separately. It is recorded rather than hidden.

## Interface numbers, not device nodes

Entries record **USB interface numbers**, never `/dev/videoN`. Node numbering is allocation-order dependent and changes between boots.

This matters more than it sounds: a multi-sensor camera declares **one set of extension units per VideoControl interface**. Reading the wrong interface's control bitmap sends emitter probing at the wrong sensor. The reference device is exactly such a camera.

## The long-term goal

The durable fix is upstream. Windows Hello is seamless because the **driver** handles the emitter, not because userspace tries harder.

Extending `uvcvideo` with IR classification and MSXU emitter control would make most of this database unnecessary — and the corpus gathered here is precisely the evidence such a patch would require. Where a quirk proves general rather than device-specific, it belongs in the kernel, not here.

That outcome would obsolete this directory. It is the intended one.
