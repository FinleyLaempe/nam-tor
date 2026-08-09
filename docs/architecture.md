# Architecture

A navigational map into the [specification](superpowers/specs/2026-08-04-nam-tor-design.md). The spec is authoritative; this document exists so you can find the right part of it quickly.

## The shape

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
 └────────────────┬───────────────────────────────────┘
                  │ pipe: frames in → vectors out
                  ▼
 ┌────────────────────────────────────────────────────┐
 │ namtor-worker    user: namtor, seccomp + landlock  │
 │  no filesystem, no network, no IPC but the pipe    │
 └────────────────────────────────────────────────────┘
```

Four processes, three trust levels. The organising principle: **a defect in the most-exposed code must not reach the most-valuable data.**

## Why each boundary exists

| Boundary | Reason | Spec |
|---|---|---|
| Worker split from daemon | ONNX Runtime and image decoders are large C++ parsers eating attacker-influenced pixels — the biggest attack surface in the project. Isolated, a memory-corruption bug there lands somewhere with no filesystem, no network, and no templates. | §3.1 |
| Daemon is not root | It needs `/dev/video*` and `/dev/tpmrm0`. Nothing more. This removes "compromise the matcher, own the machine". | §3.1 |
| PAM uses a raw socket, not D-Bus | PAM runs inside `sudo` and `login` where no session bus exists. Fewer moving parts in the most security-critical path. | §3.1 |
| UIs use D-Bus | They have a session and want signals, introspection, and polkit. | §9.1 |

The **socket ACL is the security-critical seam** (§3.2). Anyone who can assert *"authenticate user X"* and be believed has defeated everything else.

## Where to look

| Question | Section |
|---|---|
| How is the IR camera identified? | §4.1 — USB descriptor parsing, format GUID |
| How is the IR emitter found and driven? | §4.2, §4.3, §4.4 |
| Why does emitter setup need no user interaction? | §4.10 — tiered probing, lifecycle, resume, hotplug |
| What runs the models, and where? | §5 — CPU only in the auth path, and why |
| How does recognition work? | §6.1–6.3 |
| How does liveness work without depth? | §6.4 — strobe differential |
| What does dual-camera fusion add? | §6.5 |
| Why is accuracy about enrollment rather than model size? | §6.6 |
| How are templates protected? | §7 — TPM sealing |
| What happens when something fails? | §8.3 — every failure mode and its message |
| How do GNOME and KDE integrate? | §9 |
| How is any of this measured? | §10.6 — the eval harness |
| What must be true to ship a claim? | §12 — acceptance gates |
| What could still invalidate the design? | §13 — open risks |

## The three decisions that shape everything

**1. PAM, not display-manager integration.** One integration point covers `sudo`, `login`, `su`, GDM, SDDM, swaylock, hyprlock, and polkit agents. Direct display-manager integration would be N separate integrations, none reusable. This is fprintd's architecture, and GDM's own `gdm-fingerprint` stack is proof it is the right seam.

**2. Capture dominates latency, not inference.** One emitter on/off pair costs ≥66 ms at 30 fps; embedding costs ~20 ms. This inverts the usual optimisation instinct: the win is **early exit** after one pair on high confidence (§6.7), not a faster model or a GPU.

**3. Accuracy comes from enrollment and calibration.** Quality-filtered enrollment and per-user score normalisation are free and beat doubling the backbone (§6.6). Howdy's accuracy problem was never dlib.

## Component map

| Crate | Role | Licence |
|---|---|---|
| `namtor-hwdb` | Descriptor parsing, device classification, quirks DB | Apache-2.0 |
| `namtor-uvc` | Extension unit access, metadata, strobe timing | Apache-2.0 |
| `namtor-probe` | Hardware probe and contribution CLI | Apache-2.0 |
| `namtor-capture` | V4L2 streaming, capture sources, replay | GPL-3.0 |
| `namtor-pipeline` | Detect, align, embed, PAD, fusion, decision | GPL-3.0 |
| `namtor-store` | Template storage, TPM sealing | GPL-3.0 |
| `namtor-worker` | Sandboxed inference host | GPL-3.0 |
| `namtor-daemon` | Orchestration, D-Bus, socket service | GPL-3.0 |
| `namtor-pam` | PAM module | GPL-3.0 |
| `namtor-cli` | Enrollment, status, diagnostics | GPL-3.0 |
| `namtor-collect` | Consent-gated PAD data collection | GPL-3.0 |

The Apache-2.0 boundary is not arbitrary: those three crates plus `quirks/` solve a Linux-wide problem and should be reusable by anyone, including the kernel. See [`COPYING.md`](../COPYING.md) and spec §4.11.

## Testability

`CaptureSource` has a `FileReplay` implementation that replays recorded sessions from disk, plus a deterministic synthetic generator. **The entire pipeline runs in CI with no camera attached**, and published error rates are reproducible by contributors from committed fixtures.

This is why M0 — the measuring stick — is built before anything it measures.

## Manager3 enrollment contract (v3)

`org.namtor.Manager3` is the redesigned enrollment interface (plan task 2). It
sits beside the frozen v1 (`org.namtor.Manager`) and v2 (`org.namtor.Manager2`)
interfaces on the same bus name and object path. v1 and v2 are preserved
byte-for-byte inside the v3 introspection XML; v1/v2 clients are unaffected.

### Lifecycle

```
                  StartEnrollment(uid, name)
            ┌──────────────────────────────────┐
            ▼                                  │
       ┌─────────┐  StopEnrollment   ┌────────┐│
       │Capturing├──────────────────►│Stopped │├─┐
       └────┬────┘                   └───┬────┘│ │
            │                            │     │ │
            │ ResumeEnrollment           │ ReviewEnrollment
            │                       ┌────┘     │ │
            ▼                       ▼          │ │
       ┌─────────┐            ┌─────────┐     │ │
       │Capturing│            │Stopped  │     │ │
       └────┬────┘            └────┬────┘     │ │
            │                      │          │ │
            │ CommitProfile        │ CommitProfile
            │                      │          │ │
            ▼                      ▼          │ │
       ┌──────────┐          ┌──────────┐     │ │
       │Committed │          │Committed │     │ │
       └──────────┘          └──────────┘     │ │
                                               │ │
     CancelEnrollment (from any state)         │ │
            ┌──────────────────────────────────┘ │
            ▼                                     │
       ┌──────────┐                               │
       │Cancelled │◄──────────────────────────────┘
       └──────────┘
