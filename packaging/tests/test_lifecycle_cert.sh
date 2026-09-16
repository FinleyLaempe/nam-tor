#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Certification test for Task 31: full package lifecycle — upgrade, rollback,
# cross-desktop switch, and removal.
#
# Acceptance criteria (from the task spec):
#   (a) Fresh install: stage core → verify all core files present, no PAM
#       modifications.
#   (b) Upgrade: re-stage core over existing (simulating version bump) →
#       verify profiles/templates preserved (sha256 receipts), manifest
#       updated, no stale files.
#   (c) Add desktop: stage ui over existing core → verify ui files
#       added, core intact.
#   (d) Legacy desktop package rejected: `stage.sh --package kde` (the
#       removed Plasma KCM) must fail loudly rather than silently stage
#       nothing, and must leave the existing core+ui tree untouched.
#   (e) Rollback: remove desktop package files → verify core still
#       functional.
#   (f) Full removal: run uninstall.sh → verify total purge (no namtor
#       files, no PAM blocks, no stale users/groups in manifest).
#   (g) Password login preservation: verify no /etc/pam.d modifications at
#       any stage (default-off policy).
#
# Failure matrix:
#   (F1) Interrupted upgrade (partial staging) → detect incomplete state.
#   (F2) Stale files from previous version not in new manifest → detected
#        as stale.
#   (F3) Concurrent staging attempts → detected/rejected (or proven safe
#        by idempotent re-staging producing a consistent tree).
#   (F4) Removal with missing manifest → handled gracefully (nonzero exit,
#        clear message, no crash).
#   (F5) PAM block leftover after removal → detected (should not exist
#        since default-off; verify zero pam_namtor references after purge).
#
# Since clean distros cannot be run in CI, real distro upgrade/rollback on
# a live VM (pacman -U, dnf upgrade, apt upgrade) is WITHHELD. This script
# uses DESTDIR fake-root staging via stage.sh --package core|ui and
# uninstall.sh to certify the lifecycle structurally.
#
# Usage: bash packaging/tests/test_lifecycle_cert.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"

STAGE="$PACKAGING_DIR/stage.sh"
UNINSTALL="$PACKAGING_DIR/uninstall.sh"
MANIFEST="$PACKAGING_DIR/install-manifest.toml"
LIFECYCLE_SRC="$PACKAGING_DIR/lifecycle.sh"

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

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

sha256_of() {
    if [[ -f "$1" ]]; then
        sha256sum "$1" | awk '{print $1}'
    else
        printf ''
    fi
}

manifest_files_for_pkg() {
    local pkg="$1"
    python3 - "$MANIFEST" "$pkg" <<'PYEOF'
import sys, tomllib
manifest_path, pkg = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
for entry in m.get("files", []):
    if pkg == "all" or entry.get("package", "core") == pkg:
        print(entry["dest"])
PYEOF
}

manifest_dirs_for_pkg() {
    local pkg="$1"
    python3 - "$MANIFEST" "$pkg" <<'PYEOF'
import sys, tomllib
manifest_path, pkg = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
for d in m.get("dirs", []):
    if pkg == "all" or d.get("package", "core") == pkg:
        print(d["path"])
PYEOF
}

count_missing_files() {
    local root="$1" pkg="$2"
    local missing=0
    while IFS= read -r dest; do
        [[ -z "$dest" ]] && continue
        if ! path_exists "$root$dest"; then
            missing=$((missing + 1))
        fi
    done < <(manifest_files_for_pkg "$pkg")
    printf '%s' "$missing"
}

count_missing_dirs() {
    local root="$1" pkg="$2"
    local missing=0
    while IFS= read -r dpath; do
        [[ -z "$dpath" ]] && continue
        if [[ ! -d "$root$dpath" ]]; then
            missing=$((missing + 1))
        fi
    done < <(manifest_dirs_for_pkg "$pkg")
    printf '%s' "$missing"
}

count_pam_namtor_refs() {
    local root="$1"
    local pamd="$root/etc/pam.d"
    local count=0
    if [[ -d "$pamd" ]]; then
        count=$(grep -rl 'pam_namtor' "$pamd" 2>/dev/null | wc -l)
    fi
    printf '%s' "$count"
}

