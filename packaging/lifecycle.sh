#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor package-independent deterministic lifecycle helper.
# SIZE_OK: single 6-phase lifecycle state machine; phase functions are 4-8
# lines each over shared validation/receipt/pam-edit helpers. Splitting would
# move lines between sourced fragments without reducing conceptual surface.
#
# Single source of truth for the install/upgrade/remove phases that every
# distro package (Arch .install, Fedora .spec, Debian maintainer scripts)
# delegates to. Typed phases:
#
#   pre-install    register sysusers + tmpfiles; verify prerequisite config
#                  files are staged under --root. Never edits PAM.
#   post-install   validate model layout + sha256 hashes against the model
#                  manifest, verify identities/dirs, socket + D-Bus service
#                  units, PAM module. Update desktop caches. Emit receipt.
#                  Never edits PAM (PAM is default-off; Task 13).
#   pre-upgrade    snapshot current PAM state via `namtor-pam-edit snapshot`.
#                  Stop the daemon. Never edits PAM lines.
#   post-upgrade   migrate PAM entries via `namtor-pam-edit migrate`,
#                  re-validate model layout/hashes + services. Emit receipt.
#   pre-remove     disable all Nam-tor PAM services via
#                  `namtor-pam-edit disable-all`. Stop the daemon.
#   post-remove    purge all Nam-tor PAM state via `namtor-pam-edit purge`.
#                  Remove runtime dirs. Emit receipt.
#
# Usage:
#   lifecycle.sh <phase> [--root <dir>] [--manifest <path>]
#                        [--pam-edit <path>] [--receipt-dir <dir>]
#                        [--version <ver>]
#
#   phase         one of the six typed phases above.
#   --root        fake-root / DESTDIR. Default /. When non-/, root-privilege
#                 checks are skipped and systemd invocations are skipped
#                 (best-effort) so the helper is fake-root testable.
#   --manifest    path to model-manifest.toml. Default: resolved from
#                 <root>/usr/share/namtor/model-manifest.toml, falling back
#                 to the repo's packaging/model-manifest.toml.
#   --pam-edit    path to the PAM editor. Default: `<root>/usr/bin/namtor
#                 pam-edit`, falling back to a standalone
#                 <root>/usr/lib/namtor/namtor-pam-edit. Missing binary =
#                 best-effort skip (the phase still validates non-PAM state).
#   --receipt-dir directory for receipt files. Default:
#                 <root>/var/lib/namtor/lifecycle.
#   --version     package version stamped into receipts. Default: unknown.
#
# Design contract:
#   - Idempotent: running the same phase twice produces the same result
#     (same exit code, same validation outcome, receipt overwritten).
#   - Fake-root testable: every path is rebased under --root; systemd /
#     desktop-cache helpers are skipped when --root is set or the helper is
#     missing, so the suite runs without root or a running systemd.
#   - Mandatory failures return nonzero: missing model, missing sysuser
#     config, missing systemd unit, missing D-Bus service, missing PAM
#     module, tampered model (sha256 mismatch).
#   - Never edits PAM on install: pre-install and post-install never call
#     namtor-pam-edit and never write under /etc/pam.d.
#   - Never calls nonexistent CLI: the helper does not invoke model-fetch
#     subcommands or the user-facing CLI for model management.
#   - No PAM default target: the helper never names a specific PAM service.

set -uo pipefail

PHASE=""
ROOT="/"
MANIFEST=""
PAM_EDIT=""
RECEIPT_DIR=""
VERSION="unknown"

usage() {
    sed -n '2,/^$/p' "$0" >&2
    echo "usage: $0 <phase> [--root <dir>] [--manifest <path>] [--pam-edit <path>] [--receipt-dir <dir>] [--version <ver>]" >&2
}

# --- argument parsing -------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --root)        ROOT="$2"; shift 2 ;;
        --manifest)    MANIFEST="$2"; shift 2 ;;
        --pam-edit)    PAM_EDIT="$2"; shift 2 ;;
        --receipt-dir) RECEIPT_DIR="$2"; shift 2 ;;
        --version)     VERSION="$2"; shift 2 ;;
        -h|--help)     usage; exit 0 ;;
        --*)           echo ":: namtor-lifecycle: unknown option: $1" >&2; usage; exit 2 ;;
        *)
            if [[ -z "$PHASE" ]]; then
                PHASE="$1"; shift
            else
                echo ":: namtor-lifecycle: unexpected argument: $1" >&2; exit 2
            fi
            ;;
    esac
