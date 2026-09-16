#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 22: Debian split-package control/rules structure.
#
# Acceptance criteria (from the task spec):
#   (a) control has exactly 2 Package: stanzas: namtor, namtor-ui.
#   (b) Core Depends: onnxruntime, libpam0g, systemd only — NO
#       libgtk-4-1, libadwaita-1-0, libqt6, kirigami, kcoreaddons.
#   (c) Desktop Depends: namtor (= 0.2.0-1), libgtk-4-1, libadwaita-1-0
#       and NO qt6/kirigami/kcoreaddons.
#   (d) namtor-ui Provides/Replaces/Breaks its predecessors namtor-gnome
#       (renamed) and namtor-kde (removed).
#       Merely dropping it is not enough: it declared
#       `Depends: namtor (= <version>)`, so a leftover install holds apt
#       back from upgrading the core package.
#   (e) rules calls stage.sh with --package core and --package ui
#       (and no longer with --package kde).
#   (f) rules relocates PAM module to multiarch path (DEB_HOST_MULTIARCH
#       + security).
#   (g) rules comment does NOT contain "monolithic".
#   (h) namtor-ui.install exists and lists the correct files; the
#       namtor-kde.* packaging files are gone.
#   (i) namtor-ui.postinst and namtor-ui.postrm exist with desktop
#       cache refresh only.
#   (j) changelog mentions the split.
#   (k) namtor.postinst delegates to lifecycle.sh (not desktop cache).
#   (l) namtor.postrm distinguishes remove vs purge.
#   (m) SPDX headers present on all maintainer scripts.
#
# Usage: bash packaging/tests/test_debian_split.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEBIAN_DIR="$PACKAGING_DIR/debian"
CONTROL="$DEBIAN_DIR/control"
RULES="$DEBIAN_DIR/rules"
CHANGELOG="$DEBIAN_DIR/changelog"

PASS=0
FAIL=0
FAILED_TESTS=()

ok()      { echo "  PASS: $1"; PASS=$((PASS + 1)); }
not_ok()  { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); FAILED_TESTS+=("$1"); }
check()   { # check <desc> <regex> <expected: true|false>
    local desc="$1" regex="$2" exp="$3"
    if [[ "$exp" == "true" ]]; then
        if grep -qE -- "$regex" "$4"; then ok "$desc"; else not_ok "$desc (expected present)"; fi
    else
        if grep -qE -- "$regex" "$4"; then not_ok "$desc (expected absent)"; else ok "$desc"; fi
    fi
}

echo "== Task 22: Debian split-package structure =="

if [[ ! -f "$CONTROL" ]]; then
    echo "error: control not found: $CONTROL" >&2
    exit 2
fi
if [[ ! -f "$RULES" ]]; then
    echo "error: rules not found: $RULES" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# (a) control has exactly 2 Package: stanzas
# ---------------------------------------------------------------------------
echo "--- (a) two Package: stanzas ---"

package_stanzas="$(grep -cE '^Package:' "$CONTROL")"
if [[ "$package_stanzas" == "2" ]]; then
    ok "control has exactly 2 Package: stanzas"
else
    not_ok "control has $package_stanzas Package: stanzas (expected 2)"
fi

for pkg in namtor namtor-ui; do
    if grep -qE "^Package:[[:space:]]+${pkg}$" "$CONTROL"; then
        ok "control has Package: $pkg"
    else
        not_ok "control missing Package: $pkg"
    fi
done

if grep -qE "^Package:[[:space:]]+namtor-kde$" "$CONTROL"; then
    not_ok "control has Package: namtor-kde (the Plasma KCM was removed)"
else
    ok "control has no Package: namtor-kde (KCM removed)"
fi

# ---------------------------------------------------------------------------
# Helper: extract a Package stanza body from control
# ---------------------------------------------------------------------------
stanza_body() {
    local pkg="$1"
    awk -v p="$pkg" '
        $0 ~ ("^Package:[[:space:]]+" p "$") { found=1; next }
        found && /^Package:/ { exit }
        found { print }
    ' "$CONTROL"
}

CORE_BODY="$(stanza_body namtor)"
UI_BODY="$(stanza_body namtor-ui)"

# Extract the Depends block (continuation lines start with space/tab).
depends_block() {
    local body="$1"
    printf '%s\n' "$body" | awk '
        /^Depends:/ { f=1; sub(/^Depends:/, ""); print; next }
        f && /^[[:space:]]/ { sub(/^[[:space:]]+/, ""); print; next }
        f && /^[^[:space:]]/ { exit }
    '
}

