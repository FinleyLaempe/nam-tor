# Nam-tor — IR Face Authentication for Linux

**Design specification**
Date: 2026-08-04
Status: approved for planning

---

## 1. Purpose and conditions

Nam-tor is an infrared face authentication system for Linux: a privilege-separated
daemon, a PAM module, and native GNOME and KDE integration. It replaces Howdy.

The project owner set four conditions. They are treated as binding acceptance gates,
not aspirations. Each is restated as a measurable criterion in §12.

1. **Zero configuration.** Hardware is auto-detected. Nothing requires hand-editing.
2. **At least as fast as Windows Hello.**
3. **At least as accurate as Windows Hello.**
4. **Complete scope.** No feature is dropped for schedule reasons; features are
   sequenced by risk, not cut.

### 1.1 On guarantees

These conditions cannot be guaranteed in advance. Speed and accuracy are
measurements, not derivations, and four unknowns (§13) resolve only against
physical hardware.

What this specification provides instead: every condition is expressed as a
numbered gate with a stated measurement procedure and a pass threshold. The
project either produces evidence that a gate passes, or the corresponding claim
is not made. No gate is satisfied by argument.

This is the strongest honest form of the requested guarantee, and it is
deliberately stronger than what any comparable Linux project currently offers,
none of which publish measured error rates at all.

### 1.2 Reference hardware

Primary development and validation target:

| Property | Value |
|---|---|
| Machine | ASUS ROG Flow X16 (GV601VI) |
| OS | Arch Linux, systemd 261, kernel 7.1.x |
| Desktop | GNOME on Wayland (GDM) |
| CPU | Intel Core i9-13900H, 14C/20T, AVX2 + AVX-VNNI, no AVX-512, no AMX |
| iGPU | Intel Iris Xe (`/dev/dri/renderD128`) |
| dGPU | NVIDIA RTX 4070 Laptop (`/dev/dri/renderD129`) |
| NPU | None (Raptor Lake-H) |
| TPM | TPM 2.0 (`/dev/tpmrm0`), tss2 stack present |
| Secure Boot | Disabled (setup mode) |
| Camera | Sonix `3277:0018`, USB `3-8`, UVC 1.00 |

Camera detail, read from USB descriptors:

| Interface | Node | Format | Notes |
|---|---|---|---|
| if1 | `/dev/video0` | `{32595559-…}` YUY2 + MJPEG, up to 1920×1080 | Colour sensor |
| if1 meta | `/dev/video1` | `UVCH` | UVC payload header metadata |
| if3 | `/dev/video2` | `{00000032-0002-0010-8000-00aa00389b71}` L8, 640×360@30 | **IR sensor** |
| if3 meta | `/dev/video3` | `UVCH` | UVC payload header metadata |

Extension units. The device exposes **two VideoControl interfaces, one per
sensor**, and each declares both extension units. Parsed from the descriptor
blob on 2026-08-04:

| Interface | Unit | GUID | `bmControls` | Reading |
|---|---|---|---|---|
| if0 (colour VC) | 3 | `28f03370-6311-4a2e-ba2c-6890eb334016` | `bf` | Sonix vendor XU, 7 controls — belongs to the **colour** sensor |
| if0 (colour VC) | 4 | `0f3f95dc-2632-4c4e-92c9-a04782f43bc8` | `00 00` | MSXU, no controls advertised |
| **if2 (IR VC)** | **3** | `28f03370-…` | **`00`** | Sonix vendor XU, **no controls advertised on the IR sensor** |
| **if2 (IR VC)** | **4** | `0f3f95dc-…` | **`20 01`** | **MSXU with controls advertised** — bit 5 and bit 8 |

This is the most consequential detail in the table, and it is easy to get
wrong: a truncated `lsusb` reading attributes the colour sensor's `bf` to the
IR sensor and concludes the vendor XU is the emitter path. The descriptor blob
says otherwise.

Extension unit `bmControls` bit *n* corresponds to control selector *n+1*, so
`20 01` advertises **selectors 6 and 9 on unit 4** of the IR interface. Those
are the primary emitter candidates.

Two consequences:

1. **MSXU is the primary path on this device, not the fallback.** The tier-1
   silent probe of §4.10.1 is therefore expected to succeed on the reference
   hardware, which means gate G1.4 — zero user interaction — is achievable on
   the development machine itself.
2. The IR sensor's vendor XU advertising `00` does not prove it is inert;
   vendors under-declare routinely. It remains a tier-2 fallback, behind
   consent.

There is **no depth stream**: the device advertises exactly two format GUIDs
across all interfaces, neither of which is a depth format, and there is no second
IR sensor for stereo. Liveness must therefore be achieved without depth. This
matches the sensor class Windows Hello itself runs on this machine.

The IR node exposes only `region_of_interest_*` and a read-only `privacy`
control. No exposure, no gain, no emitter. **The kernel provides nothing usable
for this device**; all control must go through raw UVC extension unit ioctls.

---

## 2. Reference points

### 2.1 Windows Hello

| Metric | Requirement |
|---|---|
| FAR | 1/100,000 (0.001%) |
| FRR | < 5% |
| PAD | Required, validated per certified sensor |

Hello achieves consistent hardware behaviour through mechanisms Linux lacks:

1. **UVC 1.5 standardised IR/depth format GUIDs**, so IR nodes are self-identifying.
2. **A per-device driver metadata store** shipped by Microsoft, providing tuned
   parameters per certified model.
3. **MSXU**, a standardised extension unit for emitter and face-auth control.
4. **WHQL certification**, forcing vendor compliance.

Nam-tor can reach (1) and (3) directly — the signals exist in USB descriptors on
this hardware. It has no equivalent to (2) or (4), which is the structural gap
the quirks database (§4.6) exists to close.

### 2.2 Howdy

Howdy's deficiencies define much of this design:

| Deficiency | Nam-tor response |
|---|---|
| dlib inference as root inside the PAM process | Sandboxed unprivileged worker (§3) |
| Auth-bypass CVEs from privileged monolith | Four-process privilege separation (§3) |
| Plaintext templates on disk | TPM2-sealed, AEAD-encrypted (§7) |
| Manual device selection | Descriptor-based auto-detection (§4.1) |
| Single-frame enrollment | 25-frame quality-filtered enrollment (§6.2) |
| Global magic threshold, never validated | Per-user normalisation, measured operating points (§6.6) |
| No liveness | Analytic + learned PAD (§6.4) |
| Blocks the password prompt | Instant keypress fallback (§8.4) |
| No published error rates | Eval harness as primary deliverable (§10) |

---

## 3. Architecture

Four processes, three trust levels. The design principle: a defect in the
most-exposed code must not reach the most-valuable data.

```
 ┌─ root ──────────────┐        ┌─ user session ────────────┐
 │ sudo / login / GDM  │        │ namtor-gtk  |  KDE KCM    │
 │      │              │        │      │                    │
 │  pam_namtor.so      │        │      │ D-Bus, polkit-gated│
 └──────┼──────────────┘        └──────┼────────────────────┘
        │ AF_UNIX socket               │
        │ SO_PEERCRED verified         │
        ▼                              ▼
 ┌────────────────────────────────────────────────────┐
 │ namtor-daemon      user: namtor, groups: video, tss│
 │  owns: sealed templates, policy, TPM handle        │
 │  does: capture orchestration, emitter strobe,      │
 │        score normalisation, decision               │
 └────────────────┬───────────────────────────────────┘
                  │ pipe: frames in → vectors out
                  ▼
 ┌────────────────────────────────────────────────────┐
 │ namtor-worker    user: namtor, seccomp + landlock  │
 │  no filesystem, no network, no IPC but the pipe    │
 │  does: image decode + ONNX inference, nothing else │
 └────────────────────────────────────────────────────┘
```

### 3.1 Rationale

**The worker exists because ONNX Runtime and image decoders are the largest
attack surface in the project** — large C++ parsers consuming attacker-influenced
pixel data. Isolating them means memory corruption there lands in a process with
no filesystem, no network, and no templates. It receives raw frames and returns
float vectors. No other Linux face-auth project does this.

**The daemon is not root.** It requires `/dev/video*` (group `video`) and
`/dev/tpmrm0` (group `tss`). Nothing more. This removes the
"compromise the matcher, own the machine" path.

**PAM uses a raw socket, not D-Bus.** PAM runs inside `sudo` and `login` in
minimal environments where no session bus exists. `AF_UNIX` has no such
dependency and far fewer moving parts in the security-critical path. UIs use
D-Bus because they want signals, introspection, and polkit.

### 3.2 Socket ACL

The authorisation seam. Anyone who can assert *"authenticate user X"* and be
believed has defeated the system.

- `SO_PEERCRED` on every connection.
- Caller must be uid 0, or a member of group `namtor-auth` (for display-manager
  greeters such as GDM, which run as `gdm`).
- Every request carries a daemon-issued nonce; replies are bound to it, so a
  captured success reply cannot be replayed.
- Socket mode `0660`, owner `root:namtor-auth`, created by systemd.
- Requests are serialised; the camera is an exclusive resource.

A permissive ACL here silently voids every other control in this document. It is
covered by dedicated security tests (§11.4).

### 3.3 Components

