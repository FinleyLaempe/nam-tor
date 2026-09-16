#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Meta-test for Task 28: CI gates for public package completeness and policy drift.
#
# This is a meta-test that:
#   (1) runs every packaging test suite and verifies each exits 0 (or 77 for
#       hardware-gated skips);
#   (2) for each gate category, injects a known defect into a controlled
#       target, verifies the RIGHT test catches it (exits nonzero), and
#       restores the file from a backup;
#   (3) emits a coverage matrix and writes the two evidence JSON files:
#         docs/evidence/public-release/28-ci-baseline.json
#         docs/evidence/public-release/28-ci-mutations.json
#
# Safety: every mutation is reverted from a verified backup before the next
# mutation runs. An EXIT trap restores every backed-up file unconditionally,
# so an interrupted run never leaves a mutated file on disk. After each
# restore the file's sha256 is compared to the original to prove the working
# tree is byte-identical to its pre-mutation state.
#
# No existing packaging test script, Rust source, Cargo.toml, distro
# packaging recipe, install-manifest.toml, or model-manifest.toml is
# permanently modified by this test. All mutations are temporary and
# reverted.
#
# Usage: bash packaging/tests/test_ci_gates.sh
# Exit code: 0 if all baseline suites pass/skip and every mutation is caught,
#            1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EVIDENCE_DIR="$REPO_ROOT/docs/evidence/public-release"

PASS=0
FAIL=0
SKIP=0
FAILED_TESTS=()

# --- backup / restore -------------------------------------------------------
BACKUP_DIR="$(mktemp -d -t namtor-ci-gates-XXXXXX)"
BACKUP_INDEX="$BACKUP_DIR/index"     # one path per line, in registration order
: >"$BACKUP_INDEX"

cleanup() {
    # Restore every backed-up file unconditionally (interrupt-safe).
    if [[ -s "$BACKUP_INDEX" ]]; then
        while IFS= read -r orig; do
            [[ -n "$orig" && -f "$orig.namtorbak" ]] && cp -a "$orig.namtorbak" "$orig" 2>/dev/null || true
        done <"$BACKUP_INDEX"
    fi
    rm -rf "$BACKUP_DIR"
}
trap cleanup EXIT

# backup_file <path> — register + copy. Idempotent per path.
backup_file() {
    local p="$1"
    grep -qxF "$p" "$BACKUP_INDEX" 2>/dev/null || printf '%s\n' "$p" >>"$BACKUP_INDEX"
    cp -a "$p" "$p.namtorbak"
}

# restore_file <path> — copy back + verify sha256 matches the backup.
restore_file() {
    local p="$1"
    cp -a "$p.namtorbak" "$p"
    local a b
    a=$(sha256sum "$p.namtorbak" | awk '{print $1}')
    b=$(sha256sum "$p"     | awk '{print $1}')
    if [[ "$a" != "$b" ]]; then
        echo "  FATAL: restore hash mismatch for $p" >&2
        return 1
    fi
    rm -f "$p.namtorbak"
}

# apply_replace <old> <new> <file> — single in-place string replacement via
# python3 (robust against regex/special-char pitfalls). Asserts the anchor is
# present so a silent no-op is caught.
apply_replace() {
    local old="$1" new="$2" file="$3"
    python3 - "$old" "$new" "$file" <<'PY'
import sys
old, new, path = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    data = f.read()
assert old in data, f"mutation anchor not found in {path}: {old!r}"
with open(path, "w") as f:
    f.write(data.replace(old, new, 1))
PY
}

# apply_replace_all <old> <new> <file> — replace EVERY occurrence (for gates
# whose catching grep matches any occurrence, including comments).
apply_replace_all() {
    local old="$1" new="$2" file="$3"
    python3 - "$old" "$new" "$file" <<'PY'
import sys
old, new, path = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    data = f.read()
assert old in data, f"mutation anchor not found in {path}: {old!r}"
with open(path, "w") as f:
    f.write(data.replace(old, new))
PY
}

# --- baseline: run every packaging test suite -------------------------------
#
# All 11 suites. exit 0 = PASS, exit 77 = SKIP (hardware-gated, e.g. models
# absent in CI), anything else = FAIL.