CORE_DEPS="$(depends_block "$CORE_BODY")"
UI_DEPS="$(depends_block "$UI_BODY")"

# ---------------------------------------------------------------------------
# (b) Core Depends — no desktop deps
# ---------------------------------------------------------------------------
echo "--- (b) core Depends (no desktop deps) ---"

for dep in onnxruntime libpam0g systemd; do
    if printf '%s\n' "$CORE_DEPS" | grep -qE "^${dep}(,|[[:space:]]|$)"; then
        ok "core Depends on $dep"
    else
        not_ok "core missing dependency: $dep"
    fi
done

for nodep in libgtk-4-1 libadwaita-1-0 libqt6 kirigami kcoreaddons; do
    if printf '%s\n' "$CORE_DEPS" | grep -qiE "$nodep"; then
        not_ok "core Depends on $nodep (should not — desktop dep in core)"
    else
        ok "core does not depend on $nodep"
    fi
done

# ---------------------------------------------------------------------------
# (c) Desktop Depends — gtk4 + libadwaita, exact core version, no Qt/KDE
# ---------------------------------------------------------------------------
echo "--- (c) ui Depends ---"

# Derive the expected pin from debian/changelog rather than hardcoding it,
# so a version bump cannot leave this assertion silently checking the old
# value while its message claims the new one.
_deb_ver="$(sed -n '1s/.*(\([^)]*\)).*/\1/p' "$DEBIAN_DIR/changelog")"
if printf '%s\n' "$UI_DEPS" | grep -qE "namtor[[:space:]]*\(=[[:space:]]*${_deb_ver//./\\.}\)"; then
    ok "ui Depends on namtor (= $_deb_ver) (exact version, from changelog)"
else
    not_ok "ui does not depend on namtor (= $_deb_ver) (exact version, from changelog)"
fi

for dep in libgtk-4-1 libadwaita-1-0; do
    if printf '%s\n' "$UI_DEPS" | grep -qiE "$dep"; then
        ok "ui Depends on $dep"
    else
        not_ok "ui missing dependency: $dep"
    fi
done

for nodep in libqt6 kirigami kcoreaddons; do
    if printf '%s\n' "$UI_DEPS" | grep -qiE "$nodep"; then
        not_ok "ui Depends on $nodep (should not — Qt/KDE dep in the GTK UI)"
    else
        ok "ui does not depend on $nodep"
    fi
done

# ---------------------------------------------------------------------------
# (d) namtor-ui obsoletes both predecessors
# ---------------------------------------------------------------------------
echo "--- (d) obsoletion of namtor-gnome and namtor-kde ---"

# namtor-gnome is this same package under its old name. apt migrates nothing
# on a rename, and both own /usr/bin/namtor-enroll-wizard, the .desktop, the
# metainfo and the icon, so without Replaces/Breaks the unpack fails on a
# file conflict. Provides keeps anything that depended on the old name
# satisfiable through the transaction.
for field in Provides Replaces Breaks; do
    if printf '%s\n' "$UI_BODY" | grep -qE "^${field}:.*namtor-gnome"; then
        ok "namtor-ui ${field}: namtor-gnome (rename migration)"
    else
        not_ok "namtor-ui does not ${field} namtor-gnome — the pre-rename package will file-conflict"
    fi
done

# Dropping the stanza is not enough. namtor-kde declared
# `Depends: namtor (= <version>)`, so a machine that still has it installed
# keeps an unsatisfiable dependency and apt holds back the core upgrade.
# Replaces + Breaks tells apt to remove it as part of the transaction.
if printf '%s\n' "$UI_BODY" | grep -qE '^Replaces:.*namtor-kde'; then
    ok "namtor-ui Replaces: namtor-kde (unblocks core upgrades)"
else
    not_ok "namtor-ui does not Replace namtor-kde — a leftover install will block core upgrades"
fi
if printf '%s\n' "$UI_BODY" | grep -qE '^Breaks:.*namtor-kde'; then
    ok "namtor-ui Breaks: namtor-kde"
else
    not_ok "namtor-ui does not Break namtor-kde"
fi

# No Qt/KF6/CMake Build-Depends should remain.
for bd in cmake qt6-base-dev libkf6kirigami-dev libkf6coreaddons-dev extra-cmake-modules; do
    if grep -qE "^[[:space:]]+${bd}," "$CONTROL"; then
        not_ok "control still Build-Depends on $bd (no Qt/KF6/CMake build left)"
    else
        ok "control has no Build-Depends on $bd"
    fi
