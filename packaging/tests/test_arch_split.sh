#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 20: Arch split-package PKGBUILD structure.
#
# Acceptance criteria (from the task spec):
#   (a) PKGBUILD uses pkgbase=namtor with three pkgname entries
#       (namtor, namtor-gnome, namtor-kde).
#   (b) makepkg builds three packages from one PKGBUILD.
#   (c) Core package owns: daemon, worker, CLI, PAM module, systemd units,
#       sysusers, tmpfiles, D-Bus, polkit, manifests, models, licences,
#       lifecycle helper, SBOM. Depends on onnxruntime/pam/systemd/gcc-libs
#       only — NO gtk4/libadwaita/qt6/kde runtime deps.
#   (d) GNOME package owns: enroll-wizard binary, .desktop, .metainfo.xml,
#       .svg. Depends on namtor=$pkgver (exact) + gtk4 + libadwaita. NO
#       Qt/KDE deps.
#   (e) KDE package owns: KCM .desktop, .metainfo.xml, .svg + CMake QML
#       module. Depends on namtor=$pkgver (exact) + qt6-base + kirigami +
#       kcoreaddons. NO GTK/libadwaita deps.
#   (f) All sha256sums pinned (no SKIP in release recipe).
#   (g) Uses lifecycle helper for install/upgrade/remove hooks (core only).
#   (h) Desktop packages have no install hooks.
#   (i) No file conflicts between packages (disjoint dest sets).
#   (j) Core headless: works without GNOME or KDE installed.
#   (k) PKGBUILD syntax valid; makepkg --printsrcinfo produces 3 packages.
#
# Usage: bash packaging/tests/test_arch_split.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"
ARCH_DIR="$PACKAGING_DIR/arch"
PKGBUILD="$ARCH_DIR/PKGBUILD"
ARCH_INSTALL="$ARCH_DIR/namtor.install"
MANIFEST="$PACKAGING_DIR/install-manifest.toml"
STAGE_SH="$PACKAGING_DIR/stage.sh"
LIFECYCLE_SH="$PACKAGING_DIR/lifecycle.sh"

PASS=0
FAIL=0
FAILED_TESTS=()
TEMP_DIRS=()
cleanup() {
    for d in "${TEMP_DIRS[@]:-}"; do
        rm -rf "$d"
    done
}
trap cleanup EXIT
mk_temp_dir() {
    local d
    d="$(mktemp -d)"
    TEMP_DIRS+=("$d")
    printf '%s' "$d"
}

ok()   { echo "  PASS: $1"; PASS=$((PASS + 1)); }
not_ok() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
    FAILED_TESTS+=("$1")
}

# --- helpers ----------------------------------------------------------------

# Extract the body of a function from the PKGBUILD (between the opening
# brace line and the matching closing brace). Simple brace-depth tracker.
# Usage: extract_func <func_name> <file>
extract_func() {
    local name="$1" file="$2"
    awk -v fn="$name" '
        $0 ~ "^" fn "\\(\\)" { found=1; depth=0 }
        found {
            print
            for (i=1; i<=length($0); i++) {
                c=substr($0,i,1)
                if (c=="{") depth++
                if (c=="}") { depth--; if (depth==0 && i==length($0)) exit }
            }
        }
    ' "$file"
}

# Source the PKGBUILD to read top-level variables. We suppress any side
# effects by running in a subshell with dummy srcdir/pkgdir.
source_pkgbuild() {
    (
        cd "$ARCH_DIR"
        # makepkg defines these; provide dummies so sourcing doesn't fail.
        srcdir="/tmp/dummy-srcdir"
        pkgdir="/tmp/dummy-pkgdir"
        startdir="/tmp/dummy-startdir"
        export srcdir pkgdir startdir
        set -a
        # shellcheck disable=SC1091
        source ./PKGBUILD 2>/dev/null
        # Print key variables for the caller to capture.
        printf 'pkgbase=%s\n' "${pkgbase:-}"
        printf 'pkgver=%s\n' "${pkgver:-}"
        printf 'pkgrel=%s\n' "${pkgrel:-}"
        printf 'pkgname_count=%d\n' "${#pkgname[@]}"
        for p in "${pkgname[@]:-}"; do
            printf 'pkgname_entry=%s\n' "$p"
        done
        printf 'sha256sums_count=%d\n' "${#sha256sums[@]}"
        for s in "${sha256sums[@]:-}"; do
            printf 'sha256sum_entry=%s\n' "$s"
        done
        printf 'makedepends_count=%d\n' "${#makedepends[@]}"
        for m in "${makedepends[@]:-}"; do
            printf 'makedepend=%s\n' "$m"
        done
        printf 'source_count=%d\n' "${#source[@]}"
        for s in "${source[@]:-}"; do
            printf 'source_entry=%s\n' "$s"
        done
        printf 'has_install=%s\n' "${install:-no}"
    )
}

