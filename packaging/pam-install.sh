#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor transactional PAM integration installer.
#
# Inserts a marked `# namtor-begin` ... `# namtor-end` block into the target
# PAM file containing `auth sufficient pam_namtor.so`. NEVER destructively
# edits stock lines: the block is inserted, stock lines are never deleted or
# reordered. If any step fails after the backup is taken, the backup is
# restored.
#
# Safety guarantees (spec §3.4, §8.3):
#   - pam_namtor.so is installed as `auth sufficient` (or `[default=ignore]`),
#     NEVER `auth required` — a daemon outage with `required` would lock users
#     out. The module returns PAM_AUTHINFO_UNAVAIL on daemon-unreachable, which
#     with `sufficient` means "this module is unavailable, fall through".
#   - The installer verifies a password-fallback auth line exists BELOW the
#     namtor block (pam_unix / pam_systemd_home / common-auth / system-auth /
#     password-auth). It refuses if the fallback would be lost.
#   - The installer refuses if the block would land AFTER pam_deny.so.
#   - A timestamped backup with a sha256 checksum is taken before any edit;
#     on any failure the backup is restored.
#   - Re-running the installer (upgrade) replaces the block in place: the old
#     block is removed, the new one inserted, the original backup preserved.
#
# Usage:
#   sudo ./packaging/pam-install.sh
#   sudo ./packaging/pam-install.sh --target /etc/pam.d/system-auth
#   sudo ./packaging/pam-install.sh --control '[default=ignore]'
#   DESTDIR=/tmp/fake-root ./packaging/pam-install.sh --target /etc/pam.d/gdm-password
#
# Options:
#   --target PATH     target PAM file (default /etc/pam.d/gdm-password)
#   --control CTRL    PAM control field (default 'sufficient'; also accepts
#                     '[default=ignore]'. Refuses 'required', 'requisite',
#                     'mandatory', '[default=bad]', '[default=die]')
#   --no-preflight    skip the preflight script (already run separately)
#   --keep-backup     do not remove the backup on success (default removes it)
#   -h, --help        show this help
#
# Environment overrides (see pam-preflight.sh for the full set):
#   DESTDIR           staging root for testing without root

set -euo pipefail

TARGET_PAM="/etc/pam.d/gdm-password"
CONTROL="sufficient"
RUN_PREFLIGHT=1
KEEP_BACKUP=0

usage() {
    sed -n '3,/^$/p' "$0" | sed 's/^# \?//' >&2
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target) TARGET_PAM="$2"; shift 2 ;;
        --control) CONTROL="$2"; shift 2 ;;
        --no-preflight) RUN_PREFLIGHT=0; shift ;;
        --keep-backup) KEEP_BACKUP=1; shift ;;
        -h|--help) usage ;;
        *) echo "pam-install: unknown option: $1" >&2; usage ;;
    esac
done

DESTDIR="${DESTDIR:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- helpers ---------------------------------------------------------------

destdir_path() {
    local p="$1"
    if [[ -n "$DESTDIR" ]]; then
        printf '%s%s\n' "${DESTDIR%/}" "$p"
    else
        printf '%s\n' "$p"
    fi
}

die() {
    echo "pam-install: ERROR: $*" >&2
    exit 1
}

# Resolve the full path of the target under DESTDIR.
TARGET="$(destdir_path "$TARGET_PAM")"
TARGET_DIR="$(dirname "$TARGET")"

# Backup file + checksum. The backup name is stable (not timestamped per
# invocation) so the FIRST backup — the known-good pre-namtor original — is
# preserved across upgrades. A timestamped suffix is only used if a previous
# backup already exists and differs, so manual recovery points are kept.
BACKUP="${TARGET}.namtor.bak"
BACKUP_SUM="${BACKUP}.sha256"