done

if [[ -z "$PHASE" ]]; then
    echo ":: namtor-lifecycle: phase required" >&2
    usage
    exit 2
fi

case "$PHASE" in
    pre-install|post-install|pre-upgrade|post-upgrade|pre-remove|post-remove) ;;
    *)
        echo ":: namtor-lifecycle: unknown phase: $PHASE" >&2
        echo "  expected: pre-install|post-install|pre-upgrade|post-upgrade|pre-remove|post-remove" >&2
        exit 2
        ;;
esac

# Normalise ROOT: strip trailing slash unless it is "/".
if [[ "$ROOT" != "/" ]]; then
    ROOT="${ROOT%/}"
fi
IS_FAKE_ROOT=0
if [[ "$ROOT" != "/" ]]; then
    IS_FAKE_ROOT=1
fi

# Defaults that depend on ROOT.
#
# The PAM editor ships as a subcommand of the multi-call binary. This used to
# default to <root>/usr/lib/namtor/namtor-pam-edit, which the install manifest
# has never shipped, so on every real system call_pam_edit took its
# warn-and-skip branch: `pre-remove disable-all` and `post-remove purge` never
# ran, and removing the package left pam_namtor.so entries in /etc/pam.d
# pointing at a module that had just been deleted.
#
# PAM_EDIT_ARGV is an array because the default is two words.
if [[ -n "$PAM_EDIT" ]]; then
    PAM_EDIT_ARGV=("$PAM_EDIT")
elif [[ -x "$ROOT/usr/bin/namtor" ]]; then
    PAM_EDIT_ARGV=("$ROOT/usr/bin/namtor" pam-edit)
else
    PAM_EDIT_ARGV=("$ROOT/usr/lib/namtor/namtor-pam-edit")
fi
[[ -z "$RECEIPT_DIR" ]] && RECEIPT_DIR="$ROOT/var/lib/namtor/lifecycle"

# Manifest resolution: explicit > staged under --root. On a real root only,
# fall back to the repo-relative manifest (packaging/model-manifest.toml) so
# source-tree runs still validate hashes. Fake-root runs never use the repo
# fallback — otherwise the real manifest's hashes would mismatch the test's
# fixture weights and the presence-only path could never be exercised.
resolve_manifest() {
    if [[ -n "$MANIFEST" && -f "$MANIFEST" ]]; then
        printf '%s' "$MANIFEST"
        return
    fi
    local staged="$ROOT/usr/share/namtor/model-manifest.toml"
    if [[ -f "$staged" ]]; then
        printf '%s' "$staged"
        return
    fi
    if [[ $IS_FAKE_ROOT -eq 0 ]]; then
        local script_dir
        script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
        local repo="$script_dir/model-manifest.toml"
        if [[ -f "$repo" ]]; then
            printf '%s' "$repo"
            return
        fi
    fi
    printf ''
}

# --- logging ----------------------------------------------------------------

log_info()  { echo ":: namtor-lifecycle: $*" >&2; }
log_warn()  { echo ":: namtor-lifecycle: WARN: $*" >&2; }
log_fatal() { echo ":: namtor-lifecycle: FATAL: $*" >&2; }

# Track check counts for the receipt.
CHECKS_TOTAL=0
CHECKS_PASSED=0
CHECKS_FAILED=0
FAILURES=()
PAM_EDITED=false
MODELS_VALIDATED=""

record_pass() { CHECKS_TOTAL=$((CHECKS_TOTAL + 1)); CHECKS_PASSED=$((CHECKS_PASSED + 1)); }
record_fail() {
    CHECKS_TOTAL=$((CHECKS_TOTAL + 1))
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    FAILURES+=("$1")
    log_fatal "$1"
}

# --- mandatory file/dir checks ----------------------------------------------

# check_file <abs_path> <label>
check_file() {
    local path="$1" label="$2"
    if [[ -f "$path" ]]; then
        record_pass
    else
        record_fail "$label missing: $path"
    fi
}

# check_dir <abs_path> <label>
check_dir() {
    local path="$1" label="$2"
    if [[ -d "$path" ]]; then
        record_pass
    else
        record_fail "$label missing: $path"
    fi
}