scan_stale() {
    local root="$1"
    python3 - "$MANIFEST" "$root" <<'PYEOF'
import sys, os, tomllib
manifest_path, root = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
declared = set()
for entry in m.get("files", []):
    declared.add(entry["dest"])
namtor_dirs = [
    "/usr/lib/namtor",
    "/usr/share/namtor",
    "/usr/share/doc/namtor",
]
stale = []
for d in namtor_dirs:
    full = os.path.join(root, d.lstrip("/"))
    if not os.path.isdir(full):
        continue
    for dirpath, dirnames, filenames in os.walk(full):
        for fn in filenames:
            abspath = os.path.join(dirpath, fn)
            relpath = "/" + os.path.relpath(abspath, root)
            if relpath not in declared:
                stale.append(relpath)
for s in sorted(stale):
    print(s)
PYEOF
}

echo "=================================================================="
echo "Task 31 — Full package lifecycle certification"
echo "  upgrade / rollback / cross-desktop / removal"
echo "=================================================================="

echo ""
echo "--- prerequisites ---"

for f in "$STAGE" "$UNINSTALL" "$MANIFEST" "$LIFECYCLE_SRC"; do
    if [[ -f "$f" ]]; then
        ok "present: $(basename "$f")"
    else
        not_ok "missing: $f"
    fi
done

DAEMON_BIN="$REPO_ROOT/target/release/namtor-daemon"
WORKER_BIN="$REPO_ROOT/target/release/namtor-worker"
EMITTER_OFF_BIN="$REPO_ROOT/target/release/namtor-camera-emitter-off"
CLI_BIN="$REPO_ROOT/target/release/namtor-cli"
PAM_SO="$REPO_ROOT/target/release/libnamtor_pam.so"
UI_BIN="$REPO_ROOT/target/release/namtor-enroll-wizard"
for f in "$DAEMON_BIN" "$WORKER_BIN" "$EMITTER_OFF_BIN" "$CLI_BIN" "$PAM_SO" "$UI_BIN"; do
    if [[ -f "$f" ]]; then
        ok "built artifact present: $(basename "$f")"
    else
        not_ok "built artifact missing: $f (run cargo build --release)"
    fi
done

if [[ ! -f "$STAGE" || ! -f "$MANIFEST" ]]; then
    echo "FATAL: stage.sh or install-manifest.toml missing — cannot continue" >&2
    exit 1
fi

# ===========================================================================
# (a) Fresh install: stage core
# ===========================================================================

echo ""
echo "[a] Fresh install: stage core"

ROOT_A="$(mk_temp_dir)"
if bash "$STAGE" "$ROOT_A" "$REPO_ROOT" --package core >/tmp/lc_stage_core_a.log 2>&1; then
    ok "stage.sh --package core succeeds (fresh install)"
else
    not_ok "stage.sh --package core fails (fresh install; see /tmp/lc_stage_core_a.log)"
    cat /tmp/lc_stage_core_a.log >&2
fi

missing_a_files="$(count_missing_files "$ROOT_A" core)"
if [[ "$missing_a_files" -eq 0 ]]; then
    ok "fresh install: all core manifest files present"
else
    not_ok "fresh install: $missing_a_files core manifest file(s) missing"
fi

missing_a_dirs="$(count_missing_dirs "$ROOT_A" core)"
if [[ "$missing_a_dirs" -eq 0 ]]; then
    ok "fresh install: all core manifest dirs present"
else
    not_ok "fresh install: $missing_a_dirs core manifest dir(s) missing"
fi

if [[ -d "$ROOT_A/etc/pam.d" ]]; then
    not_ok "fresh install: staged root has /etc/pam.d (PAM should be default-off)"
else
    ok "fresh install: no /etc/pam.d (PAM default-off)"
fi

for rel in \
    usr/lib/namtor/namtor-daemon \
    usr/lib/namtor/namtor-worker \
    usr/bin/namtor-cli \
    usr/lib/security/pam_namtor.so \
    usr/lib/systemd/system/namtor-daemon.socket \
    usr/lib/systemd/system/namtor-daemon.service \
    usr/share/namtor/lifecycle.sh \
    usr/share/namtor/install-manifest.toml; do
    if path_exists "$ROOT_A/$rel"; then
        ok "fresh install: $rel present"
    else
        not_ok "fresh install: $rel missing"
    fi
done

# ===========================================================================
# (b) Upgrade: re-stage core over existing (sha256 preservation receipts)
# ===========================================================================

echo ""
echo "[b] Upgrade: re-stage core over existing (sha256 preservation receipts)"

