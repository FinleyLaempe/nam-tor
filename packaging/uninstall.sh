#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor generic uninstall — manifest-complete reverse of stage.sh.
#
# Usage: packaging/uninstall.sh [--dry-run] [SRCDIR]
#
#   --dry-run  print what would be removed without removing anything.
#              NOTE: dry-run is NOT completion evidence — it proves the
#              script's plan, not the actual closure.
#   SRCDIR     repo root containing packaging/install-manifest.toml. Defaults
#              to the directory of this script (packaging/).
#
# Reads packaging/install-manifest.toml (via python3 tomllib, same as
# stage.sh) and reverses every declared path:
#   1. Disable PAM BEFORE deleting the PAM module (namtor-pam-edit disable-all
#      → lifecycle.sh pre-remove → grep+warn fallback).
#   2. Stop/disable the systemd units if running (real root only).
#   3. Remove every file declared under [[files]] (reverse sorted for
#      determinism; symlinks use `unlink`).
#   4. Remove every [[generated_state]] path whose on_purge = "remove".
#   5. Remove /var/lib/namtor and /run/namtor (catch-all for runtime cruft
#      that may exist outside the manifest).
#   6. rmdir every [[dirs]] path (rmdir --ignore-fail-on-non-empty, reverse
#      sorted; never recursive — only empty dirs are removed).
#   7. Remove the namtor user, namtor group, and namtor-auth group (only if
#      they exist; never touch unrelated group memberships).
#
# DESTDIR env var (if set) is prepended to every absolute path, enabling
# fake-root testing without root.
#
# Idempotent: re-running after a partial uninstall completes cleanly — every
# removal is guarded by an existence check or `rm -f` / `rmdir ... || true`.
#
# Safety contract:
#   - NEVER touch stock PAM files beyond the marked namtor block (the
#     transactional namtor-pam-edit disable-all removes only the marked
#     # namtor-begin / # namtor-end block).
#   - NEVER remove files not declared in the manifest (no recursive rm of
#     parent dirs like /usr/share/applications).
#   - NEVER remove unrelated group memberships (e.g. gdm in video) — only
#     the namtor user and namtor / namtor-auth groups are removed.
#   - The only recursive rm -rf targets are /var/lib/namtor and /run/namtor
#     (Nam-tor-owned runtime state; the manifest's generated_state paths are
#     nested under /var/lib/namtor).

set -uo pipefail

DRY_RUN=0
SRCDIR=""

# --- argument parsing -------------------------------------------------------

args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" >&2
            echo "usage: $0 [--dry-run] [SRCDIR]" >&2
            exit 0
            ;;
        *) args+=("$1"); shift ;;
    esac
done
set -- "${args[@]}"