# check_nonempty_file <abs_path> <label>  (file must exist and be non-empty)
check_nonempty_file() {
    local path="$1" label="$2"
    if [[ -s "$path" ]]; then
        record_pass
    else
        record_fail "$label missing or empty: $path"
    fi
}

# --- model layout + hash validation -----------------------------------------

# Parse model-manifest.toml with awk (no python3 dependency at runtime) and
# emit lines "id|quant|sha256" for every variant. The TOML structure is
# regular (see packaging/model-manifest.toml); awk extracts the three fields
# by quote-stripping the `id`, `quant`, and `sha256` assignments.
parse_manifest_variants() {
    local manifest="$1"
    [[ -f "$manifest" ]] || return 0
    awk '
        BEGIN { model=""; inv=0 }
        /^[[:space:]]*\[\[models\]\]/ { model=""; inv=0; next }
        /^[[:space:]]*\[\[models\.variants\]\]/ { inv=1; next }
        /^[[:space:]]*id[[:space:]]*=/ && !inv {
            v=$0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); model=v; next
        }
        /^[[:space:]]*quant[[:space:]]*=/ {
            v=$0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); quant=v; next
        }
        /^[[:space:]]*sha256[[:space:]]*=/ {
            v=$0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); sha=v
            if (model != "" && quant != "") print model "|" quant "|" sha
            next
        }
    ' "$manifest"
}

# validate_models <models_dir> <manifest>
# For each manifest variant, require an onnx file at <models>/<id>.onnx OR
# <models>/<id>/<quant>.onnx and verify its sha256. Missing or mismatched
# files are mandatory failures.
validate_models() {
    local models_dir="$1" manifest="$2"
    local validated=()
    local have_manifest=1
    if [[ ! -f "$manifest" ]]; then
        have_manifest=0
        log_warn "model manifest not found at $manifest — presence-only validation"
    fi

    if [[ $have_manifest -eq 1 ]]; then
        local line id quant expected file sha
        while IFS='|' read -r id quant expected; do
            [[ -z "$id" ]] && continue
            local top="$models_dir/$id.onnx"
            local lay="$models_dir/$id/$quant.onnx"
            local found=""
            if [[ -f "$top" ]]; then
                found="$top"
            elif [[ -f "$lay" ]]; then
                found="$lay"
            else
                record_fail "model $id ($quant) missing onnx under $models_dir"
                continue
            fi
            sha="$(sha256sum "$found" | awk '{print $1}')"
            if [[ "$sha" == "$expected" ]]; then
                record_pass
                validated+=("$id:$quant")
            else
                record_fail "model $id ($quant) hash mismatch (expected $expected, got $sha)"
            fi
        done < <(parse_manifest_variants "$manifest")
    else
        # Presence-only: at least one .onnx under models_dir.
        if find "$models_dir" -name '*.onnx' -type f 2>/dev/null | grep -q .; then
            record_pass
            validated+=("presence-only")
        else
            record_fail "no ONNX models under $models_dir"
        fi
    fi
    MODELS_VALIDATED="$(IFS=,; printf '%s' "${validated[*]}")"
}

# --- systemd / sysusers / tmpfiles helpers ----------------------------------

# run_if_real <cmd...>  — execute only on a real root (IS_FAKE_ROOT=0); skip
# silently on fake-root. Used for systemd-sysusers, systemctl, etc.
run_if_real() {
    if [[ $IS_FAKE_ROOT -eq 1 ]]; then
        return 0
    fi
    command -v "$1" >/dev/null 2>&1 || return 0
    "$@" 2>/dev/null || true
}

register_sysusers() {
    local cfg="$ROOT/usr/lib/sysusers.d/namtor.conf"
    if [[ $IS_FAKE_ROOT -eq 0 && -f "$cfg" ]] && command -v systemd-sysusers >/dev/null 2>&1; then
        systemd-sysusers "$cfg" 2>/dev/null || log_warn "systemd-sysusers failed"
    fi
}

register_tmpfiles() {
    local cfg="$ROOT/usr/lib/tmpfiles.d/namtor.conf"
    if [[ $IS_FAKE_ROOT -eq 0 && -f "$cfg" ]] && command -v systemd-tmpfiles >/dev/null 2>&1; then
        systemd-tmpfiles --create "$cfg" 2>/dev/null || log_warn "systemd-tmpfiles --create failed"
    fi
}