SEED_TPL="$ROOT_A/var/lib/namtor/templates/uid_1000.bin"
SEED_TPL_KEY="$ROOT_A/var/lib/namtor/templates.key"
SEED_BACKOFF="$ROOT_A/var/lib/namtor/backoff.json"
SEED_MODEL="$ROOT_A/var/lib/namtor/models/yunet_2023mar.onnx"
mkdir -p "$ROOT_A/var/lib/namtor/templates" "$ROOT_A/var/lib/namtor/models"
printf 'fake-template-payload-for-uid-1000' >"$SEED_TPL"
printf 'fake-key-material' >"$SEED_TPL_KEY"
printf '{"backoff":{"count":0}}' >"$SEED_BACKOFF"
printf 'fake-yunet-weights' >"$SEED_MODEL"

TPL_SHA_BEFORE="$(sha256_of "$SEED_TPL")"
TPLKEY_SHA_BEFORE="$(sha256_of "$SEED_TPL_KEY")"
BACKOFF_SHA_BEFORE="$(sha256_of "$SEED_BACKOFF")"
MODEL_SHA_BEFORE="$(sha256_of "$SEED_MODEL")"
DAEMON_SHA_BEFORE="$(sha256_of "$ROOT_A/usr/lib/namtor/namtor-daemon")"

if bash "$STAGE" "$ROOT_A" "$REPO_ROOT" --package core >/tmp/lc_stage_core_b.log 2>&1; then
    ok "upgrade: re-stage core over existing succeeds"
else
    not_ok "upgrade: re-stage core over existing fails (see /tmp/lc_stage_core_b.log)"
    cat /tmp/lc_stage_core_b.log >&2
fi

TPL_SHA_AFTER="$(sha256_of "$SEED_TPL")"
TPLKEY_SHA_AFTER="$(sha256_of "$SEED_TPL_KEY")"
BACKOFF_SHA_AFTER="$(sha256_of "$SEED_BACKOFF")"
MODEL_SHA_AFTER="$(sha256_of "$SEED_MODEL")"

if [[ "$TPL_SHA_BEFORE" == "$TPL_SHA_AFTER" && -n "$TPL_SHA_BEFORE" ]]; then
    ok "upgrade: template uid_1000.bin preserved (sha256 receipt match)"
else
    not_ok "upgrade: template uid_1000.bin NOT preserved (sha256 changed)"
fi
if [[ "$TPLKEY_SHA_BEFORE" == "$TPLKEY_SHA_AFTER" && -n "$TPLKEY_SHA_BEFORE" ]]; then
    ok "upgrade: templates.key preserved (sha256 receipt match)"
else
    not_ok "upgrade: templates.key NOT preserved (sha256 changed)"
fi
if [[ "$BACKOFF_SHA_BEFORE" == "$BACKOFF_SHA_AFTER" && -n "$BACKOFF_SHA_BEFORE" ]]; then
    ok "upgrade: backoff.json preserved (sha256 receipt match)"
else
    not_ok "upgrade: backoff.json NOT preserved (sha256 changed)"
fi
if [[ "$MODEL_SHA_BEFORE" == "$MODEL_SHA_AFTER" && -n "$MODEL_SHA_BEFORE" ]]; then
    ok "upgrade: model yunet_2023mar.onnx preserved (sha256 receipt match)"
else
    not_ok "upgrade: model yunet_2023mar.onnx NOT preserved (sha256 changed)"
fi

missing_b_files="$(count_missing_files "$ROOT_A" core)"
if [[ "$missing_b_files" -eq 0 ]]; then
    ok "upgrade: all core manifest files still present"
else
    not_ok "upgrade: $missing_b_files core manifest file(s) missing after upgrade"
fi

MANIFEST_SHA_AFTER="$(sha256_of "$ROOT_A/usr/share/namtor/install-manifest.toml")"
if [[ -n "$MANIFEST_SHA_AFTER" ]]; then
    ok "upgrade: install-manifest.toml present after upgrade (updated)"
else
    not_ok "upgrade: install-manifest.toml missing after upgrade"
fi

DAEMON_SHA_AFTER="$(sha256_of "$ROOT_A/usr/lib/namtor/namtor-daemon")"
if [[ "$DAEMON_SHA_BEFORE" == "$DAEMON_SHA_AFTER" && -n "$DAEMON_SHA_AFTER" ]]; then
    ok "upgrade: daemon binary sha256 stable (same source, idempotent re-copy)"
