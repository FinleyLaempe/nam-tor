#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for the transactional PAM install/uninstall scripts.
#
# Runs WITHOUT root by using DESTDIR-style temp roots and fake PAM files.
# Covers the acceptance scenarios from the task spec:
#   (a) install preserves password fallback
#   (b) uninstall restores original
#   (c) broken module (missing .so) is caught by preflight
#   (d) daemon failure leaves PAM intact (pam_namtor fails closed -> password)
#   (e) upgrade (re-install) is idempotent
#   (f) unsafe ordering is refused
#
# Usage: bash packaging/tests/test_pam_install.sh
#
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"

PASS=0
FAIL=0
FAILED_TESTS=()

# Track temp dirs for cleanup.
# Temp dirs live under ONE root so cleanup cannot be defeated by a subshell.
#
# The previous helper appended to a TEMP_DIRS array, but every caller invokes
# it as `root="$(mk_temp_dir)"` — command substitution, i.e. a subshell. The
# append mutated the subshell's copy of the array and was discarded on exit,
# so the parent's TEMP_DIRS stayed empty and cleanup removed nothing. Each
# run leaked every directory it created; on a tmpfs /tmp that is leaked RAM.
TEMP_ROOT="$(mktemp -d)"
cleanup() {
    rm -rf "$TEMP_ROOT"
}
trap cleanup EXIT

mk_temp_dir() {
    # Children of TEMP_ROOT: the trap removes the root, so this works
    # regardless of which subshell the caller runs us in.
    mktemp -d "$TEMP_ROOT/t.XXXXXXXX"
}

# --- test helpers ----------------------------------------------------------

ok() {
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}

not_ok() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
    FAILED_TESTS+=("$1")
}

# Assert a command succeeds.
expect_success() {
    local desc="$1"; shift
    if "$@"; then
        ok "$desc"
    else
        not_ok "$desc (command failed)"
    fi
}

# Assert a command fails (returns non-zero).
expect_failure() {
    local desc="$1"; shift
    if "$@" >/dev/null 2>&1; then
        not_ok "$desc (command unexpectedly succeeded)"
    else
        ok "$desc (command failed as expected)"
    fi
}

# Assert a file contains a regex.
expect_contains() {
    local desc="$1" file="$2" pattern="$3"
    if grep -qE "$pattern" "$file" 2>/dev/null; then
        ok "$desc"
    else
        not_ok "$desc (pattern '$pattern' not found in $file)"
        echo "    --- file content ---"
        sed 's/^/    | /' "$file" 2>/dev/null || true
        echo "    --- end ---"
    fi
}

# Assert a file does NOT contain a regex.
expect_not_contains() {
    local desc="$1" file="$2" pattern="$3"
    if grep -qE "$pattern" "$file" 2>/dev/null; then
        not_ok "$desc (pattern '$pattern' unexpectedly found in $file)"
        echo "    --- file content ---"
        sed 's/^/    | /' "$file" 2>/dev/null || true
        echo "    --- end ---"
    else
        ok "$desc"
    fi
}

# Assert two files are byte-identical.
expect_same() {
    local desc="$1" a="$2" b="$3"
    if cmp -s "$a" "$b"; then
        ok "$desc"
    else
        not_ok "$desc ($a != $b)"
        echo "    --- diff ---"
        diff "$a" "$b" | sed 's/^/    | /' || true
        echo "    --- end ---"
    fi
}

# --- fixture builder -------------------------------------------------------
#
# Builds a fake DESTDIR tree with a representative PAM file and a stub
# pam_namtor.so. Returns the root path on stdout.
build_fake_root() {
    local root="$1"
    local pam_template="$2"  # which fake PAM file template to use

    mkdir -p "$root/etc/pam.d"
    mkdir -p "$root/usr/lib/security"
    mkdir -p "$root/run/namtor"

    # Stub pam_namtor.so — a zero-byte file is enough; preflight only checks
    # existence (-f), not that it is a valid ELF object.
    : >"$root/usr/lib/security/pam_namtor.so"

    case "$pam_template" in
        arch)
            # Arch-style gdm-password that includes system-auth.
            cat >"$root/etc/pam.d/gdm-password" <<'PAM'
#%PAM-1.0
auth     include  system-auth
auth     optional  pam_gnome_keyring.so
account  include  system-auth
password include  system-auth
session  include  system-auth
PAM
            ;;
        fedora)
            # Fedora-style with explicit pam_unix + pam_deny.
            cat >"$root/etc/pam.d/gdm-password" <<'PAM'
