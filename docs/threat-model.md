# Nam-tor Threat Model

Referenced by spec §12.1 and §12.2. This document states plainly what Nam-tor defends against and, more importantly, what it does not.

**Face authentication is a convenience factor.** It is not a defence against a physically present adversary. Anyone who tells you otherwise is selling something.

---

## In scope

### Presentation attacks

| Attack | Defence | Residual risk |
|---|---|---|
| Printed photograph, colour laser | Strobe differential — printer inks are largely IR-transparent, so prints read washed-out. Plus radial falloff: a flat sheet illuminates uniformly, a face does not. | Low |
| Printed photograph, IR-optimised | Radial falloff and reflectance texture. Subsurface scattering in skin has no paper analogue. | Moderate — a determined attacker can calibrate a print for 850 nm |
| Screen replay (phone, tablet, monitor) | Strobe differential. LCD and OLED emit essentially nothing at 850 nm and reflect specularly, so a screen produces a near-zero differential. Plus dual-camera cross-check. | **Low — this is the strongest discriminator available and it requires no training data** |
| 2D cutout mask with eye holes | Radial falloff, geometric consistency across sensors | Moderate |
| Video replay | Temporal coherence across the multi-frame burst; cuts break it | Low |

Every rate above is a claim until the eval harness measures it. Published figures carry APCER **per attack type**, never averaged — a single averaged number conceals exactly the attack that works.

### Data at rest

| Threat | Defence |
|---|---|
| Storage device stolen or imaged | Templates sealed to the TPM. Unreadable on any other hardware. |
| Template transplanted between users | AEAD additional data binds uid and model identifier |
| Template reused across model versions | Same AAD binding |
| Embedding inversion (reconstructing a face from a template) | Templates are treated as secrets, not derived data: encrypted at rest, `mlock`ed in memory, `MADV_DONTDUMP`, core dumps disabled, zeroised on exit |

### Local attackers without root

| Threat | Defence |
|---|---|
| Unprivileged process asks the daemon to authenticate someone | `SO_PEERCRED` on every connection; caller must be uid 0 or in `namtor-auth` |
| Replay of a captured success response | Daemon-issued nonce bound into every reply |
| Reading templates off disk | Daemon runs as `namtor`, files are not world-readable, contents are sealed |
| Memory disclosure via the inference stack | Inference runs in a worker process under seccomp and landlock with no filesystem, no network, and no template access. The worker shares the daemon's UID (`namtor`) but the sandbox denies all file opens after model load. A memory-corruption bug in ONNX Runtime or an image decoder lands nowhere useful. |
| Brute-forcing presentations | Per-uid exponential backoff with lockout |
| Learning which signal rejected them | PAD vetoes and match failures are indistinguishable to the user; the journal records the truth |

### Template poisoning

Template aging adapts the gallery to beard growth, glasses, and weight change. This creates a genuine attack: someone who marginally passes could, across many sessions, walk the gallery toward their own face.

Defences: updates occur only on high-confidence matches well above the auth threshold; cumulative drift from the enrollment anchor is bounded; **original enrollment templates are immutable and permanently retained**, so aged templates can never authorise alone; aging is opt-out and displays its last update time.

---

## Out of scope

Stated explicitly, because implying protection that does not exist is worse than offering none.

### Root compromise

**An attacker with root can simply re-enroll their own face.** No storage design prevents this — not TPM sealing, not encryption, not anything. If root is compromised, biometric authentication is over, along with everything else on the machine.

### Coerced or unconscious authentication

Your face can be presented to the camera while you are asleep, unconscious, or under duress. Unlike a password, you cannot decline to produce it, and unlike a fingerprint you need not even be touched.

**If this is in your threat model, do not enable face unlock.** This is why `sudo` is opt-in rather than enabled by default, and why the enrollment UI states it.

### Evil maid with physical access

On the reference hardware Secure Boot is disabled, so TPM sealing without PCR binding resists offline disk theft but not an attacker who boots their own kernel on your machine. PCR binding is available opt-in and is genuinely stronger when Secure Boot is enabled.

### High-quality 3D masks

A well-constructed mask with skin-like IR reflectance defeats strobe differential, radial falloff, and reflectance texture alike. Windows Hello shares this weakness on IR-only sensors.

Nam-tor's spoof battery **does not evaluate 3D masks** — the materials cost was excluded from the project budget. This is documented as unevaluated rather than assumed safe, and gate G4.3 requires it stay documented.

### Attacks against the TPM itself

Bus interposition, fault injection, and firmware vulnerabilities in the TPM are outside what this project can address.

### Remote authentication

Not a defended threat — a refused capability. Face authentication over SSH is meaningless because the camera is at the *server*. `pam_namtor` hard-fails when `PAM_RHOST` is set or `PAM_TTY` indicates a remote session, **regardless of configuration**. This is not user-adjustable.

---

## Deployment guidance

| Service | Default | Reasoning |
|---|---|---|
| Screen unlock | Enabled | The session is already authenticated; this re-asserts presence |
| Graphical login | Enabled | Equivalent to Hello's primary use |
| `sudo` | **Opt-in** | Privilege escalation deserves a factor that cannot be presented while you sleep |
| `su` | **Opt-in** | As above |
| polkit | Opt-in | Varies by action |
| `sshd` | **Refused** | See above |

Face unlock always falls back to password. Any keypress during face authentication aborts it immediately and passes the keystroke to the password prompt — you are never trapped waiting for a camera.

---

## Reporting

Report suspected vulnerabilities privately rather than in a public issue tracker. Include the component, the Nam-tor version, and hardware details.

Reports about the **socket ACL** (spec §3.2) warrant particular attention: anyone who can assert *"authenticate user X"* and be believed has defeated every other control in this document.