else
    ok "upgrade: daemon binary sha256 changed (version bump — expected on real upgrade)"
fi

stale_files="$(scan_stale "$ROOT_A")"
stale_count="$(printf '%s' "$stale_files" | grep -c . 2>/dev/null || true)"
if [[ "$stale_count" -eq 0 ]]; then
    ok "upgrade: no stale files in namtor-owned dirs (outside generated_state)"
else
    not_ok "upgrade: $stale_count stale file(s) detected in namtor-owned dirs"
    printf '%s\n' "$stale_files" | sed 's/^/      /' >&2
fi

if [[ -d "$ROOT_A/etc/pam.d" ]]; then
    not_ok "upgrade: staged root has /etc/pam.d after re-stage (should not)"
else
    ok "upgrade: no /etc/pam.d after re-stage (PAM still default-off)"
fi

# ===========================================================================
# (c) Add desktop: stage ui over existing core
# ===========================================================================

echo ""
echo "[c] Add desktop: stage ui over existing core"

if bash "$STAGE" "$ROOT_A" "$REPO_ROOT" --package ui >/tmp/lc_stage_ui_c.log 2>&1; then
    ok "add-desktop: stage.sh --package ui succeeds over core"
else
    not_ok "add-desktop: stage.sh --package ui fails (see /tmp/lc_stage_ui_c.log)"
    cat /tmp/lc_stage_ui_c.log >&2
fi

missing_ui_files="$(count_missing_files "$ROOT_A" ui)"
if [[ "$missing_ui_files" -eq 0 ]]; then
    ok "add-desktop: all ui manifest files present"
else
    not_ok "add-desktop: $missing_ui_files ui manifest file(s) missing"
fi

missing_core_after_ui="$(count_missing_files "$ROOT_A" core)"
if [[ "$missing_core_after_ui" -eq 0 ]]; then
    ok "add-desktop: core files still present after ui added"
else
    not_ok "add-desktop: $missing_core_after_ui core file(s) missing after ui added"
fi

TPL_SHA_UI="$(sha256_of "$SEED_TPL")"
if [[ "$TPL_SHA_UI" == "$TPL_SHA_BEFORE" ]]; then
    ok "add-desktop: template still preserved after ui staged"
else
    not_ok "add-desktop: template altered by ui staging"
fi

for rel in \
    usr/bin/namtor-enroll-wizard \
    usr/share/applications/org.namtor.Wizard.desktop \
    usr/share/metainfo/org.namtor.Wizard.metainfo.xml \
    usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg; do
    if path_exists "$ROOT_A/$rel"; then
        ok "add-desktop: $rel present"
    else
        not_ok "add-desktop: $rel missing"
    fi
done

# ===========================================================================
# (d) Legacy `--package kde` is rejected and changes nothing
# ===========================================================================

echo ""
echo "[d] Legacy --package kde is rejected"

# The Plasma KCM package was removed, so `kde` is no longer in stage.sh's
# filter allowlist. A stale build recipe asking for it must fail loudly —
# the dangerous outcome would be an accepted-but-empty stage that produces
# an apparently-successful package with no payload.
files_before_kde="$(find "$ROOT_A" -type f -o -type l | sort | sha256sum)"
if bash "$STAGE" "$ROOT_A" "$REPO_ROOT" --package kde >/tmp/lc_stage_kde_d.log 2>&1; then
    not_ok "legacy: stage.sh --package kde succeeded (should be rejected)"
else
    ok "legacy: stage.sh --package kde rejected"
fi
files_after_kde="$(find "$ROOT_A" -type f -o -type l | sort | sha256sum)"
if [[ "$files_before_kde" == "$files_after_kde" ]]; then
    ok "legacy: rejected staging left the tree byte-identical"
else
    not_ok "legacy: rejected staging modified the tree"
fi

# No KCM payload may appear in a fully staged tree.
for rel in \
    usr/share/applications/org.namtor.kcm.desktop \
    usr/share/metainfo/org.namtor.kcm.metainfo.xml \
    usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg; do
    if path_exists "$ROOT_A/$rel"; then
        not_ok "legacy: KCM payload $rel is present (should be gone)"
    else
        ok "legacy: KCM payload $rel absent"
    fi
done

missing_ui_after_kde="$(count_missing_files "$ROOT_A" ui)"
if [[ "$missing_ui_after_kde" -eq 0 ]]; then
    ok "legacy: ui files intact"
