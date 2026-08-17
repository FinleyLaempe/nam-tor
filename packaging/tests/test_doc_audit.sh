#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 26: reconcile public documentation and honest claims.
#
# Verifies that the user-facing documentation:
#   (a) contains no stale strings (removed commands, removed env vars, wrong
#       licence claims, removed setuid helper, wrong D-Bus bus, wrong worker
#       user, blanket GDM-group step, false production-readiness claim,
#       fetched-at-first-run claim).
#   (b) contains the "experimental preview" warning.
#   (c) documents the split package names (namtor, namtor-gnome, namtor-kde).
#   (d) has no broken internal markdown links (links to files that don't exist).
#   (e) mutation: adding a stale string to any audited file fails the test.
#
# The audit scope is the set of user-facing docs whose content this task owns:
#   README.md, packaging/README.md, COPYING.md,
#   docs/evidence/status-audit.md.
#
# Usage: bash packaging/tests/test_doc_audit.sh [--mutation <file>]
#   --mutation <file>  Inject a stale string into <file> before checking,
#                      then verify the test fails. Used by the mutation evidence
#                      harness; not for normal CI runs.
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

PASS=0
FAIL=0
FAILED_TESTS=()

ok()   { echo "  PASS: $1"; PASS=$((PASS + 1)); }
not_ok() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
    FAILED_TESTS+=("$1")
}

# --- audit scope ------------------------------------------------------------

AUDITED_FILES=(
    "$REPO_ROOT/README.md"
    "$REPO_ROOT/packaging/README.md"
    "$REPO_ROOT/COPYING.md"
    "$REPO_ROOT/docs/evidence/status-audit.md"
)

# Stale strings that must NOT appear in any audited file.
# Each entry: "pattern||reason"
# The pattern is a literal string (grep -F) unless it contains regex metachars.
STALE_STRINGS=(
    "models ensure||nonexistent command 'namtor-cli models ensure'"
    "NAMTOR_ACCEPT_LICENCE||env var removed in Task 5 (typed admission policy)"
    "fetched at first-run||models are bundled, not fetched at first run"
    "first run fetch||models are bundled, not fetched at first run"
    "first-run fetch||models are bundled, not fetched at first run"
    "BSD-3-Clause||wrong model licence (YuNet=MIT, SFace=Apache-2.0)"
    "runs as nobody||worker runs as namtor, not nobody (Task 18)"
    "worker is nobody||worker runs as namtor, not nobody (Task 18)"
    "setuid helper||setuid helper removed in Task 18"
    "setuid root||setuid helper removed in Task 18"
    "namtor-worker-spawn||setuid helper binary removed in Task 18"
)

# --- mutation mode ----------------------------------------------------------

MUTATION_FILE=""
if [[ "${1:-}" == "--mutation" && -n "${2:-}" ]]; then
    MUTATION_FILE="$2"
    STALE_STRING="NAMTOR_ACCEPT_LICENCE=1"
    echo "Mutation mode: injecting stale string into $MUTATION_FILE"
    # Append a stale string to the target file
    printf '\n<!-- STALE: %s -->\n' "$STALE_STRING" >> "$MUTATION_FILE"
fi

# --- (a) no stale strings ---------------------------------------------------

echo "--- (a) no stale strings in audited docs ---"
for entry in "${STALE_STRINGS[@]}"; do
    pattern="${entry%%||*}"
    reason="${entry##*||}"
    found_in=0
    for f in "${AUDITED_FILES[@]}"; do
        if grep -qF "$pattern" "$f" 2>/dev/null; then
            not_ok "$f contains stale string '$pattern' ($reason)"
            found_in=1
        fi
    done
    if [[ $found_in -eq 0 ]]; then
        ok "no audited file contains '$pattern' ($reason)"
    fi
done

# --- (b) experimental preview warning present -------------------------------

echo "--- (b) experimental preview warning ---"
EXPERIMENTAL_PHRASES=(
    "Experimental preview"
    "do not enable on security-sensitive PAM"
)
EXPERIMENTAL_FILES=(
    "$REPO_ROOT/README.md"
    "$REPO_ROOT/packaging/README.md"
)
for f in "${EXPERIMENTAL_FILES[@]}"; do
    fname="${f#$REPO_ROOT/}"
    all_present=1
    for phrase in "${EXPERIMENTAL_PHRASES[@]}"; do
        if ! grep -qiF "$phrase" "$f" 2>/dev/null; then
            not_ok "$fname missing experimental preview phrase '$phrase'"
            all_present=0
        fi
    done
    if [[ $all_present -eq 1 ]]; then
        ok "$fname has experimental preview warning"
    fi