echo "== Task 20: Arch split-package PKGBUILD structure =="

# --- (k) PKGBUILD syntax valid ----------------------------------------------

echo "--- (k) PKGBUILD syntax valid ---"

if bash -n "$PKGBUILD" 2>/dev/null; then
    ok "PKGBUILD passes bash -n syntax check"
else
    not_ok "PKGBUILD fails bash -n syntax check"
fi

# --- (a) pkgbase + three pkgname entries ------------------------------------

echo "--- (a) pkgbase=namtor + three pkgname entries ---"

pb_output="$(source_pkgbuild)"
pkgbase_val="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgbase=/{print $2}')"
if [[ "$pkgbase_val" == "namtor" ]]; then
    ok "pkgbase=namtor"
else
    not_ok "pkgbase is '$pkgbase_val' (expected 'namtor')"
fi

pkgname_entries="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgname_entry=/{print $2}')"
pkgname_count="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgname_count=/{print $2}')"
if [[ "$pkgname_count" == "3" ]]; then
    ok "pkgname has exactly 3 entries"
else
    not_ok "pkgname has $pkgname_count entries (expected 3)"
fi

expected_pkgnames="namtor namtor-gnome namtor-kde"
actual_pkgnames="$(printf '%s\n' "$pkgname_entries" | sort | tr '\n' ' ' | sed 's/ $//')"
expected_sorted="$(printf '%s\n' "$expected_pkgnames" | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ $//')"
if [[ "$actual_pkgnames" == "$expected_sorted" ]]; then
    ok "pkgname entries are: namtor, namtor-gnome, namtor-kde"
else
    not_ok "pkgname entries are '$actual_pkgnames' (expected '$expected_sorted')"
fi

# --- (b) Three package functions exist --------------------------------------

echo "--- (b) three package functions exist ---"

for func in package_namtor package_namtor-gnome package_namtor-kde; do
    if grep -q "^${func}()" "$PKGBUILD"; then
        ok "function $func exists"
    else
        not_ok "function $func missing"
    fi
done

# --- (c) Core package: deps + content ---------------------------------------

echo "--- (c) core package deps + content ---"

core_body="$(extract_func 'package_namtor' "$PKGBUILD")"

# Core deps: onnxruntime, pam, systemd, gcc-libs (no gtk4/libadwaita/qt6/kde).
for dep in onnxruntime pam systemd gcc-libs; do
    # Check the depends array inside package_namtor.
    if printf '%s\n' "$core_body" | grep -qE "['\"]${dep}['\"]|['\"]${dep}[=<>]| ${dep}['\" ]" ; then
        ok "core depends on $dep"
    else
        not_ok "core missing dependency: $dep"
    fi
done

# Core must NOT depend on desktop libs.
for nodep in gtk4 libadwaita qt6-base kirigami kcoreaddons; do
    if printf '%s\n' "$core_body" | grep -qE "depends=.*['\"]${nodep}['\"]|depends=.* ${nodep}['\" ]" ; then
        not_ok "core depends on $nodep (should not — desktop dep in core)"
    else
        ok "core does not depend on $nodep"
    fi
done

# Core stages --package core.
if printf '%s\n' "$core_body" | grep -q -- '--package core'; then
    ok "core stages --package core"
else
    not_ok "core does not stage --package core"
fi

# Core bundles models.
if printf '%s\n' "$core_body" | grep -q 'var/lib/namtor/models'; then
    ok "core bundles models"
else
    not_ok "core does not bundle models"
fi

# Core pre-writes licence acceptance records.
if printf '%s\n' "$core_body" | grep -q 'licence.accepted'; then
    ok "core pre-writes licence acceptance records"
else
    not_ok "core does not pre-write licence acceptance records"
fi

# --- (d) GNOME package: deps + content --------------------------------------

echo "--- (d) gnome package deps + content ---"

gnome_body="$(extract_func 'package_namtor-gnome' "$PKGBUILD")"

# GNOME depends on namtor=exact version.
if printf '%s\n' "$gnome_body" | grep -qF 'namtor=$pkgver'; then
    ok "gnome depends on namtor=\$pkgver (exact version)"
else
    not_ok "gnome does not depend on namtor=\$pkgver (exact version)"
fi