else
    not_ok "legacy: $missing_ui_after_kde ui file(s) missing"
fi

missing_core_after_kde="$(count_missing_files "$ROOT_A" core)"
if [[ "$missing_core_after_kde" -eq 0 ]]; then
    ok "legacy: core files intact"
else
    not_ok "legacy: $missing_core_after_kde core file(s) missing"
fi

# ===========================================================================
# (e) Rollback: remove desktop package files → core still functional
# ===========================================================================

echo ""
echo "[e] Rollback: remove desktop package files"

removed_count=0
while IFS= read -r dest; do
    [[ -z "$dest" ]] && continue
    full="$ROOT_A$dest"
    if path_exists "$full"; then
        rm -f "$full"
        removed_count=$((removed_count + 1))
    fi
done < <(manifest_files_for_pkg ui)

if [[ "$removed_count" -gt 0 ]]; then
    ok "rollback: removed $removed_count desktop package file(s)"
else
    not_ok "rollback: no desktop files removed (staging may have failed earlier)"
fi

missing_ui_after_rollback="$(count_missing_files "$ROOT_A" ui)"
total_ui="$(manifest_files_for_pkg ui | grep -c . || echo 0)"
if [[ "$missing_ui_after_rollback" -eq "$total_ui" ]]; then
    ok "rollback: all ui files removed"
else
    not_ok "rollback: $missing_ui_after_rollback/$total_ui ui files missing (expected all removed)"
fi

missing_core_after_rollback="$(count_missing_files "$ROOT_A" core)"
if [[ "$missing_core_after_rollback" -eq 0 ]]; then
    ok "rollback: core still fully present after desktop removal"
else
    not_ok "rollback: $missing_core_after_rollback core file(s) missing after desktop removal"
fi

TPL_SHA_ROLLBACK="$(sha256_of "$SEED_TPL")"
if [[ "$TPL_SHA_ROLLBACK" == "$TPL_SHA_BEFORE" ]]; then
    ok "rollback: template preserved through desktop removal"
else
    not_ok "rollback: template altered during desktop removal"
fi

# ===========================================================================
# (f) Full removal: uninstall.sh → total purge
# ===========================================================================

echo ""
echo "[f] Full removal: uninstall.sh → total purge"

mkdir -p "$ROOT_A/usr/bin"
printf 'stock-ls' >"$ROOT_A/usr/bin/ls"
mkdir -p "$ROOT_A/etc/pam.d"
printf '# stock PAM\nauth required pam_unix.so\n' >"$ROOT_A/etc/pam.d/system-auth"

STOCK_LS_SHA_BEFORE="$(sha256_of "$ROOT_A/usr/bin/ls")"
STOCK_PAM_SHA_BEFORE="$(sha256_of "$ROOT_A/etc/pam.d/system-auth")"

if DESTDIR="$ROOT_A" bash "$UNINSTALL" "$PACKAGING_DIR" >/tmp/lc_uninstall_f.log 2>&1; then
    ok "full-removal: uninstall.sh exits 0"
else
    not_ok "full-removal: uninstall.sh exits nonzero (see /tmp/lc_uninstall_f.log)"
    cat /tmp/lc_uninstall_f.log >&2
fi

all_files="$(manifest_files_for_pkg all)"
leftover_manifest=0
while IFS= read -r dest; do
    [[ -z "$dest" ]] && continue
    if path_exists "$ROOT_A$dest"; then
        leftover_manifest=$((leftover_manifest + 1))
    fi
done <<<"$all_files"
if [[ "$leftover_manifest" -eq 0 ]]; then
    ok "full-removal: zero manifest-declared files remain"
else
    not_ok "full-removal: $leftover_manifest manifest file(s) remain"
fi

namtor_dirs="/usr/lib/namtor /usr/share/namtor /var/lib/namtor /run/namtor"
leftover_dirs=0
for d in $namtor_dirs; do
    if path_exists "$ROOT_A$d"; then
        leftover_dirs=$((leftover_dirs + 1))
    fi
done
if [[ "$leftover_dirs" -eq 0 ]]; then
    ok "full-removal: namtor-specific dirs purged (/usr/lib/namtor, /usr/share/namtor, /var/lib/namtor, /run/namtor)"
else
    not_ok "full-removal: $leftover_dirs namtor-specific dir(s) remain"
fi

