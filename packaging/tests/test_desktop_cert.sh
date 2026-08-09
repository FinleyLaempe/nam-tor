#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Certification test for Task 30: GNOME and KDE Settings enrollment and
# PAM opt-in.
#
# Acceptance criteria (from the task spec):
#   (a) GNOME settings app binary exists and is staged (namtor-enroll-wizard).
#   (b) KDE KCM QML module files exist and are staged (libnamtorqml.so,
#       main.qml, metadata.json).
#   (c) Both UIs use the SYSTEM bus (not the session bus) — grep verified.
#   (d) GNOME presenter flow: start enrollment -> progress -> review ->
#       commit -> profile list refresh (covered by namtor-gnome unit tests).
#   (e) KDE bridge flow: start enrollment -> progress -> review -> commit ->
#       profile list refresh (covered by namtor-kde unit tests).
#   (f) PAM service toggle: list -> enable -> verify -> disable -> verify
#       (covered by namtor-gnome + namtor-kde unit tests).
#   (g) High-impact confirmation dialog for login/sudo/su/polkit services
#       (HIGH_IMPACT_WARNING constant in GNOME, highImpactWarning() in KDE).
#   (h) Privacy scan passes on all UI-facing data (no thresholds, scores,
#       embeddings, paths, gate names).
#   (i) Accessibility: GNOME Switch has tooltip_text; KDE Switch has
#       focusPolicy Qt.StrongFocus.
#   (j) No-TPM sessions show persistent software-key warning.
#   (k) Password fallback always available (PAM `sufficient` control).
#   (l) Failure matrix: daemon down, camera missing, polkit denied,
#       PAM rollback, stale signal.
#
# CI cannot run clean GNOME or Plasma sessions. The CI-testable surface is
# therefore:
#   - structural checks (binaries staged, system-bus usage, install-manifest
#     entries, accessibility anchors, warning constants, PAM sufficient
#     control);
#   - existing unit tests in namtor-gnome (29 presenter + 12 privacy) and
#     namtor-kde (18 + 3 abi-smoke).
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