#%PAM-1.0
auth        required    pam_env.so
auth        sufficient  pam_unix.so nullok try_first_pass
auth        required    pam_deny.so
account     required    pam_unix.so
password    include     system-auth
session     include     system-auth
PAM
            ;;
        deny-first)
            # Pathological: pam_deny.so is the FIRST auth line.
            cat >"$root/etc/pam.d/gdm-password" <<'PAM'
#%PAM-1.0
auth     required   pam_deny.so
auth     sufficient pam_unix.so nullok try_first_pass
account  include    system-auth
PAM
            ;;
        no-fallback)
            # No password fallback — preflight/installer must refuse.
            cat >"$root/etc/pam.d/gdm-password" <<'PAM'
#%PAM-1.0
auth     required   pam_echo.so
account  include    system-auth
PAM
            ;;
    esac
}

# Run the installer against a fake root. Args passed through to pam-install.sh.
run_install() {
    local root="$1"; shift
    DESTDIR="$root" bash "$PACKAGING_DIR/pam-install.sh" --no-preflight "$@"
}

run_install_with_preflight() {
    local root="$1"; shift
    DESTDIR="$root" bash "$PACKAGING_DIR/pam-install.sh" "$@"
}

run_uninstall() {
    local root="$1"; shift
    DESTDIR="$root" bash "$PACKAGING_DIR/pam-uninstall.sh" "$@"
}

# ===========================================================================
# Test (a) — install preserves password fallback
# ===========================================================================
test_a_install_preserves_fallback() {
    echo "=== Test (a): install preserves password fallback ==="
    local root pam
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch

    expect_success "install into Arch-style gdm-password" \
        run_install "$root" --target /etc/pam.d/gdm-password --keep-backup

    expect_contains "namtor block present" "$pam" '^# namtor-begin$'
    expect_contains "namtor line is 'auth sufficient'" "$pam" '^auth[[:space:]]+sufficient[[:space:]]+pam_namtor\.so$'
    expect_contains "namtor-end marker present" "$pam" '^# namtor-end$'
    # Stock lines untouched.
    expect_contains "system-auth include still present" "$pam" '^auth[[:space:]]+include[[:space:]]+system-auth$'
    # Ordering: namtor block BEFORE system-auth include.
    local begin_ln auth_ln
    begin_ln="$(grep -n '^# namtor-begin$' "$pam" | head -1 | cut -d: -f1)"
    auth_ln="$(grep -n '^auth[[:space:]]\+include[[:space:]]\+system-auth$' "$pam" | head -1 | cut -d: -f1)"
    if [[ -n "$begin_ln" && -n "$auth_ln" && "$begin_ln" -lt "$auth_ln" ]]; then
        ok "namtor block is before system-auth include"
    else
        not_ok "namtor block before system-auth include (begin=$begin_ln auth=$auth_ln)"
    fi
}

# ===========================================================================
# Test (b) — uninstall restores original
# ===========================================================================
test_b_uninstall_restores_original() {
    echo "=== Test (b): uninstall restores original ==="
    local root pam original
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch
    cp "$pam" "$root/original.pam"
    original="$root/original.pam"

    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1

    expect_success "uninstall removes block" \
        run_uninstall "$root" --target /etc/pam.d/gdm-password

    expect_same "PAM file matches original after uninstall" "$pam" "$original"
    expect_not_contains "no namtor reference remains" "$pam" 'namtor'
    expect_contains "password fallback still present" "$pam" 'system-auth'
}

# ===========================================================================
# Test (b2) — uninstall with --restore-backup restores original
# ===========================================================================
test_b2_uninstall_restore_backup() {
    echo "=== Test (b2): uninstall --restore-backup restores original ==="
    local root pam original
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch
    cp "$pam" "$root/original.pam"
    original="$root/original.pam"

    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1

    expect_success "uninstall --restore-backup" \
        run_uninstall "$root" --target /etc/pam.d/gdm-password --restore-backup

    expect_same "PAM file matches original after --restore-backup" "$pam" "$original"
}