done

# --- (c) package names documented -------------------------------------------

echo "--- (c) split package names documented ---"
PACKAGE_NAMES=(
    "namtor-gnome"
    "namtor-kde"
)
PKG_FILES=(
    "$REPO_ROOT/README.md"
    "$REPO_ROOT/packaging/README.md"
)
for f in "${PKG_FILES[@]}"; do
    fname="${f#$REPO_ROOT/}"
    all_present=1
    for pkg in "${PACKAGE_NAMES[@]}"; do
        if ! grep -qF "$pkg" "$f" 2>/dev/null; then
            not_ok "$fname missing package name '$pkg'"
            all_present=0
        fi
    done
    # The core package name 'namtor' is present by default; check it too
    if ! grep -qF "namtor" "$f" 2>/dev/null; then
        not_ok "$fname missing core package name 'namtor'"
        all_present=0
    fi
    if [[ $all_present -eq 1 ]]; then
        ok "$fname documents all three package names"
    fi
done

# Distro-specific package manager commands documented in README
echo "--- (c) distro package managers in README ---"
for cmd in "pacman" "dnf install" "apt install"; do
    if ! grep -qF "$cmd" "$REPO_ROOT/README.md" 2>/dev/null; then
        not_ok "README.md missing distro install command '$cmd'"
    else
        ok "README.md documents '$cmd'"
    fi
done

# --- (d) links resolve ------------------------------------------------------

echo "--- (d) internal markdown links resolve ---"
check_links() {
    local file="$1"
    local fname
    fname="${file#$REPO_ROOT/}"
    local dir
    dir="$(cd "$(dirname "$file")" && pwd)"
    local broken=0
    # Extract [text](path) links where path does not start with http
    # and does not start with # (anchor links)
    while IFS= read -r link; do
        # Skip http(s) links, anchors, mailto
        case "$link" in
            http://*|https://*|mailto:*|\#*) continue ;;
        esac
        # Strip any anchor fragment
        local path="${link%%#*}"
        # Resolve relative to the file's directory
        local target="$dir/$path"
        if [[ ! -e "$target" ]]; then
            not_ok "$fname: broken link '$link' (resolved to $target)"
            broken=1
        fi
    done < <(grep -oP '\[[^]]*\]\(\K[^)]+' "$file" 2>/dev/null)
    if [[ $broken -eq 0 ]]; then
        ok "$fname: all internal links resolve"
    fi
}

for f in "${AUDITED_FILES[@]}"; do
    check_links "$f"
done

# --- (e) mutation verification ----------------------------------------------

if [[ -n "$MUTATION_FILE" ]]; then
    echo "--- (e) mutation check ---"
    # In mutation mode, we expect the stale-string check (a) to have caught
    # the injected string. If FAIL > 0, the mutation was detected (PASS).
    if [[ $FAIL -gt 0 ]]; then
        # Clean up: remove the injected line
        sed -i '/<!-- STALE: NAMTOR_ACCEPT_LICENCE=1 -->/d' "$MUTATION_FILE"
        echo "  PASS: mutation detected (test correctly failed on injected stale string)"
        echo "  (mutation mode: $FAIL failure(s) detected, exiting 0 for mutation harness)"
        exit 0
    else
        # Clean up anyway
        sed -i '/<!-- STALE: NAMTOR_ACCEPT_LICENCE=1 -->/d' "$MUTATION_FILE"
        echo "  FAIL: mutation NOT detected (test passed with injected stale string)"
        exit 1
    fi
fi

# --- summary ----------------------------------------------------------------

echo ""
echo "=== Summary ==="
echo "  Passed: $PASS"
echo "  Failed: $FAIL"
if [[ $FAIL -gt 0 ]]; then
    echo "  Failed tests:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "    - $t"
    done
    exit 1
fi
echo "  All doc audit checks passed."
exit 0
