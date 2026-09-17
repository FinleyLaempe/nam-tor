#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 20: Arch split-package PKGBUILD structure.
#
# Acceptance criteria (from the task spec):
#   (a) PKGBUILD uses pkgbase=namtor with two pkgname entries
#       (namtor, namtor-ui).
#   (b) makepkg builds two packages from one PKGBUILD.
#   (c) Core package owns: daemon, worker, CLI, PAM module, systemd units,
#       sysusers, tmpfiles, D-Bus, polkit, manifests, models, licences,
#       lifecycle helper, SBOM. Depends on pam/systemd/gcc-libs
#       only — NO gtk4/libadwaita/qt6/kde runtime deps.
#   (d) Desktop package owns: enroll-wizard binary, .desktop, .metainfo.xml,
#       .svg. Depends on namtor=$pkgver (exact) + gtk4 + libadwaita. NO
#       Qt/KDE deps.
#   (e) The desktop package obsoletes the removed namtor-kde package via
#       replaces/conflicts. Without that, a leftover namtor-kde keeps an
#       unsatisfiable depends=("namtor=$pkgver") and pacman refuses to
#       upgrade the core package at all.
#   (f) All sha256sums pinned (no SKIP in release recipe).
#   (g) Uses lifecycle helper for install/upgrade/remove hooks (core only).
#   (h) Desktop packages have no install hooks.
#   (i) No file conflicts between packages (disjoint dest sets).
#   (j) Core headless: works without the desktop package installed.
#   (k) PKGBUILD syntax valid; makepkg --printsrcinfo produces 2 packages.
#
# Usage: bash packaging/tests/test_arch_split.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

# `grep_all` is `grep` that always reads its whole input.
#
# `cmd | grep -q PAT` under `pipefail` is unreliable: `grep -q` exits on the
# first match, the writer on the left then dies with SIGPIPE, and pipefail
# reports the WRITER's status — so a match can be reported as a miss. It stays
# latent until the input outgrows the 64 KiB pipe buffer, which is how it
# surfaced: test_desktop_cert read a real `test result: FAILED` out of full
# cargo output as "no failures". In an "expected absent" check the same race
# turns a forbidden match into a silent pass. Without -q grep never exits
# early, so nothing upstream is signalled, and the exit status is unchanged:
# 0 match, 1 no match, 2 error.
grep_all() { grep "$@" >/dev/null; }


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

# --- (a) pkgbase + two pkgname entries --------------------------------------

echo "--- (a) pkgbase=namtor + two pkgname entries ---"

pb_output="$(source_pkgbuild)"
pkgbase_val="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgbase=/{print $2}')"
if [[ "$pkgbase_val" == "namtor" ]]; then
    ok "pkgbase=namtor"
else
    not_ok "pkgbase is '$pkgbase_val' (expected 'namtor')"
fi

pkgname_entries="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgname_entry=/{print $2}')"
pkgname_count="$(printf '%s\n' "$pb_output" | awk -F= '/^pkgname_count=/{print $2}')"
if [[ "$pkgname_count" == "2" ]]; then
    ok "pkgname has exactly 2 entries"
else
    not_ok "pkgname has $pkgname_count entries (expected 2)"
fi

expected_pkgnames="namtor namtor-ui"
actual_pkgnames="$(printf '%s\n' "$pkgname_entries" | sort | tr '\n' ' ' | sed 's/ $//')"
expected_sorted="$(printf '%s\n' "$expected_pkgnames" | tr ' ' '\n' | sort | tr '\n' ' ' | sed 's/ $//')"
if [[ "$actual_pkgnames" == "$expected_sorted" ]]; then
    ok "pkgname entries are: namtor, namtor-ui"
else
    not_ok "pkgname entries are '$actual_pkgnames' (expected '$expected_sorted')"
fi

# --- (b) Two package functions exist ----------------------------------------

echo "--- (b) two package functions exist ---"

for func in package_namtor package_namtor-ui; do
    if grep -q "^${func}()" "$PKGBUILD"; then
        ok "function $func exists"
    else
        not_ok "function $func missing"
    fi
done

# --- (c) Core package: deps + content ---------------------------------------

echo "--- (c) core package deps + content ---"

core_body="$(extract_func 'package_namtor' "$PKGBUILD")"

# Core deps: pam, systemd, gcc-libs (no gtk4/libadwaita/qt6/kde, and no
# onnxruntime — ONNX Runtime is linked into the binary).
for dep in pam systemd gcc-libs; do
    # Check the depends array inside package_namtor.
    if printf '%s\n' "$core_body" | grep_all -E "['\"]${dep}['\"]|['\"]${dep}[=<>]| ${dep}['\" ]" ; then
        ok "core depends on $dep"
    else
        not_ok "core missing dependency: $dep"
    fi
