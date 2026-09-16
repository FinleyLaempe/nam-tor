#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 23: manifest-complete generic staging + uninstall.
#
# Acceptance criteria (from the task spec):
#   (a) Stage all → uninstall → zero Nam-tor files remain in DESTDIR.
#   (b) No stock files touched (unrelated files under shared dirs survive).
#   (c) Interrupted uninstall resumes (idempotent — re-run completes cleanly).
#   (d) --dry-run does not remove anything.
#   (e) PAM disabled BEFORE the PAM module binary is deleted.
#   (f) Uninstall reads install-manifest.toml (no hardcoded path lists).
#   (g) Generated state purged (templates, templates.key, models, backoff.json).
#   (h) Runtime dirs /var/lib/namtor + /run/namtor removed.
#
# Usage: bash packaging/tests/test_uninstall_closure.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"

UNINSTALL="$PACKAGING_DIR/uninstall.sh"
STAGE="$PACKAGING_DIR/stage.sh"
MANIFEST="$PACKAGING_DIR/install-manifest.toml"

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

# --- build a fake SRCDIR with stub files for every manifest src path --------

build_fake_srcdir() {
    local srcdir="$1"
    # Create stub files for every src declared in the manifest.
    # Parse the manifest with python3 to get all src paths.
    python3 - "$MANIFEST" "$srcdir" <<'PYEOF'
import sys, os, tomllib, shutil
manifest_path, srcdir = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
for entry in m.get("files", []):
    src = entry["src"]
    dest = os.path.join(srcdir, src)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    if entry.get("symlink", False):
        # Create a dummy symlink target.
        target = os.path.join(srcdir, src + ".target")
        if not os.path.exists(target):
            with open(target, "w") as tf:
                tf.write("stub-target\n")
        if not os.path.lexists(dest):
            os.symlink(src + ".target", dest)
    else:
        with open(dest, "w") as df:
            df.write("stub:" + entry["dest"] + "\n")
PYEOF
    # build_fake_srcdir overwrites srcdir/packaging/install-manifest.toml with
    # a non-TOML stub above. stage.sh reads that path as the manifest source.
    # Restore the real manifest so stage.sh can parse it.
    mkdir -p "$srcdir/packaging"
    cp "$MANIFEST" "$srcdir/packaging/install-manifest.toml"
}

# --- create generated_state paths + runtime cruft in DESTDIR ----------------

create_runtime_state() {
    local dest="$1"
    mkdir -p "$dest/var/lib/namtor/templates"
    mkdir -p "$dest/var/lib/namtor/models"
    printf 'fake-key' >"$dest/var/lib/namtor/templates.key"
    printf '{}' >"$dest/var/lib/namtor/backoff.json"
    printf 'fake-template' >"$dest/var/lib/namtor/templates/uid_1000.bin"
    printf 'fake-model' >"$dest/var/lib/namtor/models/yunet.onnx"
    mkdir -p "$dest/var/lib/namtor/lifecycle"
    printf 'phase=post-install\nstatus=ok\n' >"$dest/var/lib/namtor/lifecycle/post-install.receipt"
    mkdir -p "$dest/run/namtor"
    printf 'socket' >"$dest/run/namtor/auth.sock"
}

# --- create "stock" files that must NOT be removed --------------------------

create_stock_files() {
    local dest="$1"
    # Stock binary in /usr/bin (not namtor-cli).
    mkdir -p "$dest/usr/bin"
    printf 'stock-ls' >"$dest/usr/bin/ls"
    # Stock PAM file (not touched by uninstall — no pam_namtor line).
    mkdir -p "$dest/etc/pam.d"
    printf '# stock PAM\nauth required pam_unix.so\n' >"$dest/etc/pam.d/system-auth"
    # Stock file in a shared dir (must survive rmdir of the shared dir).
    mkdir -p "$dest/usr/share/applications"
    printf 'stock-desktop' >"$dest/usr/share/applications/other-app.desktop"
    mkdir -p "$dest/usr/share/metainfo"
    printf 'stock-metainfo' >"$dest/usr/share/metainfo/other.metainfo.xml"
    mkdir -p "$dest/usr/lib/systemd/system"
    printf 'stock-unit' >"$dest/usr/lib/systemd/system/other.service"
}

# --- check if a path exists (file or dir) -----------------------------------

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

# --- collect all Nam-tor-owned paths from the manifest ----------------------

collect_namtor_paths() {
    python3 - "$MANIFEST" <<'PYEOF'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    m = tomllib.load(f)
paths = set()
for d in m.get("dirs", []):
    paths.add(d["path"])
for f in m.get("files", []):
    paths.add(f["dest"])
for g in m.get("generated_state", []):
    paths.add(g["path"])
# Also the runtime catch-all dirs.
paths.add("/var/lib/namtor")
paths.add("/run/namtor")
for p in sorted(paths):
    print(p)
PYEOF
}

echo "== Task 23: manifest-complete uninstall closure =="

# --- (f) uninstall.sh reads the manifest (no hardcoded lists) ---------------

echo "--- (f) uninstall.sh reads install-manifest.toml ---"

