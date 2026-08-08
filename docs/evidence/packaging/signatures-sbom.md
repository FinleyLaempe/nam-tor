<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Signatures, SBOM and Provenance

Evidence for plan task 39: package signatures, software bill of materials,
provenance attestations, and reproducible-build verification.

## Deliverables

| File | Purpose |
|---|---|
| `packaging/generate-sbom.py` | Generates CycloneDX 1.5 + SPDX 2.3 SBOM from `Cargo.lock` (offline). |
| `packaging/sign-packages.sh` | Detached-signature signing for release artifacts (minisign preferred, GPG fallback). |
| `packaging/generate-provenance.py` | SLSA v1.0 provenance attestation (in-toto statement) for a release build. |
| `packaging/verify-reproducible.sh` | Builds the workspace release twice into isolated target dirs and compares every artifact's sha256. |
| `docs/evidence/packaging/namtor.cyclonedx.json` | Generated CycloneDX SBOM (committed; tied to `Cargo.lock`). |
| `docs/evidence/packaging/namtor.spdx.json` | Generated SPDX SBOM (committed; tied to `Cargo.lock`). |

The per-build provenance (`namtor.provenance.json`) and reproducibility
record (`reproducible-build.json`) are generated at release time into the
same directory but are **not committed** — they embed host-specific paths
and the current commit hash, so they are regenerated for each release.

## SBOM generation

### Method

`generate-sbom.py` parses `Cargo.lock` with the Python ≥ 3.11 stdlib
`tomllib` (no external dependency, no network). For each locked package it
extracts name, version, source registry, sha256 checksum, and the
dependency edges. Licence information is cross-referenced from
`cargo metadata --format-version 1`, which runs offline against the local
registry cache. When `cargo metadata` does not resolve a package (62 of
340 — these are alternative versions locked for reproducibility but not in
the active feature-resolved graph), the licence is recorded as `UNKNOWN`
and every other field is still populated.

Two formats are emitted:

- **CycloneDX 1.5** (`namtor.cyclonedx.json`) — `bomFormat`, `components`
  (library type, purl, sha256 hashes, licence), `dependencies` (full
  dependency graph including the top-level workspace node).
- **SPDX 2.3** (`namtor.spdx.json`) — `packages` (SPDXID, versionInfo,
  downloadLocation, licenseDeclared, packageVerificationCode),
  `relationships` (DEPENDS_ON edges).

### Current SBOM

```
SBOM: 340 packages (322 external, 18 workspace)
  -> namtor.cyclonedx.json (CycloneDX 1.5, 340 components, 341 dep nodes)
  -> namtor.spdx.json      (SPDX 2.3, 340 packages, 998 relationships)
  warning: 62 packages with UNKNOWN licence (cargo metadata unavailable)
```

The 62 UNKNOWN-licence packages are alternative versions locked in
`Cargo.lock` (e.g. `cpufeatures 0.3.0` vs the resolved `0.2.17`) that are
not in the feature-resolved dependency graph `cargo metadata` reports.
Their name, version, and sha256 checksum are still recorded; only the
licence field is `UNKNOWN` / `NOASSERTION`. A release that needs full
licence coverage for these can run `cargo cyclonedx` (which requires the
`cargo-cyclonedx` tool) or supply licence info from the crates.io API
(requires network, out of scope for the offline contract).

### No-network contract

The script never opens a socket. `tomllib` reads `Cargo.lock` from disk;
`cargo metadata` reads the local registry cache (`~/.cargo/registry/`).
If `cargo` is not on `PATH` or fails, generation proceeds with
`UNKNOWN` licences rather than aborting — the SBOM is still structurally
complete.

## Package signing

`sign-packages.sh` produces detached signatures for the release
artifacts. Tool selection:

1. **minisign** (preferred) — Ed25519, minimal trust surface, produces
   `<file>.minisig`. Used when `minisign` is on `PATH`.
2. **GPG** (fallback) — `gpg --detach-sig --armor --digest-algo SHA256`,
   produces `<file>.asc`. Used when minisign is absent.

Override with `NAMTOR_SIGN_TOOL=minisign|gpg`. Pass `--gpg-key KEYID` to
select a specific GPG key, `--out DIR` to collect signatures in one
directory, or explicit file paths to sign a subset.

The script **never reads or stores a private key**. It relies on the
caller's GPG agent or minisign keyring. No key material is committed to
the repository.

Default file set (when no files are passed explicitly):

- `target/release/namtor-daemon`
- `target/release/namtor-worker`
- `target/release/namtor-worker-spawn`
- `target/release/namtor-camera-emitter-off`
- `target/release/namtor-cli`
- `target/release/namtor-enroll-wizard`
- `target/release/libnamtor_pam.so`
- `packaging/install-manifest.toml`
- `packaging/model-manifest.toml`