done

# ---------------------------------------------------------------------------
# (e) rules calls stage.sh with --package core, ui (never kde)
# ---------------------------------------------------------------------------
echo "--- (e) stage.sh --package calls in rules ---"

for pkg in core ui; do
    if grep -qE "stage\.sh.*--package[[:space:]]+${pkg}" "$RULES"; then
        ok "rules calls stage.sh --package $pkg"
    else
        not_ok "rules does not call stage.sh --package $pkg"
    fi
done
if grep -qE "stage\.sh.*--package[[:space:]]+kde" "$RULES"; then
    not_ok "rules still calls stage.sh --package kde (stage.sh now rejects it)"
else
    ok "rules does not call stage.sh --package kde"
fi
if grep -qE '(^|[^-])cmake' "$RULES"; then
    not_ok "rules still runs cmake (the KCM was the only CMake build)"
else
    ok "rules runs no cmake"
fi

# ---------------------------------------------------------------------------
# (f) rules relocates PAM module to multiarch path
# ---------------------------------------------------------------------------
echo "--- (f) multiarch PAM relocation in rules ---"

if grep -qE 'DEB_HOST_MULTIARCH' "$RULES"; then
    ok "rules defines DEB_HOST_MULTIARCH"
else
    not_ok "rules does not define DEB_HOST_MULTIARCH"
fi

if grep -qE 'DEB_HOST_MULTIARCH.*security|security.*DEB_HOST_MULTIARCH' "$RULES"; then
    ok "rules uses DEB_HOST_MULTIARCH + security for PAM path"
else
    # Check both appear in the relocation block (mv ... security)
    if grep -qE 'DEB_HOST_MULTIARCH' "$RULES" && grep -qE '/security/pam_namtor\.so' "$RULES"; then
        ok "rules uses DEB_HOST_MULTIARCH + security for PAM path"
    else
        not_ok "rules does not relocate PAM to multiarch security path"
    fi
fi

# The mv command spans two lines with a backslash continuation; use a
# multi-line awk check instead of a single-line grep.
if awk '/mv[[:space:]]+.*pam_namtor\.so/{found=1} found && /DEB_HOST_MULTIARCH/{print; exit}' "$RULES" | grep -q .; then
    ok "rules mv relocates pam_namtor.so to multiarch path"
else
    not_ok "rules does not mv pam_namtor.so to multiarch path"
fi

# ---------------------------------------------------------------------------
# (g) rules comment does NOT contain "monolithic"
# ---------------------------------------------------------------------------
echo "--- (g) rules comment (no 'monolithic') ---"

if grep -qiE 'monolithic' "$RULES"; then
    not_ok "rules still contains 'monolithic' comment"
else
    ok "rules does not contain 'monolithic' comment"
fi

# rules should mention the split
if grep -qiE 'namtor-ui|namtor-kde|split' "$RULES"; then
    ok "rules mentions split packages"
else
    not_ok "rules does not mention split packages"
fi

# ---------------------------------------------------------------------------
# (h) .install files exist and list correct files
# ---------------------------------------------------------------------------
echo "--- (h) .install files ---"

UI_INSTALL="$DEBIAN_DIR/namtor-ui.install"

if [[ -f "$UI_INSTALL" ]]; then
    ok "namtor-ui.install exists"
else
    not_ok "namtor-ui.install does not exist"
fi

if [[ -f "$UI_INSTALL" ]]; then
    for f in \
        'usr/bin/namtor-enroll-wizard' \
        'org\.namtor\.Wizard\.desktop' \
        'org\.namtor\.Wizard\.metainfo\.xml' \
        'org\.namtor\.Wizard\.svg'; do
        if grep -qE "$f" "$UI_INSTALL"; then
            ok "namtor-ui.install lists $f"
        else
            not_ok "namtor-ui.install missing $f"
        fi
    done
fi

# Every namtor-kde packaging file must be gone, not just unreferenced.
for f in namtor-kde.install namtor-kde.postinst namtor-kde.postrm; do
    if [[ -e "$DEBIAN_DIR/$f" ]]; then
        not_ok "$f still exists (the Plasma KCM package was removed)"
    else
        ok "$f removed"
    fi
done

# ---------------------------------------------------------------------------
# (i) desktop maintainer scripts exist with cache refresh only
# ---------------------------------------------------------------------------
echo "--- (i) desktop maintainer scripts ---"

