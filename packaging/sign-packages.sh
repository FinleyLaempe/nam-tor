#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Sign Nam-tor release artifacts with a detached signature.
#
# Prefers ``minisign`` (Ed25519, minimal trust surface) when present; falls
# back to ``gpg --detach-sig --armor`` (SHA-256). Both produce a sidecar
# ``<file>.minisig`` / ``<file>.asc`` the verifier checks.
#
# The private signing key is NEVER read by this script — it relies on the
# caller's agent / keyring. No key material is committed.
#
# Two modes:
#
#   1. Default (no --release-dir): sign the release binaries + packaging
#      manifests + the deterministic model archive. Used during development
#      and by CI jobs that only build the Rust workspace.
#
#   2. Release mode (``--release-dir DIR``): sign EVERY artifact a public
#      release ships — source tarball, model archive, all split-package
#      binaries (Arch .pkg.tar.zst, Fedora .rpm, Debian .deb), the
#      checksums file, and the provenance attestation. This is the mode the
#      release operator runs against the dist/ output directory.
#
# Usage::
#
#   packaging/sign-packages.sh [--gpg-key KEYID] [--out DIR] [file...]
#   packaging/sign-packages.sh --release-dir DIR [--version VER]
#                              [--gpg-key KEYID] [--out DIR]
#                              [--checksums] [--provenance PATH]
#
#   --release-dir DIR  release output directory holding all artifacts to
#                      sign. Discovers: namtor-<ver>.tar.gz (source),
#                      namtor-models-<ver>.tar.gz (models), *.pkg.tar.zst
#                      (Arch split packages), *.rpm (Fedora split packages),
#                      *.deb (Debian split packages), SHA256SUMS (checksums),
#                      namtor.provenance.json (provenance). Explicit file
#                      args are ignored in this mode.
#   --version VER      release version tag for discovery globs
#                      (default: 0.1.0).
#   --checksums        (release-dir mode) also generate SHA256SUMS from every
#                      discovered artifact and sign it (in addition to the
#                      per-file detached signatures).
#   --provenance PATH  (release-dir mode) explicit provenance file path
#                      (default: <release-dir>/namtor.provenance.json).
#   --gpg-key          GPG key id to sign with (default: default key).
#   --out              output directory for signatures (default: alongside
#                      each file).
#   file...            files to sign (default set; ignored with --release-dir).
#
# Environment:
#   NAMTOR_SIGN_TOOL   override detection: ``minisign`` or ``gpg``.
#
# Exit codes: 0 = all signed, 1 = no tool available, 2 = bad args,
#             3 = a file was missing.

set -euo pipefail

gpg_key=""
outdir=""
tool_override="${NAMTOR_SIGN_TOOL:-}"
files=()
release_dir=""
release_version="0.1.0"
do_checksums=0
provenance_override=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gpg-key)      gpg_key="$2"; shift 2 ;;
        --out)          outdir="$2"; shift 2 ;;
        --release-dir)  release_dir="$2"; shift 2 ;;
        --version)      release_version="$2"; shift 2 ;;
        --checksums)    do_checksums=1; shift ;;
        --provenance)   provenance_override="$2"; shift 2 ;;
        --help|-h)
            sed -n '3,54p' "$0"; exit 0 ;;
        --*) echo "error: unknown option: $1" >&2; exit 2 ;;
        *)  files+=("$1"); shift ;;
    esac
done

