#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor PAM preflight — run BEFORE touching any PAM file.
#
# Verifies the conditions for a safe transactional PAM install. Exits non-zero
# with a specific message if any FATAL check fails. Warns (exit 0) on soft
# checks that may legitimately not hold yet (daemon not started, group not
# created) during first install or test runs.
#
# Usage:
#   sudo ./packaging/pam-preflight.sh
#   DESTDIR=/tmp/fake-root PAM_FILE=/etc/pam.d/gdm-password ./packaging/pam-preflight.sh
#
# Environment overrides (all have safe defaults):
#   DESTDIR             — staging root for testing without root (default "")
#   PAM_FILE            — target PAM stack file (default /etc/pam.d/gdm-password)
#   PAM_MODULE          — pam_namtor.so install path (default: first of /usr/lib/security, /usr/lib/*/security, /usr/lib64/security that has it)
#   SOCKET_PATH         — daemon socket (default /run/namtor/auth.sock)
#   NAMTOR_AUTH_GROUP   — group controlling socket ACL (default namtor-auth)
#   DM_USER             — display-manager user that must be in the auth group (default gdm)
#   SKIP_SOFT_CHECKS    — set to 1 to suppress soft warnings (default 0)

set -euo pipefail

# `grep_all` is `grep` that always reads its whole input.
#
# `cmd | grep -q PAT` under `pipefail` reports the WRITER's status when grep
# exits early and the writer dies of SIGPIPE, so a match can be read as a miss.
# Here that meant a machine WITH models could be reported as having none once
# `find`'s output outgrew the pipe buffer. Without -q grep never exits early,
# so nothing upstream is signalled; the exit status is unchanged.
grep_all() { grep "$@" >/dev/null; }


DESTDIR="${DESTDIR:-}"
PAM_FILE="${PAM_FILE:-/etc/pam.d/gdm-password}"
PAM_MODULE="${PAM_MODULE:-}"
SOCKET_PATH="${SOCKET_PATH:-/run/namtor/auth.sock}"
NAMTOR_AUTH_GROUP="${NAMTOR_AUTH_GROUP:-namtor-auth}"
DM_USER="${DM_USER:-gdm}"
SKIP_SOFT_CHECKS="${SKIP_SOFT_CHECKS:-0}"

# Resolve a system path under DESTDIR. Leading slash is preserved so
# "DESTDIR=/tmp/r /etc/pam.d/x" becomes "/tmp/r/etc/pam.d/x".
destdir_path() {
    local p="$1"
    if [[ -n "$DESTDIR" ]]; then
        printf '%s%s\n' "${DESTDIR%/}" "$p"
    else
        printf '%s\n' "$p"
    fi
}

