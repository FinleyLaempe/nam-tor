# Licensing

Nam-tor is dual-licensed by component. The split is deliberate and load-bearing — it is not an accident of history.

## The split

| Component | Licence | SPDX |
|---|---|---|
| `crates/namtor-hwdb` | Apache License 2.0 | `Apache-2.0` |
| `crates/namtor-uvc` | Apache License 2.0 | `Apache-2.0` |
| `crates/namtor-probe` | Apache License 2.0 | `Apache-2.0` |
| `quirks/` (device database) | Apache License 2.0 | `Apache-2.0` |
| `crates/namtor-capture` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-pipeline` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-store` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-worker` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-daemon` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-pam` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-cli` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `crates/namtor-collect` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `ui/` | GNU GPL v3.0 only | `GPL-3.0-only` |
| `ml/` | GNU GPL v3.0 only | `GPL-3.0-only` |
| Documentation | GNU GPL v3.0 only | `GPL-3.0-only` |

Every source file carries an `SPDX-License-Identifier` header. CI enforces this via `scripts/check-spdx.sh`.

## Why split

**GPL-3.0 for the security core.** This is authentication software. Copyleft keeps derivatives auditable and prevents a vendor shipping a closed fork of an authentication stack that users are asked to trust with a biometric they cannot rotate.

**Apache-2.0 for hardware detection.** Identifying IR cameras and reaching their emitters is a problem the whole Linux ecosystem has, not a Nam-tor problem. Those components and the device database should be usable by anyone — Howdy, other desktop projects, distribution tooling, and ideally the kernel itself. The long-term goal (spec §4.11) is to make the userspace parts unnecessary by extending `uvcvideo`, and a restrictive licence would obstruct exactly that.

Apache-2.0 was chosen over MIT for its explicit patent grant, which matters for code that touches vendor-specific hardware protocols.

## PAM linkage

`pam_namtor.so` is GPL-3.0 and is loaded by programs under other licences (`sudo` is ISC, `login` is GPL-2.0, GDM is GPL-2.0). This is the normal and accepted arrangement for PAM modules — Linux-PAM is BSD/GPL dual-licensed and many shipped modules are GPL. The module is loaded through `dlopen` as a plugin, not linked into those programs at build time.

## Missing licence texts

The canonical licence texts are installed in [`packaging/licences/`](packaging/licences/):

```
packaging/licences/MIT.txt         — MIT License (SPDX template)
packaging/licences/APACHE-2.0.txt  — Apache License 2.0
packaging/licences/GPL-3.0.txt     — GNU General Public License v3.0
packaging/licences/yunet_2023mar.LICENSE   — YuNet model-specific MIT licence
packaging/licences/sface_2021dec.LICENSE   — SFace model-specific Apache-2.0 licence
packaging/licences/minifasnet_pad.NOTICE   — MiniFASNet PAD provenance notice
```

Their SHA-256 hashes are recorded in [`packaging/model-manifest.toml`](packaging/model-manifest.toml) and verified at package build time. Do not edit these files — they are the canonical bytes the manifest hashes are computed from.

## Third-party components

**No model weights and no datasets are committed to this repository, ever.**

Model weights are acquired out-of-band (bundled in distro packages or fetched by the operator) and validated against [`packaging/model-manifest.toml`](packaging/model-manifest.toml) at every daemon startup. The manifest records each model's upstream revision, weight SHA-256, licence URL, licence SHA-256, redistribution policy, and training provenance. Redistributable models (MIT, Apache-2.0, self-trained) admit without an acceptance record. Restricted models require an on-disk `licence.accepted` record written by the CLI or an explicit operator acceptance flow; the daemon checks the record but never writes it. Training data is never redistributed; `ml/` ships reproducible recipes, and anyone who accepts a dataset's terms runs them to obtain their own weights.

### Deployed models

| Model | Purpose | Upstream | Licence | Redistribution |
|---|---|---|---|---|
| YuNet 2023mar | Face detection (5-pt landmarks) | OpenCV Zoo | MIT | Redistributable |
| SFace 2021dec | Face embedding (128-d) | OpenCV Zoo | Apache-2.0 | Redistributable |
| MiniFASNet PAD | Strobe-differential liveness | Self-trained (physics-based synthetic data) | Apache-2.0 (code ref: Silent-Face) | Redistributable |

All three models are permissively licensed and redistributable. The MiniFASNet PAD weights are self-trained on physics-based synthetic strobe-differential data — no licence-restricted dataset was used to derive them (see `packaging/licences/minifasnet_pad.NOTICE`).

See [`ml/DATASETS.md`](ml/DATASETS.md) for the licensing status of every dataset the project touches. Verify a dataset's terms before use; several prohibit commercial use and at least one has been withdrawn by its publisher.