if [[ $# -ge 1 ]]; then
    SRCDIR="$1"
else
    SRCDIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

# DESTDIR: prepend to every absolute path. Normalise: strip trailing slash
# unless it is "/" (empty DESTDIR means real root).
DESTDIR="${DESTDIR:-}"
if [[ -n "$DESTDIR" && "$DESTDIR" != "/" ]]; then
    DESTDIR="${DESTDIR%/}"
fi
export DESTDIR

# --- manifest resolution ----------------------------------------------------

# Try, in order: $DESTDIR/usr/share/namtor/install-manifest.toml (installed
# copy on the target root), then $SRCDIR/install-manifest.toml (repo copy).
manifest=""
for candidate in \
    "${DESTDIR:+$DESTDIR/usr/share/namtor/install-manifest.toml}" \
    "$SRCDIR/install-manifest.toml"; do
    if [[ -n "$candidate" && -f "$candidate" ]]; then
        manifest="$candidate"
        break
    fi
done

if [[ -z "$manifest" ]]; then
    echo ":: namtor-uninstall: FATAL: install-manifest.toml not found" >&2
    echo "  tried: ${DESTDIR:+$DESTDIR/usr/share/namtor/install-manifest.toml, }$SRCDIR/install-manifest.toml" >&2
    exit 1
fi

echo ":: namtor-uninstall: manifest=$manifest"
if [[ -n "$DESTDIR" ]]; then
    echo ":: namtor-uninstall: DESTDIR=$DESTDIR (fake-root mode)"
fi
if [[ $DRY_RUN -eq 1 ]]; then
    echo ":: namtor-uninstall: DRY-RUN — no files will be removed"
fi

# --- helpers ----------------------------------------------------------------

# rebase <abs_path> → $DESTDIR + abs_path
rebase() {
    local p="$1"
    if [[ -n "$DESTDIR" ]]; then
        printf '%s%s' "$DESTDIR" "$p"
    else
        printf '%s' "$p"
    fi
}

# remove_file <abs_path>  — rm -f under DESTDIR; honours --dry-run.
remove_file() {
    local path
    path="$(rebase "$1")"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: rm -f $path"
        return 0
    fi
    if [[ -e "$path" || -L "$path" ]]; then
        rm -f "$path"
    fi
}

# remove_dir_recursive <abs_path> — rm -rf under DESTDIR; honours --dry-run.
# Only used for Nam-tor-owned runtime state (/var/lib/namtor, /run/namtor).
remove_dir_recursive() {
    local path
    path="$(rebase "$1")"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: rm -rf $path"
        return 0
    fi
    if [[ -e "$path" ]]; then
        rm -rf "$path"
    fi
}

# rmdir_empty <abs_path> — rmdir --ignore-fail-on-non-empty; honours --dry-run.
rmdir_empty() {
    local path
    path="$(rebase "$1")"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: rmdir --ignore-fail-on-non-empty $path"
        return 0
    fi
    if [[ -d "$path" ]]; then
        rmdir --ignore-fail-on-non-empty "$path" 2>/dev/null || true
    fi
}

# is_real_root — true when DESTDIR is empty (operating on /).
is_real_root() {
    [[ -z "$DESTDIR" ]]
}

# --- 1. disable PAM BEFORE deleting the module ------------------------------

# PAM must be disabled (the marked # namtor-begin / # namtor-end block removed
# from every registry-enabled /etc/pam.d/* service) BEFORE the pam_namtor.so
# binary is deleted — otherwise a PAM service would reference a missing module
# on the next auth attempt.
#
# Order of preference:
#   a) namtor-pam-edit disable-all (the transactional engine; removes only the
#      marked block from every registry-enabled service). Looked up at
#      $DESTDIR/usr/lib/namtor/namtor-pam-edit (staged) or
#      $SRCDIR/../target/release/namtor-pam-edit (build dir).
#   b) lifecycle.sh pre-remove (delegates to namtor-pam-edit disable-all +
#      stops the daemon). Looked up at $DESTDIR/usr/share/namtor/lifecycle.sh
#      or $SRCDIR/lifecycle.sh.
#   c) grep + warn fallback: scan $DESTDIR/etc/pam.d/* for `pam_namtor` and
#      warn the operator to remove the marked block manually. NEVER auto-edit
#      with `sed -i '/pam_namtor/d'` — that would delete ALL pam_namtor lines,
#      not just the marked block, and could corrupt the PAM stack.

pam_disabled=0
pam_edit_bin=""
for candidate in \
    "$(rebase /usr/lib/namtor/namtor-pam-edit)" \
    "$SRCDIR/../target/release/namtor-pam-edit"; do
    if [[ -x "$candidate" ]]; then
        pam_edit_bin="$candidate"
        break
    fi
done

lifecycle_script=""
for candidate in \
    "$(rebase /usr/share/namtor/lifecycle.sh)" \
    "$SRCDIR/lifecycle.sh"; do
    if [[ -x "$candidate" ]]; then
        lifecycle_script="$candidate"
        break
    fi
done

if [[ -n "$pam_edit_bin" ]]; then
    echo ":: namtor-uninstall: disabling PAM via $pam_edit_bin disable-all"
    pamd_root="$(rebase /etc/pam.d)"
    reg_root="$(rebase /var/lib/namtor)"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: $pam_edit_bin disable-all --pamd-root $pamd_root --registry-root $reg_root"
    else
        if "$pam_edit_bin" disable-all --pamd-root "$pamd_root" --registry-root "$reg_root" 2>&1 \
            | sed 's/^/  pam-edit: /'; then
            pam_disabled=1
        else
            echo ":: namtor-uninstall: WARN: pam-edit disable-all failed (continuing)" >&2
        fi
    fi
elif [[ -n "$lifecycle_script" ]]; then
    echo ":: namtor-uninstall: disabling PAM via $lifecycle_script pre-remove"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: $lifecycle_script pre-remove --root ${DESTDIR:-/}"
    else
        if "$lifecycle_script" pre-remove --root "${DESTDIR:-/}" 2>&1 \
            | sed 's/^/  lifecycle: /'; then
            pam_disabled=1
        else
            echo ":: namtor-uninstall: WARN: lifecycle pre-remove failed (continuing)" >&2
        fi
    fi
else
    # grep+warn fallback — NEVER auto-edit without the transactional engine.
    pamd_dir="$(rebase /etc/pam.d)"
    if [[ -d "$pamd_dir" ]]; then
        hits=()
        while IFS= read -r -d '' f; do
            if grep -q 'pam_namtor' "$f" 2>/dev/null; then
                hits+=("$f")
            fi
        done < <(find "$pamd_dir" -maxdepth 1 -type f -print0 2>/dev/null)
        if [[ ${#hits[@]} -gt 0 ]]; then
            echo ":: namtor-uninstall: WARN: pam_namtor referenced in:" >&2
            for f in "${hits[@]}"; do
                echo "    $f" >&2
            done
            echo ":: namtor-uninstall: WARN: remove the marked '# namtor-begin' .. '# namtor-end' block manually" >&2
            echo ":: namtor-uninstall: WARN: (namtor-pam-edit not found; cannot use the transactional engine)" >&2
        else
            pam_disabled=1
        fi
    else
        pam_disabled=1
    fi
fi

if [[ $pam_disabled -eq 1 ]]; then
    echo ":: namtor-uninstall: PAM disabled"
fi

# --- 2. stop/disable systemd units (real root only) -------------------------

if is_real_root && command -v systemctl >/dev/null 2>&1; then
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: systemctl --no-reload disable --now namtor-daemon.socket namtor-daemon.service"
    else
        systemctl --no-reload disable --now namtor-daemon.socket namtor-daemon.service 2>/dev/null || true
    fi
fi

# --- 3-6. manifest-driven removal -------------------------------------------

# Parse install-manifest.toml and emit three newline-separated path lists
# to a temp file, separated by form-feed (\f) marker lines:
#   section 1 — [[files]] dest (reverse sorted, longest first so children
#               go before parents)
#   section 2 — [[dirs]] path (reverse sorted)
#   section 3 — [[generated_state]] path where on_purge == "remove"
# The bash side reads the temp file and dispatches each section.

parse_tmp="$(mktemp)"
trap 'rm -f "$parse_tmp"' EXIT

python3 - "$manifest" >"$parse_tmp" <<'PYEOF'
import sys, tomllib
manifest_path = sys.argv[1]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
files = sorted((f["dest"] for f in m.get("files", [])), reverse=True)
dirs = sorted((d["path"] for d in m.get("dirs", [])), reverse=True)
genstate = [g["path"] for g in m.get("generated_state", [])
            if g.get("on_purge") == "remove"]
out = sys.stdout
out.write("\f\n")
for p in files:
    out.write(p + "\n")
out.write("\f\n")
for p in dirs:
    out.write(p + "\n")
out.write("\f\n")
for p in genstate:
    out.write(p + "\n")
PYEOF

# Read the three sections from the temp file.
files_list=()
dirs_list=()
genstate_list=()
section=0
while IFS= read -r line; do
    if [[ "$line" == $'\f' ]]; then
        section=$((section + 1))
        continue
    fi
    [[ -z "$line" ]] && continue
    case $section in
        0) ;;  # preamble before first marker (none expected)
        1) files_list+=("$line") ;;
        2) dirs_list+=("$line") ;;
        3) genstate_list+=("$line") ;;
    esac
done <"$parse_tmp"
rm -f "$parse_tmp"
trap - EXIT

# 3. Remove declared files (reverse sorted).
echo ":: namtor-uninstall: removing ${#files_list[@]} declared file(s)"
for p in "${files_list[@]}"; do
    remove_file "$p"
done

# 4. Remove generated_state paths with on_purge = "remove".
echo ":: namtor-uninstall: purging ${#genstate_list[@]} generated-state path(s)"
for p in "${genstate_list[@]}"; do
    # generated_state entries may be files or dirs; rm -rf covers both
    # and is safe because every generated_state path is Nam-tor-owned
    # (under /var/lib/namtor — verified by the manifest schema).
    remove_dir_recursive "$p"
done

# 5. Catch-all for runtime cruft under /var/lib/namtor and /run/namtor.
# The manifest's generated_state entries are nested under these, but the
# daemon may have created additional files (lifecycle receipts, PID files,
# the auth socket). rm -rf is safe — both paths are Nam-tor-owned.
echo ":: namtor-uninstall: purging runtime state /var/lib/namtor + /run/namtor"
remove_dir_recursive "/var/lib/namtor"
remove_dir_recursive "/run/namtor"

# 6. rmdir declared directories (empty only; reverse sorted so children
#    go before parents — rmdir --ignore-fail-on-non-empty leaves shared
#    dirs like /usr/share/applications untouched).
echo ":: namtor-uninstall: rmdir ${#dirs_list[@]} declared dir(s)"
for p in "${dirs_list[@]}"; do
    rmdir_empty "$p"
done

# --- 7. remove user + groups (real root only) -------------------------------

# Only the namtor user and the namtor / namtor-auth groups are removed.
# Unrelated group memberships (e.g. gdm in video) are NEVER touched.
if is_real_root; then
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: userdel namtor (if exists); groupdel namtor namtor-auth (if exist)"
    else
        if command -v getent >/dev/null 2>&1 && getent passwd namtor >/dev/null 2>&1; then
            userdel namtor 2>/dev/null || true
        fi
        if command -v getent >/dev/null 2>&1 && getent group namtor >/dev/null 2>&1; then
            groupdel namtor 2>/dev/null || true
        fi
        if command -v getent >/dev/null 2>&1 && getent group namtor-auth >/dev/null 2>&1; then
            groupdel namtor-auth 2>/dev/null || true
        fi
    fi
fi

# --- daemon-reload (real root only) -----------------------------------------

if is_real_root && command -v systemctl >/dev/null 2>&1; then
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "  dry-run: systemctl daemon-reload"
    else
        systemctl daemon-reload 2>/dev/null || true
    fi
fi

echo ":: namtor-uninstall: done"
exit 0