# ----- release-dir mode: discover all release artifacts ----------------------
if [[ -n "$release_dir" ]]; then
    [[ -d "$release_dir" ]] || { echo "error: release dir not found: $release_dir" >&2; exit 3; }
    release_dir="$(cd "$release_dir" && pwd)"
    files=()
    # Source tarball: namtor-<ver>.tar.gz (but NOT namtor-models-<ver>.tar.gz).
    local_src=()
    for f in "$release_dir"/namtor-"$release_version".tar.gz; do
        [[ -f "$f" ]] && local_src+=("$f")
    done
    # Model archive: namtor-models-<ver>.tar.gz (+ its .sha256 sidecar).
    local_models=()
    for f in "$release_dir"/namtor-models-"$release_version".tar.gz; do
        [[ -f "$f" ]] && local_models+=("$f")
        [[ -f "$f.sha256" ]] && local_models+=("$f.sha256")
    done
    # Arch split packages: namtor, namtor-ui .pkg.tar.zst.
    local_arch=()
    for f in "$release_dir"/*.pkg.tar.zst; do
        [[ -f "$f" ]] && local_arch+=("$f")
    done
    # Fedora split packages: .rpm.
    local_fedora=()
    for f in "$release_dir"/*.rpm; do
        [[ -f "$f" ]] && local_fedora+=("$f")
    done
    # Debian split packages: .deb.
    local_debian=()
    for f in "$release_dir"/*.deb; do
        [[ -f "$f" ]] && local_debian+=("$f")
    done
    # Provenance attestation.
    local_prov=()
    prov_path="$provenance_override"
    [[ -z "$prov_path" ]] && prov_path="$release_dir/namtor.provenance.json"
    [[ -f "$prov_path" ]] && local_prov+=("$prov_path")

    # Checksums file (generated if --checksums; otherwise signed if present).
    checksums_file="$release_dir/SHA256SUMS"

    # Every discovered artifact gets a per-file detached signature. The
    # task contract requires signatures on: source tarball, model archive,
    # all .pkg.tar.zst, all .rpm, all .deb, checksums, provenance.
    files+=("${local_src[@]}" "${local_models[@]}" "${local_arch[@]}" \
            "${local_fedora[@]}" "${local_debian[@]}" "${local_prov[@]}")

    # Generate + sign SHA256SUMS manifest if --checksums (additional
    # aggregated signature; does NOT replace the per-file signatures).
    if [[ $do_checksums -eq 1 ]]; then
        checksums_out="$release_dir/SHA256SUMS"
        (
            cd "$release_dir"
            : > "$checksums_out"
            for f in "${local_src[@]}" "${local_models[@]}" \
                     "${local_arch[@]}" "${local_fedora[@]}" \
                     "${local_debian[@]}" "${local_prov[@]}"; do
                [[ -f "$f" ]] || continue
                b="$(basename "$f")"
                sha256sum "$b" >> "$checksums_out"
            done
        )
        files+=("$checksums_out")
        echo "Generated $checksums_out ($(wc -l < "$checksums_out") entries)"
    elif [[ -f "$checksums_file" ]]; then
        files+=("$checksums_file")
    fi
fi

# Default file set (only when not in release-dir mode and no explicit files).
if [[ ${#files[@]} -eq 0 && -z "$release_dir" ]]; then
    repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    for b in namtor namtor-enroll-wizard \
             libnamtor_pam.so; do
        f="$repo_root/target/release/$b"
        [[ -f "$f" ]] && files+=("$f")
    done
    for m in install-manifest.toml model-manifest.toml; do
        f="$repo_root/packaging/$m"
        [[ -f "$f" ]] && files+=("$f")
    done
    # Deterministic model archive (task 4) — signed alongside packages so
    # every distro recipe can verify it before building. Match the most
    # recent version present; build-model-archive.sh produces the canonical
    # name namtor-models-<version>.tar.gz.
    for ma in "$repo_root"/namtor-models-*.tar.gz; do
        [[ -f "$ma" ]] && files+=("$ma")
        break  # only the first (newest) match; sign one archive per release
    done
fi

[[ ${#files[@]} -eq 0 ]] && { echo "error: no files to sign" >&2; exit 3; }
for f in "${files[@]}"; do
    [[ -f "$f" ]] || { echo "error: file not found: $f" >&2; exit 3; }
done

# Tool detection.
tool="$tool_override"
if [[ -z "$tool" ]]; then
    if command -v minisign >/dev/null 2>&1; then
        tool="minisign"
    elif command -v gpg >/dev/null 2>&1; then
        tool="gpg"
    else
        echo "error: neither minisign nor gpg is installed" >&2
        exit 1
    fi
fi

sig_count=0
sign_minisign() {
    local f="$1" sig="$2"
    # minisign creates <sig>; -t adds trusted comment, -H forces SHA-256.
    minisign -Sm "$f" -s "$sig" -t "namtor release $(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        || { echo "error: minisign failed for $f" >&2; return 1; }
}

sign_gpg() {
    local f="$1" sig="$2"
    local gpg_args=(--detach-sig --armor --digest-algo SHA256 --output "$sig")
    [[ -n "$gpg_key" ]] && gpg_args+=(--local-user "$gpg_key")
    gpg "${gpg_args[@]}" "$f" \
        || { echo "error: gpg failed for $f" >&2; return 1; }
}

echo "Signing ${#files[@]} file(s) with $tool"
for f in "${files[@]}"; do
    if [[ "$tool" == "minisign" ]]; then
        sig="${outdir:-$(dirname "$f")}/$(basename "$f").minisig"
        sign_minisign "$f" "$sig"
    else
        sig="${outdir:-$(dirname "$f")}/$(basename "$f").asc"
        sign_gpg "$f" "$sig"
    fi
    sig_count=$((sig_count + 1))
    echo "  signed $f -> $sig"
done

echo "Done: $sig_count signature(s) produced with $tool."

# Emit a verification hint for the caller.
cat >&2 <<HINT
Verify with:
  $tool == minisign:  minisign -Vm <file> -P <public-key>
  $tool == gpg:       gpg --verify <file>.asc <file>
HINT