| Component | Language | Licence | Role |
|---|---|---|---|
| `namtor-hwdb` | Rust | Apache-2.0 | Descriptor parsing, device classification, quirks DB |
| `namtor-capture` | Rust | GPL-3.0 | V4L2 streaming, XU ioctls, strobe sequencing |
| `namtor-pipeline` | Rust | GPL-3.0 | Detect, align, embed, PAD, fusion, decision |
| `namtor-store` | Rust | GPL-3.0 | Template storage, TPM sealing |
| `namtor-worker` | Rust | GPL-3.0 | Sandboxed inference host |
| `namtor-daemon` | Rust | GPL-3.0 | Orchestration, D-Bus, socket service |
| `namtor-pam` | Rust | GPL-3.0 | PAM module (`cdylib`) |
| `namtor-probe` | Rust | Apache-2.0 | Hardware probe + quirks contribution CLI |
| `namtor-cli` | Rust | GPL-3.0 | Enrollment, status, diagnostics |
| `namtor-collect` | Rust | GPL-3.0 | Consent-gated PAD data collection |
| GNOME UI | Rust + GTK4/libadwaita | GPL-3.0 | Settings panel, enrollment wizard |
| KDE UI | QML + C++ KCM | GPL-3.0 | System Settings module |
| `ml/` | Python + PyTorch | GPL-3.0 | Training recipes, eval harness |
| `quirks/` | TOML | Apache-2.0 | Device database |

**Licence split rationale.** GPL-3.0 for the security core keeps forks auditable
and prevents closed derivatives of an authentication stack. Apache-2.0 for
`namtor-hwdb`, `namtor-probe`, and the quirks database so *any* project —
including Howdy — can consume the hardware database. IR camera detection is a
Linux-wide problem; that database is the component most likely to outlive the
rest of this project.

### 3.4 PAM module constraints

Non-negotiable, enforced in CI:

- `panic = "abort"` — unwinding across the C ABI into `sudo` is undefined behaviour.
- No ML dependencies, no ONNX, no image libraries.
- No dynamic linking beyond libc and libpam.
- Hard timeout, default 5 s.
- **Fail-closed**: any error, timeout, or unreachable daemon returns
  `PAM_AUTHINFO_UNAVAIL` and falls through. Never `PAM_SUCCESS`.
- Binary size budget: < 500 KB.

---

## 4. Hardware detection and the IR emitter

### 4.1 Detection: reaching Hello's signal

Windows Hello identifies IR sensors by **format GUID**. The reference camera
advertises `{00000032-0002-0010-8000-00aa00389b71}`; the leading dword `0x32`
is `D3DFMT_L8` / `MFVideoFormat_L8`, Microsoft's 8-bit luminance/IR subtype.
The trailing `-0002-` group deviates from Microsoft's standard media-format
template and is treated as vendor sloppiness; classification keys on the leading
dword, with the full GUID recorded for corpus analysis.

`uvcvideo` collapses this to V4L2 `GREY` and discards the GUID. `GREY` means
"greyscale", not "infrared" — which is precisely why every Linux tool guesses and
Howdy makes users pick a device number by hand.

`namtor-hwdb` therefore reads raw USB descriptors from
`/sys/bus/usb/devices/*/descriptors` (world-readable, no privilege required) and
correlates interfaces back to V4L2 nodes via bus info. This recovers Hello's
exact detection signal.

```
namtor-hwdb
  ├─ parse /sys/bus/usb/devices/*/descriptors
  ├─ classify streaming interfaces by format GUID (L8 → IR, D16 → depth)
  ├─ map USB interface → /dev/videoN via bus info + interface number
  ├─ locate metadata nodes (UVCH) per interface
  ├─ enumerate extension units, MSXU first, then vendor
  └─ match VID:PID[:bcdDevice] against quirks DB; else guided probe
```

Classification order: quirks DB entry → format GUID → heuristic fallback
(single-format greyscale node on a multi-interface camera device). The heuristic
is last-resort and always reported as such in diagnostics.

### 4.2 Emitter discovery

**Windows Hello does not strobe the emitter from userspace.** It asks the camera
to do it: MSXU carries a face-authentication mode control, and once set, the
*firmware* alternates illuminated and clean IR frames and tags them. That is why
Hello has no torn frames, no auto-exposure fight, and no per-device strobe
timing to tune.

Nam-tor therefore tries the same thing first, and only hand-rolls a strobe if
the device cannot do it itself. Four paths, in order:

**Tier 0 — device-driven illumination (what Windows actually does)**

Set the MSXU mode control and let the firmware alternate. Frames arrive already
paired and tagged; §4.4's torn-frame machinery becomes unnecessary, and R2
(auto-exposure hunting) disappears because the firmware owns both the emitter
and the exposure.

Measured on the reference camera, read-only, on 2026-08-04:

| Unit | Sel | Len | Info | CUR | MAX | DEF |
|---|---|---|---|---|---|---|
| 4 (MSXU) | 6 | 9 | `0x03` GET+SET | `01 03 01 00…` | `01 03 03 00…` | `01 03 01 00…` |
| 4 (MSXU) | 9 | 4 | `0x03` GET+SET | `01 00 00 00` | `01 00 00 00` | `00 00 00 00` |
| 3 (vendor) | — | — | — | *no control responds* | | |

Reading of selector 6: byte 1 is fixed at `0x03` across CUR, MAX and DEF — a
**capability bitmask advertising two supported modes**. Byte 2 is the live mode
field, currently `1` with a maximum of `3`. **Mode 2 exists and has never been
selected on this machine.** This is the shape of a mode control, and switching
it is the operation Hello performs.

Selector 9 is boolean, default `0`, currently `1` — something already enabled
it, plausibly the metadata stream.

Exact MSXU control semantics are **not assumed**. Discovery determines them
empirically: for each mode value the device accepts, capture a burst and
measure whether frames alternate in luminance (§4.4). A mode that produces
clean alternation *is* device-driven illumination, whatever it is nominally
called. Behaviour is recorded in the quirks entry rather than inferred from a
specification we cannot verify.

#### Tier 0 confirmed on the reference camera, 2026-08-04

Each accepted mode value was written to selector 6, a 40-frame burst captured,
and the original value restored. Restoration verified after every write.

| `sel 6` payload | Mode | Mean | Alternation | Verdict |
|---|---|---|---|---|
| `01 03 00 …` | 0 | 0.66 | 0.08 | inert |
| `01 03 01 …` | 1 (default) | 0.55 | 0.01 | plain IR, emitter off |
| **`01 03 02 …`** | **2** | **41.02** | **81.78** | **device-driven illumination** |
| `01 03 03 …` | 3 | 0.43 | 0.48 | inert |

Selector 9 produced no luminance change and is unrelated to the emitter.

**Mode 2 makes the firmware strobe its own emitter.** Per-frame means across
40 consecutive frames:

```
  0.4  217.6    0.4  216.8    0.4  115.6    0.0  115.0    0.0   69.7
  0.0   69.3    0.0   54.0    0.0   53.5    0.0   50.0    0.0   49.8
  0.0   46.8    0.0   46.5    0.0   46.1    0.0   45.8    0.0   45.5
  0.0   45.4    0.0   45.4    0.0   45.3    0.0   45.3    0.0   45.3
```

Strict 1:1 alternation, 50% duty cycle, sustained for the whole burst. This is
the mechanism Windows Hello uses, reached from Linux userspace with a single
`SET_CUR` and no elevated privilege.

Consequences, which simplify several later sections:

- **Risk R3 is resolved.** The emitter is controllable, by the standardised
  path, with zero user interaction — so gate G1.4 is satisfiable on this class
  of hardware.
- **§4.4's torn-frame machinery is not needed** when Tier 0 applies. The
  firmware pairs frames; there is no userspace toggle to land mid-exposure. It
  remains the Tier 1 and Tier 2 fallback.
- **Ambient IR indoors is effectively zero** (clean frames read 0.0), so the
  differential is very nearly the illuminated frame itself. Subtraction still
  matters outdoors and under incandescent light, where it is doing real work.
- **The mode persists across stream sessions.** Measured: set once, then three
  successive stream start/stop cycles with no re-set all continued alternating,
  and the control read back `01 03 02 …` each time. The daemon does not need to
  re-issue it per capture — but it should **verify with a `GET_CUR` and re-apply
  if wrong**, which costs one ioctl and covers the cases below.

  Not yet measured: whether the mode survives USB reset or S3. Tracked as R10.
  Because a device that loses it would fail the first unlock after lid-open —
  the most common interaction — §4.10.3's resume reconciliation is required
  regardless of the answer.

**Tier 1 — MSXU direct emitter control**

If no mode yields firmware alternation, use MSXU controls to toggle the emitter
ourselves, driving the strobe from userspace per §4.4. Still silent (§4.10.1),
since MSXU semantics are published.

**Tier 2 — vendor extension unit**

`{28f03370-…}`, unit 3. On the reference IR interface it advertises `00` and no
control responds to `GET_LEN`, but vendors under-declare routinely, so it
remains a fallback. Behind the consent gate, because payload search on a vendor
unit is a genuinely blind write.

**Tier 3 — manual**, from a user-supplied report.

Access privilege, measured: `UVCIOC_CTRL_QUERY` on unmapped MSXU controls
succeeded **as an ordinary desktop user**, with no `CAP_SYS_ADMIN` and no root.
The daemon therefore needs only `video` group membership, not elevated
capabilities.

Discovery always targets extension units on the **IR sensor's own
VideoControl interface**. The colour sensor declares its own instances of both
units with different control bitmaps; probing those would poke the wrong device.

Discovery always targets the extension units **on the IR sensor's own
VideoControl interface**. The colour sensor declares its own instances of both
units with different control bitmaps; probing those would poke the wrong
device.

**Verification is automatic.** `linux-enable-ir-emitter` frequently asks a human
whether the light came on, which is unreasonable for invisible 850 nm emission
and drives users to point phone cameras at their laptops. Nam-tor closes the loop
in software:

