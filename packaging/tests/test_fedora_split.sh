#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Task 21 — Fedora split-package structural test.
#
# Validates that packaging/fedora/namtor.spec produces TWO subpackages
# from one spec:
#   - namtor       (core: daemon, worker, CLI, PAM, systemd, sysusers,
#                   tmpfiles, D-Bus, polkit, manifests, models, licences,
#                   lifecycle helper, SBOM)
#   - namtor-ui (GTK4/libadwaita wizard + .desktop + .metainfo + .svg)
#
# A third subpackage, namtor-kde (the Plasma 6 KCM), is gone. It must be
# OBSOLETED rather than merely dropped: it declared
# `Requires: namtor = %{version}-%{release}`, so a leftover install keeps
# an unsatisfiable dependency and dnf refuses to upgrade the core package.
#
# No network fetch at install. PAM off by default. Removal purges approved
# state via the lifecycle helper. The desktop subpackage has no Qt/KF6 deps
# and the core has neither. systemd/sysusers/tmpfiles macros present.
# Lifecycle helper drives the typed lifecycle phases.
#
# This is a structural test (no rpmbuild/mock available in CI) — it greps
# the spec text for the required directives, %files entries, Requires and
# scriptlet bodies. It is the same approach packaging/tests/test_install_
# default_off.sh uses for the Fedora %post body.
#
# Usage: bash packaging/tests/test_fedora_split.sh
# Exit:  0 if all assertions pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SPEC="$PACKAGING_DIR/fedora/namtor.spec"

PASS=0
FAIL=0
FAILED_TESTS=()

ok()      { echo "  PASS: $1"; PASS=$((PASS + 1)); }
not_ok()  { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); FAILED_TESTS+=("$1"); }
check()   { # check <desc> <regex-or-cond> <expected: true|false>
    local desc="$1" cond="$2" exp="$3"
    if [[ "$exp" == "true" ]]; then
        if eval "$cond"; then ok "$desc"; else not_ok "$desc (expected present)"; fi
    else
        if eval "$cond"; then not_ok "$desc (expected absent)"; else ok "$desc"; fi
    fi
}
has_line()  { grep -qE -- "$1" "$SPEC"; }   # true if regex matches anywhere
# Terminator = a line that starts a new spec section. Excludes %files-internal
# directives (%license/%doc/%dir/%config/%attr/%ghost) and macro expansions
# (%systemd_post/%sysusers_create_package/%tmpfiles_create_package) that
# legitimately start with %<letter> inside a section body.
SPEC_TERM='^%(package|description|files|prep|build|install|check|pre|post|preun|postun|posttrans|changelog|clean|trigger|verifyscript)($|[[:space:]])'
section_body() { # section_body <section-name> -> prints body until next section
    awk -v t="$SPEC_TERM" -v sec="$1" '
        $0 ~ ("^%" sec "($|[[:space:]])") { in_sec = 1; next }
        in_sec && $0 ~ t { exit }
        in_sec { print }
    ' "$SPEC"
}

echo "== Task 21: Fedora split-package structure =="

if [[ ! -f "$SPEC" ]]; then
    echo "error: spec not found: $SPEC" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# 1. Subpackage definitions
# ---------------------------------------------------------------------------
echo "--- (1) subpackage definitions ---"

# Base package is `namtor` (core) — the Name: line.
check "Name: namtor present" 'grep -qE "^Name:[[:space:]]+namtor$" "$SPEC"' true

# UI subpackage: `%package ui` (yields namtor-ui) OR `-n namtor-ui`.
if grep -qE "^%package[[:space:]]+ui($|[[:space:]])" "$SPEC" \
        || grep -qE "^%package[[:space:]]+-n[[:space:]]+namtor-ui($|[[:space:]])" "$SPEC"; then
    ok "namtor-ui subpackage defined"
else
    not_ok "namtor-ui subpackage defined"
fi

# The pre-rename namtor-gnome subpackage must be GONE — but obsoleted and
# provided, not merely renamed. dnf migrates nothing on a rename, and both
# subpackages own exactly the same four files, so a leftover namtor-gnome
# file-conflicts with namtor-ui.
if grep -qE "^%package[[:space:]]+gnome($|[[:space:]])" "$SPEC" \
        || grep -qE "^%package[[:space:]]+-n[[:space:]]+namtor-gnome($|[[:space:]])" "$SPEC"; then
    not_ok "namtor-gnome subpackage is back (it was renamed to namtor-ui)"