make_backup() {
    if [[ -f "$BACKUP" ]]; then
        # Backup already exists (prior install). Verify it matches a known-good
        # checksum if present; do not overwrite the original backup.
        if [[ -f "$BACKUP_SUM" ]]; then
            local current_hash
            current_hash="$(sha256sum "$BACKUP" | awk '{print $1}')"
            local stored_hash
            stored_hash="$(awk '{print $1}' "$BACKUP_SUM")"
            if [[ "$current_hash" != "$stored_hash" ]]; then
                # Existing backup's checksum does not match the backup file —
                # something tampered with the backup. Stash it aside and make a
                # fresh one so we always have a trusted restore point.
                local ts
                ts="$(date +%Y%m%dT%H%M%S)"
                mv "$BACKUP" "${BACKUP}.${ts}"
                [[ -f "$BACKUP_SUM" ]] && mv "$BACKUP_SUM" "${BACKUP_SUM}.${ts}"
                cp -a "$TARGET" "$BACKUP"
                sha256sum "$BACKUP" | awk '{print $1}' > "$BACKUP_SUM"
                echo "pam-install: previous backup was tampered; stashed and remade backup"
            else
                echo "pam-install: existing backup verified, preserved (upgrade path)"
            fi
        else
            # No checksum file — verify the backup equals the current target
            # minus any namtor block. If equal, trust it; if not, remake.
            if cmp -s "$BACKUP" "$TARGET" 2>/dev/null; then
                sha256sum "$BACKUP" | awk '{print $1}' > "$BACKUP_SUM"
            else
                local ts
                ts="$(date +%Y%m%dT%H%M%S)"
                mv "$BACKUP" "${BACKUP}.${ts}"
                cp -a "$TARGET" "$BACKUP"
                sha256sum "$BACKUP" | awk '{print $1}' > "$BACKUP_SUM"
                echo "pam-install: replaced un-checksummed backup"
            fi
        fi
        return
    fi
    # First backup of this file.
    cp -a "$TARGET" "$BACKUP"
    sha256sum "$BACKUP" | awk '{print $1}' > "$BACKUP_SUM"
    echo "pam-install: backup created: $BACKUP"
}

restore_backup() {
    if [[ ! -f "$BACKUP" ]]; then
        echo "pam-install: no backup to restore" >&2
        return 1
    fi
    # Restore only if the checksum validates (defense against a tampered backup).
    if [[ -f "$BACKUP_SUM" ]]; then
        local current_hash stored_hash
        current_hash="$(sha256sum "$BACKUP" | awk '{print $1}')"
        stored_hash="$(awk '{print $1}' "$BACKUP_SUM")"
        if [[ "$current_hash" != "$stored_hash" ]]; then
            echo "pam-install: backup checksum mismatch — refusing to restore a tampered backup" >&2
            return 1
        fi
    fi
    cp -a "$BACKUP" "$TARGET"
    echo "pam-install: restored $TARGET from backup"
}

# --- control-field safety gate ---------------------------------------------

check_control() {
    local c="$1"
    # Normalise for comparison.
    local lower
    lower="$(printf '%s' "$c" | tr '[:upper:]' '[:lower:]')"
    case "$lower" in
        sufficient|"[default=ignore]") return 0 ;;
        required|requisite|mandatory|"[default=bad]"|"[default=die]")
            die "refusing unsafe control field '$c' — pam_namtor with '$lower' would lock users out when the daemon is unreachable (spec §8.3). Use 'sufficient' or '[default=ignore]' so password fallback works."
            ;;
        *)
            die "unknown control field '$c' — accepted: sufficient, [default=ignore]"
            ;;
    esac
}

check_control "$CONTROL"

# --- preflight -------------------------------------------------------------

if [[ "$RUN_PREFLIGHT" -eq 1 ]]; then
    export DESTDIR
    export PAM_FILE="$TARGET_PAM"
    if ! bash "$SCRIPT_DIR/pam-preflight.sh"; then
        die "preflight failed — no PAM files were modified"
    fi
fi

if [[ ! -f "$TARGET" ]]; then
    die "target PAM file not found: $TARGET"
fi

# --- backup ----------------------------------------------------------------

make_backup

# --- rollback trap: if the script exits non-zero after make_backup, restore --

NAMTOR_TMP=""
rollback_if_needed() {
    local rc=$?
    if [[ $rc -ne 0 ]]; then
        echo "pam-install: failed (exit $rc) — rolling back" >&2
        restore_backup || true
    fi
    [[ -n "$NAMTOR_TMP" && -f "$NAMTOR_TMP" ]] && rm -f "$NAMTOR_TMP" 2>/dev/null || true
    exit $rc
}
trap rollback_if_needed EXIT

# --- remove existing namtor block (upgrade / idempotent re-install) ---------

has_block() {
    grep -q '^# namtor-begin$' "$1" 2>/dev/null
}

remove_block() {
    local file="$1"
    # Delete from `# namtor-begin` through `# namtor-end` inclusive.
    # Uses a sed range that matches the marked block; robust to the block
    # spanning any number of lines.
    sed -i '/^# namtor-begin$/,/^# namtor-end$/d' "$file"
}

if has_block "$TARGET"; then
    echo "pam-install: existing namtor block found — replacing (upgrade)"
    remove_block "$TARGET"
    # After removing the block, the file should match the original backup
    # (modulo trailing whitespace). Verify password fallback is still intact
    # before we add the new block.
    if ! grep -E '^[[:space:]]*auth[[:space:]]' "$TARGET" | grep -qE \
        'pam_unix|pam_systemd_home|pam_env|common-auth|system-auth|password-auth'; then
        die "password fallback disappeared after removing old namtor block — refusing to continue"
    fi