```
capture N frames → baseline mean luminance, spatial profile
poke candidate XU control
capture N frames → mean luminance, spatial profile
delta above threshold AND spatially centred → emitter identified
record control selector + payload in candidate quirks entry
```

Runs in seconds, no human judgement, no phone.

### 4.3 Probe safety

Blind writes to vendor extension units can hang a camera or, in the worst case,
reach firmware. Enforced rules:

- Never write a control that has not been successfully `GET_CUR`-read.
- Always round-trip: read current → modify → write → restore on failure or timeout.
- Honour `GET_LEN` and `GET_INFO`; never guess payload length.
- Skip controls whose `GET_INFO` does not report SET support.
- Probing requires explicit user confirmation with the risk stated plainly.
- USB reset recovery path documented and offered on failure.
- Probe actions are journalled so a failed probe is reconstructable.

`linux-enable-ir-emitter` is looser here. Nam-tor is deliberately not.

### 4.4 Strobe timing

Liveness requires matched emitter-on/emitter-off frame pairs. XU writes are USB
control transfers with ~1–5 ms latency and **no frame synchronisation**. At
30 fps (33 ms/frame) a toggle can land mid-frame, producing a torn, partially-lit
frame that corrupts the differential.

The UVC metadata node (`UVCH`) carries per-frame SOF and PTS from the USB payload
header:

```
t_toggle = monotonic now()   → issue emitter toggle
for each frame:
    read UVCH metadata → SOF, PTS
    if frame_start < t_toggle < frame_end → mark Torn, discard
    else                                  → mark emitter state On | Off
```

Torn frames are discarded rather than silently poisoning the differential.

**Fallback** when metadata is unusable: toggle, discard one frame
unconditionally, then capture. Throughput drops to ~7.5 clean pairs/s. Slower,
never wrong.

### 4.5 Auto-exposure interaction — open risk

The IR node exposes no exposure or gain controls, so firmware auto-exposure is in
control. The concern was that AE would hunt against the strobe: a bright frame
triggers exposure reduction, the following dark frame is under-exposed, and the
loop oscillates.

**Measured under Tier 0 on 2026-08-04. AE acts, but benignly.** From the
40-frame burst in §4.2, illuminated frames decay 217.6 → 45.3 and then hold
steady (45.4, 45.4, 45.3, 45.3, 45.3), settling after roughly 15 pairs — about
one second. Two properties make this harmless:

1. **AE affects only the illuminated frames.** Clean frames stay at 0.0
   throughout. The pairing and the differential remain valid during settling.
2. **The first pair has the best signal-to-noise ratio** (217.2), not the worst.
   Early exit (§6.7) therefore benefits rather than suffers.

Design response, replacing the earlier mitigation list:

- **Normalise each differential per pair** rather than assuming a stable
  absolute scale. This absorbs AE drift completely and costs one division.
- **Do not wait for AE to settle.** Waiting would add ~1 s to every
  authentication and put gate G2.1 at risk, for a pair that is *worse* than the
  one available immediately.
- Analytic PAD thresholds (§6.4) must be defined on **normalised** statistics,
  or they will drift with exposure. This is a calibration requirement, recorded
  in the M6 plan.

Residual risk applies to Tier 1 and Tier 2 devices, where userspace drives the
strobe and AE may genuinely oscillate. R2 remains open for those (§13); it is
resolved for Tier 0.

### 4.6 Quirks database

Nam-tor's substitute for Microsoft's per-device driver metadata store. TOML,
in-tree, Apache-2.0, keyed by `VID:PID[:bcdDevice]`:

```toml
[devices."3277:0018"]
name         = "Sonix USB2.0 FHD UVC WebCam (ASUS ROG Flow X16 GV601)"
ir_interface = 3
ir_format_guid = "00000032-0002-0010-8000-00aa00389b71"
rgb_interface  = 1
metadata_node  = true

# Emitter fields below are populated by `namtor-probe` during M1.
# The schema is fixed; the reference values are not yet measured.
[devices."3277:0018".emitter]
method     = "vendor_xu"
guid       = "28f03370-6311-4a2e-ba2c-6890eb334016"
selector   = 0x00        # measured by probe
payload_on = ""          # measured by probe
payload_off= ""          # measured by probe

[devices."3277:0018".strobe]
settle_ms     = 6
use_metadata  = true

[devices."3277:0018".meta]
verified_by = "finley"
verified_on = "2026-08-04"
kernel      = "7.1.4"
```

`namtor-probe` runs discovery, emits a candidate entry plus a redacted report,
and prints the exact `gh pr create` invocation to contribute it.

**This database only works if contributing is frictionless.** `namtor-probe` is
therefore a first-class deliverable, not a developer utility. The reference
machine is entry #1.

### 4.7 Capture abstraction

```rust
trait CaptureSource {
    fn caps(&self) -> SourceCaps;               // spectrum, formats, rates
    fn start(&mut self) -> Result<FrameStream>;
    fn stop(&mut self);
}

struct Frame {
    data: Vec<u8>,
    format: PixFmt,
    width: u32,
    height: u32,
    ts_mono: Instant,
    sof: Option<u16>,          // from UVCH metadata
    emitter: EmitterState,     // On | Off | Torn | Unknown
    quality: Option<FrameQuality>,
}
```

Implementations: `IrUvc`, `RgbUvc`, `DepthUvc` (for Realsense-class hardware),
and **`FileReplay`**, which reads recorded frame sequences from disk.

`FileReplay` is what makes the project CI-testable without hardware and lets
contributors reproduce published error rates exactly. It is built first (§14, M0).

`CaptureSession` owns device acquisition and release. Authentication uses a
short-lived session; presence mode (§9) uses a long-lived one. Same abstraction,
no rework.

### 4.8 Dual-stream bandwidth — open risk

Both sensors are on one USB device sharing one USB 2.0 bus:

| Stream | Bandwidth |
|---|---|
| IR 640×360×1B @30 | ~6.9 MB/s |
| RGB MJPEG 720p @30 | ~3–6 MB/s |
| USB 2.0 practical isochronous ceiling | ~35 MB/s |

Arithmetic permits it. UVC bandwidth negotiation may still refuse, as devices
reserve conservatively and often reject a second concurrent alt-setting
regardless of actual headroom.

**Must be tested in M1.** If simultaneous capture is refused, fall back to
alternating bursts (IR burst, then RGB burst, ~100 ms apart) rather than dropping
fusion. Tracked as risk R1 (§13).

### 4.9 Privacy shutter and LED

The read-only `privacy` control is readable on both nodes. When the shutter is
closed, Nam-tor reports **"camera privacy shutter is closed"** rather than timing
out mysteriously.

The IR emitter is invisible, but the **RGB camera LED illuminates whenever fusion
is active**. This is documented prominently in the README, not a footnote. Users
must never be surprised by their camera light.

### 4.10 Emitter seamlessness

Windows Hello's emitter feels seamless because of five properties. Zero-config
detection (§4.1) delivers only the first. The remaining four are specified here
because each, if missed, produces a visible failure that no amount of accuracy
compensates for.

| Hello property | Mechanism | Nam-tor equivalent |
|---|---|---|
| No user action, ever | Driver plus per-device INF store | Quirks DB, plus **silent MSXU probing** (§4.10.1) |
| Emitter driven per frame by the pipeline | MSXU, firmware-synchronised | Userspace strobe with UVCH pairing (§4.4) |
| Never stuck on | Driver owns the lifecycle | **Lifecycle guarantees** (§4.10.2) |
| Works after suspend | Driver reinitialises | **Resume re-application** (§4.10.3) |
| Works on hotplug | Plug and play | **udev re-detection** (§4.10.4) |

#### 4.10.1 Tiered probing — removing the consent prompt where it is not earned

Spec §4.3 requires explicit user confirmation before probing, because blind
writes to vendor extension units can hang a camera. That prompt is correct for
vendor XUs and *wrong* for MSXU: Microsoft's extension unit has published
control semantics, so writing a known control with a known payload is not a
blind write.

Probing is therefore tiered:

| Tier | Scope | User interaction |
|---|---|---|
| **0 — MSXU device-driven** | Mode control on `{0f3f95dc-…}`; firmware alternates illumination | **None.** What Windows Hello does. |
| **1 — MSXU direct** | MSXU emitter toggle, userspace-driven strobe | **None.** Runs silently at first use. |
| **2 — Vendor XU** | Selector and payload search on vendor GUIDs | Explicit confirmation with the risk stated |
| **3 — Manual** | User supplies parameters from a report | Explicit, documented |

Any camera implementing MSXU therefore reaches working face unlock with **zero
interaction**, matching Hello. The consent prompt survives only where the risk
that justifies it is real.

Tier 1 still obeys every probe safety rule in §4.3 — read before write,
round-trip and restore, honour `GET_LEN` and `GET_INFO`. Silent does not mean
unsafe; it means the operation is well-defined rather than exploratory.

#### 4.10.2 Emitter lifecycle guarantees

An emitter left on after a crash is a defect Hello does not have, and on this
hardware it is invisible — the user cannot see that it happened. Four
mechanisms, in depth order:

1. **RAII restore.** Every write returns a guard that restores the prior value
   on drop, including during unwind (§4.3 rule 2).
2. **Watchdog.** The daemon enforces a maximum continuous on-time, default
   **2 seconds**. Expiry forces the emitter off regardless of pipeline state.
   This is a safety property, not only a cleanliness one: 850 nm emission at
   laptop distance should be duty-cycled, not continuous.
3. **Unit teardown.** The systemd unit carries `ExecStopPost=` invoking an
   emitter-off command, so stop and crash paths both clear it.
4. **Start-up reconciliation.** On start, the daemon forces the emitter off
   **before** anything else, recovering from a predecessor that died without
   cleanup.