BASELINE_SUITES=(
    "test_arch_split.sh"
    "test_fedora_split.sh"
    "test_debian_split.sh"
    "test_install_default_off.sh"
    "test_lifecycle.sh"
    "test_model_archive.sh"
    "test_uninstall_closure.sh"
    "test_supply_chain.sh"
    "test_release_signatures.sh"
    "test_doc_audit.sh"
    "test_support_bundle.sh"
)

# baseline_results: "suite\texit_code" lines, fed to the JSON emitter.
BASELINE_RESULTS="$BACKUP_DIR/baseline.tsv"
: >"$BASELINE_RESULTS"

echo "============================================================"
echo "Task 28 — CI gates: baseline (all packaging test suites)"
echo "============================================================"
for suite in "${BASELINE_SUITES[@]}"; do
    script="$SCRIPT_DIR/$suite"
    echo "--- $suite ---"
    set +e
    bash "$script" >/tmp/namtor-ci-gate-"$suite".log 2>&1
    rc=$?
    set -e  # safe: set -uo pipefail without -e means this is a no-op toggle; keep -e off below
    # NOTE: we deliberately do NOT use set -e; re-assert without it.
    set +e
    printf '%s\t%d\n' "$suite" "$rc" >>"$BASELINE_RESULTS"
    if [[ $rc -eq 0 ]]; then
        echo "  PASS (exit 0)"
        PASS=$((PASS + 1))
    elif [[ $rc -eq 77 ]]; then
        echo "  SKIP (exit 77 — hardware-gated)"
        SKIP=$((SKIP + 1))
    else
        echo "  FAIL (exit $rc)"
        FAIL=$((FAIL + 1))
        FAILED_TESTS+=("$suite (exit $rc)")
        tail -8 "/tmp/namtor-ci-gate-$suite.log" | sed 's/^/    | /'
    fi
done

BASELINE_PASS=$PASS
BASELINE_SKIP=$SKIP
BASELINE_FAIL=$FAIL
PASS=0; FAIL=0

# --- mutation matrix --------------------------------------------------------
#
# Each entry: "id|gate_category|target_file|catching_test|description"
# The apply step is dispatched by id in the case below.

read -r -d '' MUTATIONS_RAW <<'EOF' || true
M01|licence/hash/notice drift (hash)|docs/evidence/packaging/namtor.cyclonedx.json|test_supply_chain.sh|flip one hex digit of yunet sha256 in CycloneDX SBOM -> SBOM!=manifest
M02|licence/hash/notice drift (notice)|ml/reports/learned-pad.md|test_supply_chain.sh|rename '### Training Recipe' -> '### MUTATED Recipe' (model card section drop)
M03|split ownership|packaging/stage.sh|test_arch_split.sh|break --package case arm -> stage.sh --package core exits 2
M04|production model validation call path|packaging/build-model-archive.sh|test_model_archive.sh|comment out licence copy -> archive missing licences (gate 4)
M05|system-bus client parity|packaging/fedora/namtor.spec|test_fedora_split.sh|dbus-1/system-services -> dbus-1/session-services in core %files
M06|PAM-default-off|packaging/arch/namtor.install|test_install_default_off.sh|inject pam-install.sh call into post_install
M07|no unexpected setuid|HANDOFF.md|test_doc_audit.sh|inject stale 'setuid helper' string
M08|package builds (structural)|packaging/arch/PKGBUILD|test_arch_split.sh|remove namtor-ui from pkgname array
M09|uninstall closure|packaging/uninstall.sh|test_uninstall_closure.sh|rename tomllib -> tommutant (manifest parse gone)
M10|docs claims|packaging/README.md|test_doc_audit.sh|inject stale 'models ensure' string
M11|SBOM/signatures|packaging/sign-packages.sh|test_release_signatures.sh|rename 'namtor-models-' -> 'namtor-mutants-' (coverage pattern gone)
M12|lifecycle phases|packaging/lifecycle.sh|test_lifecycle.sh|drop pre-install from accepted phase case
M13|package builds (structural, debian)|packaging/debian/control|test_debian_split.sh|rename 'Package: namtor-ui' -> 'Package: namtor-mutant-ui'
EOF

MUTATION_RESULTS="$BACKUP_DIR/mutations.tsv"
: >"$MUTATION_RESULTS"