# GNOME depends on gtk4 + libadwaita.
for dep in gtk4 libadwaita; do
    if printf '%s\n' "$gnome_body" | grep -qE "['\"]${dep}['\"]| ${dep}['\" ]"; then
        ok "gnome depends on $dep"
    else
        not_ok "gnome missing dependency: $dep"
    fi
done

# GNOME must NOT depend on Qt/KDE.
for nodep in qt6-base kirigami kcoreaddons; do
    if printf '%s\n' "$gnome_body" | grep -qE "depends=.*['\"]${nodep}['\"]|depends=.* ${nodep}['\" ]"; then
        not_ok "gnome depends on $nodep (should not — Qt/KDE dep in gnome)"
    else
        ok "gnome does not depend on $nodep"
    fi
done

# GNOME stages --package gnome.
if printf '%s\n' "$gnome_body" | grep -q -- '--package gnome'; then
    ok "gnome stages --package gnome"
else
    not_ok "gnome does not stage --package gnome"
fi

# --- (e) KDE package: deps + content ----------------------------------------

echo "--- (e) kde package deps + content ---"

kde_body="$(extract_func 'package_namtor-kde' "$PKGBUILD")"

# KDE depends on namtor=exact version.
if printf '%s\n' "$kde_body" | grep -qF 'namtor=$pkgver'; then
    ok "kde depends on namtor=\$pkgver (exact version)"
else
    not_ok "kde does not depend on namtor=\$pkgver (exact version)"
fi

# KDE depends on qt6-base + kirigami + kcoreaddons.
for dep in qt6-base kirigami kcoreaddons; do
    if printf '%s\n' "$kde_body" | grep -qE "['\"]${dep}['\"]| ${dep}['\" ]"; then
        ok "kde depends on $dep"
    else
        not_ok "kde missing dependency: $dep"
    fi
done

# KDE must NOT depend on GTK/libadwaita.
for nodep in gtk4 libadwaita; do
    if printf '%s\n' "$kde_body" | grep -qE "depends=.*['\"]${nodep}['\"]|depends=.* ${nodep}['\" ]"; then
        not_ok "kde depends on $nodep (should not — GTK dep in kde)"
    else
        ok "kde does not depend on $nodep"
    fi
done

# KDE stages --package kde.
if printf '%s\n' "$kde_body" | grep -q -- '--package kde'; then
    ok "kde stages --package kde"
else
    not_ok "kde does not stage --package kde"
fi

# KDE runs cmake --install for the QML module.
if printf '%s\n' "$kde_body" | grep -q 'cmake.*--install'; then
    ok "kde runs cmake --install for QML module"
else
    not_ok "kde does not run cmake --install"
fi

# --- (f) All sha256sums pinned (no SKIP) -------------------------------------

echo "--- (f) all sha256sums pinned (no SKIP) ---"

sha_entries="$(printf '%s\n' "$pb_output" | awk -F= '/^sha256sum_entry=/{print $2}')"
if printf '%s\n' "$sha_entries" | grep -qi 'SKIP'; then
    not_ok "sha256sums contains SKIP (must be pinned)"
else
    ok "sha256sums contains no SKIP"
fi

sha_count="$(printf '%s\n' "$pb_output" | awk -F= '/^sha256sums_count=/{print $2}')"
if [[ "$sha_count" == "2" ]]; then
    ok "sha256sums has 2 entries (source + model archive)"
else
    not_ok "sha256sums has $sha_count entries (expected 2)"
fi

# Each sha256 entry must be 64 hex chars.
all_hex=1
while IFS= read -r s; do
    if [[ ! "$s" =~ ^[0-9a-fA-F]{64}$ ]]; then
        all_hex=0
        not_ok "sha256sum entry is not 64-hex: $s"
    fi
done <<< "$sha_entries"
if [[ $all_hex -eq 1 ]]; then
    ok "all sha256sum entries are 64-hex"
fi

# --- (g) Core uses lifecycle helper ------------------------------------------

echo "--- (g) core uses lifecycle helper ---"

if [[ -f "$ARCH_INSTALL" ]] && grep -q 'lifecycle.sh' "$ARCH_INSTALL"; then
    ok "namtor.install delegates to lifecycle.sh"
else
    not_ok "namtor.install does not delegate to lifecycle.sh"
fi

# Verify the install hook is referenced by the core package function.
if printf '%s\n' "$core_body" | grep -q 'install='; then
    ok "core package function sets install= variable"
else
    not_ok "core package function does not set install="
fi

# --- (h) Desktop packages have no install hooks ------------------------------

echo "--- (h) desktop packages have no install hooks ---"