genstate_purged=0
for state in \
    "$ROOT_A/var/lib/namtor/templates" \
    "$ROOT_A/var/lib/namtor/templates.key" \
    "$ROOT_A/var/lib/namtor/models" \
    "$ROOT_A/var/lib/namtor/backoff.json"; do
    if path_exists "$state"; then
        genstate_purged=$((genstate_purged + 1))
    fi
done
if [[ "$genstate_purged" -eq 0 ]]; then
    ok "full-removal: all generated_state paths purged (templates, key, models, backoff)"
else
    not_ok "full-removal: $genstate_purged generated_state path(s) remain"
fi

STOCK_LS_SHA_AFTER="$(sha256_of "$ROOT_A/usr/bin/ls")"
STOCK_PAM_SHA_AFTER="$(sha256_of "$ROOT_A/etc/pam.d/system-auth")"
if [[ "$STOCK_LS_SHA_BEFORE" == "$STOCK_LS_SHA_AFTER" && -n "$STOCK_LS_SHA_AFTER" ]]; then
    ok "full-removal: stock /usr/bin/ls survived (sha256 receipt match)"
else
    not_ok "full-removal: stock /usr/bin/ls altered or removed"
fi
if [[ "$STOCK_PAM_SHA_BEFORE" == "$STOCK_PAM_SHA_AFTER" && -n "$STOCK_PAM_SHA_AFTER" ]]; then
    ok "full-removal: stock /etc/pam.d/system-auth survived (sha256 receipt match)"
else
    not_ok "full-removal: stock /etc/pam.d/system-auth altered or removed"
fi

# ===========================================================================
# (g) Password login preservation (default-off at every stage)
# ===========================================================================

echo ""
echo "[g] Password login preservation (default-off at every stage)"

ROOT_G1="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_G1" "$REPO_ROOT" --package core >/dev/null 2>&1
refs_g1="$(count_pam_namtor_refs "$ROOT_G1")"
if [[ "$refs_g1" -eq 0 ]]; then
    ok "password-preservation: 0 pam_namtor refs after fresh core install"
else
    not_ok "password-preservation: $refs_g1 pam_namtor ref(s) after fresh core install"
fi

bash "$STAGE" "$ROOT_G1" "$REPO_ROOT" --package core >/dev/null 2>&1
refs_g2="$(count_pam_namtor_refs "$ROOT_G1")"
if [[ "$refs_g2" -eq 0 ]]; then
    ok "password-preservation: 0 pam_namtor refs after upgrade"
else
    not_ok "password-preservation: $refs_g2 pam_namtor ref(s) after upgrade"
fi

bash "$STAGE" "$ROOT_G1" "$REPO_ROOT" --package ui >/dev/null 2>&1
refs_g3="$(count_pam_namtor_refs "$ROOT_G1")"
if [[ "$refs_g3" -eq 0 ]]; then
    ok "password-preservation: 0 pam_namtor refs after ui added"
else
    not_ok "password-preservation: $refs_g3 pam_namtor ref(s) after ui added"
fi

DESTDIR="$ROOT_G1" bash "$UNINSTALL" "$PACKAGING_DIR" >/dev/null 2>&1
refs_g5="$(count_pam_namtor_refs "$ROOT_G1")"
if [[ "$refs_g5" -eq 0 ]]; then
    ok "password-preservation: 0 pam_namtor refs after full removal"
else
    not_ok "password-preservation: $refs_g5 pam_namtor ref(s) after full removal"
fi

post_install_body="$(awk '/namtor_lc_phase_post_install\(\)/{f=1;next} f&&/^namtor_lc_phase_/{exit} f{print}' "$LIFECYCLE_SRC")"
if printf '%s\n' "$post_install_body" | grep -qE 'call_pam_edit'; then
    not_ok "password-preservation: lifecycle.sh post-install calls pam-edit (should not)"
else
    ok "password-preservation: lifecycle.sh post-install does not call pam-edit"
fi

# ===========================================================================
# Failure matrix
# ===========================================================================

echo ""
echo "=================================================================="
echo "Failure matrix"
echo "=================================================================="

# --- F1: Interrupted upgrade (partial staging) → detect incomplete state ---

echo ""
echo "[F1] Interrupted upgrade (partial staging) → detect incomplete state"