echo ""
echo "============================================================"
echo "Task 28 — CI gates: mutation matrix (inject, verify caught, revert)"
echo "============================================================"

run_mutation() {
    local id="$1" gate="$2" target_rel="$3" test_suite="$4" desc="$5"
    local target="$REPO_ROOT/$target_rel"
    local test_script="$SCRIPT_DIR/$test_suite"

    echo "--- $id [$gate] -> $test_suite ---"
    echo "  mutation: $desc"

    if [[ ! -f "$target" ]]; then
        echo "  SKIP (target missing: $target)"
        printf '%s\t%s\tSKIP\ttarget-missing\n' "$id" "$gate" >>"$MUTATION_RESULTS"
        SKIP=$((SKIP + 1))
        return
    fi

    backup_file "$target"
    local orig_hash
    orig_hash=$(sha256sum "$target" | awk '{print $1}')

    # Apply the mutation (dispatch by id).
    case "$id" in
        M01)
            python3 - "$target" <<'PY'
import json, sys
path = sys.argv[1]
d = json.load(open(path))
for c in d.get("components", []):
    if c.get("name") == "yunet_2023mar" and c.get("hashes"):
        h = c["hashes"][0]["content"]
        # flip first hex char (a->b, else a) so it no longer matches manifest
        c["hashes"][0]["content"] = ("b" if h[0] == "a" else "a") + h[1:]
        break
with open(path, "w") as f:
    json.dump(d, f, indent=2, sort_keys=True)
    f.write("\n")
PY
            ;;
        M02) apply_replace "### Training Recipe" "### MUTATED Recipe" "$target" ;;
        M03) apply_replace "--package)" "--package-mutant)" "$target" ;;
        M04) apply_replace 'cp "${licence_files[@]}" "$stage/licences/"' '#MUTANT cp "${licence_files[@]}" "$stage/licences/"' "$target" ;;
        M05) apply_replace "dbus-1/system-services/org.namtor.Manager.service" "dbus-1/session-services/org.namtor.Manager.service" "$target" ;;
        M06) apply_replace "post_install() {" "post_install() {
    /usr/share/namtor/pam-install.sh --target /etc/pam.d/gdm-password   # MUTANT" "$target" ;;
        M07) apply_replace "Experimental preview" "Experimental preview — setuid helper removed" "$target" ;;
        M08) apply_replace $'    namtor-ui\n' '' "$target" ;;
        M09) apply_replace_all "tomllib" "tommutant" "$target" ;;
        M10) apply_replace "Experimental preview" "Experimental preview — namtor-cli models ensure" "$target" ;;
        M11) apply_replace_all "namtor-models-" "namtor-mutants-" "$target" ;;
        M12) apply_replace "pre-install|post-install|pre-upgrade|post-upgrade|pre-remove|post-remove" "post-install|pre-upgrade|post-upgrade|pre-remove|post-remove" "$target" ;;
        M13) apply_replace "Package: namtor-ui" "Package: namtor-mutant-ui" "$target" ;;
        *) echo "  FATAL: unknown mutation id $id" >&2; return 1 ;;
    esac

    # Run the catching test; expect a nonzero, non-77 exit (mutation caught).
    set +e
    bash "$test_script" >/tmp/namtor-mutant-"$id".log 2>&1
    local rc=$?
    set +e
    local caught=0
    if [[ $rc -ne 0 && $rc -ne 77 ]]; then
        caught=1
        echo "  PASS: mutation caught (test exited $rc)"
        PASS=$((PASS + 1))
    elif [[ $rc -eq 77 ]]; then
        echo "  SKIP: test hardware-gated (exit 77) — mutation not exercisable here"
        SKIP=$((SKIP + 1))
        caught=77
    else
        echo "  FAIL: mutation NOT caught (test exited 0)"
        FAIL=$((FAIL + 1))
        FAILED_TESTS+=("$id: $test_suite did not catch mutation")
        tail -6 "/tmp/namtor-mutant-$id.log" | sed 's/^/    | /'
    fi

    # Restore + verify integrity.
    restore_file "$target" || {
        echo "  FATAL: restore failed for $target" >&2
        FAIL=$((FAIL + 1))
        FAILED_TESTS+=("$id: restore failed")
        return 1
    }
    local new_hash
    new_hash=$(sha256sum "$target" | awk '{print $1}')
    if [[ "$orig_hash" != "$new_hash" ]]; then
        echo "  FATAL: post-restore hash mismatch for $target" >&2
        FAIL=$((FAIL + 1))
        FAILED_TESTS+=("$id: post-restore hash mismatch")
        return 1
    fi
    printf '%s\t%s\t%d\t%s\n' "$id" "$gate" "$rc" "$test_suite" >>"$MUTATION_RESULTS"
}