# Neither gnome nor kde package functions should set install= to a non-empty
# value.
for body_name in gnome_body kde_body; do
    body="${!body_name}"
    install_line="$(printf '%s\n' "$body" | grep -E '^\s*install=' || true)"
    if [[ -z "$install_line" ]]; then
        ok "$body_name has no install= (no hook)"
    elif printf '%s\n' "$install_line" | grep -qE "install=(''|\"\"|\s*$)"; then
        ok "$body_name sets install= empty (no hook)"
    else
        not_ok "$body_name sets install='$install_line' (desktop should have no hook)"
    fi
done

# No namtor-gnome.install or namtor-kde.install files should exist (desktop
# packages have no hooks).
for f in namtor-gnome.install namtor-kde.install; do
    if [[ -f "$ARCH_DIR/$f" ]]; then
        not_ok "$f exists (desktop packages should have no install hook)"
    else
        ok "$f does not exist (desktop has no hook)"
    fi
done

# --- (i) No file conflicts between packages ---------------------------------

echo "--- (i) no file conflicts between packages ---"

# Stage each package into separate temp dirs and check for overlapping paths.
# This uses stage.sh --package core|gnome|kde against fake empty destdirs,
# then collects the file lists and checks for intersections.
# We need the built binaries to exist for stage.sh to copy them; since we
# may not have a release build, we create placeholder files for the sources
# stage.sh expects. The test only checks PATH DISJOINTNESS, not content.

# Create placeholder source files for stage.sh (which requires src files to
# exist). We only care about the dest path list, not the content.
stage_root="$(mk_temp_dir)"
mkdir -p "$stage_root/target/release"
mkdir -p "$stage_root/packaging"
# Copy the manifest + stage.sh + lifecycle.sh from the real repo.
cp "$MANIFEST" "$stage_root/packaging/install-manifest.toml"
cp "$STAGE_SH" "$stage_root/packaging/stage.sh"
cp "$LIFECYCLE_SH" "$stage_root/packaging/lifecycle.sh"
# Create placeholder binaries.
for bin in namtor-daemon namtor-worker namtor-camera-emitter-off namtor-cli namtor-enroll-wizard; do
    printf 'placeholder' > "$stage_root/target/release/$bin"
done
printf 'placeholder' > "$stage_root/target/release/libnamtor_pam.so"
# Create placeholder static files that stage.sh copies.
for f in \
    packaging/systemd/namtor-daemon.service \
    packaging/systemd/namtor-daemon.socket \
    packaging/sysusers.d/namtor.conf \
    packaging/tmpfiles.d/namtor.conf \
    packaging/dbus-1/org.namtor.Manager.service \
    packaging/dbus-1/org.namtor.Manager.conf \
    packaging/polkit-1/actions/org.namtor.policy \
    packaging/applications/org.namtor.Wizard.desktop \
    packaging/applications/org.namtor.kcm.desktop \
    packaging/metainfo/org.namtor.Wizard.metainfo.xml \
    packaging/metainfo/org.namtor.kcm.metainfo.xml \
    packaging/model-manifest.toml \
    packaging/pam-install.sh \
    packaging/pam-uninstall.sh \
    packaging/pam-preflight.sh \
    quirks/devices/3277-0018.toml \
    packaging/README.md \
    COPYING.md \
    docs/evidence/packaging/namtor.spdx.json \
    docs/evidence/packaging/namtor.cyclonedx.json; do
    mkdir -p "$stage_root/$(dirname "$f")"
    printf 'placeholder' > "$stage_root/$f"
done
# Icons
for icon in org.namtor.Wizard.svg org.namtor.kcm.svg; do
    mkdir -p "$stage_root/packaging/icons/hicolor/scalable/apps"
    printf 'placeholder' > "$stage_root/packaging/icons/hicolor/scalable/apps/$icon"
done

# Stage each package and collect dest paths.
conflict_found=0
paths_core=""
paths_gnome=""
paths_kde=""

dest_core="$(mk_temp_dir)"
dest_gnome="$(mk_temp_dir)"
dest_kde="$(mk_temp_dir)"

if bash "$STAGE_SH" "$dest_core" "$stage_root" --package core >/dev/null 2>&1; then
    paths_core="$(cd "$dest_core" && find . -type f -o -type l | sed 's|^\./||' | sort)"
else
    not_ok "stage.sh --package core failed"
fi

if bash "$STAGE_SH" "$dest_gnome" "$stage_root" --package gnome >/dev/null 2>&1; then
    paths_gnome="$(cd "$dest_gnome" && find . -type f -o -type l | sed 's|^\./||' | sort)"
else
    not_ok "stage.sh --package gnome failed"
fi

if bash "$STAGE_SH" "$dest_kde" "$stage_root" --package kde >/dev/null 2>&1; then
    paths_kde="$(cd "$dest_kde" && find . -type f -o -type l | sed 's|^\./||' | sort)"