else
    ok "namtor-gnome subpackage renamed away"
fi
if grep -qE "^Obsoletes:[[:space:]]+namtor-gnome" "$SPEC"; then
    ok "spec Obsoletes: namtor-gnome (rename migration)"
else
    not_ok "spec does not obsolete namtor-gnome — the pre-rename subpackage will file-conflict"
fi
if grep -qE "^Provides:[[:space:]]+namtor-gnome" "$SPEC"; then
    ok "spec Provides: namtor-gnome (keeps old requirers satisfiable)"
else
    not_ok "spec does not provide namtor-gnome"
fi
# An Obsoletes only fires on an upgrade to a NEWER version, so the rename is
# inert unless Release moved past the last namtor-gnome release (0.1.0-3).
if grep -qE "^Release:[[:space:]]+([4-9]|[1-9][0-9])" "$SPEC"; then
    ok "Release bumped past the last namtor-gnome release (Obsoletes can fire)"
else
    not_ok "Release not bumped — Obsoletes: namtor-gnome never fires without a newer version"
fi

# The KDE subpackage must be GONE — but obsoleted, not merely deleted.
if grep -qE "^%package[[:space:]]+kde($|[[:space:]])" "$SPEC" \
        || grep -qE "^%package[[:space:]]+-n[[:space:]]+namtor-kde($|[[:space:]])" "$SPEC"; then
    not_ok "namtor-kde subpackage is back (the Plasma KCM was removed)"
else
    ok "namtor-kde subpackage removed"
fi
if grep -qE "^Obsoletes:[[:space:]]+namtor-kde" "$SPEC"; then
    ok "spec Obsoletes: namtor-kde (unblocks core upgrades)"
else
    not_ok "spec does not obsolete namtor-kde — a leftover install will block core upgrades"
fi
# No Qt6/KF6/CMake BuildRequires should remain.
for br in cmake qt6-qtbase-devel kf6-kirigami-devel kf6-kcoreaddons-devel extra-cmake-modules; do
    if grep -qE "^BuildRequires:[[:space:]]+${br}([[:space:]]|$)" "$SPEC"; then
        not_ok "spec still BuildRequires $br (no Qt/KF6/CMake build left)"
    else
        ok "spec has no BuildRequires $br"
    fi
done

# Each subpackage must have its own %description.
check "namtor-ui has %description" 'grep -qE "^%description[[:space:]]+ui($|[[:space:]])" "$SPEC" || grep -qE "^%description[[:space:]]+-n[[:space:]]+namtor-ui($|[[:space:]])" "$SPEC"' true

# ---------------------------------------------------------------------------
# 2. %files sections exist per subpackage
# ---------------------------------------------------------------------------
echo "--- (2) %files sections per subpackage ---"

# Core %files = bare `%files` (the base package).
if grep -qE "^%files[[:space:]]*$" "$SPEC"; then
    ok "core %files (bare) present"
else
    not_ok "core %files (bare) present"
fi
check "namtor-ui %files present" 'grep -qE "^%files[[:space:]]+ui($|[[:space:]])" "$SPEC" || grep -qE "^%files[[:space:]]+-n[[:space:]]+namtor-ui($|[[:space:]])" "$SPEC"' true

# Helper: body of a %files section (handles `-n namtor-foo` and `foo` forms).
files_body() {
    local sub="$1"  # subpackage suffix: "" (core), ui
    if [[ -z "$sub" ]]; then
        awk -v t="$SPEC_TERM" '
            /^%files[[:space:]]*$/ {f=1; next}
            f && $0 ~ t {exit}
            f {print}
        ' "$SPEC"
    else
        awk -v t="$SPEC_TERM" -v s="$sub" '
            ($0 ~ ("^%files[[:space:]]+" s "($|[[:space:]])")) ||
            ($0 ~ ("^%files[[:space:]]+-n[[:space:]]+namtor-" s "($|[[:space:]])")) { f=1; next }
            f && $0 ~ t { exit }
            f { print }
        ' "$SPEC"
    fi
}

CORE_FILES="$(files_body "")"
UI_FILES="$(files_body ui)"

contains() { printf '%s\n' "$1" | grep -qE -- "$2"; }

# ---------------------------------------------------------------------------
# 3. Core %files — daemon, worker, CLI, PAM, systemd, sysusers, tmpfiles,
#    D-Bus, polkit, manifests, lifecycle, models, licences, SBOM
# ---------------------------------------------------------------------------
echo "--- (3) core %files payload ---"