# Default module location differs per distro (Arch /usr/lib/security, Debian
# /usr/lib/<triplet>/security, Fedora /usr/lib64/security); take the first that
# exists, falling back to the Arch path so the "missing" message stays specific.
if [[ -z "$PAM_MODULE" ]]; then
    PAM_MODULE=/usr/lib/security/pam_namtor.so
    for _c in /usr/lib/security /usr/lib/*/security /usr/lib64/security; do
        if [[ -e "$(destdir_path "$_c/pam_namtor.so")" ]]; then
            PAM_MODULE="$_c/pam_namtor.so"; break
        fi
    done
fi

fatal() {
    echo "pam-preflight: FATAL: $*" >&2
    exit 1
}

warn() {
    if [[ "$SKIP_SOFT_CHECKS" != "1" ]]; then
        echo "pam-preflight: WARNING: $*" >&2
    fi
}

echo "pam-preflight: running pre-install checks (DESTDIR='${DESTDIR:-/}')"

# ---------------------------------------------------------------------------
# FATAL 1 — root, unless DESTDIR is set (test mode)
# ---------------------------------------------------------------------------
if [[ -z "$DESTDIR" ]]; then
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
        fatal "must run as root (or set DESTDIR for test mode)"
    fi
fi

# ---------------------------------------------------------------------------
# FATAL 2 — target PAM file exists
# ---------------------------------------------------------------------------
TARGET="$(destdir_path "$PAM_FILE")"
if [[ ! -f "$TARGET" ]]; then
    fatal "target PAM file not found: $TARGET"
fi
echo "pam-preflight: OK  target PAM file present: $TARGET"

# ---------------------------------------------------------------------------
# FATAL 3 — pam_namtor.so is built and installed at the expected path
# ---------------------------------------------------------------------------
MODULE="$(destdir_path "$PAM_MODULE")"
if [[ ! -f "$MODULE" ]]; then
    fatal "pam_namtor.so not found: $MODULE — build with 'cargo build --release -p namtor-pam' and install first"
fi
echo "pam-preflight: OK  pam_namtor.so present: $MODULE"

# ---------------------------------------------------------------------------
# FATAL 4 — target PAM file already has a password-fallback line that the
# namtor block can fall through to. If there is no pam_unix/systemd_home/
# common-auth/system-auth/password-auth auth line, installing pam_namtor
# would leave no fallback and a daemon outage would lock users out.
# ---------------------------------------------------------------------------
if ! grep -qE '^[[:space:]]*auth[[:space:]]' "$TARGET"; then
    fatal "target PAM file has no 'auth' lines at all — refusing to install into a file we do not understand"
fi

# Password fallback = any auth line referencing a known password module or a
# known distro include that itself provides password auth.
if ! grep -E '^[[:space:]]*auth[[:space:]]' "$TARGET" | grep_all -E \
    'pam_unix|pam_systemd_home|pam_env|common-auth|system-auth|password-auth'; then
    fatal "no password-fallback auth line found in $TARGET — installing pam_namtor with no password fallback would lock users out on daemon failure"
fi
echo "pam-preflight: OK  password fallback present in target"

# ---------------------------------------------------------------------------
# SOFT 1 — daemon socket exists (daemon may simply not be started yet)
# ---------------------------------------------------------------------------
SOCK="$(destdir_path "$SOCKET_PATH")"
if [[ ! -S "$SOCK" ]]; then
    warn "daemon socket not found: $SOCK (start namtor-daemon.socket after install)"
else
    echo "pam-preflight: OK  daemon socket present: $SOCK"
fi

# ---------------------------------------------------------------------------
# SOFT 2 — namtor-auth group exists (created by systemd-sysusers; may not
# have run yet during a fresh DESTDIR test or before sysusers drop-in install)
# ---------------------------------------------------------------------------
if [[ -z "$DESTDIR" ]]; then
    if ! getent group "$NAMTOR_AUTH_GROUP" >/dev/null 2>&1; then
        warn "group '$NAMTOR_AUTH_GROUP' does not exist — run 'systemd-sysusers /usr/lib/sysusers.d/namtor.conf' after install"
    else
        echo "pam-preflight: OK  group '$NAMTOR_AUTH_GROUP' exists"
    fi
fi

# ---------------------------------------------------------------------------
# SOFT 3 — display-manager user is in namtor-auth (so GDM can connect)
# ---------------------------------------------------------------------------
if [[ -z "$DESTDIR" ]]; then
    if getent group "$NAMTOR_AUTH_GROUP" >/dev/null 2>&1; then
        if ! getent group "$NAMTOR_AUTH_GROUP" | grep_all -E "(^|,)$DM_USER(,|$)"; then
            warn "user '$DM_USER' is not in group '$NAMTOR_AUTH_GROUP' — run 'gpasswd -a $DM_USER $NAMTOR_AUTH_GROUP'"
        else
            echo "pam-preflight: OK  '$DM_USER' is in '$NAMTOR_AUTH_GROUP'"
        fi
    fi
fi

echo "pam-preflight: all fatal checks passed"
