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

**Pre-flight item.** The canonical licence texts are not yet in this repository, because they were unavailable on the machine where the project was scaffolded. Before the first public release, add:

```
LICENSE-APACHE-2.0     https://www.apache.org/licenses/LICENSE-2.0.txt
LICENSE-GPL-3.0        https://www.gnu.org/licenses/gpl-3.0.txt
```

`scripts/check-env.sh` reports their absence. Do not paste them from memory — fetch the canonical texts.

## Third-party components

**No model weights and no datasets are committed to this repository, ever.**

Model weights are fetched at first run, and the user accepts the upstream licence at that point — the same pattern ffmpeg uses for codecs and Howdy uses for dlib's models. Training data is never redistributed; `ml/` ships reproducible recipes, and anyone who accepts a dataset's terms runs them to obtain their own weights.

See [`ml/DATASETS.md`](ml/DATASETS.md) for the licensing status of every dataset the project touches. Verify a dataset's terms before use; several prohibit commercial use and at least one has been withdrawn by its publisher.