Mechanisms 2 and 4 exist specifically because 1 and 3 cannot cover `SIGKILL`
or power loss mid-frame.

#### 4.10.3 Suspend and resume

USB devices commonly reset across S3, discarding extension unit state. Without
handling, **the first unlock after a lid-open fails** — which is the single
most common Windows Hello interaction, and therefore the most damaging
seamlessness defect available to us.

The daemon subscribes to logind's `PrepareForSleep` signal. On resume it
re-runs device reconciliation and re-applies emitter configuration before
accepting authentication requests. An authentication arriving before
reconciliation completes waits, rather than failing.

#### 4.10.4 Hotplug and multiple cameras

A udev rule on UVC device add and remove triggers daemon re-detection, so an
external IR camera works without a restart or a re-enrollment.

When several IR-capable cameras are present, selection order is: the device
named in configuration, then the enrolled device, then the internal device
(non-removable per sysfs), then the first classified IR device. The selected
device is always reported in `namtor-cli status`; selection is never silently
ambiguous.

#### 4.10.5 Always-on emitters

Some cameras energise the emitter whenever the IR stream is open, with no
control to toggle it. On such devices strobe differential is impossible, since
no emitter-off frame can be captured.

Detection is part of probing, and the result is recorded in the quirks entry as
`method = "always_on"`. The consequence is stated honestly rather than hidden:
liveness falls back to the non-differential signals (specular highlight
detection, reflectance texture, and the geometric consistency check of §6.5),
gate G4 is evaluated separately for such devices, and the reduced protection is
surfaced in the UI rather than assumed.

**Measured on the reference camera (2026-08-04):** 60 consecutive IR frames at
640×360 showed mean luminance 0.0–0.5 of 255, with an even/odd frame difference
of 0.01. The device is therefore not always-on, and its IR sensor produces no
usable image without emitter control — which is why R3 is the project's
defining risk rather than merely an important one.

**Important qualification.** That capture was taken with the MSXU mode control
at its default value of `1`. It shows only that the camera does not strobe *in
its default mode* — it does **not** rule out firmware strobing, because Hello's
alternating illumination only begins once face-authentication mode is selected.
Mode 2 has never been tried on this machine.

The idle baseline must therefore be measured **per mode**, not once: capture at
the default, then at each accepted mode value, comparing alternation. Concluding
"no firmware strobing" from a default-mode capture alone would discard the
Tier 0 path — the one that would make this work exactly as Hello does.

### 4.11 Upstream path

Windows Hello is seamless because the **driver** handles the emitter, not
because userspace tries harder. The durable fix for Linux is the same: extend
`uvcvideo` so IR classification and MSXU emitter control are exposed as
standard V4L2 controls, removing userspace XU poking entirely.

Nam-tor therefore treats upstream contribution as a project goal, not an
afterthought:

- `namtor-hwdb` and the quirks database are Apache-2.0 precisely so this work
  is reusable by the kernel community and by other projects.
- The device corpus gathered through §4.6 is the evidence base such a patch
  would require.
- Where a device-specific quirk proves general, it belongs in `uvcvideo`
  rather than in our database.

### 4.11.1 The kernel already has the hooks

Inspection of `uvcvideo` 1.1.1 on kernel 7.1.4 (2026-08-04) found:

```
uvc_msxu_guid                              MSXU GUID is already known
V4L2_META_FMT_UVC_MSXU_1_5                 an MSXU 1.5 metadata format exists
dev->info->meta_format == V4L2_META_FMT_UVC_MSXU_1_5
```

**MSXU is not a foreign concept to the kernel — it is already supported for
metadata.** The metadata format is chosen by a per-device entry in `uvcvideo`'s
device info table, and the reference camera is absent from it: its IR metadata
node reports plain `UVCH`, not the MSXU 1.5 format.

Two consequences:

1. **The upstream contribution is small and precedented.** Adding a device to an
   existing table is an ordinary patch, not a new subsystem. The road is much
   shorter than "teach the kernel about MSXU".
2. **MSXU 1.5 metadata may carry per-frame illumination tagging** — the signal
   Windows Hello uses to pair illuminated and clean frames. If so, enabling it
   for a device supplies frame tagging directly, and §4.4's timing-based
   inference becomes a fallback for devices that lack it rather than the primary
   mechanism.

Tier 0 discovery (§4.2) should therefore check whether the IR metadata node
offers `V4L2_META_FMT_UVC_MSXU_1_5` in addition to `UVCH`, and record the answer
in the quirks entry.

### 4.11.2 Why not write our own driver

Considered and rejected. The evidence against it is the measurement that
prompted the question: `UVCIOC_CTRL_QUERY` on unmapped MSXU controls **succeeded
as an ordinary desktop user**, with no root and no `CAP_SYS_ADMIN`. Access is
not the problem, so a driver would buy access we already have.

What it would cost:

| Cost | Detail |
|---|---|
| **Contradicts "just works"** | An out-of-tree module means DKMS, rebuilds on every kernel update, and either disabling Secure Boot or signing the module. That is a worse setup experience than the manual configuration we are eliminating. |
| **Architecturally backwards** | This project's central security argument is privilege separation — inference confined to a sandboxed process under seccomp and landlock. Adding ring-0 code to an authentication system inverts that. |
| **Unmaintainable at this scale** | Out-of-tree modules track a moving internal ABI. For a solo maintainer with no budget it is the highest-maintenance artifact available. |
| **Undistributable** | No distribution will package an out-of-tree kernel module for a face-authentication tool. |

What it would buy — frame-synchronous emitter toggling — is precisely what
Tier 0 obtains from the *firmware* when the device supports it, and what an
upstream `uvcvideo` patch obtains for everyone when it does not.

**The correct sequence is userspace first, upstream second, and never
out-of-tree.** A credible `uvcvideo` patch needs a device corpus demonstrating
the problem is general rather than one laptop's quirk — and that corpus only
exists once the userspace tool ships and users contribute (§4.6). Writing kernel
code first would mean arriving upstream with no evidence.

A libusb userspace driver, detaching `uvcvideo` entirely, is likewise
unnecessary: it would forfeit V4L2 streaming, metadata, and buffer management to
solve a problem we do not have.

Success here would make parts of this specification obsolete. That is the
intended outcome.

---

## 5. Inference and hardware acceleration

### 5.1 CPU is the correct target

`avx_vnni` provides hardware INT8 dot-product on the reference CPU. INT8
quantisation is therefore the highest-value acceleration available: 2–4×, no
dependencies, works everywhere, and compatible with the strictest sandbox.

Estimated per-stage cost (to be confirmed by the benchmark harness; these numbers
belong in the published eval report):

| Stage | Model class | INT8 / AVX-VNNI |
|---|---|---|
| Detect | YuNet @ 640×360 | ~3–8 ms |
| Align | — | < 1 ms |
| Embed | MobileFaceNet | ~2–4 ms |
| Embed | AdaFace R50 | ~15–25 ms |
| PAD | MiniFASNet-class | ~2–5 ms |

**Capture, not inference, is the bottleneck.** One emitter on/off pair costs
≥66 ms of capture at 30 fps. Inference completes while the camera is still
delivering frames.

### 5.2 Accelerator assessment

| Option | Verdict |
|---|---|
| **CPU + INT8/AVX-VNNI** | **Default, mandatory, always present.** No wake latency, no driver dependency, sandbox-compatible. |
| Intel Iris Xe iGPU | Viable optional EP via OpenVINO. Always powered, no wake cost. Adds a large dependency and widens the worker sandbox for ~10 ms. Low value. |
| NVIDIA RTX 4070 | **Excluded from the auth path.** Under runtime PM it drops to D3cold; waking costs ~1 s and significant power, exceeding the entire latency budget. CUDA runtime and proprietary driver in an authentication path are additionally undesirable. |
| NPU | Absent on Raptor Lake-H. EP abstraction retained; no NPU code shipped. |

### 5.3 Sandbox interaction

Any GPU execution provider requires `/dev/dri/renderD*` plus driver libraries,
directly widening the worker's landlock profile. Therefore:

- CPU EP is default and preserves the strictest profile.
- GPU EPs are opt-in only, and enabling one prints exactly which sandbox
  restrictions are relaxed.
- The sandbox never widens silently to chase performance the system does not need.

Given §5.1, this trade does not pay for authentication. The secure default is also
the fast one.

### 5.4 GPU where it belongs

- **Training** (NIR fine-tune, PAD) on the developer's RTX 5070 Ti — CUDA.
- **Eval harness**: 100k+ impostor comparisons are batch inference — minutes on
  the 5070 Ti versus hours on CPU. This is the difference between running a full
  FAR sweep on every model change and running it monthly.

The CUDA EP ships in `ml/` tooling only. It never appears in the auth daemon.

### 5.5 Warm models, cold camera — and the latency it costs

Cold-starting ONNX Runtime at authentication time would add hundreds of
milliseconds, making the first unlock after boot feel broken.

`namtor-daemon` starts at boot as a systemd service and pre-loads models into the
warm worker, **but opens no camera**. The camera LED stays dark, the privacy
property of §4.9 holds exactly, and inference latency is steady-state from the
first unlock onward. Presence mode changes only the capture lifecycle, never
the model lifecycle.

#### 5.5.1 Camera open is not free — measured

Opening the IR stream costs far more than this design originally assumed.
Measured on the reference camera 2026-08-04, capturing N frames and timing the
whole operation:

| Frames | Wall time |
|---|---|
| 2 | 811 ms |
| 4 | 920 ms |
| 10 | 1353 ms |
| 40 | 3354 ms |