GNOME_CRATE="$REPO_ROOT/crates/namtor-gnome"
KDE_CRATE="$REPO_ROOT/crates/namtor-kde"
MANIFEST="$PACKAGING_DIR/install-manifest.toml"
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
# (a) GNOME settings app binary staged
# ---------------------------------------------------------------------------
echo ""
echo "[a] GNOME settings app binary staged"
GNOME_BIN_REL=$(grep -A4 'dest = "/usr/bin/namtor-enroll-wizard"' "$MANIFEST" \
                | grep -E '^src' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
GNOME_BIN_PATH="$REPO_ROOT/$GNOME_BIN_REL"
if [[ -n "$GNOME_BIN_REL" ]] \
   && grep -q 'package = "gnome"' <(grep -A6 'dest = "/usr/bin/namtor-enroll-wizard"' "$MANIFEST"); then
    ok "install-manifest declares namtor-enroll-wizard under package=gnome"
else
    not_ok "install-manifest missing namtor-enroll-wizard gnome entry"
fi
check_file "namtor-enroll-wizard binary" "$GNOME_BIN_PATH"
# Also accept the debug build as a fallback (CI may build debug only).
if [[ ! -f "$GNOME_BIN_PATH" ]] && [[ -f "$REPO_ROOT/target/debug/namtor-enroll-wizard" ]]; then
    ok "namtor-enroll-wizard debug build present at target/debug/"
fi

# ---------------------------------------------------------------------------
# (b) KDE KCM QML module files staged
# ---------------------------------------------------------------------------
echo ""
echo "[b] KDE KCM QML module files staged"
check_file "KDE main.qml source" "$KDE_CRATE/qml/main.qml"
check_file "KDE metadata.json source" "$KDE_CRATE/metadata.json"
check_file "KDE CMakeLists.txt" "$KDE_CRATE/CMakeLists.txt"
check_file "KDE C++ bridge source" "$KDE_CRATE/src/cpp/namtor_kcm.cpp"
# CMake-built artifact: libnamtorqml.so under build-kde/
KDE_SO="$KDE_CRATE/build-kde/libnamtorqml.so"
check_file "libnamtorqml.so (CMake build artifact)" "$KDE_SO"
# Rust cdylib that the QML module links against.
check_file "libnamtor_kde.so (Rust cdylib, debug)" "$REPO_ROOT/target/debug/libnamtor_kde.so"
# CMake install rules must declare libnamtorqml + main.qml + metadata.json.
if grep -q 'install(TARGETS namtorqml' "$KDE_CRATE/CMakeLists.txt" \
   && grep -q 'install(FILES qml/main.qml' "$KDE_CRATE/CMakeLists.txt" \
   && grep -q 'install(FILES metadata.json' "$KDE_CRATE/CMakeLists.txt"; then
    ok "CMakeLists.txt install rules cover libnamtorqml + main.qml + metadata.json"
else
    not_ok "CMakeLists.txt missing one of: install TARGETS namtorqml / main.qml / metadata.json"
fi
# install-manifest must declare the static KDE desktop-integration files
# owned by package=kde (desktop, metainfo, icon).
KDE_DESKTOP_COUNT=$(grep -c 'package = "kde"' "$MANIFEST" 2>/dev/null || echo 0)
if [[ "$KDE_DESKTOP_COUNT" -ge 3 ]]; then
    ok "install-manifest has >=3 kde-owned desktop-integration entries (found $KDE_DESKTOP_COUNT)"
else
    not_ok "install-manifest has only $KDE_DESKTOP_COUNT kde entries (expected >=3)"
fi

# ---------------------------------------------------------------------------
# (c) Both UIs use the SYSTEM bus, not the session bus
# ---------------------------------------------------------------------------
echo ""
echo "[c] System-bus usage (sessionBus must be 0)"
SESSION_BUS_GNOME=$(count_matches 'Connection::session' \
    "$GNOME_CRATE/src/dbus_client.rs")
SESSION_BUS_KDE_BRIDGE=$(count_matches 'Connection::session' \
    "$KDE_CRATE/src/bridge.rs")
SESSION_BUS_KDE_CPP=$(count_matches 'sessionBus' \
    "$KDE_CRATE/src/cpp/namtor_kcm.cpp")
TOTAL_SESSION=$((SESSION_BUS_GNOME + SESSION_BUS_KDE_BRIDGE + SESSION_BUS_KDE_CPP))
if [[ "$TOTAL_SESSION" -eq 0 ]]; then
    ok "no Connection::session/sessionBus references in GNOME or KDE clients (total=$TOTAL_SESSION)"
else
    not_ok "session-bus references found: gnome=$SESSION_BUS_GNOME kde_bridge=$SESSION_BUS_KDE_BRIDGE kde_cpp=$SESSION_BUS_KDE_CPP"
fi

SYSTEM_BUS_GNOME=$(count_matches 'Connection::system' "$GNOME_CRATE/src/dbus_client.rs")
SYSTEM_BUS_KDE_BRIDGE=$(count_matches 'Connection::system' "$KDE_CRATE/src/bridge.rs")
SYSTEM_BUS_KDE_CPP=$(count_matches 'systemBus' "$KDE_CRATE/src/cpp/namtor_kcm.cpp")
if [[ "$SYSTEM_BUS_GNOME" -ge 1 ]] && [[ "$SYSTEM_BUS_KDE_BRIDGE" -ge 1 ]] && [[ "$SYSTEM_BUS_KDE_CPP" -ge 1 ]]; then
    ok "system-bus references present in GNOME ($SYSTEM_BUS_GNOME), KDE bridge ($SYSTEM_BUS_KDE_BRIDGE), KDE C++ ($SYSTEM_BUS_KDE_CPP)"
else
    not_ok "missing system-bus reference: gnome=$SYSTEM_BUS_GNOME kde_bridge=$SYSTEM_BUS_KDE_BRIDGE kde_cpp=$SYSTEM_BUS_KDE_CPP"
fi

# ---------------------------------------------------------------------------
# (d) GNOME presenter flow covered by unit tests
# ---------------------------------------------------------------------------
echo ""
echo "[d] GNOME presenter flow unit tests"
if cargo test -p namtor-gnome --test presenter_flow --no-fail-fast 2>&1 | grep -qE 'test result: ok\. ([0-9]+) passed; 0 failed'; then
    GNOME_PASSED=$(cargo test -p namtor-gnome --test presenter_flow --no-fail-fast 2>&1 \
                   | grep -E 'test result:' | head -1 \
                   | sed -E 's/.*ok\. ([0-9]+) passed.*/\1/')
    ok "namtor-gnome presenter_flow tests pass ($GNOME_PASSED passed)"
else
    not_ok "namtor-gnome presenter_flow tests failed"
    GNOME_PASSED=0
fi
# Flow coverage: each phase has a dedicated test.
FLOW_TESTS=(
    "happy_path_start_enroll_commit_lists_profile"
    "review_flow_stages_and_commits"
    "commit_profile_persists_to_store"
    "reconnect_restores_state"
)
for t in "${FLOW_TESTS[@]}"; do
    if cargo test -p namtor-gnome --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "GNOME flow test present: $t"
    else
        not_ok "GNOME flow test missing/failed: $t"
    fi
done

# ---------------------------------------------------------------------------
# (e) KDE bridge flow covered by unit tests
# ---------------------------------------------------------------------------
echo ""
echo "[e] KDE bridge flow unit tests"
# Capture full output first to avoid SIGPIPE when grep exits early (pipefail
# would otherwise turn a SIGPIPE on the cargo side into a pipeline failure).
KDE_OUT=$(cargo test -p namtor-kde --no-fail-fast 2>&1)
if echo "$KDE_OUT" | grep -qE 'test result: ok\. ([0-9]+) passed; 0 failed'; then
    KDE_PASSED=$(echo "$KDE_OUT" | grep -E 'test result:' | head -1 \
                 | sed -E 's/.*ok\. ([0-9]+) passed.*/\1/')
    ok "namtor-kde tests pass ($KDE_PASSED passed in lib.rs)"
else
    not_ok "namtor-kde tests failed"
    KDE_PASSED=0
fi
KDE_FLOW_TESTS=(
    "happy_path"
    "enrollment_signals_round_trip"
    "review_returns_coarse_quality"
    "profile_and_status_changed_signals_round_trip"
)
for t in "${KDE_FLOW_TESTS[@]}"; do
    if echo "$KDE_OUT" | grep -qE "^test tests::${t} \.\.\. ok"; then
        ok "KDE flow test present: $t"
    else
        not_ok "KDE flow test missing/failed: $t"
    fi
done
# ABI smoke (C ABI version + bus name + object path) must pass.
KDE_ABI_OUT=$(cargo test -p namtor-kde --test abi_smoke --no-fail-fast 2>&1)
if echo "$KDE_ABI_OUT" | grep -qE 'test result: ok\. 3 passed'; then
    ok "KDE abi_smoke tests pass (3 passed)"
else
    not_ok "KDE abi_smoke tests failed"
fi

# ---------------------------------------------------------------------------
# (f) PAM service toggle flow exists in both UIs
# ---------------------------------------------------------------------------
echo ""
echo "[f] PAM service toggle flow (list -> enable -> verify -> disable)"
# GNOME: SetPamServiceEnabled + ListPamServices in client trait + presenter.
if grep -q 'fn set_pam_service_enabled' "$GNOME_CRATE/src/client.rs" \
   && grep -q 'fn list_pam_services' "$GNOME_CRATE/src/client.rs"; then
    ok "GNOME client trait declares list_pam_services + set_pam_service_enabled"
else
    not_ok "GNOME client trait missing PAM v2 methods"
fi
if grep -q 'fn request_pam_toggle' "$GNOME_CRATE/src/presenter.rs" \
   && grep -q 'fn confirm_pam_toggle' "$GNOME_CRATE/src/presenter.rs"; then
    ok "GNOME presenter has request_pam_toggle + confirm_pam_toggle"
else
    not_ok "GNOME presenter missing PAM toggle methods"
fi
# KDE: Q_INVOKABLE listPamServices + setPamServiceEnabled in C++ bridge.
if grep -q 'void NamtorKcm::listPamServices' "$KDE_CRATE/src/cpp/namtor_kcm.cpp" \
   && grep -q 'void NamtorKcm::setPamServiceEnabled' "$KDE_CRATE/src/cpp/namtor_kcm.cpp"; then
    ok "KDE C++ bridge has Q_INVOKABLE listPamServices + setPamServiceEnabled"
else
    not_ok "KDE C++ bridge missing PAM v2 methods"
fi
# QML must call setPamServiceEnabled.
if grep -q 'setPamServiceEnabled' "$KDE_CRATE/qml/main.qml"; then
    ok "KDE QML calls setPamServiceEnabled"
else
    not_ok "KDE QML does not call setPamServiceEnabled"
fi
# Unit tests covering toggle + revert + unsupported refusal.
PAM_TESTS_GNOME=(
    "pam_service_toggle_works"
    "failed_pam_toggle_reverts_to_previous_state"
    "unsupported_pam_service_cannot_be_toggled"
    "set_pam_service_enabled_refuses_unsupported"
)
for t in "${PAM_TESTS_GNOME[@]}"; do
    if cargo test -p namtor-gnome --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "GNOME PAM test present: $t"
    else
        not_ok "GNOME PAM test missing/failed: $t"
    fi
done

# ---------------------------------------------------------------------------
# (g) High-impact confirmation dialog for risky services
# ---------------------------------------------------------------------------
echo ""
echo "[g] High-impact confirmation dialog"
# GNOME constant.
if grep -q 'pub const HIGH_IMPACT_WARNING' "$GNOME_CRATE/src/components/pam_services.rs" \
   && grep -q 'FAR/FRR and real-attack PAD gates remain unmeasured' \
        "$GNOME_CRATE/src/components/pam_services.rs"; then
    ok "GNOME HIGH_IMPACT_WARNING constant present and mentions FAR/FRR + PAD"
else
    not_ok "GNOME HIGH_IMPACT_WARNING constant missing or wrong text"
fi
# GNOME tests for high-impact dialog.
for t in "high_impact_pam_service_requires_confirmation" \
         "high_impact_warning_contains_required_phrases" \
         "disabling_risky_service_needs_no_confirmation"; do
    if cargo test -p namtor-gnome --test presenter_flow "$t" --no-fail-fast 2>&1 \
       | grep -qE "^test ${t} \.\.\. ok"; then
        ok "GNOME high-impact test present: $t"
    else
        not_ok "GNOME high-impact test missing/failed: $t"
    fi
done
# KDE: highImpactWarning() Q_INVOKABLE + QML confirm dialog.
if grep -q 'QString NamtorKcm::highImpactWarning' "$KDE_CRATE/src/cpp/namtor_kcm.cpp" \
   && grep -q 'FAR/FRR and real-attack PAD gates remain unmeasured' \
        "$KDE_CRATE/src/cpp/namtor_kcm.cpp"; then
    ok "KDE highImpactWarning() present and mentions FAR/FRR + PAD"
else
    not_ok "KDE highImpactWarning() missing or wrong text"
fi
if grep -q 'pamConfirmDialog' "$KDE_CRATE/qml/main.qml" \
   && grep -q 'highImpactWarning' "$KDE_CRATE/qml/main.qml"; then
    ok "KDE QML has pamConfirmDialog bound to highImpactWarning"
else
    not_ok "KDE QML missing pamConfirmDialog / highImpactWarning binding"
fi

# ---------------------------------------------------------------------------
# (h) Privacy scan passes
# ---------------------------------------------------------------------------
echo ""
echo "[h] Privacy scan (no thresholds/scores/embeddings/paths/gate names)"
if cargo test -p namtor-gnome --test privacy_scan --no-fail-fast 2>&1 \
   | grep -qE 'test result: ok\. 12 passed; 0 failed'; then
    ok "namtor-gnome privacy_scan tests pass (12 passed)"
else
    not_ok "namtor-gnome privacy_scan tests failed"
fi
# KDE has its own privacy scan test in the lib suite.
KDE_PRIV_OUT=$(cargo test -p namtor-kde privacy_scan_no_biometric_data --no-fail-fast 2>&1)
if echo "$KDE_PRIV_OUT" | grep -qE '^test tests::privacy_scan_no_biometric_data \.\.\. ok'; then
    ok "namtor-kde privacy_scan_no_biometric_data test passes"
else
    not_ok "namtor-kde privacy_scan_no_biometric_data test failed"
fi
# Forbidden substrings list is non-empty (the privacy scanner itself is gated).
if grep -q 'forbidden_substrings_list_is_nonempty' "$GNOME_CRATE/tests/privacy_scan.rs"; then
    ok "privacy_scan has forbidden_substrings_list_is_nonempty guard"
else
    not_ok "privacy_scan missing forbidden_substrings_list_is_nonempty guard"
fi
# Spot-check a few forbidden tokens are listed in the privacy module.
PRIV="$GNOME_CRATE/src/privacy.rs"
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
# GNOME: PAM service Switch has tooltip_text.
if grep -q 'tooltip_text' "$GNOME_CRATE/src/components/pam_services.rs" \
   && grep -q 'set_tooltip_text' "$GNOME_CRATE/src/components/pam_services.rs"; then
    ok "GNOME PAM Switch has tooltip_text + set_tooltip_text"
else
    not_ok "GNOME PAM Switch missing tooltip_text"
fi
# GNOME: accessible roles assigned in components.
GNOME_ACC_ROLES=$(grep -rE 'AccessibleRole::' "$GNOME_CRATE/src/" 2>/dev/null | wc -l)
if [[ "$GNOME_ACC_ROLES" -ge 10 ]]; then
    ok "GNOME assigns AccessibleRole in $GNOME_ACC_ROLES places (>=10)"
else
    not_ok "GNOME assigns AccessibleRole in only $GNOME_ACC_ROLES places (<10)"
fi
# KDE: PAM Switch has focusPolicy Qt.StrongFocus.
if grep -q 'focusPolicy: Qt.StrongFocus' "$KDE_CRATE/qml/main.qml"; then
    ok "KDE QML Switch has focusPolicy Qt.StrongFocus"
else
    not_ok "KDE QML Switch missing focusPolicy Qt.StrongFocus"
fi
# KDE: Accessible.name/description assigned across the QML.
KDE_ACC=$(grep -cE 'Accessible\.(name|role|description)' "$KDE_CRATE/qml/main.qml" 2>/dev/null || echo 0)
if [[ "$KDE_ACC" -ge 20 ]]; then
    ok "KDE QML has $KDE_ACC Accessible.* annotations (>=20)"
else
    not_ok "KDE QML has only $KDE_ACC Accessible.* annotations (<20)"
fi

# ---------------------------------------------------------------------------
# (j) No-TPM sessions show persistent software-key warning
# ---------------------------------------------------------------------------
echo ""
echo "[j] No-TPM software-fallback warning"
# GNOME: ProtectionView.is_software_fallback drives a warning banner.
if grep -q 'pub is_software_fallback: bool' "$GNOME_CRATE/src/view_model.rs" \
   && grep -q 'is_software_fallback' "$GNOME_CRATE/src/bin/wizard.rs" \
   && grep -q 'Software fallback in use' "$GNOME_CRATE/src/bin/wizard.rs"; then
    ok "GNOME ProtectionView.is_software_fallback surfaces 'Software fallback in use' banner"
else
    not_ok "GNOME software-fallback warning incomplete"
fi
# GNOME test for software fallback.
if cargo test -p namtor-gnome --test presenter_flow software_fallback_protection_surfaced --no-fail-fast 2>&1 \
   | grep -qE '^test software_fallback_protection_surfaced \.\.\. ok'; then
    ok "GNOME test software_fallback_protection_surfaced passes"
else
    not_ok "GNOME test software_fallback_protection_surfaced missing/failed"
fi
# KDE: protectionHasTpm drives "No TPM2 device — software fallback." label.
if grep -q 'protectionHasTpm' "$KDE_CRATE/qml/main.qml" \
   && grep -q 'No TPM2 device' "$KDE_CRATE/qml/main.qml"; then
    ok "KDE QML protectionHasTpm drives 'No TPM2 device — software fallback.' label"
else
    not_ok "KDE QML software-fallback label missing"
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
GNOME_DAEMON_DOWN=$(cargo test -p namtor-gnome --test presenter_flow daemon_unavailable_shows_error_and_recovery --no-fail-fast 2>&1)
KDE_DAEMON_DOWN=$(cargo test -p namtor-kde error_daemon_unavailable --no-fail-fast 2>&1)
if echo "$GNOME_DAEMON_DOWN" | grep -qE '^test daemon_unavailable_shows_error_and_recovery \.\.\. ok' \
   && echo "$KDE_DAEMON_DOWN" | grep -qE '^test tests::error_daemon_unavailable \.\.\. ok'; then
    ok "daemon-down failure covered in GNOME + KDE"
else
    not_ok "daemon-down failure not covered"
fi
# Polkit denied.
POLKIT_OUT=$(cargo test -p namtor-gnome --test presenter_flow polkit_denied_shows_auth_error --no-fail-fast 2>&1)
if echo "$POLKIT_OUT" | grep -qE '^test polkit_denied_shows_auth_error \.\.\. ok'; then
    ok "polkit-denied failure covered in GNOME"
else
    not_ok "polkit-denied failure not covered in GNOME"
fi
# Camera missing / low diversity.
LOWDIV_OUT=$(cargo test -p namtor-gnome --test presenter_flow low_diversity_shows_guidance --no-fail-fast 2>&1)
if echo "$LOWDIV_OUT" | grep -qE '^test low_diversity_shows_guidance \.\.\. ok'; then
    ok "camera-capture failure (low_diversity) covered in GNOME"
else
    not_ok "low_diversity guidance not covered"
fi
# PAM rollback (failed toggle reverts).
PAMROLL_OUT=$(cargo test -p namtor-gnome --test presenter_flow failed_pam_toggle_reverts_to_previous_state --no-fail-fast 2>&1)
if echo "$PAMROLL_OUT" | grep -qE '^test failed_pam_toggle_reverts_to_previous_state \.\.\. ok'; then
    ok "PAM rollback (failed toggle reverts) covered in GNOME"
else
    not_ok "PAM rollback not covered"
fi
# Stale signal handling (KDE generation-guarded replies).
KDE_STALE_OUT=$(cargo test -p namtor-kde --no-fail-fast 2>&1)
if echo "$KDE_STALE_OUT" | grep -qE '^test tests::stale_progress_signal_ignored \.\.\. ok' \
   && echo "$KDE_STALE_OUT" | grep -qE '^test tests::daemon_restart_old_bridge_errors_new_bridge_works \.\.\. ok'; then
    ok "stale-signal failure covered in KDE (stale_progress_signal_ignored + daemon_restart)"
else
    not_ok "stale-signal failure not covered in KDE"
fi
# GNOME reconnect restores state after daemon restart.
GNOME_RECONNECT=$(cargo test -p namtor-gnome --test presenter_flow reconnect_restores_state --no-fail-fast 2>&1)
if echo "$GNOME_RECONNECT" | grep -qE '^test reconnect_restores_state \.\.\. ok'; then
    ok "GNOME reconnect_restores_state covers daemon-restart recovery"
else
    not_ok "GNOME reconnect_restores_state missing/failed"
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
