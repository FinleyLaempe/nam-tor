<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Release Signing

How Nam-tor release artifacts are signed, how to verify them, and where the
public key lives. Implements plan task 25.

## Signed artifacts

Every public release ships the following artifact classes, and **every one**
carries a detached signature (`.minisig` for minisign, `.asc` for GPG):

| Artifact class | Filename pattern | Produced by |
|---|---|---|
| Source tarball | `namtor-<ver>.tar.gz` | `git archive` / release workflow |
| Model archive | `namtor-models-<ver>.tar.gz` (+ `.sha256` sidecar) | `packaging/build-model-archive.sh` |
| Arch split package | `namtor-<ver>-1-*.pkg.tar.zst` (×3: core, gnome, kde) | `makepkg` |
| Fedora split package | `namtor-<ver>-1.fcNN.*.rpm` (×3) | `rpmbuild` |
| Debian split package | `namtor_<ver>-1_*.deb` (×3) | `dpkg-buildpackage` |
| Checksums manifest | `SHA256SUMS` | `sign-packages.sh --checksums` |
| Provenance attestation | `namtor.provenance.json` | `packaging/generate-provenance.py` |

The checksums manifest is an **aggregated** signature: one detached
signature over a file that lists the sha256 of every other artifact. It does
not replace the per-file signatures — both are shipped. A verifier can
check either layer (or both).

## Signing tool

`packaging/sign-packages.sh` prefers **minisign** (Ed25519, minimal trust
surface) when present and falls back to **GPG**
(`--detach-sig --armor --digest-algo SHA256`) otherwise. Override with
`NAMTOR_SIGN_TOOL=minisign|gpg`. Pass `--gpg-key KEYID` to select a specific
GPG key.

The script **never reads or stores a private key**. It relies on the
caller's GPG agent or minisign keyring. No key material is committed to the
repository. The script contains no `--generate-key`, `--export-secret-key`,
or `--import-secret-key` operations — verified by
`packaging/tests/test_release_signatures.sh` gate 6.

## Release operator workflow

```sh
# 1. Build release artifacts (cargo + distro packages) into dist/
cargo build --release --workspace
# ... assemble source tarball, run makepkg / rpmbuild / dpkg-buildpackage ...

# 2. Build the deterministic model archive
packaging/build-model-archive.sh --out dist/ --version 0.1.0

# 3. Generate the SLSA provenance attestation
python3 packaging/generate-provenance.py --out dist/namtor.provenance.json \
    --model-archive dist/namtor-models-0.1.0.tar.gz --version 0.1.0

# 4. Verify reproducibility (builds twice, compares hashes)
packaging/verify-reproducible.sh --release-dir dist/

# 5. Sign every artifact + generate+sign the SHA256SUMS manifest
NAMTOR_SIGN_TOOL=gpg packaging/sign-packages.sh \
    --release-dir dist/ --version 0.1.0 \
    --checksums --gpg-key <KEYID>
```

Step 5 is the only step that requires a private key. The operator's GPG
agent or minisign keyring handles the key; the script never sees it.

For development/CI use (no distro packages assembled), omit `--release-dir`:

```sh
packaging/sign-packages.sh   # signs release binaries + manifests + model archive
```

## Public key and fingerprint

> **PLACEHOLDER** — populate on the first production release.

The release operator generates the signing key out-of-band and publishes:

- the **fingerprint** below,
- the **public key** (`namtor-release-signing.asc`), and
- the **minisign public key** (`namtor-release.minisign.pub`, if minisign is
  the chosen tool)

in the release notes and on the project's trusted key-distribution page.
The private key reference for automated release signing is held in a CI
secret (GitHub Actions encrypted secret or equivalent), never in the repo.

```
GPG fingerprint:  <PLACEHOLDER — 40-hex fingerprint of the release signing key>
GPG keyring:      <PLACEHOLDER — URL where namtor-release-signing.asc is published>
minisign pubkey:  <PLACEHOLDER — contents of namtor-release.minisign.pub>
```