A linear fit gives ~66.9 ms per frame and a **fixed startup cost of roughly
677 ms**: device open, format negotiation, buffer allocation, and stream-on.

**This measurement was taken through subprocess spawn, so it is an upper
bound** — an in-process V4L2 open avoids process creation and binary loading.
The true fixed cost must be re-measured in-process during M2 and recorded here.
But even a fraction of 677 ms is serious: gate G2.1 budgets **500 ms median in
total**, and a cold camera open could consume all of it before a single frame
arrives.

#### 5.5.2 Consequence: warm on intent, not on request

Windows Hello does not open the camera when you ask to authenticate — **it is
already looking** by the time you sit down. The lock screen appearing is a
strong signal that authentication is imminent, and that is the moment to warm
the pipeline.

The D-Bus API therefore separates warming from verifying:

```
org.namtor.Device
  PrepareForAuth()     open the stream, apply the emitter mode, let AE settle
  VerifyStart(user)    score frames that are already arriving
  Release()            close the stream, emitter off
```

Greeters and lock screens call `PrepareForAuth()` when they display, and
`VerifyStart()` when the user is actually present. The expensive part happens
while the user is still reaching for the laptop.

This is a **deliberate, visible** relaxation of §4.9's cold-camera property, and
it is bounded rather than blanket:

- Warming happens only on an explicit request from a greeter or lock screen,
  never speculatively by the daemon.
- The camera LED behaviour is unchanged for the colour sensor, which is not
  opened for IR-only authentication.
- A warm stream is closed after a timeout (default 30 s) if no `VerifyStart`
  follows, so an abandoned lock screen does not hold the camera open.
- `namtor-cli status` reports whether a stream is currently open and who
  requested it.

If G2.1 proves reachable with a cold open once measured in-process, warming
becomes an optimisation rather than a requirement. That determination belongs to
M2, on real numbers.

### 5.6 Backend abstraction

```rust
trait InferenceBackend {
    fn load(&mut self, model: &ModelSpec) -> Result<Session>;
    fn run(&self, s: &Session, input: Tensor) -> Result<Tensor>;
}
// OnnxCpu       — default, shipped, INT8/AVX-VNNI
// OnnxOpenVino  — optional, Intel iGPU, opt-in, widens sandbox
// OnnxCuda      — ml/ tooling only, never in the auth path
```

---

## 6. Recognition and liveness

### 6.1 Pipeline

```
CaptureSession
  └─ frame pairs (emitter On / Off, torn frames dropped)
       ├─ D = On − Off                     ambient IR cancelled
       ├─ detect face on On frame          better SNR than D
       ├─ 5-pt landmarks → similarity transform → 112×112 crop
       ├─ embed(crop), embed(mirror(crop)) → averaged, L2-normalised
       ├─ PAD: analytic gates on D, then CNN
       ├─ per-user score normalisation
       ├─ quality-weighted multi-frame fusion
       └─ decision
```

### 6.2 Same-spectrum matching

Enrollment and matching both occur in NIR. This is **not** the cross-spectral
NIR-probe-against-VIS-gallery problem that dominates the literature; it is
substantially easier and materially lowers what the NIR fine-tune must achieve.

The RGB path matches RGB against RGB templates. **Fusion occurs at score level.**
An IR embedding is never compared to an RGB embedding.

### 6.3 Models

| Stage | Choice | Rationale |
|---|---|---|
| Detect | **YuNet** primary, SCRFD-500MF alternate | YuNet is permissively licensed and small, which matters for a project shipping no encumbered weights. Both sit behind the backend trait; the eval harness selects. |
| Align | 5-pt similarity transform | Standard, sufficient, no additional model. |
| Embed | **AdaFace R50** primary, ArcFace R50 baseline | AdaFace's quality-adaptive margin targets low-quality inputs, which 640×360 IR crops are. Highest-value model decision available. |
| PAD | Analytic gates, then MiniFASNet-class trained in-house | Pretrained PAD weights are RGB-trained and near-useless on differential images. |

Backbone size: R50 by default. R100 is available and evaluated, but doubles cost
for less gain than the §6.6 levers. Promoted only if the ablation table justifies it.

### 6.4 Liveness — physics first

`D = On − Off` cancels ambient IR (sunlight, incandescent lighting, heaters).
Three separable signals result:

1. **Screen replay collapses.** LCD and OLED panels emit essentially no 850 nm and
   reflect specularly. Under strobe, a screen yields a near-zero differential — it
   emits rather than reflects. This is the strongest single discriminator available
   and it requires no training data.
2. **Printed photographs are weak in IR.** Most printer inks are substantially
   IR-transparent, so colour prints read washed-out or blank. Skin exhibits
   subsurface scattering that paper does not, producing a distinct spatial
   reflectance profile.
3. **Inverse-square falloff yields crude depth.** Illumination falls as 1/d². A
   flat photograph illuminates uniformly; a real face has a nose nearer than its
   ears, producing a measurable radial intensity gradient. This partially recovers
   the class of signal a depth camera would provide.

**Analytic PAD** implements these as explicit statistical gates on `D`: total
differential energy, spatial gradient consistency against a face model, specular
highlight detection, and radial falloff regression. These require **zero training
data** and make verifiable claims about what they detect and why.

**Learned PAD** (MiniFASNet-class, trained on `D` from `namtor-collect` data)
handles residual hard cases the analytic gates cannot separate.

**Stated limitation:** a well-constructed 3D mask with skin-like IR reflectance
defeats all three signals, as does an IR-reflective print calibrated to 850 nm.
This appears in the README under an explicit threat-model heading. Windows Hello
shares this weakness on IR-only sensors.

### 6.5 Dual-camera fusion

Three independent checks. The third has no equivalent in Windows Hello:

1. IR embedding against IR gallery.
2. RGB embedding against RGB gallery.
3. **Cross-sensor geometric consistency.** The sensors are ~2 cm apart on one
   bezel, so at typical working distance a real face produces landmark
   correspondences under a stable transform between views. A printed IR photo does
   not yield a matching RGB face in the correct position; a phone displaying a face
   fails the IR path entirely.

Extrinsics are unknown and are never requested from the user: **the transform is
fitted during enrollment** from landmark correspondences across enrollment frames.
Per-device, automatic, no configuration.

**Darkness policy.** RGB is useless in darkness, so the system falls back to
IR-only. **Fusion may only add confidence, never subtract it.** When RGB is
unavailable, IR and PAD confidence requirements increase correspondingly. The bar
is never lowered to compensate, otherwise "attack in a dark room" becomes the
trivial bypass.

### 6.6 Accuracy levers

Ranked by measured value per unit cost. The critical observation: **the largest
wins are free and fast, and come from enrollment and calibration rather than model
size.**

**Tier 1 — free, fast, largest effect**

1. **Enrollment quality.** Most FRR originates in a thin gallery. Capture 25
   frames across poses (±15–20° yaw/pitch), distances, ambient conditions, and
   with and without glasses. **Actively reject** bad frames: variance-of-Laplacian
   blur threshold, minimum face size, pose angle limit, exposure sanity, landmark
   confidence. Retain the 8 most diverse high-quality templates. All cost is at
   enrollment; none at authentication.

2. **Per-user score normalisation.** Rather than a global cosine threshold,
   calibrate per user at enrollment: derive the intra-user score distribution from
   enrollment frames and an impostor distribution against a fixed background
   cohort, then threshold on the normalised score (z-norm/t-norm). Standard
   practice in speaker and face verification; absent from every Linux face-auth
   project. Cost at authentication: one subtraction, one division.

   *Open item:* a cohort derived from licence-restricted datasets is legally
   ambiguous even as bare vectors. Preferred sources are a synthetic cohort or one
   contributed via `namtor-collect` under consent. Tracked as risk R5 (§13).

3. **Quality-weighted embedding fusion.** Average L2-normalised embeddings
   weighted by frame quality, then match once — lower variance than k-of-n voting
   on per-frame scores, at the cost of one matmul.

4. **Flip test-time augmentation.** Embed crop and mirror, average. One extra
   embedding pass (~15–25 ms) for a consistent small gain.

5. **Alignment discipline.** Poor alignment is a leading error source and a free
   win. Correct before considering larger backbones.

**Tier 2 — free, real speed cost, modest gain**

| Lever | Cost | Gain |
|---|---|---|
| R50 → R100 | ~25 → ~50 ms | Real but modest; behind all of Tier 1 |
| AdaFace + ArcFace ensemble | 2× embedding | Small |
| Template aging | free | Slow drift correction over months |

**Tier 3 — data-constrained**

NIR fine-tune (§10.2) and learned PAD (§10.3). Free in compute, constrained by
dataset licensing and collection.

### 6.7 Multi-frame consensus and early exit

Frames are expensive; inference is cheap. Latency is therefore optimised by
**early exit**:

```
pair 1 → high-confidence match AND all PAD gates pass → ACCEPT   (~110 ms)
otherwise → escalate to full burst, up to 5 pairs               (~450 ms)
```

The common case — the enrolled user in normal conditions — resolves in one pair.
Difficult cases spend more time. Median latency stays low without sacrificing
tail accuracy.

**Temporal coherence** is required across a burst: embeddings must remain mutually
close. A replayed video with cuts, or a photo swapped mid-burst, breaks coherence.

Two signals deliberately **not** used as hard gates:

- **Micro-motion.** Real faces move involuntarily, but a hand holding a photograph
  also shakes. Low weight only.
- **Blink detection.** Blinks occur roughly every 4 s; requiring one damages both
  latency and FRR. Windows Hello does not require it. Available as an opt-in
  paranoid-mode addition.

### 6.8 Decision logic

Two rules carry most of the security weight:

- **PAD is a gate, not a term.** A spoof detection vetoes outright. It must never
  be outvoted by a strong match score — precisely what a naive weighted sum would
  permit, since a successful spoof produces a very strong match by construction.
- **Thresholds derive from measured data.** Three named operating points, each
  labelled with its measured FAR/FRR from the eval harness:

| Mode | Target |
|---|---|
| Paranoid | FAR ≤ 1e-6 |
| **Balanced (default)** | FAR ≤ 1e-5, FRR ≤ 3% |
| Convenient | FAR ≤ 1e-4, FRR ≤ 1% |

Users select a named point. **Raw thresholds are not exposed in the GUI.** A user
who types `0.4` into a box has silently disabled their own security.

### 6.9 Template aging

Enrollment captures multiple poses and conditions into a gallery. On
high-confidence authentication, a new embedding may join it, tracking beard
growth, glasses, and weight change.

This creates a **template-poisoning path**: an attacker who marginally passes
could, across repeated sessions, walk the gallery toward their own face.
Mitigations:

- Update only on high-confidence matches, well above the authentication threshold.
- Bound cumulative drift from the original enrollment anchor.
- **Original enrollment templates are immutable and permanently retained.** Aged
  templates can never authorise alone.
- Aging is opt-out in the GUI and displays its last update time.

---

## 7. Template storage and trust model

### 7.1 Decision

TPM2-sealed with graceful fallback. Templates are unrotatable biometric data, and
embedding-inversion techniques can reconstruct an approximate face from an
embedding, so they are treated as secrets rather than as derived data.

### 7.2 Format

```
TemplateRecord {
    version: u16,
    uid: u32,
    spectrum: Ir | Rgb,
    model_id: String,          // e.g. "adaface_r50_nirft_v3"
    embedding: [f32; 512],
    quality: f32,
    pose_bucket: u8,
    created_at: i64,
    is_anchor: bool,           // original enrollment, immutable
}
```

Per-user container at `/var/lib/namtor/templates/<uid>.sealed`:

- AES-256-GCM.
- **AAD binds uid and model_id**, preventing template transplant between users or
  reuse across model versions.
- Key sealed to TPM2.

### 7.3 TPM2 sealing policy

**Default: TPM-bound with no PCR policy.** This protects against offline disk
theft — the templates are unreadable on any other machine — while surviving kernel
and firmware updates without user intervention.

PCR binding is available opt-in, with a documented re-seal flow after updates.
Honest assessment: with Secure Boot disabled (as on the reference machine), PCR
sealing resists offline attack but not an adversary who boots their own kernel,
so its marginal value is limited. When Secure Boot is enabled, binding to PCR 7 is
offered and recommended.

**Fallback when no TPM exists:** root-only AEAD-encrypted files with a key stored
under `/var/lib/namtor/`, mode `0600`. Weaker, portable, still well ahead of
Howdy's plaintext. The active mode is always reported in `namtor-cli status`;
degraded protection is never silent.

### 7.4 Runtime handling

- The unsealed key lives in `mlock`ed memory, never swapped.
- `MADV_DONTDUMP` on key pages; core dumps disabled for the daemon.
- Keys zeroised on exit and on session teardown.
- Templates are decrypted per authentication and zeroised after.

### 7.5 Backup and portability

Sealed templates are **deliberately not exportable**. They are device-bound by
design. Moving to a new machine requires re-enrollment. This is stated in the UI
at enrollment time so it is never a surprise.

### 7.6 Root compromise

An attacker with root can simply re-enroll their own face. No storage design
prevents this. Root compromise is explicitly out of scope (§12.2), and the
documentation says so rather than implying protection that does not exist.

---

## 8. PAM integration

### 8.1 Module

`pam_namtor.so`, `auth` type. Installed to the distribution's PAM module
directory. Configuration via drop-ins under `/etc/pam.d/`; the installer never
silently rewrites existing stacks.

Recommended default posture:

| Service | Default |
|---|---|
| Screen unlock (`gdm-password`, `kde`, `swaylock`, `hyprlock`) | Enabled |
| Graphical login | Enabled |
| `sudo` | Opt-in |
| `su` | Opt-in |
| **`sshd`** | **Refused** |
| `polkit-1` | Opt-in |

### 8.2 Remote authentication is refused

Face authentication over SSH is meaningless and dangerous: the camera is at the
*server*, not the client. `pam_namtor` hard-fails when `PAM_RHOST` is set or
`PAM_TTY` indicates a remote session, regardless of configuration. This is not
user-configurable.

### 8.3 Failure modes

Every one returns `PAM_AUTHINFO_UNAVAIL` and falls through to the next module,
with a distinct journal reason:

| Condition | User-visible message |
|---|---|
| No camera present | "No infrared camera found" |
| Camera in use by another application | "Camera is busy" |
| Privacy shutter closed | "Camera privacy shutter is closed" |
| No enrollment for this user | (silent — fall through) |
| Daemon unreachable | "Face unlock unavailable" |
| TPM unavailable, templates sealed | "Face unlock unavailable" |
| Timeout | "Face not recognised" |
| PAD veto | "Face not recognised" |
| Rate-limited | "Too many attempts" |

PAD vetoes and match failures are **deliberately indistinguishable to the user**,
so the system does not tell an attacker which signal caught them. The journal
records the true reason.

### 8.4 Instant fallback

Howdy is widely disliked for blocking the password prompt while it stares at the
user. Nam-tor polls stdin during face authentication; **any keypress immediately
aborts face auth and falls through to password entry.** The keystroke is passed
through to the password prompt, so typing simply works.

This is a hard requirement, covered by an integration test.

### 8.5 Rate limiting

Presentation attacks are cheap to repeat. Failures are counted per uid with
exponential backoff, persisted across process lifetimes in the daemon, with a
lockout threshold that falls back to password-only for a cooldown period.
Successful password authentication clears the counter.

### 8.6 Audit

Every attempt is journalled with uid, service, result, reason code, latency, and
operating point. **No biometric data, no embeddings, no images are ever logged**,
at any log level.

---

## 9. Desktop integration

### 9.1 D-Bus API

System service `org.namtor.Manager`, deliberately shaped to resemble `fprintd`
so existing tooling and mental models transfer:

```
org.namtor.Manager
  ListDevices() → [ObjectPath]
  GetDefaultDevice() → ObjectPath
  Signal: DeviceAdded / DeviceRemoved

org.namtor.Device
  Claim(username)
  PrepareForAuth()                  warm the stream ahead of a request (§5.5.2)
  Release()
  EnrollStart(spectrum) / EnrollStop()
  VerifyStart(username) / VerifyStop()
  DeleteEnrollment(username)
  ListEnrollments(username) → [EnrollmentInfo]
  Property: OperatingPoint, HasTpm, IrNode, RgbNode, EmitterMethod
  Signal: EnrollStatus(progress, quality, guidance)
  Signal: VerifyStatus(result, reason)

org.namtor.Settings
  Property: OperatingPoint, AgingEnabled, FusionEnabled, PerServicePolicy
```

polkit actions: `org.namtor.enroll-self`, `enroll-other`, `delete`, `configure`.
`enroll-other` requires administrator authentication.

### 9.2 GNOME

- **Settings application**: GTK4 + libadwaita, Rust. Enrollment wizard, device
  status, operating point selection, per-service policy, aging control, enrollment
  deletion.
- **Shell extension** (JavaScript, optional): lock-screen feedback — "Look at the
  camera", progress, and failure reason. Without it, PAM conversation messages
  still appear in GDM, just less richly.

### 9.3 KDE

- **KCM** in QML with a thin C++ `KCModule` shim, integrated into System Settings
  under Users / Security.
- Same D-Bus API, so the two UIs share a common client crate (`namtor-client`) for wire types and D-Bus proxies, but neither UI process is privileged — all privileged operations go through the polkit-gated system bus.

### 9.4 Enrollment wizard

Identical flow on both desktops:

1. Device detection and status (with `namtor-probe` invocation offered if the
   device is unknown).
