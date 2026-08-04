# Nam-tor — Handoff

**For the agent picking this up.** You have no prior context. This file is the entry point. Read it fully before touching anything.

---

## What this is

Nam-tor is an infrared face authentication system for Linux — a privilege-separated daemon, a PAM module, and native GNOME and KDE integration. It replaces Howdy, targeting Windows Hello parity on speed and accuracy.

**Current state: design, planning, and scaffolding complete. Zero implementation.** No source code has been written; that is deliberate and is your job.

```
README.md                  project front door
COPYING.md                 the GPL-3.0 / Apache-2.0 split and why
CONTRIBUTING.md            workflow, non-negotiables, rejected "improvements"
HANDOFF.md                 this file
rust-toolchain.toml        pinned stable + rustfmt + clippy
rustfmt.toml .editorconfig formatting
scripts/check-env.sh       pre-flight check — RUN THIS FIRST
fixtures/descriptors/      pre-captured, validated USB descriptor blob
docs/architecture.md       navigational map into the spec
docs/threat-model.md       what is defended, and explicitly what is not
ml/DATASETS.md             dataset licensing — read before touching training data
docs/superpowers/specs/2026-08-04-nam-tor-design.md            the spec — read first
docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md           11 milestones, deps, gates
docs/superpowers/plans/2026-08-04-nam-tor-m0-eval-harness.md   16 TDD tasks, ready
docs/superpowers/plans/2026-08-04-nam-tor-m1-hardware-spike.md 15 TDD tasks, ready
```

Branch `main`. All commits are documentation and scaffolding.

---

## Start here

```bash
./scripts/check-env.sh
```

It reports what your machine is missing and which milestone each gap blocks. On the reference machine it exits 0 with no blocking items.

**Then execute M1 first, not M0**, despite the numbering — *if* you are at the reference machine. M1 needs physical hardware; M0 is pure software and can be done any time.

R3, the risk that could have invalidated the design, is **already resolved PASS** (below), so M1 is now confirmation plus the two remaining unknowns — R1 (dual-stream bandwidth) and R2 for Tier 1/2 devices — rather than a make-or-break gate.

If you are not at the reference machine, do M0.

Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` on the milestone plan files. They are bite-sized TDD: write the failing test, run it, implement, run it, commit.

**Do not create files the plans instruct creating** — `Cargo.toml`, `.gitignore`, `scripts/check-spdx.sh`, `.github/workflows/ci.yml`, and the `quirks/` files are all M0 Task 1 or M1 Task 15 deliverables. Everything already present was deliberately kept out of the plans' path.

---

## R3 is already resolved — PASS

**The project's defining risk was answered on 2026-08-04, before any implementation.**

`MSXU unit 4, selector 6, mode 2` makes the camera **strobe its own IR emitter**, which is the mechanism Windows Hello uses. One `SET_CUR`, no elevated privilege, no user interaction.

```
mode 0  mean  0.66  alternation  0.08   inert
mode 1  mean  0.55  alternation  0.01   plain IR, emitter off (default)
mode 2  mean 41.02  alternation 81.78   ALTERNATING ILLUMINATION  <-
mode 3  mean  0.43  alternation  0.48   inert

per-frame means under mode 2, 40 consecutive frames:
  0.4 217.6   0.4 216.8   0.4 115.6   0.0 115.0   0.0  69.7
  0.0  69.3   0.0  54.0   0.0  53.5   0.0  50.0   0.0  49.8
  0.0  46.8   0.0  46.5   0.0  46.1   0.0  45.8   0.0  45.5
  0.0  45.4   0.0  45.4   0.0  45.3   0.0  45.3   0.0  45.3