enable_socket() {
    if [[ $IS_FAKE_ROOT -eq 0 ]] && command -v systemctl >/dev/null 2>&1; then
        systemctl enable --now namtor-daemon.socket 2>/dev/null || true
    fi
}

disable_daemon() {
    if [[ $IS_FAKE_ROOT -eq 0 ]] && command -v systemctl >/dev/null 2>&1; then
        systemctl --no-reload disable --now namtor-daemon.socket namtor-daemon.service 2>/dev/null || true
    fi
}

update_desktop_caches() {
    if [[ $IS_FAKE_ROOT -eq 0 ]]; then
        command -v update-desktop-database >/dev/null 2>&1 && \
            update-desktop-database -q "$ROOT/usr/share/applications" 2>/dev/null || true
        command -v gtk-update-icon-cache >/dev/null 2>&1 && \
            gtk-update-icon-cache -q "$ROOT/usr/share/icons/hicolor" 2>/dev/null || true
    fi
}

# --- namtor-pam-edit subprocess ---------------------------------------------

# call_pam_edit <action>  — invokes the pam-edit binary with the action and
# roots rebased under --root. Records that PAM was edited. Best-effort: a
# missing binary is a warning, not a mandatory failure (PAM may legitimately
# not be configured yet on a fresh system).
call_pam_edit() {
    local action="$1"
    if [[ ! -x "${PAM_EDIT_ARGV[0]}" ]]; then
        log_warn "PAM editor not found at ${PAM_EDIT_ARGV[0]} — skipping $action"
        return 0
    fi
    PAM_EDITED=true
    local pamd_root="$ROOT/etc/pam.d"
    local reg_root="$ROOT/var/lib/namtor"
    if ! "${PAM_EDIT_ARGV[@]}" "$action" --pamd-root "$pamd_root" --registry-root "$reg_root"; then
        log_warn "PAM editor $action failed"
        return 1
    fi
    return 0
}

# --- receipt ----------------------------------------------------------------