## Verifying a release

Download the release artifacts, the checksums manifest, the checksums
signature, and the public key. Then:

### GPG path

```sh
# Import the published public key once.
gpg --import namtor-release-signing.asc

# Verify the aggregated checksums manifest.
gpg --verify SHA256SUMS.asc SHA256SUMS

# Verify every artifact hash against the manifest.
sha256sum -c SHA256SUMS

# Verify any individual artifact's detached signature.
gpg --verify namtor-0.1.0.tar.gz.asc namtor-0.1.0.tar.gz
gpg --verify namtor-models-0.1.0.tar.gz.asc namtor-models-0.1.0.tar.gz
gpg --verify namtor-0.1.0-1-x86_64.pkg.tar.zst.asc namtor-0.1.0-1-x86_64.pkg.tar.zst
gpg --verify namtor-0.1.0-1.fc40.x86_64.rpm.asc namtor-0.1.0-1.fc40.x86_64.rpm
gpg --verify namtor_0.1.0-1_amd64.deb.asc namtor_0.1.0-1_amd64.deb
gpg --verify namtor.provenance.json.asc namtor.provenance.json
```

### minisign path

```sh
# Verify the aggregated checksums manifest.
minisign -Vm SHA256SUMS -P <minisign-pubkey>

# Verify any individual artifact.
minisign -Vm namtor-0.1.0.tar.gz -P <minisign-pubkey>
```

A verification failure means the artifact was tampered with or signed by the
wrong key. Do not install a release whose signatures do not verify.

## Reproducibility

`packaging/verify-reproducible.sh` builds the workspace release twice into
two isolated `CARGO_TARGET_DIR` roots, builds the model archive twice, and —
with `--release-dir DIR` — stages the release directory twice and compares
every artifact's sha256. The comparison record is written to
`docs/evidence/packaging/reproducible-build.json` with per-artifact
`kind`, `sha256_a`, `sha256_b`, and `match` fields.

Determinism guarantees:

- Rust `[profile.release]`: `strip=true`, `lto=true`, `codegen-units=1`,
  `panic=abort` — no embedded build paths, no parallel-codegen noise.
- Model archive: fixed mtime epoch 0, sorted tar entries, `gzip -n` (no
  original timestamp/name), fixed owner/group 0.
- Same rustc version + same host across both builds.

## AppStream screenshots

AppStream `metainfo.xml` files for the GNOME enrollment wizard
(`packaging/gnome/org.namtor.Wizard.metainfo.xml`) and the KDE KCM
(`packaging/kde/org.namtor.kcm.metainfo.xml`) ship **without** `<screenshots>`
blocks.

**Rationale:** AppStream `<screenshot>` elements require stable public URLs.
The project does not yet have a stable hosting location for screenshot
images. Rather than point at transient or placeholder URLs (which would
break AppStream validators and user experience once the URLs decay), the
screenshot metadata elements are omitted until stable hosting
(e.g. `release-artifacts.namtor.dev`) is configured.

AppStream validators accept metainfo files without screenshots — screenshots
are recommended, not required. The omission is a deliberate, documented
decision, not an oversight.

## Test

```sh
bash packaging/tests/test_release_signatures.sh
```

45 assertions across 7 gates: coverage, repro-flag, sign/verify, tamper,
wrong-key, no-private-key, and release-dir end-to-end. Uses throwaway GPG
keys in an isolated `GNUPGHOME` (cleaned on exit); the operator's keyring is
never touched and no key material persists.

Evidence:

- Happy path: [`docs/evidence/public-release/25-release-signatures.json`](../docs/evidence/public-release/25-release-signatures.json)
- Tamper/wrong-key failure: [`docs/evidence/public-release/25-release-signature-failure.json`](../docs/evidence/public-release/25-release-signature-failure.json)