core_expect=(
    '/usr/lib/namtor/namtor-daemon'
    '/usr/lib/namtor/namtor-worker'
    '/usr/bin/namtor-cli'
    '/usr/lib/security/pam_namtor\.so'
    'namtor-daemon\.service'
    'namtor-daemon\.socket'
    'sysusers\.d/namtor\.conf'
    'tmpfiles\.d/namtor\.conf'
    'dbus-1/system-services/org\.namtor\.Manager\.service'
    'polkit-1/actions/org\.namtor\.policy'
    'model-manifest\.toml'
    'install-manifest\.toml'
    'pam-install\.sh'
    'pam-uninstall\.sh'
    'lifecycle\.sh'
)
for e in "${core_expect[@]}"; do
    if contains "$CORE_FILES" "$e"; then ok "core %files has $e"; else not_ok "core %files has $e"; fi
done

# SBOM (SPDX + CycloneDX) under %doc.
if contains "$CORE_FILES" 'namtor\.spdx\.json'; then ok "core %files has SBOM (spdx)"; else not_ok "core %files has SBOM (spdx)"; fi
if contains "$CORE_FILES" 'namtor\.cyclonedx\.json'; then ok "core %files has SBOM (cyclonedx)"; else not_ok "core %files has SBOM (cyclonedx)"; fi

# Models bundled under /var/lib/namtor/models (core owns the models dir).
if contains "$CORE_FILES" '/var/lib/namtor/models'; then ok "core %files bundles models dir"; else not_ok "core %files bundles models dir"; fi

# Licence notices bundled via %license (model licence texts).
if contains "$CORE_FILES" '%license'; then ok "core %files has %license directive"; else not_ok "core %files has %license directive"; fi
if contains "$CORE_FILES" 'licences/'; then ok "core %files bundles licence notices"; else not_ok "core %files bundles licence notices"; fi

# ---------------------------------------------------------------------------
# 4. Core %files must NOT contain desktop-only files (or any KDE leftovers)
# ---------------------------------------------------------------------------
echo "--- (4) core %files excludes desktop-only files ---"

core_exclude=(
    'namtor-enroll-wizard'
    'org\.namtor\.Wizard\.desktop'
    'org\.namtor\.Wizard\.metainfo\.xml'
    'org\.namtor\.Wizard\.svg'
    'org\.namtor\.kcm\.desktop'
    'org\.namtor\.kcm\.metainfo\.xml'
    'org\.namtor\.kcm\.svg'
    'qt6/qml/org/namtor'
    'kpackage/kcms/kcm_namtor'
)
for e in "${core_exclude[@]}"; do
    if contains "$CORE_FILES" "$e"; then not_ok "core %files wrongly contains $e"; else ok "core %files excludes $e"; fi
done

# ---------------------------------------------------------------------------
# 5. UI %files — wizard binary + .desktop + .metainfo + .svg
# ---------------------------------------------------------------------------
echo "--- (5) UI %files payload ---"

ui_expect=(
    'namtor-enroll-wizard'
    'org\.namtor\.Wizard\.desktop'
    'org\.namtor\.Wizard\.metainfo\.xml'
    'org\.namtor\.Wizard\.svg'
)
for e in "${ui_expect[@]}"; do
    if contains "$UI_FILES" "$e"; then ok "ui %files has $e"; else not_ok "ui %files has $e"; fi
done

# The UI subpackage must NOT contain KDE files or core daemon files.
ui_exclude=(
    'namtor-daemon'
    'namtor-worker'
    'pam_namtor\.so'
    'org\.namtor\.kcm'
    'qt6/qml/org/namtor'
    'kpackage/kcms'
    'namtor-daemon\.service'
)
for e in "${ui_exclude[@]}"; do
    if contains "$UI_FILES" "$e"; then not_ok "ui %files wrongly contains $e"; else ok "ui %files excludes $e"; fi
done

# ---------------------------------------------------------------------------
# 6. No KDE payload anywhere in the spec
# ---------------------------------------------------------------------------
echo "--- (6) no KDE payload in any %files section ---"

kde_payload=(
    'org\.namtor\.kcm\.desktop'
    'org\.namtor\.kcm\.metainfo\.xml'
    'org\.namtor\.kcm\.svg'
    'qt6/qml/org/namtor'
    'kpackage/kcms/kcm_namtor'
    'libnamtorqml\.so'
    'kcm_namtor\.so'
)
for e in "${kde_payload[@]}"; do
    if grep -qE -- "$e" "$SPEC"; then
        not_ok "spec still ships KDE payload $e"
    else
        ok "spec has no KDE payload $e"
    fi