if grep -q 'tomllib' "$UNINSTALL"; then
    ok "uninstall.sh parses manifest via tomllib"
else
    not_ok "uninstall.sh does not use tomllib (no manifest parsing)"
fi

if grep -q 'install-manifest.toml' "$UNINSTALL"; then
    ok "uninstall.sh references install-manifest.toml"
else
    not_ok "uninstall.sh does not reference install-manifest.toml"
fi

# Verify uninstall.sh does NOT contain hardcoded rm of specific namtor paths
# (the old script had `rm -f /usr/lib/namtor/namtor-daemon` etc.).
if grep -qE 'rm -f /usr/lib/namtor/namtor-daemon|rm -f /usr/bin/namtor-cli|rm -f /usr/lib/security/pam_namtor' "$UNINSTALL"; then
    not_ok "uninstall.sh has hardcoded rm of specific namtor files (should use manifest)"
else
    ok "uninstall.sh has no hardcoded rm of specific namtor files"
fi

# --- (e) PAM disabled before module deletion --------------------------------

echo "--- (e) PAM disabled before PAM module deleted ---"

# Build a fake root: stage, add runtime state, add stock files, add a
# pam-edit stub that logs its call.
dest="$(mk_temp_dir)"
srcdir="$(mk_temp_dir)"
build_fake_srcdir "$srcdir"
bash "$STAGE" "$dest" "$srcdir" --package all >/dev/null 2>&1
create_runtime_state "$dest"
create_stock_files "$dest"

# Create a pam-edit stub that logs its invocation + checks if the module
# still exists at call time.
mkdir -p "$dest/usr/lib/namtor"
pam_edit_stub="$dest/usr/lib/namtor/namtor-pam-edit"
pam_edit_log="$dest/pam-edit-call.log"
cat >"$pam_edit_stub" <<'PAMSTUB'
#!/bin/bash
echo "CALLED:$*" >>"$DESTDIR/pam-edit-call.log"
# Record whether pam_namtor.so still exists at call time.
if [[ -e "$DESTDIR/usr/lib/security/pam_namtor.so" ]]; then
    echo "MODULE_EXISTS_AT_CALL=yes" >>"$DESTDIR/pam-edit-call.log"
else
    echo "MODULE_EXISTS_AT_CALL=no" >>"$DESTDIR/pam-edit-call.log"
fi
exit 0
PAMSTUB
chmod +x "$pam_edit_stub"

# Run uninstall with DESTDIR.
DESTDIR="$dest" bash "$UNINSTALL" "$PACKAGING_DIR" >/dev/null 2>&1

if [[ -f "$pam_edit_log" ]]; then
    if grep -q 'disable-all' "$pam_edit_log"; then
        ok "pam-edit disable-all was called"
    else
        not_ok "pam-edit disable-all was NOT called"
    fi
    if grep -q 'MODULE_EXISTS_AT_CALL=yes' "$pam_edit_log"; then
        ok "PAM module existed when disable-all was called (disabled before deletion)"
    else
        not_ok "PAM module was deleted BEFORE disable-all was called"
    fi
else
    not_ok "pam-edit was never called (no log file)"
fi

# Remove the pam-edit stub — it is test scaffolding not in the manifest and
# would keep /usr/lib/namtor non-empty, masking the dir-removal check.
rm -f "$pam_edit_stub"
rmdir "$dest/usr/lib/namtor" 2>/dev/null || true

# --- (a) stage all → uninstall → zero Nam-tor files remain ------------------

echo "--- (a) zero Nam-tor files remain after uninstall ---"

# Use the same dest from above (already uninstalled).
namtor_files="$(python3 - "$MANIFEST" <<'PYEOF'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    m = tomllib.load(f)
for entry in m.get("files", []):
    print(entry["dest"])
PYEOF
)"

leftover=0
while IFS= read -r p; do
    [[ -z "$p" ]] && continue
    full="$dest$p"
    if path_exists "$full"; then
        leftover=$((leftover + 1))
        echo "    leftover file: $full"
    fi
done <<<"$namtor_files"

# Nam-tor-specific dirs must be gone. Shared dirs (e.g. /usr/share/applications)
# correctly survive rmdir when they contain stock files — not a leftover.
namtor_specific_dirs="/usr/lib/namtor /usr/share/namtor /var/lib/namtor /var/lib/namtor/models /var/lib/namtor/templates /usr/lib/security /run/namtor"
for d in $namtor_specific_dirs; do
    full="$dest$d"
    if path_exists "$full"; then
        leftover=$((leftover + 1))
        echo "    leftover dir: $full"
    fi
done

if [[ $leftover -eq 0 ]]; then
    ok "zero Nam-tor-owned paths remain after uninstall"
else
    not_ok "$leftover Nam-tor-owned path(s) remain after uninstall"
fi

# --- (b) no stock files touched ---------------------------------------------

echo "--- (b) stock files untouched ---"

