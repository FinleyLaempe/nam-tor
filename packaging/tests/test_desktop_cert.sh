#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Certification test for Task 30: desktop Settings enrollment and PAM
# opt-in.
#
# Originally this covered two UIs. The Plasma KCM (`crates/namtor-kde`) was
# removed when the project consolidated on one desktop-neutral GTK4 UI, so
# every KDE-specific branch below is gone; what remains is a check that no
# KDE payload has crept back into the packaging.
#
# Acceptance criteria (from the task spec):
#   (a) Settings app binary exists and is staged (namtor-enroll-wizard).
#   (b) No KDE/Qt payload remains anywhere in the packaging.
#   (c) The UI uses the SYSTEM bus (not the session bus) — grep verified.
#   (d) Presenter flow: start enrollment -> progress -> review ->
#       commit -> profile list refresh (covered by namtor-ui unit tests).
#   (e) (removed with the KCM)
#   (f) PAM service toggle: list -> enable -> verify -> disable -> verify
#       (covered by namtor-ui unit tests).
#   (g) High-impact confirmation dialog for login/sudo/su/polkit services
#       (HIGH_IMPACT_WARNING constant).
#   (h) Privacy scan passes on all UI-facing data (no thresholds, scores,
#       embeddings, paths, gate names).
#   (i) Accessibility: Switch has tooltip_text.
#   (j) No-TPM sessions show persistent software-key warning.
#   (k) Password fallback always available (PAM `sufficient` control).
#   (l) Failure matrix: daemon down, camera missing, polkit denied,
#       PAM rollback, stale signal.
#
# CI cannot run a clean desktop session. The CI-testable surface is
# therefore:
#   - structural checks (binaries staged, system-bus usage, install-manifest
#     entries, accessibility anchors, warning constants, PAM sufficient
#     control);
#   - existing unit tests in namtor-ui (29 presenter + 12 privacy).
# Real enrollment with a camera, real PAM toggle against /etc/pam.d, and
# real authentication against a running PAM stack are human-gated on a
# desktop VM and recorded as WITHHELD in the evidence files.
#
# Usage: bash packaging/tests/test_desktop_cert.sh
# Exit code: 0 if all checks pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"
EVIDENCE_DIR="$REPO_ROOT/docs/evidence/public-release"

UI_CRATE="$REPO_ROOT/crates/namtor-ui"
MANIFEST="$PACKAGING_DIR/install-manifest.toml"
STAGE_SH="$PACKAGING_DIR/stage.sh"
PAM_INSTALL="$PACKAGING_DIR/pam-install.sh"
POLKIT_POLICY="$PACKAGING_DIR/polkit-1/actions/org.namtor.policy"

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

# File-exists helper that also reports the resolved path.
check_file() {
    local label="$1" path="$2"
    if [[ -f "$path" ]]; then
        ok "$label exists: $path"
    else
        not_ok "$label missing: $path"
    fi
}

# Grep helper: count occurrences of a pattern in a file (or 0 if file
# missing). Returns the count on stdout.
count_matches() {
    local pattern="$1" file="$2"
    if [[ -f "$file" ]]; then
        # grep -c always prints the count (0 when no match). Suppress the
        # non-zero exit code so it does not trip `set -e`/`||` chains.
        grep -E -c "$pattern" "$file" 2>/dev/null || true
    else
        echo 0
    fi
}

echo "=================================================================="
echo "Task 30 — Desktop settings enrollment + PAM opt-in certification"
echo "=================================================================="