# ===========================================================================
# Test (c) — broken module (missing .so) is caught by preflight
# ===========================================================================
test_c_broken_module_caught() {
    echo "=== Test (c): broken module (missing .so) caught by preflight ==="
    local root
    root="$(mk_temp_dir)"
    build_fake_root "$root" arch
    # Remove the .so to simulate a broken/not-yet-built module.
    rm -f "$root/usr/lib/security/pam_namtor.so"

    expect_failure "install with missing .so fails (preflight)" \
        run_install_with_preflight "$root" --target /etc/pam.d/gdm-password

    # PAM file must be untouched (preflight ran before any edit).
    expect_not_contains "no namtor block after failed preflight" \
        "$root/etc/pam.d/gdm-password" 'namtor'
}

# ===========================================================================
# Test (d) — daemon failure leaves PAM intact
# ===========================================================================
#
# This test verifies the PAM *file structure* is safe under daemon failure:
# the namtor line is `auth sufficient` (so PAM_AUTHINFO_UNAVAIL falls through)
# and a password fallback exists below it. The actual PAM_AUTHINFO_UNAVAIL
# behaviour is asserted by the Rust unit tests in namtor-pam (see
# crates/namtor-pam/src/lib.rs: run_auth returns PAM_AUTHINFO_UNAVAIL on
# daemon_unreachable, never PAM_SUCCESS). Here we assert the *stack shape*
# that makes that fall-through reach password auth.
test_d_daemon_failure_leaves_pam_intact() {
    echo "=== Test (d): daemon failure leaves PAM intact (stack shape) ==="
    local root pam
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch
    # No socket present — simulates daemon down.

    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1

    # The namtor line must use a control that falls through on UNAVAIL.
    expect_contains "namtor uses 'sufficient' (falls through on PAM_AUTHINFO_UNAVAIL)" \
        "$pam" '^auth[[:space:]]+sufficient[[:space:]]+pam_namtor\.so$'
    # A password fallback must exist after the block.
    expect_contains "password fallback exists after block" \
        "$pam" 'system-auth'
    # Verify the source-level guarantee: the PAM module returns
    # PAM_AUTHINFO_UNAVAIL (not PAM_SUCCESS) on daemon-unreachable. We grep
    # the source to assert the fail-closed contract is in the code.
    local pam_src="$REPO_ROOT/crates/namtor-pam/src/lib.rs"
    expect_contains "lib.rs documents 'Never PAM_SUCCESS' on failure" \
        "$pam_src" 'Never .*PAM_SUCCESS'
    expect_contains "lib.rs has daemon_unreachable fail-closed path" \
        "$pam_src" '"daemon_unreachable"'
    expect_contains "lib.rs maps daemon failure to PAM_AUTHINFO_UNAVAIL" \
        "$pam_src" 'PAM_AUTHINFO_UNAVAIL'
}

# ===========================================================================
# Test (e) — upgrade (re-install) is idempotent
# ===========================================================================
test_e_upgrade_idempotent() {
    echo "=== Test (e): upgrade (re-install) is idempotent ==="
    local root pam after_first after_second
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch

    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1
    cp "$pam" "$root/after_first.pam"
    after_first="$root/after_first.pam"

    # Count namtor blocks — must be exactly one.
    local block_count
    block_count="$(grep -c '^# namtor-begin$' "$pam" || true)"
    if [[ "$block_count" -eq 1 ]]; then
        ok "exactly one namtor block after first install"
    else
        not_ok "exactly one namtor block after first install (got $block_count)"
    fi

    # Re-run install (upgrade).
    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1
    cp "$pam" "$root/after_second.pam"
    after_second="$root/after_second.pam"

    block_count="$(grep -c '^# namtor-begin$' "$pam" || true)"
    if [[ "$block_count" -eq 1 ]]; then
        ok "exactly one namtor block after re-install (no duplication)"
    else
        not_ok "exactly one namtor block after re-install (got $block_count)"
    fi

    expect_same "PAM file identical after first and second install" "$after_first" "$after_second"

    # A third re-install with a different control value should replace, not
    # append.
    run_install "$root" --target /etc/pam.d/gdm-password --control '[default=ignore]' --keep-backup >/dev/null 2>&1
    block_count="$(grep -c '^# namtor-begin$' "$pam" || true)"
    if [[ "$block_count" -eq 1 ]]; then
        ok "exactly one namtor block after control change (in-place replace)"
    else
        not_ok "exactly one namtor block after control change (got $block_count)"
    fi
    expect_contains "new control value applied" "$pam" '^auth[[:space:]]+\[default=ignore\][[:space:]]+pam_namtor\.so$'
    expect_not_contains "old 'sufficient' line gone" "$pam" '^sufficient[[:space:]]+pam_namtor'
}

