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

# Resolve the PAM editor. `namtor pam-edit` is the shipped path: the install
# manifest ships /usr/bin/namtor and has never shipped a standalone
# namtor-pam-edit, so looking only for that name failed on every real system.
#
# The repo build tree comes FIRST so running this from a checkout exercises the
# binary just built rather than whatever older namtor happens to be installed.
# A shipped copy lives in /usr/share/namtor/, where ../target does not exist,
# so it falls through to /usr/bin/namtor.
PAM_EDIT=()
for candidate in \
    "$SCRIPT_DIR/../target/release/namtor" \
    "$SCRIPT_DIR/../target/debug/namtor" \
    "/usr/bin/namtor"; do
    if [[ -x "$candidate" ]]; then
        PAM_EDIT=("$candidate" pam-edit)
        break
    fi
done
# Development fallback: the standalone binary, if someone built only that.
if [[ ${#PAM_EDIT[@]} -eq 0 ]]; then
    for candidate in \
        "$SCRIPT_DIR/../target/release/namtor-pam-edit" \
        "$SCRIPT_DIR/../target/debug/namtor-pam-edit" \
        "/usr/lib/namtor/namtor-pam-edit"; do
        if [[ -x "$candidate" ]]; then
            PAM_EDIT=("$candidate")
            break
        fi
    done
fi

if [[ ${#PAM_EDIT[@]} -eq 0 ]]; then
    for candidate in \
        "/usr/lib/namtor/namtor-pam-edit" \
        "$SCRIPT_DIR/../target/release/namtor-pam-edit" \
        "$SCRIPT_DIR/../target/debug/namtor-pam-edit"; do
        if [[ -x "$candidate" ]]; then
            PAM_EDIT=("$candidate")
            break
        fi
    done
fi

if [[ ${#PAM_EDIT[@]} -eq 0 ]]; then
    echo "pam-install: ERROR: no PAM editor found (tried 'namtor pam-edit')." >&2
    echo "  Build it with: cargo build -p namtor" >&2
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
# Root the pam_namtor.so preflight searches. Under DESTDIR the module lives
# in the staging tree, not on the live system, so the two must agree — a
# preflight against / would pass on a machine that already has Nam-tor
# installed and report a staging tree as sound when it is empty.
MODULE_ROOT="/"
if [[ -n "$DESTDIR" ]]; then
    PAMD_ROOT="${DESTDIR%/}/etc/pam.d"
    REGISTRY_ROOT="${DESTDIR%/}/var/lib/namtor"
    MODULE_ROOT="${DESTDIR%/}"
    mkdir -p "$PAMD_ROOT" "$REGISTRY_ROOT"
fi

echo "pam-install: enabling pam_namtor.so for service '$SERVICE_ID'"
if "${PAM_EDIT[@]}" enable \
    --service "$SERVICE_ID" \
    --pamd-root "$PAMD_ROOT" \
    --registry-root "$REGISTRY_ROOT" \
    --module-root "$MODULE_ROOT" \
    --control "$CONTROL_FLAG"; then
    echo "pam-install: done. Service $SERVICE_ID now has the namtor block."
else
    rc=$?
    echo "pam-install: FAILED (exit $rc). No PAM files were left in an inconsistent state." >&2
    exit $rc
fi