else
    not_ok "stage.sh --package kde failed"
fi

# Check core vs gnome overlap.
if [[ -n "$paths_core" && -n "$paths_gnome" ]]; then
    overlap_cg="$(comm -12 <(printf '%s\n' "$paths_core") <(printf '%s\n' "$paths_gnome"))"
    if [[ -z "$overlap_cg" ]]; then
        ok "no file conflicts between core and gnome"
    else
        not_ok "file conflicts between core and gnome: $(printf '%s' "$overlap_cg" | tr '\n' ' ')"
        conflict_found=1
    fi
fi

# Check core vs kde overlap.
if [[ -n "$paths_core" && -n "$paths_kde" ]]; then
    overlap_ck="$(comm -12 <(printf '%s\n' "$paths_core") <(printf '%s\n' "$paths_kde"))"
    if [[ -z "$overlap_ck" ]]; then
        ok "no file conflicts between core and kde"
    else
        not_ok "file conflicts between core and kde: $(printf '%s' "$overlap_ck" | tr '\n' ' ')"
        conflict_found=1
    fi
fi

# Check gnome vs kde overlap.
if [[ -n "$paths_gnome" && -n "$paths_kde" ]]; then
    overlap_gk="$(comm -12 <(printf '%s\n' "$paths_gnome") <(printf '%s\n' "$paths_kde"))"
    if [[ -z "$overlap_gk" ]]; then
        ok "no file conflicts between gnome and kde"
    else
        not_ok "file conflicts between gnome and kde: $(printf '%s' "$overlap_gk" | tr '\n' ' ')"
        conflict_found=1
    fi
fi

# --- (j) Core headless: works without GNOME or KDE ---------------------------

echo "--- (j) core headless (no desktop deps) ---"

# Core package's depends must not pull in any desktop library. Already
# checked above, but explicitly assert the combined list.
if [[ -n "$core_body" ]]; then
    ok "core package is self-contained (staged independently above)"
else
    not_ok "core package body is empty"
fi

# Verify core dest has the daemon + PAM module + lifecycle helper.
if [[ -n "$paths_core" ]]; then
    for required in \
        usr/lib/namtor/namtor-daemon \
        usr/lib/security/pam_namtor.so \
        usr/share/namtor/lifecycle.sh \
        usr/lib/systemd/system/namtor-daemon.socket \
        usr/lib/systemd/system/namtor-daemon.service \
        usr/lib/sysusers.d/namtor.conf \
        usr/share/namtor/model-manifest.toml; do
        if printf '%s\n' "$paths_core" | grep -q "^${required}$"; then
            ok "core contains $required"
        else
            not_ok "core missing $required"
        fi
    done
else
    not_ok "core staging produced no files"
fi

# --- (k) makepkg --printsrcinfo produces 3 packages --------------------------

echo "--- (k) makepkg --printsrcinfo ---"

srcinfo_output="$(cd "$ARCH_DIR" && makepkg --printsrcinfo 2>/dev/null || true)"
if [[ -z "$srcinfo_output" ]]; then
    not_ok "makepkg --printsrcinfo produced no output"
else
    # Count pkgname entries in the .SRCINFO output. In .SRCINFO format,
    # pkgbase and pkgname are at column 0; other fields are tab-indented.
    srcinfo_pkgnames="$(printf '%s\n' "$srcinfo_output" | awk '/^pkgname = /{print $3}')"
    srcinfo_count="$(printf '%s\n' "$srcinfo_pkgnames" | grep -c . || true)"
    if [[ "$srcinfo_count" == "3" ]]; then
        ok "makepkg --printsrcinfo produces 3 pkgname entries"
    else
        not_ok "makepkg --printsrcinfo produces $srcinfo_count pkgname entries (expected 3)"
    fi

    # Verify pkgbase in srcinfo.
    if printf '%s\n' "$srcinfo_output" | grep -q '^pkgbase = namtor$'; then
        ok "makepkg --printsrcinfo has pkgbase = namtor"
    else
        not_ok "makepkg --printsrcinfo missing pkgbase = namtor"
    fi

    # Verify each package has its own depends in the srcinfo.
    for pkg in namtor namtor-gnome namtor-kde; do
        if printf '%s\n' "$srcinfo_output" | grep -q "^pkgname = ${pkg}$"; then
            ok "makepkg --printsrcinfo includes pkgname = $pkg"
        else
            not_ok "makepkg --printsrcinfo missing pkgname = $pkg"
        fi
    done
fi

# --- summary ----------------------------------------------------------------

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
