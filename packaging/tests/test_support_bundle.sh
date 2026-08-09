#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for the privacy-safe support bundle (Task 27).
#
# Runs WITHOUT root. Exercises:
#   (a) diagnostics run unprivileged (cargo test covers this; here we verify
#       the binary builds and the test suite passes)
#   (b) privacy scan passes — no forbidden tokens in diagnostic output
#   (c) hex redaction works — model hashes are truncated to first 8 chars
#   (d) migration detection works — old install artifacts are detected
#   (e) forbidden-data mutation fails — injecting forbidden tokens is caught
#
# Usage: bash packaging/tests/test_support_bundle.sh
#
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"

PASS=0
FAIL=0
FAILED_TESTS=()

ok() {
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}

not_ok() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
    FAILED_TESTS+=("$1")
}

# Forbidden tokens that must NOT appear in diagnostic output.
FORBIDDEN_TOKENS=(
    "embedding"
    "template"
    "threshold"
    "score"
    "biometric"
    "gate"
    "raw"
    "nonce"
    "pixel"
    "frame"
    "crop"
)

# --- (a) cargo test — diagnostics run unprivileged -------------------------

echo "== (a) cargo test — diagnostics run unprivileged =="

if cargo test -p namtor-cli --lib 2>&1 | grep -q "test result: ok"; then
    ok "cargo test -p namtor-cli --lib passes"
else
    not_ok "cargo test -p namtor-cli --lib failed"
fi

# Count passing diagnostics tests
DIAG_TEST_COUNT=$(cargo test -p namtor-cli --lib 2>&1 | grep -c "^test diagnostics::" || true)
PKG_TEST_COUNT=$(cargo test -p namtor-cli --lib 2>&1 | grep -c "^test package_diagnostics::" || true)
TOTAL_DIAG=$((DIAG_TEST_COUNT + PKG_TEST_COUNT))
if [ "$TOTAL_DIAG" -gt 20 ]; then
    ok "diagnostics + package_diagnostics tests run ($TOTAL_DIAG tests)"
else
    not_ok "too few diagnostics tests ($TOTAL_DIAG)"
fi

# --- (b) privacy scan — no forbidden tokens in diagnostic output -----------

echo "== (b) privacy scan — no forbidden tokens =="

# Run a Rust test that produces a full diagnostic report and prints it,
# then scan the output for forbidden tokens.
PRIVACY_OUTPUT=$(cargo test -p namtor-cli --lib -- full_report_passes_privacy_scanner --nocapture 2>&1)

for token in "${FORBIDDEN_TOKENS[@]}"; do
    if echo "$PRIVACY_OUTPUT" | grep -qi "$token"; then
        not_ok "forbidden token '$token' found in diagnostic output"
    else
        ok "forbidden token '$token' absent from diagnostic output"
    fi
done

# Verify the privacy scanner test explicitly passes
if cargo test -p namtor-cli --lib -- full_report_passes_privacy_scanner 2>&1 | grep -q "ok"; then
    ok "full_report_passes_privacy_scanner test passes"
else
    not_ok "full_report_passes_privacy_scanner test failed"
fi

# Verify bundle passes privacy scan
if cargo test -p namtor-cli --lib -- full_bundle_passes_privacy_scanner 2>&1 | grep -q "ok"; then
    ok "full_bundle_passes_privacy_scanner test passes"
else
    not_ok "full_bundle_passes_privacy_scanner test failed"
fi

# --- (c) hex redaction — model hashes truncated ----------------------------

echo "== (c) hex redaction — model hashes truncated =="

# Verify the redact_hex_id test passes
if cargo test -p namtor-cli --lib -- redact_hex_id_long_truncated 2>&1 | grep -q "ok"; then
    ok "redact_hex_id_long_truncated test passes"
else
    not_ok "redact_hex_id_long_truncated test failed"
fi

# Verify SHA-256 is redacted in model info
if cargo test -p namtor-cli --lib -- model_sha256_does_not_trigger_privacy_scanner 2>&1 | grep -q "ok"; then
    ok "model_sha256_does_not_trigger_privacy_scanner test passes"
else
    not_ok "model_sha256_does_not_trigger_privacy_scanner test failed"
fi

# Verify collect_model_info produces redacted hashes
if cargo test -p namtor-cli --lib -- collect_model_info_reads_onnx_files 2>&1 | grep -q "ok"; then
    ok "collect_model_info_reads_onnx_files test passes (redacted sha256)"
else
    not_ok "collect_model_info_reads_onnx_files test failed"
fi

# --- (d) migration detection — old artifacts detected ---------------------

echo "== (d) migration detection — old artifacts detected =="