```

States: **Created** (session allocated, capture not started) → **Capturing**
(active capture, reservoir filling) → **Stopped** (capture paused, reservoir
retained) → **Committed** (profile saved) or **Cancelled** (session discarded).
`ResumeEnrollment` transitions Stopped → Capturing. `ReviewEnrollment` is
valid only from Stopped; calling it while Capturing returns `org.namtor.Error.State`.

### Progress signal fields `(ssuuuuuys)`

| Field | Type | Meaning |
|---|---|---|
| `session` | s | Enrollment session id |
| `phase` | s | Current lifecycle phase: "capturing", "stopped" |
| `quality_percent` | u | Capture suitability 0-100. **NOT authentication accuracy.** |
| `diversity_percent` | u | Pose/angle coverage 0-100 |
| `seen` | u | Total captures observed |
| `accepted` | u | Captures accepted into the reservoir |
| `rejected` | u | Captures rejected by quality filtering |
| `quality_bucket` | y | Coarse label: 0=low, 1=medium, 2=high |
| `guidance` | s | Closed enum mapped client-side to a localized string |

`quality_percent` measures how suitable the current capture is for enrollment
(lighting, pose, sharpness), not how well the face will authenticate. Display
it as a progress bar, never as a "security" or "accuracy" percentage.

`guidance` is a closed enum (e.g. "center", "closer", "lighting", "hold_still",
"ready"). The daemon sends the enum value; the client maps it to a localized
human-readable string. The daemon never sends raw biometric feedback.

### ReviewEnrollment return `(sbuuus)`

| Field | Type | Meaning |
|---|---|---|
| `session` | s | Enrollment session id |
| `ready_to_save` | b | Whether the reservoir is sufficient to commit |
| `quality_percent` | u | Capture suitability 0-100 |
| `diversity_percent` | u | Pose/angle coverage 0-100 |
| `accepted` | u | Captures accepted into the reservoir |
| `guidance` | s | Closed enum mapped client-side |

`ReviewEnrollment` is session-scoped (summarizes a stopped enrollment session),
distinct from v1's `ReviewProfile` which is profile-scoped (summarizes a saved
profile).

### Version negotiation

A v3 client negotiates `API_VERSION_V3` (3) via `negotiate_version_v3`. Only
version 3 is accepted; versions 1 and 2 are rejected with
`IncompatibleVersion` — v3 is a new interface, not backward-compatible.
Clients wanting v1 or v2 use `negotiate_version` / `negotiate_version_v2` and
speak those interfaces directly. All three interfaces coexist on the same
object path.
