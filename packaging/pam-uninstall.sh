#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor PAM integration removal — thin wrapper around the Rust
# transactional editor (namtor-pam-edit).
#
# The Rust engine is the single source of truth for PAM editing. This
# script exists for backwards compatibility with the packaging scripts.
#
# Usage:
#   sudo ./packaging/pam-uninstall.sh
#   sudo ./packaging/pam-uninstall.sh --target /etc/pam.d/system-auth
#   DESTDIR=/tmp/fake-root ./packaging/pam-uninstall.sh --target /etc/pam.d/gdm-password
#
# Options:
#   --target PATH     target PAM service (default /etc/pam.d/gdm-password)
#   --restore-backup  restore the file byte-for-byte from the enable-time
#                     backup instead of removing the block from the file as
#                     it stands now. Use this when the stack has been damaged
#                     since; it fails rather than guesses if no backup exists.
#   -h, --help        show this help

set -euo pipefail

TARGET_PAM="/etc/pam.d/gdm-password"
ACTION="disable"

usage() {
    sed -n '3,/^$/p' "$0" | sed 's/^# \?//' >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET_PAM="$2"; shift 2 ;;
        --restore-backup) ACTION="restore"; shift ;;
        -h|--help) usage ;;
        *) echo "pam-uninstall: unknown option: $1" >&2; usage ;;
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
    echo "pam-uninstall: ERROR: no PAM editor found (tried 'namtor pam-edit')." >&2
    exit 1
fi

SERVICE_ID="$(basename "$TARGET_PAM")"

PAMD_ROOT="/etc/pam.d"
REGISTRY_ROOT="/var/lib/namtor"
if [[ -n "$DESTDIR" ]]; then
    PAMD_ROOT="${DESTDIR%/}/etc/pam.d"
    REGISTRY_ROOT="${DESTDIR%/}/var/lib/namtor"
fi

if [[ "$ACTION" == "restore" ]]; then
    echo "pam-uninstall: restoring service '$SERVICE_ID' from its backup"
else
    echo "pam-uninstall: disabling pam_namtor.so for service '$SERVICE_ID'"
fi
if "${PAM_EDIT[@]}" "$ACTION" \
    --service "$SERVICE_ID" \
    --pamd-root "$PAMD_ROOT" \
    --registry-root "$REGISTRY_ROOT"; then
    echo "pam-uninstall: done."
else
    rc=$?
    echo "pam-uninstall: FAILED (exit $rc)." >&2
    exit $rc
fi
