# Nam-tor

Infrared face authentication for Linux. A privilege-separated daemon, a PAM module, and native GNOME and KDE integration.

**Status: implementation in progress.** M0-M9 are complete and committed on the reference hardware (400+ tests green, clippy/fmt/SPDX clean, CI security gates enforced). A production-readiness program is underway to close the remaining gates with measured evidence. No gate claim is published unless evidenced. The authoritative readiness ledger is [`docs/evidence/status-audit.md`](docs/evidence/status-audit.md); see also [`HANDOFF.md`](HANDOFF.md) for the cold-start entry point.

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

**Security architecture.** Four processes across three trust levels. Model inference — the largest attack surface, being big C++ parsers eating attacker-influenced pixels — runs in a `nobody` worker under seccomp and landlock with no filesystem, no network, and no access to templates. The daemon is not root. The PAM module contains no ML code and fails closed on every path.

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
| [`docs/superpowers/plans/`](docs/superpowers/plans/) | Milestone roadmap and executable implementation plans |
| [`ml/DATASETS.md`](ml/DATASETS.md) | Dataset licensing status — read before touching training data |
| [`HANDOFF.md`](HANDOFF.md) | Cold-start entry point for a new contributor or agent |
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

Face authentication is a **convenience factor**. It does not defend against a physically present adversary: a face can be presented while you are asleep or under duress. Root compromise defeats any biometric storage design, because an attacker with root can simply re-enroll.

See [`docs/threat-model.md`](docs/threat-model.md) for what is in scope and what is not. Report vulnerabilities privately rather than in a public issue.