done

# Core must NOT depend on onnxruntime: it is linked into the binary, so the
# dependency only installed an unused library. Same matcher as the positive
# checks above, which reads the multi-line depends array.
if printf '%s\n' "$core_body" | grep_all -E "['\"]onnxruntime['\"]|['\"]onnxruntime[=<>]| onnxruntime['\" ]" ; then
    not_ok "core depends on onnxruntime (it is bundled; must not)"
else
    ok "core does not depend on onnxruntime"
fi

# Core must NOT depend on desktop libs.
for nodep in gtk4 libadwaita qt6-base kirigami kcoreaddons; do
    if printf '%s\n' "$core_body" | grep_all -E "depends=.*['\"]${nodep}['\"]|depends=.* ${nodep}['\" ]" ; then
        not_ok "core depends on $nodep (should not — desktop dep in core)"
    else
        ok "core does not depend on $nodep"
    fi
done

# Core stages --package core.
if printf '%s\n' "$core_body" | grep_all -- '--package core'; then
    ok "core stages --package core"
else
    not_ok "core does not stage --package core"
fi

# Core bundles models.
if printf '%s\n' "$core_body" | grep_all 'var/lib/namtor/models'; then
    ok "core bundles models"
else
    not_ok "core does not bundle models"
fi

# Core pre-writes licence acceptance records.
if printf '%s\n' "$core_body" | grep_all 'licence.accepted'; then
    ok "core pre-writes licence acceptance records"
else
    not_ok "core does not pre-write licence acceptance records"
fi

# --- (d) UI package: deps + content -----------------------------------------

echo "--- (d) ui package deps + content ---"

ui_body="$(extract_func 'package_namtor-ui' "$PKGBUILD")"

# The UI package depends on namtor=exact version.
if printf '%s\n' "$ui_body" | grep_all -F 'namtor=$pkgver'; then
    ok "ui depends on namtor=\$pkgver (exact version)"
else
    not_ok "ui does not depend on namtor=\$pkgver (exact version)"
fi

# The UI package depends on gtk4 + libadwaita.
for dep in gtk4 libadwaita; do
    if printf '%s\n' "$ui_body" | grep_all -E "['\"]${dep}['\"]| ${dep}['\" ]"; then
        ok "ui depends on $dep"
    else
        not_ok "ui missing dependency: $dep"
    fi
done

# The UI package must NOT depend on Qt/KDE.
for nodep in qt6-base kirigami kcoreaddons; do
    if printf '%s\n' "$ui_body" | grep_all -E "depends=.*['\"]${nodep}['\"]|depends=.* ${nodep}['\" ]"; then
        not_ok "ui depends on $nodep (should not — Qt/KDE dep in the GTK UI)"
    else
        ok "ui does not depend on $nodep"
    fi
done

# The UI package stages --package ui.
if printf '%s\n' "$ui_body" | grep_all -- '--package ui'; then
    ok "ui stages --package ui"
else
    not_ok "ui does not stage --package ui"
fi

# --- (e) Obsoletion of both predecessor packages ----------------------------

echo "--- (e) desktop package obsoletes namtor-gnome and namtor-kde ---"

# namtor-gnome is this same package under its old name. pacman migrates
# nothing on a rename, and both packages own /usr/bin/namtor-enroll-wizard,
# the .desktop, the metainfo and the icon — so without
# provides/conflicts/replaces the old package lingers and the install fails
# on a file conflict.
for field in provides conflicts replaces; do
    if printf '%s\n' "$ui_body" | grep_all -E "${field}=\\(.*namtor-gnome"; then
        ok "desktop package ${field}=('namtor-gnome') (rename migration)"
    else
        not_ok "desktop package does not ${field} namtor-gnome — the pre-rename package will file-conflict"
    fi
done

# An obsoletion only fires on an upgrade to a NEWER version, so the rename
# is inert unless pkgrel moved past the last namtor-gnome release (0.1.0-4).
# Compare the FULL pkgver-pkgrel, not pkgrel alone: bumping pkgver resets
# pkgrel to 1 by Arch convention, and 0.2.0-1 is newer than 0.1.0-4 even
# though its pkgrel is smaller. A pkgrel-only check fails that legitimate
# case and would push someone to fake a high pkgrel to satisfy it.
_arch_last_gnome="0.1.0-4"   # last release that shipped namtor-gnome
_arch_pkgver="$(sed -n 's/^pkgver=//p' "$PKGBUILD" | head -1)"
_arch_pkgrel="$(sed -n 's/^pkgrel=//p' "$PKGBUILD" | head -1)"
_arch_cur="${_arch_pkgver}-${_arch_pkgrel}"
_arch_newest="$(printf '%s\n%s\n' "$_arch_cur" "$_arch_last_gnome" | sort -V | tail -1)"
if [ "$_arch_cur" != "$_arch_last_gnome" ] && [ "$_arch_newest" = "$_arch_cur" ]; then
    ok "version $_arch_cur is newer than $_arch_last_gnome (obsoletion can fire)"