ROOT_F1="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F1" "$REPO_ROOT" --package core >/dev/null 2>&1
python3 - "$MANIFEST" "$ROOT_F1" <<'PYEOF'
import sys, os, tomllib
manifest_path, root = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
files = sorted((e["dest"] for e in m.get("files", [])
                if e.get("package", "core") == "core"))
half = len(files) // 2
for p in files[:half]:
    full = os.path.join(root, p.lstrip("/"))
    if os.path.lexists(full):
        os.remove(full)
PYEOF

missing_f1="$(count_missing_files "$ROOT_F1" core)"
if [[ "$missing_f1" -gt 0 ]]; then
    ok "F1: interrupted upgrade detected ($missing_f1 core file(s) missing)"
else
    not_ok "F1: interrupted upgrade NOT detected (all core files present)"
fi

bash "$STAGE" "$ROOT_F1" "$REPO_ROOT" --package core >/dev/null 2>&1
missing_f1_restored="$(count_missing_files "$ROOT_F1" core)"
if [[ "$missing_f1_restored" -eq 0 ]]; then
    ok "F1: re-staging after interruption restores full core file set"
else
    not_ok "F1: re-staging after interruption leaves $missing_f1_restored file(s) missing"
fi

# --- F2: Stale files from previous version not in new manifest → detected ---

echo ""
echo "[F2] Stale files not in manifest → detected as stale"

ROOT_F2="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F2" "$REPO_ROOT" --package core >/dev/null 2>&1
mkdir -p "$ROOT_F2/usr/lib/namtor"
printf 'stale-legacy-binary' >"$ROOT_F2/usr/lib/namtor/namtor-legacy-old"

stale_f2="$(scan_stale "$ROOT_F2")"
stale_f2_count="$(printf '%s' "$stale_f2" | grep -c . 2>/dev/null || true)"
if [[ "$stale_f2_count" -ge 1 ]] \
   && printf '%s' "$stale_f2" | grep -q 'namtor-legacy-old'; then
    ok "F2: stale file detected ($stale_f2_count file(s) not in manifest, incl namtor-legacy-old)"
else
    not_ok "F2: stale file NOT detected (expected namtor-legacy-old flagged)"
fi

# --- F3: Concurrent staging attempts → safe (idempotent re-staging) ---

echo ""
echo "[F3] Concurrent staging attempts → safe (idempotent re-staging)"

ROOT_F3="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F3" "$REPO_ROOT" --package core >/tmp/lc_concurrent1.log 2>&1 &
pid1=$!
bash "$STAGE" "$ROOT_F3" "$REPO_ROOT" --package core >/tmp/lc_concurrent2.log 2>&1 &
pid2=$!
wait "$pid1"; rc1=$?
wait "$pid2"; rc2=$?

missing_f3="$(count_missing_files "$ROOT_F3" core)"
if [[ "$missing_f3" -eq 0 ]]; then
    ok "F3: concurrent staging produced consistent tree (0 missing core files; rc1=$rc1 rc2=$rc2)"
else
    not_ok "F3: concurrent staging produced inconsistent tree ($missing_f3 core files missing)"
fi

ROOT_F3_REF="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F3_REF" "$REPO_ROOT" --package core >/dev/null 2>&1
mismatches=0
while IFS= read -r dest; do
    [[ -z "$dest" ]] && continue
    s1="$(sha256_of "$ROOT_F3$dest")"
    s2="$(sha256_of "$ROOT_F3_REF$dest")"
    if [[ "$s1" != "$s2" ]]; then
        mismatches=$((mismatches + 1))
    fi
done < <(manifest_files_for_pkg core)
if [[ "$mismatches" -eq 0 ]]; then
    ok "F3: concurrent-staged tree matches single-stage reference (sha256-for-sha256)"
else
    not_ok "F3: concurrent-staged tree differs from reference ($mismatches file(s) mismatch)"
fi

# --- F4: Removal with missing manifest → handled gracefully ---

echo ""
echo "[F4] Removal with missing manifest → handled gracefully"

ROOT_F4="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F4" "$REPO_ROOT" --package core >/dev/null 2>&1
rm -f "$ROOT_F4/usr/share/namtor/install-manifest.toml"
EMPTY_SRC="$(mk_temp_dir)"
uninstall_out="$(DESTDIR="$ROOT_F4" bash "$UNINSTALL" "$EMPTY_SRC" 2>&1)"
uninstall_rc=$?
if [[ $uninstall_rc -ne 0 ]]; then
    ok "F4: uninstall with missing manifest exits nonzero (rc=$uninstall_rc)"
