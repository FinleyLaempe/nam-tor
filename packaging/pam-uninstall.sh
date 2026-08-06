#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor transactional PAM removal.
#
# Removes ONLY the `# namtor-begin` ... `# namtor-end` marked block from the
# target PAM file. Never touches stock lines. After removal, verifies a
# password-fallback auth line is still present so password login is intact.
# If a namtor backup exists and the block was the only modification, the
# uninstall can optionally restore the file from backup; by default it just
# deletes the block (least destructive).
#
# Usage:
#   sudo ./packaging/pam-uninstall.sh
#   sudo ./packaging/pam-uninstall.sh --target /etc/pam.d/system-auth
#   DESTDIR=/tmp/fake-root ./packaging/pam-uninstall.sh --target /etc/pam.d/gdm-password
#
# Options:
#   --target PATH     target PAM file (default /etc/pam.d/gdm-password)
#   --restore-backup  restore the file from the namtor backup instead of
#                     just removing the block (verifies checksum first)
#   --purge-backup    remove the backup files after a successful uninstall
#   -h, --help        show this help
#
# Environment: DESTDIR (staging root for testing without root)

set -euo pipefail

TARGET_PAM="/etc/pam.d/gdm-password"
RESTORE_BACKUP=0
PURGE_BACKUP=0

usage() {
    sed -n '3,/^$/p' "$0" | sed 's/^# \?//' >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET_PAM="$2"; shift 2 ;;
        --restore-backup) RESTORE_BACKUP=1; shift ;;
        --purge-backup) PURGE_BACKUP=1; shift ;;
        -h|--help) usage ;;
        *) echo "pam-uninstall: unknown option: $1" >&2; usage ;;
    esac
done

DESTDIR="${DESTDIR:-}"

destdir_path() {
    local p="$1"
    if [[ -n "$DESTDIR" ]]; then
        printf '%s%s\n' "${DESTDIR%/}" "$p"
    else
        printf '%s\n' "$p"
    fi
}

die() {
    echo "pam-uninstall: ERROR: $*" >&2
    exit 1
}

TARGET="$(destdir_path "$TARGET_PAM")"
TARGET_DIR="$(dirname "$TARGET")"
BACKUP="${TARGET}.namtor.bak"
BACKUP_SUM="${BACKUP}.sha256"

if [[ ! -f "$TARGET" ]]; then
    die "target PAM file not found: $TARGET"
fi

has_block() {
    grep -q '^# namtor-begin$' "$1" 2>/dev/null
}

remove_block() {
    sed -i '/^# namtor-begin$/,/^# namtor-end$/d' "$1"
}

# --- restore-from-backup path ----------------------------------------------

if [[ "$RESTORE_BACKUP" -eq 1 ]]; then
    if [[ ! -f "$BACKUP" ]]; then
        die "--restore-backup requested but no backup found: $BACKUP"
    fi
    if [[ -f "$BACKUP_SUM" ]]; then
        current_hash="$(sha256sum "$BACKUP" | awk '{print $1}')"
        stored_hash="$(awk '{print $1}' "$BACKUP_SUM")"
        if [[ "$current_hash" != "$stored_hash" ]]; then
            die "backup checksum mismatch — refusing to restore a tampered backup"
        fi
    fi
    cp -a "$BACKUP" "$TARGET"
    echo "pam-uninstall: restored $TARGET from backup"
else
    # --- block-removal path ------------------------------------------------
    if ! has_block "$TARGET"; then
        echo "pam-uninstall: no namtor block found in $TARGET — nothing to do"
    else
        remove_block "$TARGET"
        echo "pam-uninstall: removed namtor block from $TARGET"
    fi
fi

# --- verify password login is intact ---------------------------------------

if ! grep -E '^[[:space:]]*auth[[:space:]]' "$TARGET" | grep -qE \
    'pam_unix|pam_systemd_home|pam_env|common-auth|system-auth|password-auth'; then
    die "no password-fallback auth line found in $TARGET after removal — password login may be broken! Manual review required."
fi

# Sanity: no stray namtor references remain.
if grep -q 'namtor' "$TARGET"; then
    die "stray 'namtor' reference remains in $TARGET after removal — manual review required"
fi

echo "pam-uninstall: OK  password fallback intact"

# --- optionally purge the backup -------------------------------------------

if [[ "$PURGE_BACKUP" -eq 1 ]]; then
    rm -f "$BACKUP" "$BACKUP_SUM"
    echo "pam-uninstall: purged backup files"
fi

echo "pam-uninstall: done."