done

# ---------------------------------------------------------------------------
# 7. Requires — subpackages depend on core; cross-deps forbidden
# ---------------------------------------------------------------------------
echo "--- (7) Requires wiring ---"

# UI subpackage Requires block (lines after `%package ui` up to next %).
ui_requires="$(awk '
    /^%package[[:space:]]+ui($|[[:space:]])/ ||
    /^%package[[:space:]]+-n[[:space:]]+namtor-ui($|[[:space:]])/ { f=1; next }
    f && /^%[a-zA-Z]/ { exit }
    f && /^Requires:/ { print }
' "$SPEC")"

# Core Requires block (top-level, before any %package).
core_requires="$(awk '
    /^%package[[:space:]]/ { exit }
    /^Requires:/ { print }
' "$SPEC")"

# The UI subpackage requires core exactly once with version-release pin.
if printf '%s\n' "$ui_requires" | grep -qE 'Requires:[[:space:]]+%?\{?name\}?[[:space:]]*=[[:space:]]*%?\{?version\}?-%?\{?release\}?'; then
    ok "ui Requires: namtor = %{version}-%{release}"
else
    not_ok "ui Requires: namtor = %{version}-%{release}"
fi
# The UI subpackage pulls GTK4 + libadwaita.
if printf '%s\n' "$ui_requires" | grep -qi 'gtk4'; then ok "ui Requires gtk4"; else not_ok "ui Requires gtk4"; fi
if printf '%s\n' "$ui_requires" | grep -qi 'libadwaita'; then ok "ui Requires libadwaita"; else not_ok "ui Requires libadwaita"; fi
# The desktop subpackage must NOT pull Qt6/KF6 — nothing in the tree does
# any more.
if printf '%s\n' "$ui_requires" | grep -qiE 'qt6|kf6|kirigami|kcoreaddons'; then
    not_ok "ui Requires Qt6/KF6 (forbidden)"
else
    ok "ui has no Qt6/KF6 Requires"
fi

# Core must NOT pull GTK4/libadwaita or Qt6/KF6 runtime.
if printf '%s\n' "$core_requires" | grep -qiE 'gtk4|libadwaita'; then
    not_ok "core Requires GTK4/libadwaita (must be in subpackages)"
else
    ok "core has no GTK4/libadwaita Requires"
fi
if printf '%s\n' "$core_requires" | grep -qiE 'qt6|kf6|kirigami|kcoreaddons'; then
    not_ok "core Requires Qt6/KF6 (must be in subpackages)"
else
    ok "core has no Qt6/KF6 Requires"
fi
# Core must pull onnxruntime + pam + systemd.
if printf '%s\n' "$core_requires" | grep -qi 'onnxruntime'; then ok "core Requires onnxruntime"; else not_ok "core Requires onnxruntime"; fi
if printf '%s\n' "$core_requires" | grep -qi 'pam'; then ok "core Requires pam"; else not_ok "core Requires pam"; fi
if printf '%s\n' "$core_requires" | grep -qi 'systemd'; then ok "core Requires systemd"; else not_ok "core Requires systemd"; fi

# ---------------------------------------------------------------------------
# 8. License + Source fields correct
# ---------------------------------------------------------------------------
echo "--- (8) License + Source fields ---"

# License must be a SPDX AND-expression covering GPL-3.0-only + MIT + Apache-2.0
# (the bundled models carry their own permissive licences).
license_line="$(grep -E '^License:' "$SPEC" | head -1)"
if printf '%s\n' "$license_line" | grep -q 'GPL-3.0-only'; then ok "License has GPL-3.0-only"; else not_ok "License has GPL-3.0-only"; fi
if printf '%s\n' "$license_line" | grep -q 'MIT'; then ok "License has MIT"; else not_ok "License has MIT"; fi
if printf '%s\n' "$license_line" | grep -q 'Apache-2.0'; then ok "License has Apache-2.0"; else not_ok "License has Apache-2.0"; fi

# Source0 + Source1 present (release tarball + model archive).
check "Source0 present" 'grep -qE "^Source0:" "$SPEC"' true
check "Source1 present (model archive)" 'grep -qE "^Source1:" "$SPEC"' true