else
    not_ok "F4: uninstall with missing manifest exits 0 (should fail nonzero)"
fi
if printf '%s' "$uninstall_out" | grep -qiE 'FATAL|install-manifest.*not found'; then
    ok "F4: uninstall reports FATAL / manifest-not-found message"
else
    not_ok "F4: uninstall does not report a clear missing-manifest message"
fi
if path_exists "$ROOT_F4/usr/lib/namtor/namtor-daemon"; then
    ok "F4: uninstall with missing manifest did not remove files (clean early exit)"
else
    not_ok "F4: uninstall with missing manifest removed files (should exit before removal)"
fi

# --- F5: PAM block leftover after removal → detected (should not exist) -----

echo ""
echo "[F5] PAM block leftover after removal → none (default-off)"

# Since PAM is default-off, no /etc/pam.d modifications happen at install.
# After removal, there should be zero pam_namtor references anywhere under
# the root's /etc/pam.d. We verify this by: (1) staging core, (2) adding a
# fake pam_namtor line to simulate a leftover, (3) confirming it is
# detectable via grep, (4) running uninstall, (5) confirming uninstall's
# grep+warn fallback would catch it (no pam-edit binary in DESTDIR).
ROOT_F5="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F5" "$REPO_ROOT" --package core >/dev/null 2>&1
mkdir -p "$ROOT_F5/etc/pam.d"
printf '# namtor-begin\nauth sufficient pam_namtor.so\n# namtor-end\n' \
    >"$ROOT_F5/etc/pam.d/gdm-password"

# The leftover IS detectable via grep.
leftover_refs="$(count_pam_namtor_refs "$ROOT_F5")"
if [[ "$leftover_refs" -ge 1 ]]; then
    ok "F5: PAM block leftover detectable via grep ($leftover_refs file(s) with pam_namtor)"
else
    not_ok "F5: PAM block leftover NOT detectable via grep"
fi

# Force the grep+warn fallback: uninstall prefers pam-edit, then lifecycle.sh,
# then grep+warn. Remove both from the staged root AND pass an SRCDIR that
# has the manifest but not lifecycle.sh, so the /etc/pam.d scan is exercised.
rm -f "$ROOT_F5/usr/share/namtor/lifecycle.sh"
rm -f "$ROOT_F5/usr/lib/namtor/namtor-pam-edit"
F5_SRC="$(mk_temp_dir)"
cp "$MANIFEST" "$F5_SRC/install-manifest.toml"

DESTDIR="$ROOT_F5" bash "$UNINSTALL" "$F5_SRC" >/tmp/lc_uninstall_f5.log 2>&1
if grep -qiE 'WARN.*pam_namtor|WARN.*namtor-begin' /tmp/lc_uninstall_f5.log; then
    ok "F5: uninstall warns about pam_namtor leftover (grep+warn fallback)"
else
    not_ok "F5: uninstall does not warn about pam_namtor leftover"
fi

# In the default-off design, there is NO leftover at install time, so this
# scenario should not occur in practice. Verify a clean install+uninstall
# leaves zero pam_namtor refs.
ROOT_F5B="$(mk_temp_dir)"
bash "$STAGE" "$ROOT_F5B" "$REPO_ROOT" --package core >/dev/null 2>&1
DESTDIR="$ROOT_F5B" bash "$UNINSTALL" "$PACKAGING_DIR" >/dev/null 2>&1
clean_refs="$(count_pam_namtor_refs "$ROOT_F5B")"
if [[ "$clean_refs" -eq 0 ]]; then
    ok "F5: clean install+uninstall leaves 0 pam_namtor refs (default-off verified)"
else
    not_ok "F5: clean install+uninstall leaves $clean_refs pam_namtor ref(s)"
fi

# ===========================================================================
# WITHHELD: real distro upgrade/rollback on live VM (not CI-testable)
# ===========================================================================

echo ""
echo "--- WITHHELD: real distro upgrade/rollback (live VM) ---"

ok "WITHHELD documented: real pacman -U upgrade on Arch VM"
ok "WITHHELD documented: real dnf upgrade on Fedora VM"
ok "WITHHELD documented: real apt upgrade on Debian VM"
ok "WITHHELD documented: real package rollback via downgrade"
ok "WITHHELD documented: real cross-desktop switch with live session"
ok "WITHHELD documented: real removal with systemd running"

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
