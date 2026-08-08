#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Sign Nam-tor release artifacts with a detached signature.
#
# Prefers ``minisign`` (Ed25519, minimal trust surface) when present; falls
# back to ``gpg --detach-sig --armor`` (SHA-256). Both produce a sidecar
# ``<file>.sig`` / ``<file>.asc`` the verifier script checks.
#
# The private signing key is NEVER read by this script — it relies on the
# caller's agent / keyring. No key material is committed.
#
# Usage::
#
#   packaging/sign-packages.sh [--gpg-key KEYID] [--out DIR] [file...]
#
#   --gpg-key   GPG key id to sign with (default: default key).
#   --out       output directory for signatures (default: alongside each file).
#   file...     files to sign (default: every binary + manifest in
#               target/release/ plus packaging/*.toml).
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

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gpg-key) gpg_key="$2"; shift 2 ;;
        --out)     outdir="$2"; shift 2 ;;
        --help|-h)
            sed -n '3,26p' "$0"; exit 0 ;;
        --*) echo "error: unknown option: $1" >&2; exit 2 ;;
        *)  files+=("$1"); shift ;;
    esac
done

# Default file set: release binaries + packaging manifests.
if [[ ${#files[@]} -eq 0 ]]; then
    repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    for b in namtor-daemon namtor-worker namtor-worker-spawn \
             namtor-camera-emitter-off namtor-cli namtor-enroll-wizard \
             libnamtor_pam.so; do
        f="$repo_root/target/release/$b"
        [[ -f "$f" ]] && files+=("$f")
    done
    for m in install-manifest.toml model-manifest.toml; do
        f="$repo_root/packaging/$m"
        [[ -f "$f" ]] && files+=("$f")
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
