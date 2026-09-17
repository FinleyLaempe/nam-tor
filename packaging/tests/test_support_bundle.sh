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

# Run a cargo test and check pass/fail. Shows output on failure for debugging.
run_test() {
    local test_name="$1"
    local desc="$2"
    local out
    out=$(cargo test -p namtor-cli --lib -- "$test_name" 2>&1)
    if echo "$out" | grep_all "test result: ok"; then
        ok "$desc"
    else
        not_ok "$desc"
        echo "$out" | tail -20 | sed 's/^/    /'
    fi
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

if cargo test -p namtor-cli --lib 2>&1 | grep_all "test result: ok"; then
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
    if echo "$PRIVACY_OUTPUT" | grep_all -i "$token"; then
        not_ok "forbidden token '$token' found in diagnostic output"
    else
        ok "forbidden token '$token' absent from diagnostic output"
    fi
done

run_test full_report_passes_privacy_scanner "full_report_passes_privacy_scanner test passes"

# Verify bundle passes privacy scan
run_test full_bundle_passes_privacy_scanner "full_bundle_passes_privacy_scanner test passes"

# --- (c) hex redaction — model hashes truncated ----------------------------

echo "== (c) hex redaction — model hashes truncated =="

run_test redact_hex_id_long_truncated "redact_hex_id_long_truncated test passes"

# Verify SHA-256 is redacted in model info
run_test model_sha256_does_not_trigger_privacy_scanner "model_sha256_does_not_trigger_privacy_scanner test passes"

# Verify collect_model_info produces redacted hashes
run_test collect_model_info_reads_onnx_files "collect_model_info_reads_onnx_files test passes (redacted sha256)"

# --- (d) migration detection — old artifacts detected ---------------------

echo "== (d) migration detection — old artifacts detected =="

# Verify legacy setuid detection
run_test detect_legacy_setuid_artifact "detect_legacy_setuid_artifact test passes"

# Verify legacy install script detection
run_test detect_legacy_install_script "detect_legacy_install_script test passes"

# Verify manual install detection
run_test detect_manual_install_artifact "detect_manual_install_artifact test passes"

# Verify clean install has no migration artifacts
run_test no_artifacts_when_clean_split_install "no_artifacts_when_clean_split_install test passes"

# Verify migration artifacts in full doctor report
run_test doctor_report_detects_migration_artifacts "doctor_report_detects_migration_artifacts test passes"

# Verify migration display is privacy-safe
run_test migration_artifact_display_is_privacy_safe "migration_artifact_display_is_privacy_safe test passes"

# --- (e) forbidden-data mutation — scanner rejects injected tokens ---------

echo "== (e) forbidden-data mutation — scanner rejects injected tokens =="

# Verify the bundle rejects content with biometric terms
run_test bundle_rejects_content_with_biometric_term "bundle_rejects_content_with_biometric_term test passes"

# Verify the privacy scanner rejects each forbidden term
run_test privacy_scanner_rejects_embedding_template_pixel "privacy_scanner_rejects_embedding_template_pixel test passes"

run_test privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce "privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce test passes"

# Verify log filtering removes biometric terms
run_test filter_log_lines_removes_biometric_terms "filter_log_lines_removes_biometric_terms test passes"

# --- (f) package detection and PAM count -----------------------------------

echo "== (f) package detection and PAM count =="

run_test detect_all_three_split_packages "detect_all_three_split_packages test passes"

run_test count_enabled_pam_services_counts_namtor_entries "count_enabled_pam_services_counts_namtor_entries test passes"

run_test doctor_report_includes_package_state "doctor_report_includes_package_state test passes"

run_test doctor_report_includes_pam_count "doctor_report_includes_pam_count test passes"

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