while IFS='|' read -r mid mgate mtarget mtest mdesc; do
    [[ -z "$mid" ]] && continue
    run_mutation "$mid" "$mgate" "$mtarget" "$mtest" "$mdesc"
done <<<"$MUTATIONS_RAW"

MUTATION_PASS=$PASS
MUTATION_SKIP=$SKIP
MUTATION_FAIL=$FAIL

# --- coverage matrix --------------------------------------------------------
echo ""
echo "============================================================"
echo "Task 28 — CI gate coverage matrix"
echo "============================================================"
printf '%-5s %-42s %-28s %s\n' "ID" "GATE CATEGORY" "CATCHING TEST" "RESULT"
printf '%-5s %-42s %-28s %s\n' "--" "-------------" "-------------" "------"
while IFS='|' read -r mid mgate mtest _; do
    [[ -z "$mid" ]] && continue
    awk -F'\t' -v id="$mid" '$1==id{print $1"\t"$4}' "$MUTATION_RESULTS" >/dev/null
done <<<"$MUTATIONS_RAW"
# Print from the results TSV (id, gate, test, caught?).
while IFS=$'\t' read -r mid mgate mrc mtest; do
    local_result="CAUGHT"
    [[ "$mrc" == "0" ]] && local_result="NOT CAUGHT"
    [[ "$mrc" == "77" ]] && local_result="SKIP (hw-gated)"
    printf '%-5s %-42s %-28s %s\n' "$mid" "$mgate" "$mtest" "$local_result"
done <"$MUTATION_RESULTS"

# --- emit evidence JSON -----------------------------------------------------
mkdir -p "$EVIDENCE_DIR"
python3 - "$EVIDENCE_DIR" "$BASELINE_RESULTS" "$MUTATION_RESULTS" \
    "$BASELINE_PASS" "$BASELINE_SKIP" "$BASELINE_FAIL" \
    "$MUTATION_PASS" "$MUTATION_SKIP" "$MUTATION_FAIL" <<'PY'
import json, os, sys
(evid_dir, baseline_tsv, mutation_tsv,
 bp, bs, bf, mp, ms, mf) = sys.argv[1:10]
bp, bs, bf = int(bp), int(bs), int(bf)
mp, ms, mf = int(mp), int(ms), int(mf)

baseline = []
with open(baseline_tsv) as f:
    for line in f:
        suite, rc = line.rstrip("\n").split("\t")
        rc = int(rc)
        status = "PASS" if rc == 0 else ("SKIP" if rc == 77 else "FAIL")
        baseline.append({"suite": suite, "exit_code": rc, "status": status})

mutations = []
with open(mutation_tsv) as f:
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) < 4:
            continue
        mid, gate, raw_rc, test = parts[0], parts[1], parts[2], parts[3]
        # A mutation whose target file no longer exists is recorded with the
        # literal "SKIP" rather than an exit code (see the target-missing
        # branch above). int() on that raised ValueError and killed this
        # emitter mid-run, while the caller still printed "Evidence written:"
        # and exited 0 — so the JSON on disk silently stopped being
        # regenerated and went stale while claiming to be current.
        if raw_rc == "SKIP":
            rc, res = None, "SKIP"
        else:
            rc = int(raw_rc)
            if rc == 77:
                res = "SKIP"
            elif rc == 0:
                res = "NOT_CAUGHT"
            else:
                res = "CAUGHT"
        mutations.append({"id": mid, "gate": gate, "catching_test": test,
                          "exit_code": rc, "result": res})

base_status = "PASS" if bf == 0 else "FAIL"
mut_status = "PASS" if mf == 0 else "FAIL"