```

Strict 1:1 alternation, 50% duty cycle, sustained. Every control was restored and restoration verified.

Full parameters are in `quirks/devices/3277-0018.toml`, already committed.

**What this changes:**

- Torn-frame handling (spec §4.4) is **unnecessary** on Tier 0 devices — the firmware pairs frames. It remains the Tier 1/2 fallback.
- Gate G1.4 (zero user interaction) is satisfiable on this hardware.
- **The mode is not persistent.** It reverts and does not survive device reset, so the daemon must set it per capture session — which is also why resume handling (§4.10.3) is mandatory, not optional.

### Remaining open risks

| Risk | Question | If it fails |
|---|---|---|
| **R1** | Will UVC allow simultaneous IR + RGB streams? | Dual-camera fusion falls back to alternating bursts. Adds ~100 ms. Fusion is not lost. **Still untested** — `scripts/spike-dual-stream.sh`. |
| **R2** | Does firmware auto-exposure hunt against the strobe? | **Resolved for Tier 0, still open for Tier 1/2.** See below. |

### Auto-exposure, measured under mode 2

Illuminated frames decay `217.6 → 45.3` and then hold steady, settling after ~15 pairs (~1 s). **Clean frames stay at 0.0 throughout.**

Two properties make this harmless, and one design rule follows:

1. AE touches **only** the illuminated phase, so pairing and the differential stay valid while it settles.
2. **The first pair has the best SNR** (217.2), not the worst.

→ **Normalise each differential per pair, and do not wait for AE to settle.** Waiting would add ~1 s to every authentication and put gate G2.1 at risk, in exchange for a pair that is *worse* than the one available immediately. Analytic PAD thresholds must be defined on normalised statistics or they will drift with exposure.

### One algorithm correction worth carrying forward

Classifying illuminated vs clean frames by a **global brightness threshold is wrong.** It nearly misclassified the settled tail above (45.3 against a burst mean of ~35) and would fail outright if AE settled lower. Classify by **frame parity** after verifying the alternation shape via sign-flips in consecutive deltas. M1 Task 14 has this fixed, with the real decay sequence as a test fixture.

### Idle baseline, no control set

```
means: 0.0 … 0.4 … 0.5 (of 255)   alternation=0.01
```

The IR sensor is **black without the emitter** — not degraded, unusable. Recorded in spec §4.10.5. M1 Task 13 scripts this so it is reproducible on other devices; run it **before** emitter probing, since an `always_on` or `firmware_strobed` device changes what probing means. Measure it **per mode**, not once.

M1 Task 4 is a falsifier for the spec's central detection claim: it parses the real USB descriptor blob and asserts an L8 format GUID exists. If spec §4.1 is wrong, that test fails in hour one.

---

## Hardware facts already measured

Do not re-probe. These were read off the reference machine and are recorded in spec §1.2.

**Machine:** ASUS ROG Flow X16 GV601VI, Arch Linux, kernel 7.1.x, systemd 261, GNOME/Wayland (GDM).

**CPU:** Intel i9-13900H, 14C/20T. `avx2`, `avx_vnni`, `f16c`. **No AVX-512** (fused off on Raptor Lake), no AMX. `avx_vnni` is why INT8 inference on CPU is the design's default and why no GPU appears in the auth path.

**GPUs:** Intel Iris Xe (`renderD128`), NVIDIA RTX 4070 Laptop (`renderD129`). **No NPU** — the `Signal processing controller` PCI entries are DPTF and telemetry, not a VPU.

**TPM:** 2.0 present and usable (`/dev/tpmrm0`, tss2 stack). **Secure Boot disabled (setup mode)** — this is why the design seals templates to the TPM with *no* PCR policy by default.

**Toolchain, verified 2026-08-04:** rustc/cargo 1.96.1, Python 3.14.6, v4l-utils 1.32.0, git 2.55.0, `gh` present. Missing but not yet needed: `tpm2-tools` (M4), `jq`, `onnxruntime` system library (M3 — the `ort` crate can fetch its own; Arch has `onnxruntime-cpu`).

**Group membership caveat.** The user is in neither `video` nor `tss`; desktop camera access works via a logind **seat ACL**. That ACL does not extend to system daemons, so the `namtor` system user created in M3 needs **real group membership**. This will not surface until the daemon runs as its own user — expect it, and don't debug it as a permissions mystery.

**Camera:** Sonix `3277:0018`, USB `3-8`, UVC 1.00.

| Interface | Node | Format | Role |
|---|---|---|---|
| if1 | `/dev/video0` | YUY2 + MJPEG, to 1920×1080 | Colour |
| if1 meta | `/dev/video1` | `UVCH` | Metadata |
| if3 | `/dev/video2` | L8 `{00000032-0002-…}`, 640×360@30 | **IR** |
| if3 meta | `/dev/video3` | `UVCH` | Metadata |

**Extension units — read this carefully, it is easy to get wrong.** The device has **two VideoControl interfaces, one per sensor**, and each declares both extension units. Parsed from the committed descriptor blob:

```
if0 unit=3  28f03370-…  bmControls=bf      colour sensor, Sonix vendor XU
if0 unit=4  0f3f95dc-…  bmControls=0000    colour sensor, MSXU — nothing
if2 unit=3  28f03370-…  bmControls=00      IR sensor, Sonix vendor XU — nothing
if2 unit=4  0f3f95dc-…  bmControls=2001    IR sensor, MSXU — SELECTORS 6 AND 9
```

A truncated `lsusb` reading attributes the colour sensor's `bf` to the IR sensor and concludes the vendor XU is the emitter path. **It is not.** An earlier draft of this project made exactly that error.

Extension unit `bmControls` bit *n* maps to selector *n+1*, so `20 01` means selectors 6 and 9 on unit 4 of interface 2.

Always filter extension units by `interface` before probing, or you will poke the wrong sensor.

### Those two controls were probed read-only, 2026-08-04

```
MSXU unit 4, /dev/video2
  selector 6  len=9  info=0x03 [GET,SET]
      CUR 01 03 01 00 00 00 00 00 00
      MAX 01 03 03 00 00 00 00 00 00
      DEF 01 03 01 00 00 00 00 00 00
  selector 9  len=4  info=0x03 [GET,SET]
      CUR 01 00 00 00   MAX 01 00 00 00   DEF 00 00 00 00