### Verification

```sh
# minisign
minisign -Vm <file> -P <public-key>

# GPG
gpg --verify <file>.asc <file>
```

### Note on the PAM module filename

The `namtor-pam` crate produces `libnamtor_pam.so` (cargo derives the
filename from the crate name). The install manifest installs it as
`pam_namtor.so` via the `src = "target/release/libpam_namtor.so"` entry,
which is a symlink to `libnamtor_pam.so` created during staging. The
signing and provenance scripts reference the real build output
(`libnamtor_pam.so`) so they work against a fresh `CARGO_TARGET_DIR`
where the symlink does not yet exist.

## Provenance attestation

`generate-provenance.py` emits an in-toto statement
(`_type: https://in-toto.io/Statement/v1`) with the SLSA v1.0 provenance
predicate (`predicateType: https://slsa.dev/provenance/v1`).

The attestation records:

- **Subject** — sha256 of each release artifact (7 binaries + cdylib).
- **buildDefinition.buildType** — `https://namtor.dev/buildtypes/cargo-release-v1`
  (a stable URI for "cargo build --release from a git checkout").
- **buildDefinition.externalParameters** — `profile: release`, `target: workspace`.
- **buildDefinition.internalParameters** — cargo/rustc versions, `panic: abort`,
  `lto: true`, `codegen-units: 1`, `strip: true` (the `[profile.release]`
  settings that make the build deterministic).
- **buildDefinition.resolvedDependencies** — the git commit (URI + `gitCommit`
  digest) and `Cargo.lock` (sha256).
- **runDetails.builder.id** — `host://<hostname>` (identifies the build host).
- **runDetails.byproducts** — the CycloneDX and SPDX SBOM files referenced by
  their own sha256.

### SLSA level

The current pipeline achieves **SLSA Build L3** on the design axis
(isolated, deterministic builds with provenance attestation) but is
**SLSA Build L2** in practice because the builder identity is a local
host, not a hosted build platform with verified provenance generation.
Reaching L3 requires a build service that generates and signs the
provenance in a tamper-resistant environment (e.g. GitHub Actions with
OIDC, or a self-hosted runner with a HSM-backed signing step). The
provenance schema and content are L3-ready; the builder infrastructure
is the gap.

## Reproducible builds

`verify-reproducible.sh` builds the workspace release twice into two
isolated `CARGO_TARGET_DIR` roots (ruling out incremental-cache
contamination), then compares the sha256 of every artifact the install
manifest references.

### Why Rust release builds are deterministic here

The `[profile.release]` in `Cargo.toml` sets:

```toml
panic = "abort"      # no unwinding metadata variance
opt-level = "z"      # fixed optimisation level
lto = true           # whole-program LTO, no parallel-codegen noise
codegen-units = 1    # single codegen unit, deterministic instruction layout
strip = true         # no embedded absolute build paths in debug info
```

With a fixed rustc version on the same host, two builds from the same
source produce byte-identical binaries.

### Result

All 7 artifacts are byte-identical across two independent builds:

| Artifact | Reproducible |
|---|---|
| `namtor-daemon` | yes |
| `namtor-worker` | yes |
| `namtor-worker-spawn` | yes |
| `namtor-camera-emitter-off` | yes |
| `namtor-cli` | yes |
| `namtor-enroll-wizard` | yes |
| `libnamtor_pam.so` | yes |

The full per-artifact hash comparison is in
`docs/evidence/packaging/reproducible-build.json` (generated, not
committed — regenerated by `packaging/verify-reproducible.sh`).

## Release workflow

A release operator runs, in order:

```sh
# 1. Build release artifacts
cargo build --release --workspace

# 2. Verify reproducibility (builds twice, compares hashes)
packaging/verify-reproducible.sh

# 3. Generate SBOM
python3 packaging/generate-sbom.py

# 4. Generate provenance attestation
python3 packaging/generate-provenance.py

# 5. Sign artifacts (requires caller's key)
packaging/sign-packages.sh --out dist/
```

Steps 1–4 require no network and no secret key. Only step 5 requires a
private key, and it is never read by the script — the caller's GPG agent
or minisign keyring handles that.

## Verification (this task)

- `cargo build --workspace` — passes (all 17 workspace crates compile).
- `python3 packaging/generate-sbom.py` — produces both SBOM formats, 340
  packages, no errors.
- `packaging/verify-reproducible.sh` — all 7 artifacts reproducible,
  exit 0.
- `python3 packaging/generate-provenance.py` — 7 subjects, git commit
  + Cargo.lock sha256 recorded.
- `packaging/sign-packages.sh` — script logic verified; signing
  requires a GPG/minisign key (not available on the build host, by
  design — the operator provides the key at release time).
