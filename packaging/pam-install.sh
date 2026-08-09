#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor PAM integration installer — thin wrapper around the Rust
# transactional editor (namtor-pam-edit).
#
# The Rust engine is the single source of truth for PAM editing. This
# script exists for backwards compatibility with the install manifest
# and packaging scripts that call pam-install.sh.
#
# Usage:
#   sudo ./packaging/pam-install.sh
#   sudo ./packaging/pam-install.sh --target /etc/pam.d/system-auth
#   sudo ./packaging/pam-install.sh --control '[default=ignore]'
#   DESTDIR=/tmp/fake-root ./packaging/pam-install.sh --target /etc/pam.d/gdm-password
#
# Options:
#   --target PATH     target PAM service (default /etc/pam.d/gdm-password)
#   --control CTRL    sufficient | [default=ignore] (default sufficient)
#   -h, --help        show this help

set -euo pipefail

TARGET_PAM="/etc/pam.d/gdm-password"
CONTROL="sufficient"

usage() {
    sed -n '3,/^$/p' "$0" | sed 's/^# \?//' >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET_PAM="$2"; shift 2 ;;
        --control) CONTROL="$2"; shift 2 ;;
        --no-preflight) shift ;;  # no-op: Rust binary handles preflight internally
        --keep-backup) shift ;;   # no-op: Rust binary keeps backups by default
        -h|--help) usage ;;
        *) echo "pam-install: unknown option: $1" >&2; usage ;;
    esac
done

DESTDIR="${DESTDIR:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Resolve the binary: prefer release, fall back to debug, then repo root.
PAM_EDIT=""
for candidate in \
    "/usr/lib/namtor/namtor-pam-edit" \
    "$SCRIPT_DIR/../target/release/namtor-pam-edit" \
    "$SCRIPT_DIR/../target/debug/namtor-pam-edit"; do
    if [[ -x "$candidate" ]]; then
        PAM_EDIT="$candidate"
        break
    fi
done

if [[ -z "$PAM_EDIT" ]]; then
    echo "pam-install: ERROR: namtor-pam-edit binary not found. Build it with:" >&2
    echo "  cargo build -p namtor-pam-config" >&2
    exit 1
fi

# Extract the service ID (basename of the target path).
SERVICE_ID="$(basename "$TARGET_PAM")"

# Map the shell control names to the Rust binary's flags.
CONTROL_FLAG="sufficient"
case "$CONTROL" in
    sufficient) CONTROL_FLAG="sufficient" ;;
    "[default=ignore]") CONTROL_FLAG="default-ignore" ;;
    *)
        echo "pam-install: ERROR: unsafe control field '$CONTROL' — use 'sufficient' or '[default=ignore]'" >&2
        exit 1
        ;;
esac

# Resolve paths under DESTDIR.
PAMD_ROOT="/etc/pam.d"
REGISTRY_ROOT="/var/lib/namtor"
if [[ -n "$DESTDIR" ]]; then
    PAMD_ROOT="${DESTDIR%/}/etc/pam.d"
    REGISTRY_ROOT="${DESTDIR%/}/var/lib/namtor"
    mkdir -p "$PAMD_ROOT" "$REGISTRY_ROOT"
fi

echo "pam-install: enabling pam_namtor.so for service '$SERVICE_ID'"
if "$PAM_EDIT" enable \
    --service "$SERVICE_ID" \
    --pamd-root "$PAMD_ROOT" \
    --registry-root "$REGISTRY_ROOT" \
    --control "$CONTROL_FLAG"; then
    echo "pam-install: done. Service $SERVICE_ID now has the namtor block."
else
    rc=$?
    echo "pam-install: FAILED (exit $rc). No PAM files were left in an inconsistent state." >&2
    exit $rc
fi