# ===========================================================================
# Test (f) — unsafe ordering is refused
# ===========================================================================
test_f_unsafe_ordering_refused() {
    echo "=== Test (f): unsafe ordering is refused ==="

    # (f1) `auth required` control refused.
    local root pam
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch

    expect_failure "install with --control required is refused" \
        run_install "$root" --target /etc/pam.d/gdm-password --control required

    expect_not_contains "no namtor block after refused install" "$pam" 'namtor'

    # (f2) `auth requisite` control refused.
    expect_failure "install with --control requisite is refused" \
        run_install "$root" --target /etc/pam.d/gdm-password --control requisite

    # (f3) Block would land after pam_deny.so (deny-first pathological file).
    local root2
    root2="$(mk_temp_dir)"
    build_fake_root "$root2" deny-first

    # Even with a safe control, inserting before the first auth line (which is
    # pam_deny here) means deny is before the block — but our check is "deny
    # before insertion point". Since deny IS the first auth line, the block
    # goes before deny, which is actually fine (block runs, then deny). But the
    # *post-block* check should catch that there's no password fallback before
    # deny after the block. Let's verify it's refused.
    expect_failure "install into deny-first file is refused (no fallback before deny)" \
        run_install "$root2" --target /etc/pam.d/gdm-password

    expect_not_contains "no namtor block in deny-first file" \
        "$root2/etc/pam.d/gdm-password" 'namtor-begin'

    # (f4) Fedora-style file (pam_deny after pam_unix) — should SUCCEED because
    # the block goes before pam_env, and pam_unix is the fallback after the
    # block, with pam_deny after that.
    local root3
    root3="$(mk_temp_dir)"
    build_fake_root "$root3" fedora

    expect_success "install into Fedora-style file (deny is after fallback) succeeds" \
        run_install "$root3" --target /etc/pam.d/gdm-password --keep-backup

    local pam3="$root3/etc/pam.d/gdm-password"
    expect_contains "namtor block present in Fedora file" "$pam3" '^# namtor-begin$'
    expect_contains "pam_unix fallback present" "$pam3" 'pam_unix'
    expect_contains "pam_deny still present after block" "$pam3" 'pam_deny'

    # (f5) No password fallback at all — installer refuses.
    local root4
    root4="$(mk_temp_dir)"
    build_fake_root "$root4" no-fallback

    expect_failure "install into file with no password fallback is refused" \
        run_install_with_preflight "$root4" --target /etc/pam.d/gdm-password

    expect_not_contains "no namtor block in no-fallback file" \
        "$root4/etc/pam.d/gdm-password" 'namtor-begin'
}

# ===========================================================================
# Test (g) — rollback on failure restores the backup
# ===========================================================================
test_g_rollback_restores_backup() {
    echo "=== Test (g): rollback restores backup on simulated failure ==="
    local root pam original
    root="$(mk_temp_dir)"
    pam="$root/etc/pam.d/gdm-password"
    build_fake_root "$root" arch
    cp "$pam" "$root/original.pam"
    original="$root/original.pam"

    # Simulate a failure AFTER backup by using the no-fallback file but with
    # the .so present — the installer will back up, insert the block, then
    # fail the post-block fallback verification and roll back.
    # We craft a file that passes preflight (has an auth line with pam_env,
    # which preflight accepts as fallback) but fails the post-block check
    # (which requires the fallback AFTER the block). Actually preflight and
    # post-block use the same fallback list. Instead, directly test the
    # rollback path by inserting into deny-first (no fallback before deny)
    # and checking the file is restored.
    build_fake_root "$root" deny-first
    cp "$pam" "$original"

    run_install "$root" --target /etc/pam.d/gdm-password --keep-backup >/dev/null 2>&1 || true

    expect_same "file restored to original after rollback" "$pam" "$original"
}

# ===========================================================================
# Run all tests
# ===========================================================================
echo "Running Nam-tor PAM install/uninstall test matrix..."
echo

test_a_install_preserves_fallback
test_b_uninstall_restores_original
test_b2_uninstall_restore_backup
test_c_broken_module_caught
test_d_daemon_failure_leaves_pam_intact
test_e_upgrade_idempotent
test_f_unsafe_ordering_refused
test_g_rollback_restores_backup

echo
echo "============================================="
echo "Results: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    echo "Failed tests:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    exit 1
fi
echo "All tests passed."
exit 0