Sonix unit 3: no control responds to GET_LEN.
UVCIOC_CTRL_QUERY succeeded as an ORDINARY DESKTOP USER — no root, no CAP_SYS_ADMIN.
```

**Selector 6 is a mode control.** Byte 1 is fixed at `0x03` across CUR/MAX/DEF — a capability bitmask advertising two modes. Byte 2 is the live mode field: currently `1`, maximum `3`. **Mode 2 has never been selected on this machine.**

This matters because **Windows Hello does not strobe the emitter from userspace** — it sets an MSXU mode control and the firmware alternates illuminated and clean frames, tagging them. That is why Hello has no torn frames and no auto-exposure fight.

So M1 Task 14 (`namtor-spike modes`) is the highest-value task in the milestone, and **must run before Task 10's emitter search**. A `TIER-0-PASS` means several spec sections become unnecessary rather than merely satisfied.

**Correction to a conclusion you may read elsewhere:** the 60-frame capture below showing no alternation was taken at the *default* mode. It does not rule out firmware strobing — Hello's alternation only starts once face-auth mode is selected. Measure the idle baseline **per mode**, not once.

### The kernel already knows MSXU

From `uvcvideo` 1.1.1 on kernel 7.1.4:

```
uvc_msxu_guid                     MSXU GUID already known
V4L2_META_FMT_UVC_MSXU_1_5        MSXU 1.5 metadata format exists
```

Your camera's IR metadata node reports plain `UVCH`, because the metadata format comes from a **per-device entry in uvcvideo's info table** and this Sonix isn't in it. MSXU 1.5 metadata plausibly carries the per-frame illumination tagging Hello uses.

**This is why writing our own driver would be wrong** (spec §4.11.2): XU access already works unprivileged, so a driver buys access we have. An out-of-tree module means DKMS, Secure Boot signing, and breakage on every kernel update — a worse setup experience than the manual configuration we are removing, and ring-0 code in a project whose security argument is privilege separation. The correct path is userspace now, a small upstream `uvcvideo` patch later, never out-of-tree — and that patch needs the device corpus the userspace tool collects first.

**No depth stream.** The device advertises exactly two format GUIDs across all interfaces and has no second IR sensor. Liveness must work without depth. This is the same sensor class Windows Hello runs on this machine.

**The IR node exposes only `region_of_interest_*` and a read-only `privacy` control.** No exposure, no gain, no emitter. The kernel gives us nothing; everything goes through raw XU ioctls.

---

## Decisions already made — do not relitigate

Each was reasoned through with the user. If you think one is wrong, say why and ask; don't silently deviate.

| Decision | Choice |
|---|---|
| Integration point | **PAM**, plus a D-Bus service for UIs. The fprintd architecture. Not direct SDDM/GDM integration. |
| Language | **Rust** core (daemon, PAM, worker), **Python** for ML tooling only, GTK4-Rust for GNOME, QML + thin C++ KCM for KDE. |
| Process model | Four processes. Inference runs in a `nobody` worker under seccomp + landlock with no filesystem. This is the fix for Howdy's CVE class. |
| Detection | Parse USB descriptors from sysfs, keying on format GUID. `uvcvideo` discards the GUID; `GREY` means greyscale, not infrared. This is how Hello does it. |
| Emitter seamlessness | **Tiered probing** (spec §4.10.1): MSXU is standardised, so it is probed **silently with no prompt**; only vendor XU brute-force keeps the consent gate. Plus watchdog, teardown, start-up reconciliation, resume re-application, udev hotplug. |
| Long-term | Upstream IR classification and MSXU emitter control into `uvcvideo`. Hello is seamless because the *driver* handles the emitter, not because userspace tries harder. Success would make parts of the spec obsolete — that is intended. |
| Liveness | Strobe differential, **analytic gates first, learned PAD second**. Analytic needs zero training data and already kills screen replay. |
| Differentiator | **Dual-camera RGB+IR fusion** with cross-sensor geometric consistency. Hello is IR-only. Extrinsics are fitted at enrollment, never configured. |
| Acceleration | **CPU only in the auth path.** The dGPU is at D3cold under runtime PM; waking it costs ~1 s, exceeding the whole latency budget. GPU is for training and eval on the user's desktop 5070 Ti. |
| Storage | TPM2-sealed, AEAD, graceful fallback, device-bound and deliberately non-exportable. |
| Licence | **GPL-3.0** core; **Apache-2.0** for `namtor-hwdb`, `namtor-probe`, `namtor-uvc`, and `quirks/` so any project can consume the hardware database. |
| Accuracy strategy | Enrollment quality and per-user score normalisation **before** model size. R100 is worse value than fixing enrollment. |
| Latency strategy | Early exit. **Capture dominates, not inference** — one emitter pair costs ≥66 ms at 30 fps. |

---

## Constraints that must hold

- **No model weights, ever committed.** No datasets committed. Essentially every usable face dataset is research-licensed and non-redistributable. Ship recipes; a first-run fetcher pulls upstream weights under the upstream licence.
- **PAM module:** `panic = "abort"`, no ML deps, no image decoders, < 500 KB, fail-closed on every error path. Enforced in CI.
- **No biometric data in logs at any level.** Asserted by a CI log scan.
- **Probe safety** (spec §4.3): never write an XU control that hasn't been read; always round-trip and restore; honour `GET_LEN`/`GET_INFO`; skip controls without SET support. M1 Task 3 makes these structurally unbypassable rather than merely documented.
- TDD throughout. Commit after every task.

---

## The user's conditions, and how they're encoded

The user asked for zero-config, Hello-or-better speed, Hello-or-better accuracy, and full scope with no timeboxing — and asked for a guarantee.

**A guarantee was not given, deliberately.** Speed and accuracy are measurements, not derivations. Instead each condition became a numbered acceptance gate with a stated measurement procedure and pass threshold (spec §12), under the rule that **a claim is not made unless its gate passes with published evidence.**

| Gate | Threshold |
|---|---|
| G1 | Known device: zero config. Unknown device: automatic probe succeeds ≥80%. Every failure gets a specific message. **G1.4** MSXU devices need zero interaction. **G1.5** emitter off within 200 ms of teardown, and after stop, `SIGKILL`, and resume. **G1.6** first unlock after S3 succeeds, 20 of 20 cycles. |
| G2 | Median ≤ 500 ms, p95 ≤ 1000 ms |
| G3 | FAR ≤ 1e-5 at FRR ≤ **3%** (Hello's own requirement is FRR < 5%) |
| G4 | APCER ≤ 5% per attack type, **reported per type, never averaged** |
| G5 | Security posture, CI-asserted |

Hold this line. If a measurement misses a gate, report the number and scope the claim — do not soften the gate.

Two honest limits already written into the spec, which should not be quietly dropped: FAR 1e-5 cannot be established from one subject (needs ~100k+ impostor comparisons), and a PAD model trained on a handful of subjects will not generalise like Microsoft's. Both are stated in §10.6 and §10.3.

---

## What not to do

- Don't write M2–M10 task plans before M1's measurements land. Their details depend on R1/R2/R3. That's why they're scoped but not decomposed.
- Don't add a GPU execution provider to the auth path to chase latency. The CPU numbers already fit and the sandbox is the point.
- Don't expose raw match thresholds in any GUI. Named operating points only — a user who types `0.4` into a box has silently disabled their own security.
- Don't let PAD become a weighted term in the decision. It is a veto gate. A successful spoof produces a very strong match by construction.
- Don't commit anything to `fixtures/` containing a real face without recorded consent.
- Don't put a consent prompt in front of MSXU probing. The prompt exists because *blind* writes to vendor units are risky; MSXU has published semantics, so prompting there buys nothing and costs the zero-interaction property (G1.4).
- Don't ship emitter control without the watchdog and start-up reconciliation. An emitter stuck on is invisible at 850 nm — the user cannot tell it happened.
- Don't skip the suspend/resume path. Lid-open is the most common interaction in the product, and no test that fails to actually suspend the machine will catch it (G1.6).

---

## Open questions for the user

- **Project name.** `Nam-tor` was taken from the directory name and never confirmed.
- **`git init` was run by the previous session** to commit the design docs, since the directory was not a repository. Confirm that's wanted, and whether a remote should exist.
- Nothing else is blocking. Everything else needed to start M0 or M1 is in the plans.