# ---------------------------------------------------------------------------
# 9. systemd / sysusers / tmpfiles macros present
# ---------------------------------------------------------------------------
echo "--- (9) systemd / sysusers / tmpfiles macros ---"
check "%systemd_post present" 'grep -qE "%systemd_post" "$SPEC"' true
check "%systemd_preun present" 'grep -qE "%systemd_preun" "$SPEC"' true
check "%systemd_postun present" 'grep -qE "%systemd_postun" "$SPEC"' true
check "%sysusers_create_package present" 'grep -qE "%sysusers_create_package" "$SPEC"' true
check "%tmpfiles_create_package present" 'grep -qE "%tmpfiles_create_package" "$SPEC"' true

# ---------------------------------------------------------------------------
# 10. Lifecycle helper drives the typed phases
# ---------------------------------------------------------------------------
echo "--- (10) lifecycle helper wiring ---"
check "lifecycle.sh post-install in %post" 'grep -qE "lifecycle\.sh[[:space:]]+post-install" "$SPEC"' true
check "lifecycle.sh pre-remove in %preun" 'grep -qE "lifecycle\.sh[[:space:]]+pre-remove" "$SPEC"' true
check "lifecycle.sh post-remove in %postun (cached)" 'grep -qE "(lifecycle\.sh|namtor-lifecycle\.sh)[[:space:]]+post-remove" "$SPEC"' true
check "lifecycle.sh post-upgrade in %posttrans" 'grep -qE "lifecycle\.sh[[:space:]]+post-upgrade" "$SPEC"' true
# pre-remove must cache the helper so %postun can call it after payload delete.
check "%preun caches lifecycle.sh to /var/tmp" 'grep -qE "cp.*lifecycle\.sh.*/var/tmp/namtor-lifecycle\.sh" "$SPEC"' true

# ---------------------------------------------------------------------------
# 11. No network fetch at install; models bundled; PAM off by default
# ---------------------------------------------------------------------------
echo "--- (11) no network fetch / models bundled / PAM off ---"
# %install must extract Source1 (the model archive) — no curl/wget. Strip
# comments so a doc line mentioning "no curl" does not trip the check.
install_body="$(section_body install | sed '/^[[:space:]]*#/d')"
if printf '%s\n' "$install_body" | grep -qE 'curl|wget|ftp'; then
    not_ok "%install performs network fetch (forbidden)"
else
    ok "%install performs no network fetch"
fi
check "%install extracts model archive (tar)" 'printf "%s\n" "$install_body" | grep -qE "tar[[:space:]]+-xzf.*models"' true
# Pre-written licence.accepted records (auto-accept permissive model licences).
check "%install pre-writes licence.accepted" 'printf "%s\n" "$install_body" | grep -q "licence.accepted"' true

# PAM off by default: %post must not CALL pam-install.sh or write /etc/pam.d.
# Strip comment lines first — comments may mention /etc/pam.d to document
# that the hook does NOT touch it.
post_body="$(section_body post | sed '/^[[:space:]]*#/d')"
if printf '%s\n' "$post_body" | grep -qE 'pam-install\.sh|/etc/pam\.d'; then
    not_ok "%post edits PAM (must be default-off)"
else
    ok "%post does not edit PAM (default-off)"
fi

# ---------------------------------------------------------------------------
# 12. Removal purges approved state
# ---------------------------------------------------------------------------
echo "--- (12) removal purges approved state ---"
# %postun $1 == 0 path must invoke the cached lifecycle post-remove (which
# drives `namtor-pam-edit purge` and removes runtime dirs).
postun_body="$(section_body postun)"
check "%postun handles removal (\$1 -eq 0)" 'printf "%s\n" "$postun_body" | grep -qE "\\\$1[[:space:]]*-eq[[:space:]]*0"' true
check "%postun calls cached post-remove on purge" 'printf "%s\n" "$postun_body" | grep -qE "post-remove"' true
check "%postun removes the lifecycle cache" 'printf "%s\n" "$postun_body" | grep -qE "rm[[:space:]]+.*namtor-lifecycle\.sh"' true

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "==============================================="
echo "Task 21 Fedora split: $PASS passed, $FAIL failed"
echo "==============================================="
if [[ "$FAIL" -ne 0 ]]; then
    echo "FAILED:" >&2
    for t in "${FAILED_TESTS[@]}"; do echo "  - $t" >&2; done
    exit 1
fi
exit 0
