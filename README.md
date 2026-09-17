# Nam-tor

Infrared face authentication for Linux. A privilege-separated daemon, a PAM module, and a desktop-neutral GTK4 settings and enrollment app.

> **Experimental preview.** Do not enable on security-sensitive PAM services.
> No G1-G5 production gate has passed with measured evidence yet. Biometric
> accuracy (FAR/FRR), spoof resistance (APCER/BPCER), TPM2 cross-hardware
> sealing, suspend/resume reliability, and score normalisation are all
> unmeasured or withheld. Face authentication is a convenience factor, not a
> hardening measure. See the [WITHHELD gates](#withheld-gates) list below and
> the [authoritative readiness ledger](docs/evidence/status-audit.md).

**Status: experimental preview.** M0-M9 are complete and committed on the reference hardware (400+ tests green, clippy/fmt/SPDX clean, CI security gates enforced). A production-readiness program is underway to close the remaining gates with measured evidence. No gate claim is published unless evidenced. The authoritative readiness ledger is [`docs/evidence/status-audit.md`](docs/evidence/status-audit.md); the milestone roadmap is [`docs/roadmap.md`](docs/roadmap.md).

---

## Packages

Nam-tor ships as two split packages on Arch, Fedora, and Debian:

| Package | Contents | Depends on |
|---|---|---|
| `namtor` | Daemon, worker, PAM module, CLI, bundled models, systemd units, D-Bus service, polkit policy, lifecycle helper, SBOM | `onnxruntime`, `pam`, `systemd` |
| `namtor-ui` | Enrollment wizard and settings app (GTK4/libadwaita). Desktop-neutral — it is the only UI. | `namtor` (exact version), `gtk4`, `libadwaita` |

The core package is headless-safe: it has no desktop-library dependencies, so a server can install `namtor` alone for daemon + PAM + CLI without pulling GTK.

### Upgrading from a release that had `namtor-gnome` or `namtor-kde`

The desktop package used to be called **`namtor-gnome`**. It is the same
package — GTK4/libadwaita, no GNOME-only runtime dependency — renamed to
`namtor-ui` because it is now the single, desktop-neutral UI.

A rename is not free: no package manager migrates one on its own, and the
two packages own exactly the same four files (`/usr/bin/namtor-enroll-wizard`,
the `.desktop` entry, the AppStream metainfo and the icon). Left installed,
the old one **file-conflicts** with the new one. `namtor-ui` therefore
declares `provides`/`conflicts`/`replaces` (Arch), `Provides:`/`Obsoletes:`
(Fedora) and `Provides:`/`Replaces:`/`Breaks:` (Debian) on `namtor-gnome`,
so a normal upgrade swaps it for you.

Releases up to 0.1.0 also shipped a **`namtor-kde`** package: a Plasma 6 KCM
written in Qt6/QML. It is gone; Nam-tor now has one desktop-neutral GTK4
UI, which runs on Plasma like any other GTK app. It too is **obsoleted, not
just dropped**, because it declared an exact-version dependency on `namtor`:
left installed, it would block core upgrades.

The installed binary, the desktop entry id (`org.namtor.Wizard`) and the
polkit and D-Bus names are unchanged by the rename — only the package name
moved.

If you pinned or held packages and end up with a file conflict or an
unsatisfiable dependency, remove `namtor-gnome` / `namtor-kde` by hand and
upgrade again.

## Setup

Setup is Settings-only. No CLI or root shell is needed:

1. Install the package(s) with your distro's package manager (`pacman -U`, `dnf install`, `apt install`).
2. Open the Nam-tor Settings app.
3. Enroll your face through the enrollment wizard.
4. Choose which PAM services to enable (e.g. `gdm-password`) via the Settings toggle.

PAM services are **off by default** at install time. No PAM config is edited during package installation. The Settings app enables PAM services per-service through the D-Bus `org.namtor.Manager2` interface (polkit-authorized); it adds the display-manager user to `namtor-auth` and inserts the transactional `pam_namtor.so` block for you.

### Per-service opt-in

Each PAM service (`gdm-password`, `sddm`, `sudo`, `su`, `polkit-1`, etc.) is toggled independently. The Settings app classifies every service as `safe`, `risky`, or `unsupported` and refuses to enable unsupported services (e.g. `sshd`, which is refused because `pam_namtor` rejects `PAM_RHOST`). You pick the services you want; nothing is enabled blanket.

### TPM warning

Template storage uses AES-256-GCM with TPM2 sealing where a TPM2 device is available. If no TPM2 is present, storage falls back to a software key and the Settings app shows a visible warning. The fallback is never silent. Cross-hardware TPM2 unseal (moving sealed templates to a different machine) is untested and withheld.

### Bundled models

The package bundles three permissively-licensed ONNX models:

| Model | Purpose | Licence |
|---|---|---|
| YuNet 2023mar | Face detection (5-pt landmarks) | MIT |
| SFace 2021dec | Face embedding (128-d) | Apache-2.0 |
| MiniFASNet PAD | Strobe-differential liveness | Apache-2.0 (self-trained) |

Models are bundled in the package, not fetched at install time. No network access is needed. The daemon validates every model against [`packaging/model-manifest.toml`](packaging/model-manifest.toml) at startup (sha256, tensor contract, architecture, redistribution policy) and fails closed on any mismatch.

### Notices and source availability

The source tree, licence texts, and SBOM (SPDX 2.3 + CycloneDX 1.5) are shipped in the package under `/usr/share/doc/namtor/`. Upstream model licence texts are installed under `/usr/share/licenses/namtor/`. The model source archive is reproducible and tamper-evident ([`packaging/build-model-archive.sh`](packaging/build-model-archive.sh)).

## WITHHELD gates

The following production gates are withheld pending owner-gated or hardware-gated inputs. No claim is made for any of them. See [`docs/evidence/status-audit.md`](docs/evidence/status-audit.md) for the full ledger.

| Gate | What is withheld |
|---|---|
| G1.1-G1.6 | Zero-config, auto-probe, failure messaging, MSXU silence, emitter teardown, S3 first-auth (all require HIL) |
| G2.1-G2.3 | Latency distribution (no production-path measurement) |
| G3.1-G3.2 | FAR/FRR at operating point (no lawful cohort dataset selected) |
| G4.1-G4.3 | APCER/BPCER per attack type (analytic PAD uncalibrated; learned PAD synthetic-only) |
| G5.3 | Full fault-injection matrix on the installed path |
| G5.5 | TPM2 cross-hardware unseal (single TPM machine) |
| R5 | z-norm cohort (no licence-verified dataset selected) |
| R10 | Emitter survives suspend (no real S3 cycle) |
| R4 | INT8 vs FP32 evaluation (Python onnxruntime binding incompatible) |
| S3 | 20x HIL session (no scripted S3 run) |

Packages are unsigned (no signing keys yet). The restricted-model admission policy is implemented but no restricted model is bundled.

---

## What it is

Nam-tor replaces [Howdy](https://github.com/boltgolt/howdy), targeting Windows Hello parity on speed and accuracy while fixing the architectural problems that produced Howdy's authentication-bypass CVEs.

It is designed around four commitments, each expressed as a measurable acceptance gate rather than a claim:

| | Target |
|---|---|
| **Speed** | Median ≤ 500 ms, p95 ≤ 1000 ms |
| **Accuracy** | FAR ≤ 1/100,000 at FRR ≤ 3% — stricter than Hello's own FRR < 5% requirement |
| **Anti-spoof** | APCER ≤ 5% per attack type, reported per type and never averaged |
| **Setup** | Zero configuration. Zero interaction on cameras implementing MSXU. |

**No claim is published unless its gate passes with evidence.** The evaluation harness is a primary deliverable, not a supporting tool — no comparable Linux project publishes measured error rates at all.

## How it differs

**Security architecture.** Four processes across three trust levels. Model inference — the largest attack surface, being big C++ parsers eating attacker-influenced pixels — runs in a worker process under seccomp and landlock with no filesystem, no network, and no access to templates. The worker shares the daemon's UID (`namtor`) and, since the merge into a single `namtor` executable, the same file — the daemon re-execs `/proc/self/exe` as `namtor worker` — but it is still a separate process, and the sandbox denies all file opens after model load. The daemon is not root. The PAM module contains no ML code and fails closed on every path.

**Detection that matches Windows Hello's.** Hello identifies IR sensors by their `MFVideoFormat_L8` format GUID. `uvcvideo` collapses that to V4L2 `GREY` and discards the GUID — and `GREY` means greyscale, not infrared, which is why every Linux tool guesses and Howdy makes you pick a device number by hand. Nam-tor reads the raw USB descriptors from sysfs and recovers the same signal.

**Liveness without depth.** The reference hardware, like most Windows Hello laptops, has no depth sensor. Liveness comes from strobe differential — driving the emitter on and off across frame pairs and subtracting. Screen replay collapses because panels emit almost nothing at 850 nm; prints are weak in IR; and inverse-square falloff across a real face gives a crude depth signal. Analytic gates carry this before any model is trained.

**Dual-camera fusion, which Hello does not do.** Most laptops have both an IR and a colour sensor. Nam-tor cross-checks that both see the same face in the same pose under a transform fitted at enrollment — no calibration step. A printed IR photo does not produce a matching colour face in the right place.

**Accuracy from enrollment, not model size.** Howdy's accuracy problem is not that dlib's ResNet is weak. It is single-frame enrollment, a global magic threshold, no quality filtering, and no score normalisation. Fixing those costs nothing and beats doubling the backbone.

## Documentation

| Document | Contents |
|---|---|
| [`docs/superpowers/specs/2026-08-04-nam-tor-design.md`](docs/superpowers/specs/2026-08-04-nam-tor-design.md) | The specification. Read first. |
| [`docs/architecture.md`](docs/architecture.md) | Navigational map into the spec |
| [`docs/threat-model.md`](docs/threat-model.md) | What Nam-tor defends against, and explicitly what it does not |
| [`docs/roadmap.md`](docs/roadmap.md) | Milestone roadmap and global constraints |
| [`ml/DATASETS.md`](ml/DATASETS.md) | Dataset licensing status — read before touching training data |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | How to contribute, including hardware reports |

## Hardware support

Nam-tor needs a UVC infrared camera. Support depends on reaching the IR emitter, which vendors expose inconsistently:

- **MSXU cameras** (Microsoft's standardised extension unit) work with **zero interaction**.
- **Vendor extension unit cameras** need a one-time automatic probe, gated behind a consent prompt because payload search on a vendor unit is a genuinely blind write.
- Results land in [`quirks/`](quirks/), an Apache-2.0 database any project may use.

`namtor-probe` verifies the emitter optically rather than asking whether a light came on — a question no one can answer about invisible 850 nm emission, and which drives people to point phone cameras at their laptops.

**The durable fix is upstream.** Windows Hello is seamless because the *driver* handles the emitter. Extending `uvcvideo` with IR classification and MSXU emitter control would make much of this project unnecessary, and the quirks database is the evidence base such a patch would need. That is the intended outcome.

## Licence

Split, deliberately. See [`COPYING.md`](COPYING.md).

- **GPL-3.0** for the daemon, PAM module, and UIs — this is authentication software; copyleft keeps forks auditable.
- **Apache-2.0** for `namtor-hwdb`, `namtor-uvc`, `namtor-probe`, and `quirks/` — IR camera detection is a Linux-wide problem, and that work should be reusable by anyone, including Howdy and the kernel.

## Security

Face authentication is a **convenience factor**. It does not defend against a physically present adversary: a face can be presented while you are asleep or under duress. Root compromise defeats any biometric storage design, because an attacker with root can simply re-enroll. **This is an experimental preview: do not enable it on security-sensitive PAM services until the G1-G5 production gates pass.**

See [`docs/threat-model.md`](docs/threat-model.md) for what is in scope and what is not. Report vulnerabilities privately rather than in a public issue.