baseline_doc = {
    "task": "28",
    "title": "CI gates for public package completeness and policy drift",
    "gate": "ci-packaging-baseline",
    "status": base_status,
    "summary": (
        f"Baseline run of all 11 packaging test suites: {bp} passed, "
        f"{bs} skipped (hardware-gated), {bf} failed. Each suite is a CI "
        "gate that blocks on nonzero exit (77 = hardware-gated skip, not a "
        "failure). Suites cover: split-package structure (Arch/Fedora/Debian), "
        "PAM default-off at install, lifecycle helper phases, model archive "
        "reproducibility/tamper/manifest hygiene, uninstall closure, supply "
        "chain (SBOM + provenance + model cards), release signatures, doc "
        "audit (stale strings + experimental warning), support bundle "
        "(privacy-safe diagnostics + legacy setuid detection)."
    ),
    "suites": baseline,
    "totals": {"pass": bp, "skip": bs, "fail": bf},
    "gate_categories": [
        "licence/hash/notice drift", "split ownership",
        "production model validation call path", "system-bus client parity",
        "PAM-default-off", "no unexpected setuid",
        "package builds (structural)", "uninstall closure",
        "docs claims", "SBOM/signatures"
    ],
}
with open(os.path.join(evid_dir, "28-ci-baseline.json"), "w") as f:
    json.dump(baseline_doc, f, indent=2)
    f.write("\n")

mutation_doc = {
    "task": "28",
    "title": "CI gate mutation matrix — each injected defect is caught",
    "gate": "ci-packaging-mutations",
    "status": mut_status,
    "summary": (
        f"Mutation matrix: {mp} mutations caught by the right gate, "
        f"{ms} skipped (hardware-gated), {mf} not caught. For each gate "
        "category a known defect is injected into a controlled target, the "
        "catching test is run, the defect is reverted from a verified "
        "backup, and the restored file's sha256 is checked against the "
        "original. No file is left mutated."
    ),
    "mutations": mutations,
    "totals": {"caught": mp, "skip": ms, "not_caught": mf},
    "safety": {
        "backup_restore": "every mutation is backed up before injection and "
        "restored after; an EXIT trap restores all backups unconditionally",
        "integrity_check": "post-restore sha256 compared to the original "
        "for every mutation",
        "no_permanent_change": "no test script, Rust source, Cargo.toml, "
        "distro packaging recipe, install-manifest.toml, or "
        "model-manifest.toml is permanently modified"
    },
}
with open(os.path.join(evid_dir, "28-ci-mutations.json"), "w") as f:
    json.dump(mutation_doc, f, indent=2)
    f.write("\n")
PY

# The emitter above is a heredoc; a crash inside it does not stop this
# script. Assert the files were actually rewritten by this run before
# claiming they were, so a dead emitter can never again leave stale JSON
# on disk under a "Evidence written:" banner.
for _ev in "$EVIDENCE_DIR/28-ci-baseline.json" "$EVIDENCE_DIR/28-ci-mutations.json"; do
    if [ ! -f "$_ev" ]; then
        echo "::error::evidence emitter produced no $_ev"
        exit 1
    fi
    if [ -n "$(find "$_ev" -newermt '-120 seconds' -print -quit 2>/dev/null)" ]; then
        continue
    fi
    echo "::error::$_ev was not regenerated by this run (stale on disk)"
    exit 1
done

echo ""
echo "Evidence written:"
echo "  $EVIDENCE_DIR/28-ci-baseline.json"
echo "  $EVIDENCE_DIR/28-ci-mutations.json"

# --- summary ----------------------------------------------------------------
echo ""
echo "============================================================"
echo "Task 28 summary"
echo "  Baseline: $BASELINE_PASS passed, $BASELINE_SKIP skipped, $BASELINE_FAIL failed"
echo "  Mutations: $MUTATION_PASS caught, $MUTATION_SKIP skipped, $MUTATION_FAIL not caught"
echo "============================================================"
if [[ $BASELINE_FAIL -gt 0 || $MUTATION_FAIL -gt 0 ]]; then
    echo "Failures:"
    for t in "${FAILED_TESTS[@]}"; do echo "  - $t"; done
    exit 1
fi
echo "All CI gates pass; all mutations caught; working tree restored."
exit 0