for script in namtor-ui.postinst namtor-ui.postrm; do
    if [[ -f "$DEBIAN_DIR/$script" ]]; then
        ok "$script exists"
    else
        not_ok "$script does not exist"
    fi
done

for postinst in namtor-ui.postinst; do
    if [[ -f "$DEBIAN_DIR/$postinst" ]] && grep -qE 'update-desktop-database' "$DEBIAN_DIR/$postinst"; then
        ok "$postinst refreshes desktop cache"
    else
        not_ok "$postinst does not refresh desktop cache"
    fi
    # Desktop postinst should NOT call lifecycle.sh
    if [[ -f "$DEBIAN_DIR/$postinst" ]] && grep -qE 'lifecycle\.sh' "$DEBIAN_DIR/$postinst"; then
        not_ok "$postinst calls lifecycle.sh (desktop should not)"
    else
        ok "$postinst does not call lifecycle.sh"
    fi
done

for postrm in namtor-ui.postrm; do
    if [[ -f "$DEBIAN_DIR/$postrm" ]] && grep -qE 'update-desktop-database|gtk-update-icon-cache' "$DEBIAN_DIR/$postrm"; then
        ok "$postrm refreshes desktop/icon cache"
    else
        not_ok "$postrm does not refresh caches"
    fi
    if [[ -f "$DEBIAN_DIR/$postrm" ]] && grep -qE 'lifecycle\.sh' "$DEBIAN_DIR/$postrm"; then
        not_ok "$postrm calls lifecycle.sh (desktop should not)"
    else
        ok "$postrm does not call lifecycle.sh"
    fi
done

# ---------------------------------------------------------------------------
# (j) changelog mentions the split
# ---------------------------------------------------------------------------
echo "--- (j) changelog mentions split ---"

if grep -qiE 'split' "$CHANGELOG"; then
    ok "changelog mentions 'split'"
else
    not_ok "changelog does not mention 'split'"
fi

if grep -qiE 'namtor-ui' "$CHANGELOG"; then
    ok "changelog mentions namtor-ui"
else
    not_ok "changelog does not mention namtor-ui"
fi

# ... and must keep documenting namtor-gnome as the obsoleted predecessor.
# Dropping it from the changelog is how a user on the old package ends up
# stranded with no record of what happened.
if grep -qiE 'namtor-gnome' "$CHANGELOG"; then
    ok "changelog still documents namtor-gnome (obsoleted predecessor)"
else
    not_ok "changelog no longer documents namtor-gnome"
fi

# The newest changelog entry must record the namtor-gnome rename — the
# obsoletion is a user-visible upgrade behaviour, not an internal detail.
newest_entry="$(awk 'NR>1 && /^[a-z].*\(.*\).*urgency=/{exit} {print}' "$CHANGELOG")"
if printf '%s\n' "$newest_entry" | grep -qiE 'namtor-gnome'; then
    ok "newest changelog entry documents the namtor-gnome rename"
else
    not_ok "newest changelog entry does not document the namtor-gnome rename"
fi

# The namtor-kde removal must stay documented somewhere in the changelog.
if grep -qiE 'namtor-kde' "$CHANGELOG"; then
    ok "changelog documents the namtor-kde removal"
else
    not_ok "changelog does not document the namtor-kde removal"
fi

# ---------------------------------------------------------------------------
# (k) namtor.postinst delegates to lifecycle.sh (not desktop cache)
# ---------------------------------------------------------------------------
echo "--- (k) core namtor.postinst delegates to lifecycle.sh ---"

CORE_POSTINST="$DEBIAN_DIR/namtor.postinst"
if [[ -f "$CORE_POSTINST" ]] && grep -qE 'lifecycle\.sh' "$CORE_POSTINST"; then
    ok "namtor.postinst delegates to lifecycle.sh"
else
    not_ok "namtor.postinst does not delegate to lifecycle.sh"
fi

# Core postinst should NOT do desktop cache refresh (that's desktop pkgs' job)
if [[ -f "$CORE_POSTINST" ]] && grep -qE 'update-desktop-database' "$CORE_POSTINST"; then
    not_ok "namtor.postinst does desktop cache refresh (should delegate to lifecycle.sh only)"
else
    ok "namtor.postinst does not do desktop cache refresh"
fi

# PAM default-off: postinst must not call pam-install.sh or write /etc/pam.d
if [[ -f "$CORE_POSTINST" ]]; then
    core_postinst_body="$(sed '/^[[:space:]]*#/d' "$CORE_POSTINST")"
    if printf '%s\n' "$core_postinst_body" | grep -qE 'pam-install\.sh|/etc/pam\.d'; then
        not_ok "namtor.postinst edits PAM (must be default-off)"
    else
        ok "namtor.postinst does not edit PAM (default-off)"
    fi