emit_receipt() {
    local status="ok"
    if [[ ${#FAILURES[@]} -gt 0 ]]; then
        status="failed"
    fi
    mkdir -p "$RECEIPT_DIR" 2>/dev/null || true
    local receipt="$RECEIPT_DIR/$PHASE.receipt"
    {
        echo "phase=$PHASE"
        echo "version=$VERSION"
        echo "root=$ROOT"
        echo "status=$status"
        echo "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
        echo "checks_total=$CHECKS_TOTAL"
        echo "checks_passed=$CHECKS_PASSED"
        echo "checks_failed=$CHECKS_FAILED"
        echo "pam_edited=$PAM_EDITED"
        echo "models_validated=$MODELS_VALIDATED"
        if [[ ${#FAILURES[@]} -gt 0 ]]; then
            echo "failures:"
            for f in "${FAILURES[@]}"; do
                echo "  - $f"
            done
        fi
    } >"$receipt" 2>/dev/null || log_warn "could not write receipt $receipt"
}

# --- shared mandatory payload validation (post-install + post-upgrade) -------

namtor_lc_validate_payload() {
    check_file "$ROOT/usr/lib/sysusers.d/namtor.conf" "sysusers.d/namtor.conf"
    check_file "$ROOT/usr/lib/tmpfiles.d/namtor.conf" "tmpfiles.d/namtor.conf"
    check_file "$ROOT/usr/lib/systemd/system/namtor-daemon.socket" "namtor-daemon.socket unit"
    check_file "$ROOT/usr/lib/systemd/system/namtor-daemon.service" "namtor-daemon.service unit"
    check_file "$ROOT/usr/share/dbus-1/system-services/org.namtor.Manager.service" "D-Bus service org.namtor.Manager"
    check_file "$ROOT/usr/lib/security/pam_namtor.so" "pam_namtor.so module"
    local manifest
    manifest="$(resolve_manifest)"
    validate_models "$ROOT/var/lib/namtor/models" "$manifest"
    check_dir "$ROOT/var/lib/namtor" "runtime dir /var/lib/namtor"
    check_dir "$ROOT/var/lib/namtor/models" "models dir /var/lib/namtor/models"
    check_dir "$ROOT/var/lib/namtor/templates" "templates dir /var/lib/namtor/templates"
    # /run/namtor is a tmpfs dir created by systemd-tmpfiles. On first install
    # pacman's post-install hook may run before the tmpfiles hook, so this is
    # a SOFT warning, not a mandatory failure.
    if [[ -d "$ROOT/run/namtor" ]]; then
        record_pass
    else
        log_warn "socket dir /run/namtor missing (will be created by systemd-tmpfiles on next boot or 'systemd-tmpfiles --create')"
    fi
}

# --- phase: pre-install -----------------------------------------------------

namtor_lc_phase_pre_install() {
    log_info "pre-install: verifying prerequisites"
    # Prerequisite config files (present in the package payload). On a real
    # pre-install before unpack these may not yet be on disk — the check
    # only fails if a file IS staged under --root but missing, which means
    # the package payload is damaged. On a fresh real install at %pre time
    # the files are not under / yet, so this records passes vacuously only
    # for files that exist; missing files are FATAL because if they are
    # staged in the payload they MUST be present.
    check_file "$ROOT/usr/lib/sysusers.d/namtor.conf" "sysusers.d/namtor.conf"
    check_file "$ROOT/usr/lib/tmpfiles.d/namtor.conf" "tmpfiles.d/namtor.conf"
    check_file "$ROOT/usr/lib/systemd/system/namtor-daemon.socket" "namtor-daemon.socket unit"
    check_file "$ROOT/usr/lib/systemd/system/namtor-daemon.service" "namtor-daemon.service unit"
    register_sysusers
    register_tmpfiles
}

# --- phase: post-install ----------------------------------------------------

namtor_lc_phase_post_install() {
    log_info "post-install: validating payload + services"
    register_sysusers
    register_tmpfiles
    namtor_lc_validate_payload
    enable_socket
    update_desktop_caches
    # NEVER edit PAM on install. No namtor-pam-edit call here.
}

# --- phase: pre-upgrade -----------------------------------------------------

namtor_lc_phase_pre_upgrade() {
    log_info "pre-upgrade: snapshotting PAM state + stopping daemon"
    call_pam_edit snapshot || true
    disable_daemon
}

# --- phase: post-upgrade ----------------------------------------------------

namtor_lc_phase_post_upgrade() {
    log_info "post-upgrade: migrating PAM + re-validating payload"
    call_pam_edit migrate || true
    register_sysusers
    register_tmpfiles
    namtor_lc_validate_payload
    enable_socket
    update_desktop_caches
}

# --- phase: pre-remove ------------------------------------------------------

namtor_lc_phase_pre_remove() {
    log_info "pre-remove: disabling PAM services + stopping daemon"
    call_pam_edit disable-all || true
    disable_daemon
}

# --- phase: post-remove -----------------------------------------------------

namtor_lc_phase_post_remove() {
    log_info "post-remove: purging PAM state + runtime dirs"
    call_pam_edit purge || true
    rm -rf "$ROOT/var/lib/namtor" 2>/dev/null || true
    rm -rf "$ROOT/run/namtor" 2>/dev/null || true
    if [[ $IS_FAKE_ROOT -eq 0 ]] && command -v systemd-tmpfiles >/dev/null 2>&1; then
        local cfg="$ROOT/usr/lib/tmpfiles.d/namtor.conf"
        [[ -f "$cfg" ]] && systemd-tmpfiles --remove "$cfg" 2>/dev/null || true
    fi
}

# --- dispatch ---------------------------------------------------------------

case "$PHASE" in
    pre-install)   namtor_lc_phase_pre_install ;;
    post-install)  namtor_lc_phase_post_install ;;
    pre-upgrade)   namtor_lc_phase_pre_upgrade ;;
    post-upgrade)  namtor_lc_phase_post_upgrade ;;
    pre-remove)    namtor_lc_phase_pre_remove ;;
    post-remove)   namtor_lc_phase_post_remove ;;
esac

emit_receipt

if [[ ${#FAILURES[@]} -gt 0 ]]; then
    echo ":: namtor-lifecycle: $PHASE: ${#FAILURES[@]} mandatory check(s) failed" >&2
    exit 1
fi
log_info "$PHASE: OK"
exit 0