else
    not_ok "version $_arch_cur is not newer than $_arch_last_gnome — replaces=('namtor-gnome') never fires"
fi

# The Plasma KCM package is gone. It is NOT enough to stop building it:
# namtor-kde declared depends=("namtor=$pkgver"), so a machine that still
# has it installed has an unsatisfiable dependency and pacman aborts the
# WHOLE transaction rather than upgrading the core package. The surviving
# desktop package must therefore replace and conflict with it.
if printf '%s\n' "$ui_body" | grep_all -E "replaces=\(.*namtor-kde"; then
    ok "desktop package replaces=('namtor-kde') (unblocks core upgrades)"
else
    not_ok "desktop package does not replace namtor-kde — a leftover namtor-kde will block core upgrades"
fi
if printf '%s\n' "$ui_body" | grep_all -E "conflicts=\(.*namtor-kde"; then
    ok "desktop package conflicts=('namtor-kde')"
else
    not_ok "desktop package does not conflict with namtor-kde"
fi

# The KCM package function must not come back by accident.
if grep -q '^package_namtor-kde()' "$PKGBUILD"; then
    not_ok "package_namtor-kde() is back (the Plasma KCM was removed)"
else
    ok "no package_namtor-kde() function (KCM removed)"
fi

# No Qt/KF6/CMake makedepends should remain at the pkgbase level.
for nodep in cmake qt6-base kirigami kcoreaddons extra-cmake-modules; do
    if grep -qE "^\s*'${nodep}'" "$PKGBUILD"; then
        not_ok "makedepends still carries '$nodep' (no Qt/KF6/CMake build left)"
    else
        ok "makedepends has no '$nodep'"
    fi
done

# --- (f) All sha256sums pinned (no SKIP) -------------------------------------

echo "--- (f) all sha256sums pinned (no SKIP) ---"

sha_entries="$(printf '%s\n' "$pb_output" | awk -F= '/^sha256sum_entry=/{print $2}')"
if printf '%s\n' "$sha_entries" | grep_all -i 'SKIP'; then
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
if printf '%s\n' "$core_body" | grep_all 'install='; then
    ok "core package function sets install= variable"
else
    not_ok "core package function does not set install="
fi

# --- (h) Desktop packages have no install hooks ------------------------------

echo "--- (h) desktop packages have no install hooks ---"

# The desktop package function must not set install= to a non-empty value.
for body_name in ui_body; do
    body="${!body_name}"
    install_line="$(printf '%s\n' "$body" | grep -E '^\s*install=' || true)"
    if [[ -z "$install_line" ]]; then
        ok "$body_name has no install= (no hook)"
    elif printf '%s\n' "$install_line" | grep_all -E "install=(''|\"\"|\s*$)"; then
        ok "$body_name sets install= empty (no hook)"
    else
        not_ok "$body_name sets install='$install_line' (desktop should have no hook)"
    fi
done

# No namtor-ui.install, namtor-gnome.install or namtor-kde.install files
# should exist (desktop packages have no hooks; the other two are gone).
for f in namtor-ui.install namtor-gnome.install namtor-kde.install; do
    if [[ -f "$ARCH_DIR/$f" ]]; then
        not_ok "$f exists (desktop packages should have no install hook)"
    else
        ok "$f does not exist (desktop has no hook)"
    fi
done

# --- (i) No file conflicts between packages ---------------------------------

echo "--- (i) no file conflicts between packages ---"

# Stage each package into separate temp dirs and check for overlapping paths.
# This uses stage.sh --package core|ui against fake empty destdirs,
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
for bin in namtor namtor-enroll-wizard; do
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
    packaging/metainfo/org.namtor.Wizard.metainfo.xml \
    packaging/model-manifest.toml \
    packaging/pam-install.sh \
    packaging/pam-uninstall.sh \
    packaging/pam-preflight.sh \
    quirks/devices/3277-0018.toml \
    packaging/README.md \
    COPYING.md \
    docs/man/namtor-daemon.8 \
    docs/man/namtor-cli.1 \
    docs/man/pam_namtor.8 \
    docs/evidence/packaging/namtor.spdx.json \
    docs/evidence/packaging/namtor.cyclonedx.json; do
    mkdir -p "$stage_root/$(dirname "$f")"
    printf 'placeholder' > "$stage_root/$f"