fi

# ---------------------------------------------------------------------------
# (l) namtor.postrm distinguishes remove vs purge
# ---------------------------------------------------------------------------
echo "--- (l) core namtor.postrm remove vs purge ---"

CORE_POSTRM="$DEBIAN_DIR/namtor.postrm"
if [[ -f "$CORE_POSTRM" ]] && grep -qE 'remove\)' "$CORE_POSTRM" && grep -qE 'purge\)' "$CORE_POSTRM"; then
    ok "namtor.postrm distinguishes remove vs purge"
else
    not_ok "namtor.postrm does not distinguish remove vs purge"
fi

if [[ -f "$CORE_POSTRM" ]] && grep -qE 'NAMTOR_LIFECYCLE_CACHE' "$CORE_POSTRM"; then
    ok "namtor.postrm references lifecycle cache"
else
    not_ok "namtor.postrm does not reference lifecycle cache"
fi

# remove path should NOT call lifecycle.sh post-remove (keeps state)
# purge path SHOULD call cached lifecycle post-remove
if [[ -f "$CORE_POSTRM" ]]; then
    postrm_remove_body="$(awk '/remove\)/{f=1;next} /purge\)|upgrade|abort|disappear|\*\)/{f=0} f' "$CORE_POSTRM")"
    if printf '%s\n' "$postrm_remove_body" | grep -qE 'post-remove'; then
        not_ok "postrm remove path calls post-remove (should keep state for reinstall)"
    else
        ok "postrm remove path does not call post-remove (keeps state)"
    fi
    postrm_purge_body="$(awk '/purge\)/{f=1;next} /upgrade|abort|disappear|\*\)/{f=0} f' "$CORE_POSTRM")"
    if printf '%s\n' "$postrm_purge_body" | grep -qE 'post-remove'; then
        ok "postrm purge path calls post-remove (full purge)"
    else
        not_ok "postrm purge path does not call post-remove"
    fi
fi

# ---------------------------------------------------------------------------
# (m) SPDX headers present on all maintainer scripts
# ---------------------------------------------------------------------------
echo "--- (m) SPDX headers on maintainer scripts ---"

for script in \
    namtor.postinst namtor.prerm namtor.postrm \
    namtor-ui.postinst namtor-ui.postrm; do
    if [[ -f "$DEBIAN_DIR/$script" ]]; then
        if grep -qE 'SPDX-License-Identifier' "$DEBIAN_DIR/$script"; then
            ok "$script has SPDX header"
        else
            not_ok "$script missing SPDX header"
        fi
    else
        not_ok "$script does not exist (cannot check SPDX)"
    fi
done

# ---------------------------------------------------------------------------
# (n) namtor.prerm caches lifecycle.sh (for postrm purge path)
# ---------------------------------------------------------------------------
echo "--- (n) namtor.prerm caches lifecycle.sh ---"

CORE_PRERM="$DEBIAN_DIR/namtor.prerm"
if [[ -f "$CORE_PRERM" ]] && grep -qE 'NAMTOR_LIFECYCLE_CACHE' "$CORE_PRERM"; then
    ok "namtor.prerm references lifecycle cache"
else
    not_ok "namtor.prerm does not reference lifecycle cache"
fi

if [[ -f "$CORE_PRERM" ]] && grep -qE 'cp.*lifecycle\.sh.*NAMTOR_LIFECYCLE_CACHE|cp.*lifecycle\.sh.*/var/tmp' "$CORE_PRERM"; then
    ok "namtor.prerm caches lifecycle.sh"
else
    not_ok "namtor.prerm does not cache lifecycle.sh"
fi

# ---------------------------------------------------------------------------
# (o) Architecture fields
# ---------------------------------------------------------------------------
echo "--- (o) Architecture fields ---"

for pkg in namtor namtor-ui; do
    body="$(stanza_body "$pkg")"
    if printf '%s\n' "$body" | grep -qE '^Architecture:[[:space:]]+(amd64[[:space:]]+arm64|arm64[[:space:]]+amd64)'; then
        ok "$pkg has Architecture: amd64 arm64"
    else
        not_ok "$pkg missing or wrong Architecture field"
    fi
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "== Results: $PASS passed, $FAIL failed =="
if [[ $FAIL -gt 0 ]]; then
    echo "Failed tests:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    exit 1
fi
exit 0