stock_ok=1
for stock_file in \
    "$dest/usr/bin/ls" \
    "$dest/etc/pam.d/system-auth" \
    "$dest/usr/share/applications/other-app.desktop" \
    "$dest/usr/share/metainfo/other.metainfo.xml" \
    "$dest/usr/lib/systemd/system/other.service"; do
    if ! path_exists "$stock_file"; then
        stock_ok=0
        echo "    missing stock file: $stock_file"
    fi
done

# Also verify shared dirs survive (they contain stock files).
for stock_dir in \
    "$dest/usr/share/applications" \
    "$dest/usr/share/metainfo" \
    "$dest/usr/lib/systemd/system"; do
    if [[ ! -d "$stock_dir" ]]; then
        stock_ok=0
        echo "    shared dir removed: $stock_dir"
    fi
done

if [[ $stock_ok -eq 1 ]]; then
    ok "all stock files + shared dirs untouched"
else
    not_ok "stock files or shared dirs were removed"
fi

# --- (c) interrupted uninstall resumes (idempotent) -------------------------

echo "--- (c) interrupted uninstall resumes (idempotent) ---"

# Fresh stage + runtime state.
dest2="$(mk_temp_dir)"
build_fake_srcdir "$srcdir"
bash "$STAGE" "$dest2" "$srcdir" --package all >/dev/null 2>&1
create_runtime_state "$dest2"
create_stock_files "$dest2"

# Simulate interruption: delete half the files manually before running uninstall.
python3 - "$MANIFEST" "$dest2" <<'PYEOF'
import sys, os, tomllib
manifest_path, dest = sys.argv[1], sys.argv[2]
with open(manifest_path, "rb") as f:
    m = tomllib.load(f)
files = sorted((f["dest"] for f in m.get("files", [])))
# Delete the first half to simulate a partial uninstall.
half = len(files) // 2
for p in files[:half]:
    full = os.path.join(dest, p.lstrip("/"))
    if os.path.lexists(full):
        os.remove(full)
PYEOF

# Now run uninstall — it must complete cleanly (exit 0) and remove the rest.
if DESTDIR="$dest2" bash "$UNINSTALL" "$PACKAGING_DIR" >/dev/null 2>&1; then
    ok "uninstall succeeds after partial removal (exit 0)"
else
    not_ok "uninstall fails after partial removal (should exit 0)"
fi

# Verify no Nam-tor files or specific dirs remain.
leftover2=0
while IFS= read -r p; do
    [[ -z "$p" ]] && continue
    if path_exists "$dest2$p"; then
        leftover2=$((leftover2 + 1))
    fi
done <<<"$namtor_files"
for d in $namtor_specific_dirs; do
    if path_exists "$dest2$d"; then
        leftover2=$((leftover2 + 1))
    fi
done
if [[ $leftover2 -eq 0 ]]; then
    ok "zero Nam-tor paths remain after resumed uninstall"
else
    not_ok "$leftover2 Nam-tor path(s) remain after resumed uninstall"
fi

# --- (d) --dry-run removes nothing ------------------------------------------

echo "--- (d) --dry-run removes nothing ---"

dest3="$(mk_temp_dir)"
build_fake_srcdir "$srcdir"
bash "$STAGE" "$dest3" "$srcdir" --package all >/dev/null 2>&1
create_runtime_state "$dest3"
create_stock_files "$dest3"

# Count files before dry-run.
before_count=$(find "$dest3" -type f 2>/dev/null | wc -l)

# Run dry-run.
DESTDIR="$dest3" bash "$UNINSTALL" --dry-run "$PACKAGING_DIR" >/dev/null 2>&1

# Count files after dry-run.
after_count=$(find "$dest3" -type f 2>/dev/null | wc -l)

if [[ "$before_count" -eq "$after_count" ]]; then
    ok "dry-run removed nothing (file count unchanged: $before_count)"
else
    not_ok "dry-run changed file count ($before_count → $after_count)"
fi

# Spot-check: pam_namtor.so still exists after dry-run.
if path_exists "$dest3/usr/lib/security/pam_namtor.so"; then
    ok "pam_namtor.so still exists after dry-run"
else
    not_ok "pam_namtor.so was removed by dry-run (should not be)"
fi

# --- (g) generated state purged ---------------------------------------------

echo "--- (g) generated state purged ---"

# The dest3 dry-run root still has generated state. Run real uninstall.
DESTDIR="$dest3" bash "$UNINSTALL" "$PACKAGING_DIR" >/dev/null 2>&1

for state_path in \
    "$dest3/var/lib/namtor/templates" \
    "$dest3/var/lib/namtor/templates.key" \
    "$dest3/var/lib/namtor/models" \
    "$dest3/var/lib/namtor/backoff.json"; do
    if path_exists "$state_path"; then
        not_ok "generated state not purged: $state_path"
    else
        ok "generated state purged: $state_path"
    fi
done

# --- (h) runtime dirs removed -----------------------------------------------

echo "--- (h) runtime dirs removed ---"

if path_exists "$dest3/var/lib/namtor"; then
    not_ok "/var/lib/namtor not removed"
else
    ok "/var/lib/namtor removed"
fi

if path_exists "$dest3/run/namtor"; then
    not_ok "/run/namtor not removed"
else
    ok "/run/namtor removed"
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