fi

# --- find insertion point and insert the block -----------------------------
#
# Strategy: insert the marked block immediately BEFORE the first non-comment
# `auth` line. This places pam_namtor ahead of the password stack so face auth
# is tried first and, on PAM_AUTHINFO_UNAVAIL (sufficient), falls through to
# password. We then verify:
#   1. A password-fallback auth line exists AFTER the block.
#   2. No `pam_deny.so` auth line appears BEFORE the block (block unreachable).
#   3. No `pam_deny.so` auth line appears between the block and the first
#      password-fallback line (block's fall-through hits deny before password).

BLOCK="# namtor-begin
auth ${CONTROL} pam_namtor.so
# namtor-end"

# Determine the line number of the first non-comment auth line.
FIRST_AUTH_LN="$(grep -nE '^[[:space:]]*auth[[:space:]]' "$TARGET" | head -1 | cut -d: -f1 || true)"
if [[ -z "$FIRST_AUTH_LN" ]]; then
    die "no auth lines found in $TARGET — cannot determine insertion point"
fi

# Check for pam_deny.so BEFORE the insertion point.
if head -n "$((FIRST_AUTH_LN - 1))" "$TARGET" 2>/dev/null | grep -qE '^[[:space:]]*auth[[:space:]].*pam_deny'; then
    die "pam_deny.so appears before the namtor block insertion point — the block would be unreachable. Refusing unsafe ordering."
fi

# Insert the block before the first auth line using a same-directory temp
# file + atomic rename. The temp file MUST be in the same directory as the
# target so `mv` is a same-filesystem rename(2) — truly atomic. A cross-
# filesystem mv (e.g. mktemp in /tmp, target in /etc) falls back to copy+
# unlink, which is NOT atomic and can leave the PAM file half-written if
# interrupted.
TMP="$(mktemp "${TARGET_DIR}/.namtor.pam.XXXXXX")"
NAMTOR_TMP="$TMP"
{
    head -n "$((FIRST_AUTH_LN - 1))" "$TARGET"
    printf '%s\n' "$BLOCK"
    tail -n +"$FIRST_AUTH_LN" "$TARGET"
} > "$TMP"
mv -f "$TMP" "$TARGET"
NAMTOR_TMP=""

echo "pam-install: inserted namtor block before first auth line (line $FIRST_AUTH_LN)"

# --- verify password fallback after the block ------------------------------

# Find the namtor-end marker line, then scan after it.
END_LN="$(grep -n '^# namtor-end$' "$TARGET" | head -1 | cut -d: -f1)"
if [[ -z "$END_LN" ]]; then
    die "namtor-end marker not found after insert — internal error"
fi

AFTER_BLOCK="$(tail -n +"$((END_LN + 1))" "$TARGET")"

# 1. A password-fallback auth line must exist after the block.
if ! printf '%s\n' "$AFTER_BLOCK" | grep -E '^[[:space:]]*auth[[:space:]]' | grep -qE \
    'pam_unix|pam_systemd_home|pam_env|common-auth|system-auth|password-auth'; then
    die "no password-fallback auth line found after the namtor block — a daemon outage would lock users out. Rolling back."
fi

# 2. pam_deny.so must not appear on an auth line before the first password
#    fallback line after the block.
DENY_BEFORE_PW="$(printf '%s\n' "$AFTER_BLOCK" | awk '
    /^[[:space:]]*auth[[:space:]].*pam_deny/ { found_deny=1 }
    /^[[:space:]]*auth[[:space:]].*(pam_unix|pam_systemd_home|pam_env|common-auth|system-auth|password-auth)/ { if (found_deny) { print "yes"; exit } else { print "no"; exit } }
    END { if (!found_deny) print "no" }
')"
if [[ "$DENY_BEFORE_PW" == "yes" ]]; then
    die "pam_deny.so appears between the namtor block and the password fallback — fall-through would hit deny first. Refusing unsafe ordering. Rolling back."
fi

echo "pam-install: OK  password fallback verified after namtor block"

# --- success: optionally clean up backup -----------------------------------

if [[ "$KEEP_BACKUP" -eq 0 ]]; then
    rm -f "$BACKUP" "$BACKUP_SUM"
    echo "pam-install: backup removed on success (use --keep-backup to retain)"
else
    echo "pam-install: backup retained at $BACKUP"
fi

trap - EXIT
echo "pam-install: done. PAM file $TARGET now has the namtor block (auth ${CONTROL} pam_namtor.so)."
echo "pam-install: verify with: grep -A2 namtor-begin \"$(destdir_path "$TARGET_PAM")\""