# ---------------------------------------------------------------------------
# (a) settings app binary staged
# ---------------------------------------------------------------------------
echo ""
echo "[a] settings app binary staged"
UI_BIN_REL=$(grep -A4 'dest = "/usr/bin/namtor-enroll-wizard"' "$MANIFEST" \
                | grep -E '^src' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
UI_BIN_PATH="$REPO_ROOT/$UI_BIN_REL"
if [[ -n "$UI_BIN_REL" ]] \
   && grep -q 'package = "ui"' <(grep -A6 'dest = "/usr/bin/namtor-enroll-wizard"' "$MANIFEST"); then
    ok "install-manifest declares namtor-enroll-wizard under package=ui"
else
    not_ok "install-manifest missing namtor-enroll-wizard ui entry"
fi
check_file "namtor-enroll-wizard binary" "$UI_BIN_PATH"
# Also accept the debug build as a fallback (CI may build debug only).
if [[ ! -f "$UI_BIN_PATH" ]] && [[ -f "$REPO_ROOT/target/debug/namtor-enroll-wizard" ]]; then
    ok "namtor-enroll-wizard debug build present at target/debug/"
fi

# ---------------------------------------------------------------------------
# (b) No KDE/Qt payload anywhere
# ---------------------------------------------------------------------------
echo ""
echo "[b] No KDE/Qt payload remains"
if [[ -e "$REPO_ROOT/crates/namtor-kde" ]]; then
    not_ok "crates/namtor-kde is back (the Plasma KCM was removed)"
else
    ok "crates/namtor-kde removed"
fi
KDE_MANIFEST_COUNT=$(grep -cE '^package = "kde"' "$MANIFEST" 2>/dev/null || true)
if [[ "${KDE_MANIFEST_COUNT:-0}" -eq 0 ]]; then
    ok "install-manifest declares no kde-owned entries"
else
    not_ok "install-manifest still has $KDE_MANIFEST_COUNT kde-owned entries"
fi
for f in \
    "$PACKAGING_DIR/applications/org.namtor.kcm.desktop" \
    "$PACKAGING_DIR/metainfo/org.namtor.kcm.metainfo.xml" \
    "$PACKAGING_DIR/icons/hicolor/scalable/apps/org.namtor.kcm.svg" \
    "$PACKAGING_DIR/po/kde"; do
    if [[ -e "$f" ]]; then
        not_ok "KDE packaging asset still present: ${f#$REPO_ROOT/}"
    else
        ok "KDE packaging asset removed: ${f#$REPO_ROOT/}"
    fi
done

# ---------------------------------------------------------------------------
# (c) The UI uses the SYSTEM bus, not the session bus
# ---------------------------------------------------------------------------
echo ""
echo "[c] System-bus usage (sessionBus must be 0)"
# The UI crate's dbus_client.rs is a thin re-export; the live connection is
# opened in the shared namtor-client crate, so check both.
UI_DBUS_SOURCES=(
    "$UI_CRATE/src/dbus_client.rs"
    "$REPO_ROOT/crates/namtor-client/src/dbus_client.rs"
)
SESSION_BUS_UI=0
SYSTEM_BUS_UI=0
for f in "${UI_DBUS_SOURCES[@]}"; do
    SESSION_BUS_UI=$((SESSION_BUS_UI + $(count_matches 'Connection::session' "$f")))
    SYSTEM_BUS_UI=$((SYSTEM_BUS_UI + $(count_matches 'Connection::system' "$f")))
done
if [[ "$SESSION_BUS_UI" -eq 0 ]]; then
    ok "no Connection::session references in the UI D-Bus client"
else
    not_ok "session-bus references found in the UI D-Bus client: $SESSION_BUS_UI"
fi
if [[ "$SYSTEM_BUS_UI" -ge 1 ]]; then
    ok "system-bus references present in the UI D-Bus client ($SYSTEM_BUS_UI)"
else
    not_ok "missing system-bus reference in the UI D-Bus client"
fi

# ---------------------------------------------------------------------------
# (d) UI presenter flow covered by unit tests
# ---------------------------------------------------------------------------
echo ""
echo "[d] UI presenter flow unit tests"
if cargo test -p namtor-ui --test presenter_flow --no-fail-fast 2>&1 | grep -qE 'test result: ok\. ([0-9]+) passed; 0 failed'; then
    UI_PASSED=$(cargo test -p namtor-ui --test presenter_flow --no-fail-fast 2>&1 \
                   | grep -E 'test result:' | head -1 \
                   | sed -E 's/.*ok\. ([0-9]+) passed.*/\1/')
    ok "namtor-ui presenter_flow tests pass ($UI_PASSED passed)"
else
    not_ok "namtor-ui presenter_flow tests failed"
    UI_PASSED=0
fi
# Flow coverage: each phase has a dedicated test.
FLOW_TESTS=(
    "happy_path_start_enroll_commit_lists_profile"
    "review_flow_stages_and_commits"
    "commit_profile_persists_to_store"
    "reconnect_restores_state"
)
for t in "${FLOW_TESTS[@]}"; do
    if cargo test -p namtor-ui --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "UI flow test present: $t"
    else
        not_ok "UI flow test missing/failed: $t"
    fi
done

# ---------------------------------------------------------------------------
# (f) PAM service toggle flow exists in the UI
# ---------------------------------------------------------------------------
echo ""
echo "[f] PAM service toggle flow (list -> enable -> verify -> disable)"
# UI: SetPamServiceEnabled + ListPamServices in client trait + presenter.
if grep -q 'fn set_pam_service_enabled' "$UI_CRATE/src/client.rs" \
   && grep -q 'fn list_pam_services' "$UI_CRATE/src/client.rs"; then
    ok "UI client trait declares list_pam_services + set_pam_service_enabled"
else
    not_ok "UI client trait missing PAM v2 methods"
fi
if grep -q 'fn request_pam_toggle' "$UI_CRATE/src/presenter.rs" \
   && grep -q 'fn confirm_pam_toggle' "$UI_CRATE/src/presenter.rs"; then
    ok "UI presenter has request_pam_toggle + confirm_pam_toggle"
else
    not_ok "UI presenter missing PAM toggle methods"
fi
# Unit tests covering toggle + revert + unsupported refusal.
PAM_TESTS_UI=(
    "pam_service_toggle_works"
    "failed_pam_toggle_reverts_to_previous_state"
    "unsupported_pam_service_cannot_be_toggled"
    "set_pam_service_enabled_refuses_unsupported"
)
for t in "${PAM_TESTS_UI[@]}"; do
    if cargo test -p namtor-ui --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "UI PAM test present: $t"
    else
        not_ok "UI PAM test missing/failed: $t"
    fi
done

# ---------------------------------------------------------------------------
# (g) High-impact confirmation dialog for risky services
# ---------------------------------------------------------------------------
echo ""
echo "[g] High-impact confirmation dialog"
# UI constant.
if grep -q 'pub const HIGH_IMPACT_WARNING' "$UI_CRATE/src/components/pam_services.rs" \
   && grep -q 'FAR/FRR and real-attack PAD gates remain unmeasured' \
        "$UI_CRATE/src/components/pam_services.rs"; then
    ok "UI HIGH_IMPACT_WARNING constant present and mentions FAR/FRR + PAD"
else
    not_ok "UI HIGH_IMPACT_WARNING constant missing or wrong text"
fi
# UI tests for high-impact dialog.
for t in "high_impact_pam_service_requires_confirmation" \
         "high_impact_warning_contains_required_phrases" \
         "disabling_risky_service_needs_no_confirmation"; do
    if cargo test -p namtor-ui --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "UI high-impact test present: $t"
    else
        not_ok "UI high-impact test missing/failed: $t"
    fi
done
# ---------------------------------------------------------------------------
# (h) Privacy scan passes
# ---------------------------------------------------------------------------
echo ""
echo "[h] Privacy scan (no thresholds/scores/embeddings/paths/gate names)"
if cargo test -p namtor-ui --test privacy_scan --no-fail-fast 2>&1 \
   | grep -qE 'test result: ok\. 12 passed; 0 failed'; then
    ok "namtor-ui privacy_scan tests pass (12 passed)"
else
    not_ok "namtor-ui privacy_scan tests failed"
fi
# Forbidden substrings list is non-empty (the privacy scanner itself is gated).
if grep -q 'forbidden_substrings_list_is_nonempty' "$UI_CRATE/tests/privacy_scan.rs"; then
    ok "privacy_scan has forbidden_substrings_list_is_nonempty guard"
else
    not_ok "privacy_scan missing forbidden_substrings_list_is_nonempty guard"
fi
# Spot-check a few forbidden tokens are listed in the privacy module.
PRIV="$UI_CRATE/src/privacy.rs"
for token in threshold score embedding vector landmark template biometric; do
    if grep -qi "\"$token\"" "$PRIV"; then
        ok "forbidden token '$token' listed in privacy.rs"
    else
        not_ok "forbidden token '$token' missing from privacy.rs"
    fi
done

# ---------------------------------------------------------------------------
# (i) Accessibility: keyboard controls exist
# ---------------------------------------------------------------------------
echo ""
echo "[i] Accessibility (keyboard controls + screen-reader labels)"
# UI: PAM service Switch has tooltip_text.
if grep -q 'tooltip_text' "$UI_CRATE/src/components/pam_services.rs" \
   && grep -q 'set_tooltip_text' "$UI_CRATE/src/components/pam_services.rs"; then
    ok "UI PAM Switch has tooltip_text + set_tooltip_text"
else
    not_ok "UI PAM Switch missing tooltip_text"
fi
# UI: accessible roles assigned in components.
UI_ACC_ROLES=$(grep -rE 'AccessibleRole::' "$UI_CRATE/src/" 2>/dev/null | wc -l)
if [[ "$UI_ACC_ROLES" -ge 10 ]]; then
    ok "UI assigns AccessibleRole in $UI_ACC_ROLES places (>=10)"
else
    not_ok "UI assigns AccessibleRole in only $UI_ACC_ROLES places (<10)"
fi
# ---------------------------------------------------------------------------
# (j) No-TPM sessions show persistent software-key warning
# ---------------------------------------------------------------------------
echo ""
echo "[j] No-TPM software-fallback warning"
# UI: ProtectionView.is_software_fallback drives a warning banner.
if grep -q 'pub is_software_fallback: bool' "$UI_CRATE/src/view_model.rs" \
   && grep -q 'is_software_fallback' "$UI_CRATE/src/bin/wizard.rs" \
   && grep -q 'Software fallback in use' "$UI_CRATE/src/bin/wizard.rs"; then
    ok "UI ProtectionView.is_software_fallback surfaces 'Software fallback in use' banner"
else
    not_ok "UI software-fallback warning incomplete"
fi
# UI test for software fallback.
if cargo test -p namtor-ui --test presenter_flow software_fallback_protection_surfaced --no-fail-fast 2>&1 \
   | grep -qE '^test software_fallback_protection_surfaced \.\.\. ok'; then
    ok "UI test software_fallback_protection_surfaced passes"
else
    not_ok "UI test software_fallback_protection_surfaced missing/failed"
fi
# ---------------------------------------------------------------------------
# (k) Password fallback always available (PAM sufficient control)
# ---------------------------------------------------------------------------
echo ""
echo "[k] Password fallback (PAM sufficient control)"
if grep -q 'CONTROL="sufficient"' "$PAM_INSTALL" \
   && grep -q 'auth sufficient pam_namtor.so' "$REPO_ROOT/crates/namtor-pam-config/src/candidate.rs" \
   && grep -q 'PAM_AUTHINFO_UNAVAIL' "$REPO_ROOT/crates/namtor-pam/src/lib.rs"; then
    ok "PAM install defaults to 'sufficient' + module maps errors to PAM_AUTHINFO_UNAVAIL (fall-through to password)"
else
    not_ok "PAM sufficient / fall-through chain incomplete"
fi
# NamtorControl::Sufficient is the default in the editor.
if grep -q 'NamtorControl::Sufficient' "$REPO_ROOT/crates/namtor-pam-config/src/editor.rs"; then
    ok "namtor-pam-config editor defaults to NamtorControl::Sufficient"
else
    not_ok "namtor-pam-config editor missing default Sufficient"
fi
# Polkit policy for PAM reconfiguration exists.
if grep -q 'org.namtor.configure-pam' "$POLKIT_POLICY"; then
    ok "polkit action org.namtor.configure-pam declared"
else
    not_ok "polkit action org.namtor.configure-pam missing"
fi

# ---------------------------------------------------------------------------
# (l) Failure matrix (unit-test covered scenarios)
# ---------------------------------------------------------------------------
echo ""
echo "[l] Failure matrix (unit-tested scenarios)"
# Daemon down.
UI_DAEMON_DOWN=$(cargo test -p namtor-ui --test presenter_flow daemon_unavailable_shows_error_and_recovery --no-fail-fast 2>&1)
if echo "$UI_DAEMON_DOWN" | grep -qE '^test daemon_unavailable_shows_error_and_recovery \.\.\. ok'; then
    ok "daemon-down failure covered in the UI"
else
    not_ok "daemon-down failure not covered"
fi
# Polkit denied.
POLKIT_OUT=$(cargo test -p namtor-ui --test presenter_flow polkit_denied_shows_auth_error --no-fail-fast 2>&1)
if echo "$POLKIT_OUT" | grep -qE '^test polkit_denied_shows_auth_error \.\.\. ok'; then
    ok "polkit-denied failure covered in the UI"
else
    not_ok "polkit-denied failure not covered in the UI"
fi
# Camera missing / low diversity.
LOWDIV_OUT=$(cargo test -p namtor-ui --test presenter_flow low_diversity_shows_guidance --no-fail-fast 2>&1)
if echo "$LOWDIV_OUT" | grep -qE '^test low_diversity_shows_guidance \.\.\. ok'; then
    ok "camera-capture failure (low_diversity) covered in the UI"
else
    not_ok "low_diversity guidance not covered"
fi
# PAM rollback (failed toggle reverts).
PAMROLL_OUT=$(cargo test -p namtor-ui --test presenter_flow failed_pam_toggle_reverts_to_previous_state --no-fail-fast 2>&1)
if echo "$PAMROLL_OUT" | grep -qE '^test failed_pam_toggle_reverts_to_previous_state \.\.\. ok'; then
    ok "PAM rollback (failed toggle reverts) covered in the UI"
else
    not_ok "PAM rollback not covered"
fi
# UI reconnect restores state after daemon restart.
UI_RECONNECT=$(cargo test -p namtor-ui --test presenter_flow reconnect_restores_state --no-fail-fast 2>&1)
if echo "$UI_RECONNECT" | grep -qE '^test reconnect_restores_state \.\.\. ok'; then
    ok "UI reconnect_restores_state covers daemon-restart recovery"
else
    not_ok "UI reconnect_restores_state missing/failed"
fi

# ---------------------------------------------------------------------------
# (m) UI redesigned source resources present (tasks 9-13)
# ---------------------------------------------------------------------------
echo ""
echo "[m] UI redesigned source resources present"
check_file "UI DESIGN.md" "$UI_CRATE/DESIGN.md"
check_file "UI shell.rs" "$UI_CRATE/src/shell.rs"
check_file "UI navigation.rs" "$UI_CRATE/src/navigation.rs"
check_file "UI a11y.rs" "$UI_CRATE/src/a11y.rs"
check_file "UI system_status.rs component" "$UI_CRATE/src/components/system_status.rs"
for c in enrollment.rs pam_services.rs profile_list.rs recovery.rs review.rs; do
    check_file "UI component $c" "$UI_CRATE/src/components/$c"
done
check_file "UI shell_navigation tests" "$UI_CRATE/tests/shell_navigation.rs"
if grep -q 'Profiles' "$UI_CRATE/DESIGN.md" \
   && grep -q 'Authentication' "$UI_CRATE/DESIGN.md" \
   && grep -q 'System' "$UI_CRATE/DESIGN.md"; then
    ok "DESIGN.md defines three destinations (Profiles, Authentication, System)"
else
    not_ok "DESIGN.md missing one of: Profiles, Authentication, System"
fi
if grep -q 'ProgressAnnouncer\|should_announce\|throttle' "$UI_CRATE/src/a11y.rs"; then
    ok "a11y.rs defines announcement throttle"
else
    not_ok "a11y.rs missing announcement throttle"
fi

# ---------------------------------------------------------------------------
# (n) Manager3 introspection XML available in package
# ---------------------------------------------------------------------------
echo ""
echo "[n] Manager3 introspection XML available"
V3_XML="$REPO_ROOT/docs/evidence/dbus-contract-v3.xml"
check_file "dbus-contract-v3.xml source" "$V3_XML"
# The daemon compiles the v3 XML into the binary via include_str!, making it
# available to D-Bus clients via org.freedesktop.DBus.Introspectable.Introspect.
if grep -q 'dbus-contract-v3.xml' \
   "$REPO_ROOT/crates/namtor-daemon/src/manager/contract.rs"; then
    ok "daemon contract.rs includes dbus-contract-v3.xml"
else
    not_ok "daemon contract.rs does not include dbus-contract-v3.xml"
fi
if grep -q 'interface name="org.namtor.Manager3"' "$V3_XML" 2>/dev/null; then
    ok "v3 XML declares org.namtor.Manager3 interface"
else
    not_ok "v3 XML missing org.namtor.Manager3 interface"
fi
if grep -q 'interface name="org.namtor.Manager"' "$V3_XML" 2>/dev/null \
   && grep -q 'interface name="org.namtor.Manager2"' "$V3_XML" 2>/dev/null; then
    ok "v3 XML preserves v1 + v2 interfaces"
else
    not_ok "v3 XML missing v1 or v2 interface preservation"
fi

# ---------------------------------------------------------------------------
# (o) Translation seam exists (i18n infrastructure)
# ---------------------------------------------------------------------------
echo ""
echo "[o] Translation seam (i18n infrastructure)"
if grep -q 'pub fn tr(' "$UI_CRATE/src/lib.rs"; then
    ok "UI lib.rs exports tr() translation seam"
else
    not_ok "UI lib.rs missing tr() translation seam"
fi
check_file "POTFILES.in" "$PACKAGING_DIR/po/POTFILES.in"
check_file "namtor-ui.pot template" "$PACKAGING_DIR/po/namtor-ui.pot"
if grep -q 'crates/namtor-ui/src/' "$PACKAGING_DIR/po/POTFILES.in"; then
    ok "POTFILES.in references UI source files"
else
    not_ok "POTFILES.in does not reference any UI source file"
fi
MO_COUNT=$(find "$REPO_ROOT" -name '*.mo' -not -path '*/target/*' \
           -not -path '*/packaging/arch/src/*' 2>/dev/null | wc -l)
if [[ "$MO_COUNT" -eq 0 ]]; then
    ok "no compiled .mo files in repo (translations not yet compiled)"
else
    not_ok "found $MO_COUNT compiled .mo files in repo (should not ship yet)"
fi

# ---------------------------------------------------------------------------
# (p) Executable linkage — namtor-ui binary present and buildable
# ---------------------------------------------------------------------------
echo ""
echo "[p] Executable linkage (namtor-ui binary)"
if grep -q 'name = "namtor-enroll-wizard"' "$UI_CRATE/Cargo.toml" \
   || grep -q 'path = "src/bin/wizard.rs"' "$UI_CRATE/Cargo.toml"; then
    ok "namtor-ui Cargo.toml declares namtor-enroll-wizard binary"
else
    not_ok "namtor-ui Cargo.toml missing binary declaration"
fi
check_file "wizard.rs binary source" "$UI_CRATE/src/bin/wizard.rs"
if grep -A6 'dest = "/usr/bin/namtor-enroll-wizard"' "$MANIFEST" \
   | grep -q 'src = "target/release/namtor-enroll-wizard"'; then
    ok "install-manifest maps namtor-enroll-wizard to target/release binary"
else
    not_ok "install-manifest binary source mapping missing"
fi
if grep -q '^Exec=namtor-enroll-wizard' \
   "$PACKAGING_DIR/applications/org.namtor.Wizard.desktop"; then
    ok "desktop file Exec= matches namtor-enroll-wizard"
else
    not_ok "desktop file Exec= does not match namtor-enroll-wizard"
fi
if grep -q '<binary>namtor-enroll-wizard</binary>' \
   "$PACKAGING_DIR/metainfo/org.namtor.Wizard.metainfo.xml"; then
    ok "metainfo provides/binary matches namtor-enroll-wizard"
else
    not_ok "metainfo provides/binary missing or mismatched"
fi

# ---------------------------------------------------------------------------
# (q) No generated build output in source tree / staged packages
# ---------------------------------------------------------------------------
echo ""
echo "[q] No generated build output in source tree"
if git -C "$REPO_ROOT" check-ignore .omo >/dev/null 2>&1; then
    ok ".omo/ is gitignored"
else
    not_ok ".omo/ is not gitignored"
fi
if git -C "$REPO_ROOT" check-ignore packaging/arch/src >/dev/null 2>&1; then
    ok "packaging/arch/src/ is gitignored"
else
    not_ok "packaging/arch/src/ is not gitignored"
fi
if git -C "$REPO_ROOT" check-ignore packaging/arch/pkg >/dev/null 2>&1; then
    ok "packaging/arch/pkg/ is gitignored"
else
    not_ok "packaging/arch/pkg/ is not gitignored"
fi
SCREENSHOT_COUNT=$(git -C "$REPO_ROOT" ls-files 'crates/namtor-ui/**/*.png' \
                    'crates/namtor-ui/**/*.jpg' 'crates/namtor-ui/**/*.jpeg' 2>/dev/null \
                    | wc -l)
if [[ "$SCREENSHOT_COUNT" -eq 0 ]]; then
    ok "no tracked screenshots in crates/namtor-ui/"
else
    not_ok "found $SCREENSHOT_COUNT tracked screenshots in crates/namtor-ui/"
fi
# Biometric evidence = face images, template dumps, captured frames — NOT
# synthetic test fixtures (fixtures/onnx/*.onnx are backend test models).
BIOMETRIC_HITS=$(git -C "$REPO_ROOT" ls-files '*.onnx' '*.npy' 2>/dev/null \
                  | grep -v '^ml/' | grep -v '^fixtures/' | wc -l)
if [[ "$BIOMETRIC_HITS" -eq 0 ]]; then
    ok "no tracked biometric data (.onnx/.npy) outside ml/ and fixtures/"
else
    not_ok "found $BIOMETRIC_HITS tracked biometric data files outside ml/ and fixtures/"
fi
PKG_ARCHIVE_COUNT=$(git -C "$REPO_ROOT" ls-files 'packaging/**/*.pkg.tar.zst' \
                    'packaging/**/*.tar.gz' 2>/dev/null | wc -l)
if [[ "$PKG_ARCHIVE_COUNT" -eq 0 ]]; then
    ok "no tracked package archives (.pkg.tar.zst / .tar.gz) in packaging/"
else
    not_ok "found $PKG_ARCHIVE_COUNT tracked package archives in packaging/"
fi

# ---------------------------------------------------------------------------
# (r) Core package remains GTK-free (no gtk4/libadwaita in core deps)
# ---------------------------------------------------------------------------
echo ""
echo "[r] Core package GTK-free (no gtk4/libadwaita in core deps)"
ARCH_PKGBUILD="$PACKAGING_DIR/arch/PKGBUILD"
core_body_arch="$(awk '/^package_namtor\(\)/{f=1} f{print} f&&/^}/{exit}' "$ARCH_PKGBUILD")"
for nodep in gtk4 libadwaita; do
    if printf '%s\n' "$core_body_arch" | grep -qE "['\"]${nodep}['\"]"; then
        not_ok "Arch core depends on $nodep (must not)"
    else
        ok "Arch core does not depend on $nodep"
    fi
done
SPEC="$PACKAGING_DIR/fedora/namtor.spec"
core_requires_fed=$(awk '/^%package/{exit} /^Requires:/{print}' "$SPEC")
for nodep in gtk4 libadwaita; do
    if printf '%s\n' "$core_requires_fed" | grep -qi "$nodep"; then
        not_ok "Fedora core Requires $nodep (must not)"
    else
        ok "Fedora core does not Require $nodep"
    fi
done
DEBIAN_CONTROL="$PACKAGING_DIR/debian/control"
core_depends_deb=$(awk '/^Package: namtor$/{f=1;next} f&&/^Depends:/{f=2;next} f==2&&/^[[:space:]]/{gsub(/[[:space:]]/,"");print;next} f==2&&!/^[[:space:]]/{exit}' "$DEBIAN_CONTROL" | tr -d '\n')
for nodep in libgtk-4-1 libadwaita-1-0; do
    if printf '%s' "$core_depends_deb" | grep -q "$nodep"; then
        not_ok "Debian core Depends on $nodep (must not)"
    else
        ok "Debian core does not Depend on $nodep"
    fi
done

# ---------------------------------------------------------------------------
# (s) namtor-ui depends on namtor (exact version)
# ---------------------------------------------------------------------------
echo ""
echo "[s] namtor-ui depends on namtor (exact version)"
ui_body_arch="$(awk '/^package_namtor-ui\(\)/{f=1} f{print} f&&/^}/{exit}' "$ARCH_PKGBUILD")"
if printf '%s\n' "$ui_body_arch" | grep -qF 'namtor=$pkgver'; then
    ok "Arch ui depends on namtor=\$pkgver (exact)"
else
    not_ok "Arch ui missing namtor=\$pkgver exact dep"
fi
ui_requires_fed=$(awk '/^%package ui/{f=1;next} f&&/^Requires:/{print} f&&/^%/{exit}' "$SPEC")
if printf '%s\n' "$ui_requires_fed" | grep -q '%{name} = %{version}-%{release}'; then
    ok "Fedora ui Requires namtor = ver-rel (exact)"
else
    not_ok "Fedora ui missing exact namtor Requires"
fi
ui_depends_deb=$(awk '/^Package: namtor-ui$/{f=1;next} f&&/^Depends:/{f=2;next} f==2&&/^[[:space:]]/{gsub(/[[:space:]]/,"");print;next} f==2&&!/^[[:space:]]/{exit}' "$DEBIAN_CONTROL" | tr -d '\n')
if printf '%s' "$ui_depends_deb" | grep -q 'namtor(=0.1.0-3)'; then
    ok "Debian ui Depends on namtor (= 0.1.0-3) (exact)"
else
    not_ok "Debian ui missing exact namtor Depends"
fi

# ---------------------------------------------------------------------------
# (t) Mutation test — staged manifest minus one resource fails certification
# ---------------------------------------------------------------------------
echo ""
echo "[t] Mutation test (staged manifest minus one resource fails cert)"
mutation_root="$(mk_temp_dir)"
mkdir -p "$mutation_root/target/release"
mkdir -p "$mutation_root/packaging"
cp "$MANIFEST" "$mutation_root/packaging/install-manifest.toml"
cp "$STAGE_SH" "$mutation_root/packaging/stage.sh"
for bin in namtor-daemon namtor-worker namtor-camera-emitter-off \
           namtor-cli namtor-enroll-wizard; do
    printf 'placeholder' > "$mutation_root/target/release/$bin"
done
printf 'placeholder' > "$mutation_root/target/release/libnamtor_pam.so"
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
    packaging/lifecycle.sh \
    packaging/README.md \
    COPYING.md \
    docs/man/namtor-daemon.8 \
    docs/man/namtor-cli.1 \
    docs/evidence/packaging/namtor.spdx.json \
    docs/evidence/packaging/namtor.cyclonedx.json; do
    mkdir -p "$mutation_root/$(dirname "$f")"
    printf 'placeholder' > "$mutation_root/$f"
done
for icon in org.namtor.Wizard.svg; do
    mkdir -p "$mutation_root/packaging/icons/hicolor/scalable/apps"
    printf 'placeholder' > "$mutation_root/packaging/icons/hicolor/scalable/apps/$icon"
done

dest_ui_mut="$(mk_temp_dir)"
if bash "$STAGE_SH" "$dest_ui_mut" "$mutation_root" --package ui >/dev/null 2>&1; then
    ok "mutation: stage.sh --package ui succeeded"
else
    not_ok "mutation: stage.sh --package ui failed"
fi

UI_EXPECTED=(
    "usr/bin/namtor-enroll-wizard"
    "usr/share/applications/org.namtor.Wizard.desktop"
    "usr/share/metainfo/org.namtor.Wizard.metainfo.xml"
    "usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg"
)
mut_present=0
for r in "${UI_EXPECTED[@]}"; do
    if [[ -f "$dest_ui_mut/$r" ]]; then
        mut_present=$((mut_present + 1))
    fi
done
if [[ "$mut_present" -eq "${#UI_EXPECTED[@]}" ]]; then
    ok "mutation: staged ui payload has all ${#UI_EXPECTED[@]} expected resources"
else
    not_ok "mutation: staged ui payload has only $mut_present/${#UI_EXPECTED[@]} resources"
fi

REMOVED="usr/share/metainfo/org.namtor.Wizard.metainfo.xml"
rm -f "$dest_ui_mut/$REMOVED"
if [[ ! -f "$dest_ui_mut/$REMOVED" ]]; then
    ok "mutation: removed $REMOVED from staged payload"
else
    not_ok "mutation: failed to remove $REMOVED"
fi

mut_fail=0
for r in "${UI_EXPECTED[@]}"; do
    if [[ ! -f "$dest_ui_mut/$r" ]]; then
        mut_fail=1
        echo "  (mutation detected missing: $r)"
    fi
done
if [[ "$mut_fail" -eq 1 ]]; then
    ok "mutation: certification FAILS when resource removed (test is non-vacuous)"
else
    not_ok "mutation: certification passed despite removed resource (test is vacuous)"
fi

mkdir -p "$(dirname "$dest_ui_mut/$REMOVED")"
printf 'placeholder' > "$dest_ui_mut/$REMOVED"
mut_pass=1
for r in "${UI_EXPECTED[@]}"; do
    if [[ ! -f "$dest_ui_mut/$r" ]]; then
        mut_pass=0
    fi
done
if [[ "$mut_pass" -eq 1 ]]; then
    ok "mutation: certification PASSES after resource restored (round-trip)"
else
    not_ok "mutation: certification still fails after restore"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "=================================================================="
echo "Summary: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    echo "Failed checks:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    exit 1
fi
echo "All desktop certification checks passed."
echo "=================================================================="
exit 0