# Verify legacy setuid detection
if cargo test -p namtor-cli --lib -- detect_legacy_setuid_artifact 2>&1 | grep -q "ok"; then
    ok "detect_legacy_setuid_artifact test passes"
else
    not_ok "detect_legacy_setuid_artifact test failed"
fi

# Verify legacy install script detection
if cargo test -p namtor-cli --lib -- detect_legacy_install_script 2>&1 | grep -q "ok"; then
    ok "detect_legacy_install_script test passes"
else
    not_ok "detect_legacy_install_script test failed"
fi

# Verify manual install detection
if cargo test -p namtor-cli --lib -- detect_manual_install_artifact 2>&1 | grep -q "ok"; then
    ok "detect_manual_install_artifact test passes"
else
    not_ok "detect_manual_install_artifact test failed"
fi

# Verify clean install has no migration artifacts
if cargo test -p namtor-cli --lib -- no_artifacts_when_clean_split_install 2>&1 | grep -q "ok"; then
    ok "no_artifacts_when_clean_split_install test passes"
else
    not_ok "no_artifacts_when_clean_split_install test failed"
fi

# Verify migration artifacts in full doctor report
if cargo test -p namtor-cli --lib -- doctor_report_detects_migration_artifacts 2>&1 | grep -q "ok"; then
    ok "doctor_report_detects_migration_artifacts test passes"
else
    not_ok "doctor_report_detects_migration_artifacts test failed"
fi

# Verify migration display is privacy-safe
if cargo test -p namtor-cli --lib -- migration_artifact_display_is_privacy_safe 2>&1 | grep -q "ok"; then
    ok "migration_artifact_display_is_privacy_safe test passes"
else
    not_ok "migration_artifact_display_is_privacy_safe test failed"
fi

# --- (e) forbidden-data mutation — scanner rejects injected tokens ---------

echo "== (e) forbidden-data mutation — scanner rejects injected tokens =="

# Verify the bundle rejects content with biometric terms
if cargo test -p namtor-cli --lib -- bundle_rejects_content_with_biometric_term 2>&1 | grep -q "ok"; then
    ok "bundle_rejects_content_with_biometric_term test passes"
else
    not_ok "bundle_rejects_content_with_biometric_term test failed"
fi

# Verify the privacy scanner rejects each forbidden term
if cargo test -p namtor-cli --lib -- privacy_scanner_rejects_embedding_template_pixel 2>&1 | grep -q "ok"; then
    ok "privacy_scanner_rejects_embedding_template_pixel test passes"
else
    not_ok "privacy_scanner_rejects_embedding_template_pixel test failed"
fi

if cargo test -p namtor-cli --lib -- privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce 2>&1 | grep -q "ok"; then
    ok "privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce test passes"
else
    not_ok "privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce test failed"
fi

# Verify log filtering removes biometric terms
if cargo test -p namtor-cli --lib -- filter_log_lines_removes_biometric_terms 2>&1 | grep -q "ok"; then
    ok "filter_log_lines_removes_biometric_terms test passes"
else
    not_ok "filter_log_lines_removes_biometric_terms test failed"
fi

# --- (f) package detection and PAM count -----------------------------------

echo "== (f) package detection and PAM count =="

if cargo test -p namtor-cli --lib -- detect_all_three_split_packages 2>&1 | grep -q "ok"; then
    ok "detect_all_three_split_packages test passes"
else
    not_ok "detect_all_three_split_packages test failed"
fi

if cargo test -p namtor-cli --lib -- count_enabled_pam_services_counts_namtor_entries 2>&1 | grep -q "ok"; then
    ok "count_enabled_pam_services_counts_namtor_entries test passes"
else
    not_ok "count_enabled_pam_services_counts_namtor_entries test failed"
fi

if cargo test -p namtor-cli --lib -- doctor_report_includes_package_state 2>&1 | grep -q "ok"; then
    ok "doctor_report_includes_package_state test passes"
else
    not_ok "doctor_report_includes_package_state test failed"
fi

if cargo test -p namtor-cli --lib -- doctor_report_includes_pam_count 2>&1 | grep -q "ok"; then
    ok "doctor_report_includes_pam_count test passes"
else
    not_ok "doctor_report_includes_pam_count test failed"
fi

# --- summary ---------------------------------------------------------------

echo ""
echo "==============================================="
echo "  Support bundle test results: $PASS passed, $FAIL failed"
echo "==============================================="

if [ "$FAIL" -gt 0 ]; then
    echo "Failed tests:"
    for t in "${FAILED_TESTS[@]}"; do
        echo "  - $t"
    done
    exit 1
fi

exit 0