2. Non-image framing guidance: a face-position outline and coarse guidance labels (from the daemon's Guidance enum) are shown during capture. No camera preview is displayed; no pixel data crosses the D-Bus boundary into any UI process.
3. Guided pose capture — centre, then ±yaw, ±pitch, near, far.
4. Optional glasses pass.
5. Optional RGB pass for fusion, with an explicit note that the camera LED will light.
6. Quality filtering, template selection, cross-sensor transform fitting.
7. Immediate verification test, showing measured margin.
8. Statement that templates are device-bound and non-exportable.

Poor enrollment is the leading cause of poor accuracy (§6.6), so the wizard
refuses to complete on insufficient quality rather than producing a bad gallery
silently.

### 9.5 Presence mode

Deferred but architecturally accommodated (§4.7). When enabled:

- Long-lived capture session, IR-only, low frame rate.
- Lock on absence after a configurable interval, via logind.
- **Opt-in only**, with an explicit statement that the camera is then in continuous
  use, and a persistent session indicator.

Wake-on-approach is a later addition on the same foundation.

---

## 10. Training and evaluation

Python, PyTorch, under `ml/`. **Never shipped to users.** No dataset and no
derived weights are committed to the repository.

### 10.1 Licensing posture

Essentially every usable face dataset is research-licensed and
non-redistributable — CASIA NIR-VIS 2.0, LAMP-HQ, Oulu-CASIA NIR-VIS, BUAA-VisNir,
CASIA-SURF, WMCA. MS-Celeb-1M was withdrawn by Microsoft and is avoided entirely.

Therefore:

- **The repository ships recipes, not weights.**
- A first-run fetcher retrieves upstream weights; the user accepts the upstream
  licence. This is the ffmpeg-codec and Howdy-dlib pattern.
- Fine-tuning is a committed, reproducible script. Anyone accepting the dataset
  terms runs it and obtains their own weights.
- Measured results are published without redistributing anything.

Every dataset's licence is verified before use and recorded in `ml/DATASETS.md`.

### 10.2 NIR embedding fine-tune

725 identities (CASIA NIR-VIS 2.0) against MS1M's ~85,000 means full fine-tuning
would overfit and cause catastrophic forgetting. The feature space is therefore
**adapted into, not retrained**, in increasing order of effect:

1. **BatchNorm recalibration.** Freeze all weights, re-estimate BN statistics on
   NIR. Requires very little data, recovers a meaningful part of the domain gap.
   Measured first.
2. **Paired VIS↔NIR distillation.** CASIA NIR-VIS 2.0 contains paired images of
   the same subject in both spectra. Train the NIR encoder so its embedding matches
   the *frozen* VIS encoder's embedding of the paired VIS image. This needs pairs
   rather than identity diversity — precisely the axis on which these datasets are
   adequate — and preserves the VIS feature space.
3. **Pseudo-NIR augmentation** from large RGB sets. Not physically faithful, but
   useful as augmentation.

The §6.2 simplification applies: the requirement is identity separation *within*
NIR, not state-of-the-art general NIR recognition.

### 10.3 PAD training

No public dataset contains this modality. CASIA-SURF and WMCA hold NIR PAD data,
but not `D = On − Off` strobe differentials from this sensor and emitter geometry.

Three factors make it tractable:

1. **PAD is binary, not identity** — far fewer subjects required.
2. **Attack samples are nearly free.** Print a photo, display on phone, tablet, and
   monitor, then vary distance, angle, tilt, and ambient light. One person produces
   thousands of labelled attack frames in an afternoon.
3. **Most of the signal is physics, not learning** (§6.4). Analytic gates require
   no training data at all; the CNN handles only the residual.

**Sequencing consequence:** analytic PAD ships first and is never blocked on data
that does not exist. Learned PAD follows once `namtor-collect` has produced
breadth no single developer could generate.

**Stated limitation:** a PAD model trained on a handful of subjects and one sensor
family will not generalise like Microsoft's, which is validated across certified
hardware with laboratory-scale attack batteries. The eval report states this rather
than obscuring it.

### 10.4 Data collection and consent

`namtor-collect` records labelled bonafide and attack sessions with correct
metadata. Rules:

- **Local-only by default.** Nothing leaves the machine without explicit upload.
- **Explicit informed consent** at collection time — not a checkbox in a licence.
- **Prefer derived features and differential statistics over raw face images**
  wherever they suffice; most PAD signal survives the reduction.
- Per-session identifiers, so withdrawal and deletion are actually possible.
- Contributor-facing documentation explains exactly what is stored and why.

### 10.5 Training cost

Estimates for a single RTX 5070 Ti (16 GB), bf16/AMP:

| Job | Data | Model | Wall time |
|---|---|---|---|
| BN recalibration | ~17k NIR | R50, frozen | seconds |
| NIR fine-tune (CASIA NIR-VIS 2.0) | ~17.5k | AdaFace R50 | ~10–15 min |
| NIR fine-tune (+LAMP-HQ) | ~75k | AdaFace R50 | ~45–60 min |
| PAD model | ~100k frames | MiniFASNet | ~30–40 min |
| Full FAR sweep | 100k+ pairs | inference only | ~1–2 min |

**Compute is not the constraint.** The time cost is experiments: a proper sweep
over margin, frozen-layer depth, distillation weight, and augmentation policy is
20–40 runs, i.e. one overnight job.

Practical note: MiniFASNet is too small to saturate the GPU and will starve on
data loading. Cache the dataset in RAM (~640 MB for 100k 80×80 greyscale frames)
or the measurement is of disk throughput.

The constraint ordering is **data collection ≫ experiment design ≫ GPU time.**

### 10.6 Eval harness

The project's primary evidence artefact. Outputs a committed Markdown report plus
CSV.

**Metrics**

- FAR / FRR across the threshold sweep, with DET curve.
- **APCER and BPCER per attack type**, following ISO/IEC 30107-3. A single averaged
  APCER conceals exactly the attack that works, so per-type reporting is mandatory.
- End-to-end latency distribution: median, p95, p99, separated into capture and
  compute.

**Ablation table** — the deliverable that determines which levers earned their cost:

```
baseline (stock weights, 1 frame, global threshold)     FAR / FRR / latency
  + quality-filtered enrollment                          FAR / FRR / latency
  + per-user score normalisation                         FAR / FRR / latency
  + quality-weighted multi-frame fusion                  FAR / FRR / latency
  + flip TTA                                             FAR / FRR / latency
  + dual-camera fusion                                   FAR / FRR / latency
  + NIR fine-tune                                        FAR / FRR / latency
  + R100 backbone                                        FAR / FRR / latency
  + INT8 quantisation                                    FAR / FRR / latency
```

**INT8 caveat.** Quantisation can degrade *embedding* quality more than
classification accuracy, and the failure mode is subtle rather than a crash.
Post-training static quantisation with a proper calibration set, then FAR/FRR
verification against the FP32 baseline. FP16 ships as fallback if INT8 does not
pay. Never assumed.

**Statistical honesty.** FAR ≤ 1e-5 requires on the order of 100k+ impostor
comparisons and cannot be established from one subject. Impostor scores come from
public NIR datasets under evaluation-only use; genuine scores come from the
developer and volunteers. Subject count and confidence intervals are reported
alongside every rate. A number without its sample size is not published.

**Spoof battery** — scripted scoring, documented manual procedure:

| Attack | Status |
|---|---|
| Printed photograph (colour laser) | Evaluated |
| Printed photograph (IR-optimised) | Evaluated |
| Phone screen replay | Evaluated |
| Tablet screen replay | Evaluated |
| Monitor screen replay | Evaluated |
| 2D cutout mask, eye holes | Evaluated |
| 3D mask | **Not evaluated** — cost-excluded, documented as such |

---

## 11. Testing

### 11.1 Unit

Per crate, Rust. Descriptor parsing against captured descriptor fixtures from
multiple devices. Strobe pairing logic against synthetic metadata sequences.
Score normalisation against known distributions.

### 11.2 Integration

Driven by `FileReplay` (§4.7) — the full pipeline runs in CI with no camera.
Recorded sessions serve as fixtures; small consented fixtures are committed, larger
sets distributed out-of-band with checksums.

Any published error rate must be reproducible by a contributor from committed
fixtures plus the documented dataset acquisition steps.

### 11.3 Hardware-in-the-loop

Tagged, run on developer machines with real cameras: emitter discovery, strobe
timing, dual-stream bandwidth, AE behaviour, end-to-end latency. Results feed the
quirks database.

### 11.4 Security

- Socket ACL: connection attempts from unauthorised uids and groups must be
  rejected; fuzzing of the request protocol.
- Nonce replay: a captured success reply must not authenticate a second time.
- PAM fail-closed: fault injection on every §8.3 condition asserts
  `PAM_AUTHINFO_UNAVAIL` and never `PAM_SUCCESS`.
- Remote refusal: `PAM_RHOST` set must fail regardless of configuration.
- Worker sandbox: attempted filesystem, network, and syscall escapes must fail.
- Keypress fallback: asserted by integration test (§8.4).
- No biometric data in logs at any level: asserted by log scanning in CI.

### 11.5 Reproducibility

Pinned toolchain and dependency versions, fixed seeds, dataset manifests with
checksums. The eval report records the exact commit, model hashes, and hardware it
was produced on.

---

## 12. Acceptance gates

The project owner's conditions, restated as falsifiable criteria. Each states its
measurement procedure. A claim is made only when its gate passes with published
evidence.

### G1 — Zero configuration

- **G1.1** On a device present in the quirks database, enrollment completes with no
  manual configuration and no file editing. *Measured:* clean-install integration
  test on reference hardware.
- **G1.2** On a device absent from the database, `namtor-probe` identifies the IR
  node and a working emitter control automatically, without human judgement about
  whether a light illuminated. *Measured:* success rate across contributed devices;
  target ≥ 80% of UVC IR cameras with a vendor or MS extension unit.
- **G1.3** Every failure mode in §8.3 produces a specific, actionable message. No
  unexplained timeouts. *Measured:* fault-injection test coverage of the table.
- **G1.4** On a device implementing MSXU, emitter configuration completes with
  **zero user interaction** — no prompt, no wizard, no confirmation (§4.10.1).
  *Measured:* integration test asserting the tier-1 path issues no prompt and
  reaches a working emitter.
- **G1.5** The emitter is off within **200 ms** of an authentication attempt
  ending, and is off after daemon stop, daemon `SIGKILL`, and system resume
  (§4.10.2). *Measured:* hardware-in-the-loop luminance check after each of the
  four teardown paths.
- **G1.6** The first authentication after resume from S3 succeeds without user
  intervention (§4.10.3). *Measured:* scripted suspend/resume cycle on reference
  hardware, repeated 20 times, requiring 20 successes.

G1.6 is called out separately because lid-open is the most common interaction
in the product, and the failure it guards against is invisible in every test
that does not actually suspend the machine.

### G2 — Speed

- **G2.1** Median end-to-end authentication latency ≤ 500 ms, measured from PAM
  invocation to decision, on reference hardware at the Balanced operating point.
- **G2.2** p95 ≤ 1000 ms.
- **G2.3** First unlock after boot is within 20% of steady-state latency (validates
  §5.5).
- *Measured:* latency distribution in the eval report, capture and compute reported
  separately.

Windows Hello's practical lid-open-to-desktop time is on the order of 1–2 s, of
which recognition is a part. G2 targets the comparable recognition path and is set
to be competitive rather than merely adequate.

### G3 — Accuracy

- **G3.1** FAR ≤ 1e-5 at FRR ≤ 3%, at the Balanced operating point. Hello's
  requirement is FAR 1e-5 at FRR < 5%; G3.1 is deliberately stricter on FRR.
- **G3.2** Reported with sample size and confidence intervals. Impostor comparisons
  ≥ 100k.
- *Measured:* DET curve and operating-point table in the eval report.

### G4 — Presentation attack resistance

- **G4.1** APCER ≤ 5% for printed photograph, IR-optimised print, and screen replay
  attacks, at BPCER ≤ 5%.
- **G4.2** Reported **per attack type**, never averaged.
- **G4.3** 3D mask attacks documented as not evaluated.

### G5 — Security posture

- **G5.1** No ML code, ONNX runtime, or image decoder is linked into the PAM module.
  *Measured:* linker output assertion in CI.
- **G5.2** The worker cannot open files, network sockets, or unauthorised syscalls.
  *Measured:* escape tests (§11.4).
- **G5.3** All §8.3 conditions fail closed. *Measured:* fault injection.
- **G5.4** No biometric data appears in logs at any level. *Measured:* CI log scan.
- **G5.5** Templates are unreadable when the storage volume is mounted on other
  hardware (TPM mode). *Measured:* manual procedure, documented.

### 12.1 Threat model — in scope

- Presentation attacks: printed photographs, IR-optimised prints, screen replay,
  2D cutout masks.
- Offline theft of the storage device.
- Unprivileged local attackers attempting template extraction or socket abuse.
- Replay of captured authentication responses.
- Template poisoning via aging.

### 12.2 Threat model — out of scope

Stated explicitly in the README rather than implied away:

- **Root compromise.** An attacker with root can re-enroll. No design prevents it.
- **Evil-maid with physical access and Secure Boot disabled.**
- **Coerced authentication.** A face can be presented while unconscious or under
  duress. Face unlock is a convenience factor, not a defence against a physically
  present adversary.
- **High-quality 3D masks** by a resourced, targeted attacker.
- Attacks against the TPM itself.

---

## 13. Open risks

Each must be resolved by measurement. R1–R3 are M1 blockers because they can
invalidate design sections.

| ID | Risk | Impact if realised | Resolution |
|---|---|---|---|
| ~~**R1**~~ | ~~UVC refuses simultaneous IR + RGB streams~~ | — | **RESOLVED PASS, 2026-08-04.** Concurrent capture succeeded. Cost: IR drops from ~25 fps solo to **19.8 fps** under contention, and RGB ran at 12 fps with one dropped buffer. A strobe pair therefore costs ~101 ms during fusion rather than ~66 ms — fusion is available but not free. |
| **R11** | Camera stream open costs ~677 ms fixed (§5.5.1), consuming the entire G2.1 budget | Gate G2.1 unreachable with a cold open; `PrepareForAuth` warming (§5.5.2) becomes mandatory rather than an optimisation | M2, re-measured in-process |
| **R2** | Firmware AE hunts against the strobe (§4.5) | Strobe differential degrades; learned PAD carries more weight; G4 harder | **RESOLVED for Tier 0, 2026-08-04.** AE settles in ~15 pairs and affects only illuminated frames; clean frames hold at 0.0. Per-pair normalisation absorbs it, and the first pair has the best SNR. **Remains open for Tier 1 and Tier 2 devices**, where userspace drives the strobe. |
| ~~**R3**~~ | ~~Neither MSXU nor vendor XU yields emitter control~~ | — | **RESOLVED PASS, 2026-08-04.** MSXU unit 4 selector 6 mode 2 makes the firmware strobe its own emitter: strict 1:1 alternation, illuminated 81.9 vs clean 0.13. Reached with one `SET_CUR`, no elevated privilege, no user interaction (§4.2). |
| **R4** | INT8 degrades embedding quality beyond tolerance (§10.6) | Fall back to FP16; latency budget tightens but G2 likely still met | M0 eval harness |
| **R5** | Normalisation cohort cannot be sourced without licence ambiguity (§6.6) | Synthetic cohort, or contributed cohort via `namtor-collect`; possible accuracy cost | M3 |
| **R6** | PAD generalisation from few subjects is inadequate (§10.3) | G4 met on reference hardware only; claim scoped accordingly | M6, ongoing |
| **R7** | Quirks database fails to attract contributions | G1.2 unverifiable at scale; zero-config claim limited to tested devices | M8, ongoing |
| **R8** | Kernel changes to `uvcvideo` MSXU handling alter XU access | Detection layer needs maintenance | Ongoing |
| **R9** | Some devices energise the emitter whenever streaming, with no off control (§4.10.5) | Strobe differential impossible on those devices; G4 evaluated separately and reduced protection surfaced in the UI | M2, per device |
| **R10** | Extension unit state is lost across S3, and re-application proves unreliable | G1.6 fails; first unlock after lid-open fails, the most damaging seamlessness defect available | M2, via the §4.10.3 resume path |

**R3 is the single greatest threat to the project.** If the emitter cannot be
controlled, strobing is impossible, and liveness reduces to what a static IR image
supports — approximately Howdy's position. M1 exists to answer this in days.

---

## 14. Milestones

Ordered by risk retired, not by feature value. Nothing is cut; everything in this
document ships.

| M | Milestone | Retires |
|---|---|---|
| **M0** | `FileReplay` source, eval harness skeleton, metrics implementation, CI | Nothing is measurable without a measuring stick. Also forces the abstraction that makes the project CI-testable and independently reproducible. |
| **M1** | **Hardware spike:** XU emitter discovery, strobe timing against UVCH metadata, dual-stream bandwidth, AE behaviour | **R1, R2, R3** — the only unknowns that can invalidate the design. Days, not weeks. Immediately. |
| **M2** | `namtor-hwdb`, `namtor-probe`, quirks database, capture layer | G1.1, G1.2 |
| **M3** | Daemon, sandboxed worker, socket protocol, stock weights, CPU inference | Core authentication path; R5 |
| **M4** | Enrollment, template store, TPM sealing, cross-sensor transform fitting | G5.5, §6.6 Tier 1 |
| **M5** | PAM module, socket ACL, failure modes, keypress fallback, rate limiting | G5.1–G5.4 |
| **M6** | Analytic PAD, dual-camera fusion, multi-frame consensus, early exit | G2, G4; R6 |
| **M7** | GNOME settings app and enrollment wizard, then KDE KCM, then Shell extension | Desktop integration |
| **M8** | `namtor-collect`, consent flow, community quirks contribution | R7; data for M9 |
| **M9** | NIR fine-tune, learned PAD, ablation table, published eval report | G3, G4 at full strength |
| **M10** | Presence mode, depth-capable sensor path, additional backbones | Remaining scope |

M1 is scheduled first among implementation work. Discovering R1–R3 in month six
rather than week one is the largest avoidable cost in the project.

---

## 15. Repository layout

```
Nam-tor/
├── Cargo.toml                    workspace
├── crates/
│   ├── namtor-hwdb/              Apache-2.0
│   ├── namtor-capture/
│   ├── namtor-pipeline/
│   ├── namtor-store/
│   ├── namtor-worker/
│   ├── namtor-daemon/
│   ├── namtor-pam/
│   ├── namtor-probe/             Apache-2.0
│   ├── namtor-collect/
│   └── namtor-cli/
├── ui/
│   ├── gnome/                    GTK4 + libadwaita (Rust)
│   ├── gnome-shell-extension/    JavaScript
│   └── kde/                      QML + C++ KCM
├── ml/
│   ├── DATASETS.md               licence status per dataset
│   ├── recipes/                  BN recal, distillation, PAD training
│   ├── eval/                     FAR/FRR, APCER/BPCER, ablation, latency
│   └── reports/                  committed eval reports
├── quirks/                       Apache-2.0 device database
├── fixtures/                     consented FileReplay sessions
├── packaging/                    PKGBUILD, COPR spec, deb
└── docs/
    ├── threat-model.md
    ├── architecture.md
    └── superpowers/specs/
```

---

## 16. Summary of decisions

| Decision | Choice |
|---|---|
| Integration point | PAM, with a D-Bus service for UIs (fprintd architecture) |
| Language | Rust core, Python for ML tooling, native UI per desktop |
| Process model | Four processes, privilege-separated, sandboxed inference worker |
| Detection | USB descriptor parsing (format GUID), not V4L2 heuristics |
| Emitter | MSXU first, vendor XU fallback, automatic optical verification |
| Emitter seamlessness | Tiered probing — MSXU silent, vendor XU consented; watchdog, teardown, and start-up reconciliation; resume re-application; udev hotplug |
| Long-term | Upstream IR classification and MSXU emitter control into `uvcvideo`, making userspace XU poking unnecessary |
| Liveness | Strobe differential, analytic gates first, learned PAD second |
| Differentiator | Dual-camera RGB+IR fusion with cross-sensor geometric consistency |
| Embedding | AdaFace R50, INT8 on AVX-VNNI, NIR fine-tuned |
| Accuracy strategy | Enrollment quality and per-user normalisation before model size |
| Latency strategy | Early exit; capture dominates, not inference |
| Acceleration | CPU only in the auth path; GPU for training and eval |
| Storage | TPM2-sealed, AEAD, graceful fallback, device-bound |
| Licence | GPL-3.0 core, Apache-2.0 hardware database and probe |
| Evidence | Eval harness is a primary deliverable, not a supporting tool |