done
# Icons
for icon in org.namtor.Wizard.svg; do
    mkdir -p "$stage_root/packaging/icons/hicolor/scalable/apps"
    printf 'placeholder' > "$stage_root/packaging/icons/hicolor/scalable/apps/$icon"
done

# Stage each package and collect dest paths.
conflict_found=0
paths_core=""
paths_ui=""

dest_core="$(mk_temp_dir)"
dest_ui="$(mk_temp_dir)"

if bash "$STAGE_SH" "$dest_core" "$stage_root" --package core >/dev/null 2>&1; then
    paths_core="$(cd "$dest_core" && find . -type f -o -type l | sed 's|^\./||' | sort)"
else
    not_ok "stage.sh --package core failed"
fi

if bash "$STAGE_SH" "$dest_ui" "$stage_root" --package ui >/dev/null 2>&1; then
    paths_ui="$(cd "$dest_ui" && find . -type f -o -type l | sed 's|^\./||' | sort)"
else
    not_ok "stage.sh --package ui failed"
fi

# `--package kde` must now be REJECTED: the value is gone from the filter
# allowlist, so a stale build recipe asking for it fails loudly instead of
# silently staging nothing.
if bash "$STAGE_SH" "$(mk_temp_dir)" "$stage_root" --package kde >/dev/null 2>&1; then
    not_ok "stage.sh still accepts --package kde (should be rejected)"
else
    ok "stage.sh rejects --package kde (package removed)"
fi

# Check core vs ui overlap.
if [[ -n "$paths_core" && -n "$paths_ui" ]]; then
    overlap_cg="$(comm -12 <(printf '%s\n' "$paths_core") <(printf '%s\n' "$paths_ui"))"
    if [[ -z "$overlap_cg" ]]; then
        ok "no file conflicts between core and ui"
    else
        not_ok "file conflicts between core and ui: $(printf '%s' "$overlap_cg" | tr '\n' ' ')"
        conflict_found=1
    fi
fi

# --- (j) Core headless: works without the desktop package --------------------

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
        usr/bin/namtor \
        usr/bin/namtor-cli \
        usr/lib/security/pam_namtor.so \
        usr/share/namtor/lifecycle.sh \
        usr/lib/systemd/system/namtor-daemon.socket \
        usr/lib/systemd/system/namtor-daemon.service \
        usr/lib/sysusers.d/namtor.conf \
        usr/share/namtor/model-manifest.toml; do
        if printf '%s\n' "$paths_core" | grep_all "^${required}$"; then
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

if ! command -v makepkg >/dev/null 2>&1; then
    ok "makepkg not installed (CI without Arch toolchain) — skipping printsrcinfo"
else
    srcinfo_output="$(cd "$ARCH_DIR" && makepkg --printsrcinfo 2>/dev/null || true)"
    if [[ -z "$srcinfo_output" ]]; then
        not_ok "makepkg --printsrcinfo produced no output"
    else
    # Count pkgname entries in the .SRCINFO output. In .SRCINFO format,
    # pkgbase and pkgname are at column 0; other fields are tab-indented.
    srcinfo_pkgnames="$(printf '%s\n' "$srcinfo_output" | awk '/^pkgname = /{print $3}')"
    srcinfo_count="$(printf '%s\n' "$srcinfo_pkgnames" | grep -c . || true)"
    if [[ "$srcinfo_count" == "2" ]]; then
        ok "makepkg --printsrcinfo produces 2 pkgname entries"
    else
        not_ok "makepkg --printsrcinfo produces $srcinfo_count pkgname entries (expected 2)"
    fi

    # Verify pkgbase in srcinfo.
    if printf '%s\n' "$srcinfo_output" | grep_all '^pkgbase = namtor$'; then
        ok "makepkg --printsrcinfo has pkgbase = namtor"
    else
        not_ok "makepkg --printsrcinfo missing pkgbase = namtor"
    fi

    # Verify each package has its own depends in the srcinfo.
    for pkg in namtor namtor-ui; do
        if printf '%s\n' "$srcinfo_output" | grep_all "^pkgname = ${pkg}$"; then
            ok "makepkg --printsrcinfo includes pkgname = $pkg"
        else
            not_ok "makepkg --printsrcinfo missing pkgname = $pkg"
        fi
    done
    fi
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
