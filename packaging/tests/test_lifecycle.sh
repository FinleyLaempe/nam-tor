#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 19: package-independent deterministic lifecycle helper.
#
# Acceptance criteria (from the task spec):
#   (a) Typed phases: pre-install, post-install, pre-upgrade, post-upgrade,
#       pre-remove, post-remove — all supported by packaging/lifecycle.sh.
#   (b) post-install verifies model layout/hashes, identities/dirs, socket
#       units, D-Bus service, PAM module. Emits a receipt. Does NOT edit PAM.
#   (c) Mandatory failures return nonzero: missing model, missing sysuser
#       config, missing systemd unit, missing D-Bus service, tampered model.
#   (d) Idempotent: running the same phase twice produces the same result.
#   (e) Fake-root testable: all operations work with --root <DESTDIR>.
#   (f) pre-upgrade calls `namtor-pam-edit snapshot`; post-upgrade calls
#       `namtor-pam-edit migrate`; pre-remove calls `disable-all`;
#       post-remove calls `purge`.
#   (g) Never calls nonexistent CLI (no `models ensure`, no `namtor-cli`).
#   (h) Never edits PAM on install (post-install does not touch /etc/pam.d).
#   (i) No PAM default target — the helper never names a specific PAM
#       service (e.g. gdm-password) as a default.
#   (j) All three distro hooks (Arch .install, Fedora .spec, Debian
#       postinst/prerm/postrm) invoke the shared lifecycle helper.
#
# Usage: bash packaging/tests/test_lifecycle.sh
# Exit code: 0 if all tests pass, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGING_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PACKAGING_DIR/.." && pwd)"

LIFECYCLE="$PACKAGING_DIR/lifecycle.sh"
ARCH_INSTALL="$PACKAGING_DIR/arch/namtor.install"
FEDORA_SPEC="$PACKAGING_DIR/fedora/namtor.spec"
DEB_POSTINST="$PACKAGING_DIR/debian/namtor.postinst"
DEB_PRERM="$PACKAGING_DIR/debian/namtor.prerm"
DEB_POSTRM="$PACKAGING_DIR/debian/namtor.postrm"

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

# Emit a fake model-manifest.toml at $1 whose sha256 entries match the
# given onnx files. Each model is fp32-only. Arguments: manifest_path
# then triples of (id, onnx_path).
build_fake_manifest() {
    local out="$1"; shift
    {
        echo "# SPDX-License-Identifier: GPL-3.0-only"
        echo "manifest_version = 1"
        while [[ $# -ge 2 ]]; do
            local id="$1" onnx="$2"; shift 2
            local h
            h="$(sha256sum "$onnx" | awk '{print $1}')"
            echo ""
            echo "[[models]]"
            echo "id = \"$id\""
            echo "licence_url = \"https://example.invalid/$id.txt\""
            echo "licence_sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\""
            echo "redistribution_policy = \"redistributable\""
            echo "target_arch = \"both\""
            echo "  [models.tensor_contract.input]"
            echo "  shape = [1, 1, 1, 1]"
            echo "  dtype = \"f32\""
            echo "  normalisation = \"none\""
            echo "  [models.tensor_contract.output]"
            echo "  shape = [1, 1]"
            echo "  dtype = \"f32\""
            echo "  [[models.variants]]"
            echo "  quant = \"fp32\""
            echo "  source_url = \"file:///var/lib/namtor/models/$id.onnx\""
            echo "  sha256 = \"$h\""
        done
    } >"$out"
}

# Build a fake pam-edit stub at $1 that records every invocation to the
# file at $2 and exits 0.
build_pam_edit_stub() {
    local stub="$1" log="$2"
    : >"$log"
    cat >"$stub" <<EOF
#!/bin/bash
echo "ARGS:\$*" >>"$log"
exit 0
EOF
    chmod +x "$stub"
}

# Build a complete fake root with every mandatory piece present.
# $1 = root, $2 = pam-edit-stub path, $3 = pam-edit-call-log path
build_complete_root() {
    local root="$1" stub="$2" log="$3"
    mkdir -p "$root/usr/lib/systemd/system"
    mkdir -p "$root/usr/lib/sysusers.d"
    mkdir -p "$root/usr/lib/tmpfiles.d"
    mkdir -p "$root/usr/share/dbus-1/system-services"
    mkdir -p "$root/usr/lib/security"
    mkdir -p "$root/usr/lib/namtor"
    mkdir -p "$root/usr/share/namtor"
    mkdir -p "$root/var/lib/namtor/models"
    mkdir -p "$root/var/lib/namtor/templates"
    mkdir -p "$root/var/lib/namtor/lifecycle"
    mkdir -p "$root/run/namtor"
    : >"$root/usr/lib/systemd/system/namtor-daemon.socket"
    : >"$root/usr/lib/systemd/system/namtor-daemon.service"
    : >"$root/usr/lib/sysusers.d/namtor.conf"
    : >"$root/usr/lib/tmpfiles.d/namtor.conf"
    : >"$root/usr/share/dbus-1/system-services/org.namtor.Manager.service"
    : >"$root/usr/lib/security/pam_namtor.so"
    : >"$root/usr/lib/namtor/namtor-daemon"

    # Three fake models, top-level + worker-dir symlink (matches Fedora spec).
    local m yunet sface mini
    yunet="$root/var/lib/namtor/models/yunet_2023mar.onnx"
    sface="$root/var/lib/namtor/models/sface_2021dec.onnx"
    mini="$root/var/lib/namtor/models/minifasnet_pad.onnx"
    printf 'yunet-fake-weights' >"$yunet"
    printf 'sface-fake-weights' >"$sface"
    printf 'mini-fake-weights' >"$mini"
    mkdir -p "$root/var/lib/namtor/models/yunet_2023mar" "$root/var/lib/namtor/models/sface_2021dec"
    ln -s ../yunet_2023mar.onnx "$root/var/lib/namtor/models/yunet_2023mar/fp32.onnx"
    ln -s ../sface_2021dec.onnx "$root/var/lib/namtor/models/sface_2021dec/fp32.onnx"

    build_fake_manifest "$root/usr/share/namtor/model-manifest.toml" \
        yunet_2023mar "$yunet" sface_2021dec "$sface" minifasnet_pad "$mini"

    build_pam_edit_stub "$stub" "$log"
}

# Run the lifecycle helper. Args passed through.
run_lc() {
    "$LIFECYCLE" "$@"
}

echo "== Task 19: shared lifecycle helper =="

# --- (a) helper exists + supports all six phases ----------------------------

echo "--- (a) helper supports all six typed phases ---"

if [[ -f "$LIFECYCLE" && -x "$LIFECYCLE" ]]; then
    ok "packaging/lifecycle.sh exists and is executable"
else
    not_ok "packaging/lifecycle.sh missing or not executable"
fi

# Unknown phase must fail nonzero.
if run_lc bogus-phase >/dev/null 2>&1; then
    not_ok "unknown phase returns 0 (should fail nonzero)"
else
    ok "unknown phase fails nonzero"
fi

# Each typed phase must be accepted (exit 0 on a complete root).
for phase in pre-install post-install pre-upgrade post-upgrade pre-remove post-remove; do
    root="$(mk_temp_dir)"
    stub="$root/usr/lib/namtor/namtor-pam-edit"
    log="$root/pam-edit-calls.log"
    build_complete_root "$root" "$stub" "$log"
    if run_lc "$phase" --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
        ok "phase '$phase' accepted on complete root"
    else
        not_ok "phase '$phase' fails on complete root (should succeed)"
    fi
done

# --- (b) post-install validates + emits receipt + no PAM editing ------------

echo "--- (b) post-install validates, emits receipt, no PAM edit ---"

root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    ok "post-install succeeds on complete root"
else
    not_ok "post-install fails on complete root (should succeed)"
fi

receipt="$root/var/lib/namtor/lifecycle/post-install.receipt"
if [[ -f "$receipt" ]]; then
    ok "post-install emits receipt"
else
    not_ok "post-install does not emit receipt"
fi

if [[ -f "$receipt" ]] && grep -q '^phase=post-install$' "$receipt" && grep -q '^status=ok$' "$receipt"; then
    ok "receipt has phase=post-install status=ok"
else
    not_ok "receipt missing phase/status fields"
fi

# post-install must NOT call pam-edit.
if [[ -f "$log" ]] && [[ ! -s "$log" ]]; then
    ok "post-install does not call pam-edit"
else
    not_ok "post-install called pam-edit (should not edit PAM on install)"
fi

# post-install must not write under /etc/pam.d.
if [[ -d "$root/etc/pam.d" ]]; then
    not_ok "post-install created /etc/pam.d (should not touch PAM)"
else
    ok "post-install does not create /etc/pam.d"
fi

# --- (c) mandatory failures return nonzero ----------------------------------

echo "--- (c) mandatory failures return nonzero ---"

# Missing model (models dir empty).
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
rm -rf "$root/var/lib/namtor/models"
mkdir -p "$root/var/lib/namtor/models"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with missing models (should fail nonzero)"
else
    ok "post-install fails nonzero when models missing"
fi

# Missing sysusers config.
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
rm -f "$root/usr/lib/sysusers.d/namtor.conf"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with missing sysusers (should fail nonzero)"
else
    ok "post-install fails nonzero when sysusers config missing"
fi

# Missing systemd unit.
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
rm -f "$root/usr/lib/systemd/system/namtor-daemon.service"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with missing service unit (should fail nonzero)"
else
    ok "post-install fails nonzero when service unit missing"
fi

# Missing D-Bus service.
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
rm -f "$root/usr/share/dbus-1/system-services/org.namtor.Manager.service"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with missing D-Bus service (should fail nonzero)"
else
    ok "post-install fails nonzero when D-Bus service missing"
fi

# Missing PAM module.
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
rm -f "$root/usr/lib/security/pam_namtor.so"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with missing PAM module (should fail nonzero)"
else
    ok "post-install fails nonzero when PAM module missing"
fi

# Tampered model (hash mismatch).
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
printf 'TAMPERED' >>"$root/var/lib/namtor/models/yunet_2023mar.onnx"
if run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1; then
    not_ok "post-install succeeds with tampered model (should fail nonzero)"
else
    ok "post-install fails nonzero when model hash mismatches"
fi

# --- (d) idempotent ---------------------------------------------------------

echo "--- (d) idempotent ---"

root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1
rc1=$?
r1="$(cat "$root/var/lib/namtor/lifecycle/post-install.receipt" 2>/dev/null | grep -v '^timestamp=')"
run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1
rc2=$?
r2="$(cat "$root/var/lib/namtor/lifecycle/post-install.receipt" 2>/dev/null | grep -v '^timestamp=')"
if [[ $rc1 -eq 0 && $rc2 -eq 0 && "$r1" == "$r2" ]]; then
    ok "post-install idempotent (same rc + same receipt minus timestamp)"
else
    not_ok "post-install not idempotent (rc1=$rc1 rc2=$rc2)"
fi

# --- (e) fake-root already exercised above; confirm --root is honoured ------

echo "--- (e) fake-root testable ---"

# post-install with --root must not touch the real /var/lib/namtor.
# We verify by confirming the receipt lands inside the fake root.
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc post-install --root "$root" --pam-edit "$stub" >/dev/null 2>&1
if [[ -f "$root/var/lib/namtor/lifecycle/post-install.receipt" ]]; then
    ok "receipt written under fake root (--root honoured)"
else
    not_ok "receipt not under fake root"
fi

# --- (f) pre/post upgrade + remove call pam-edit ----------------------------

echo "--- (f) upgrade/remove call pam-edit subprocess ---"

# pre-upgrade → snapshot
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc pre-upgrade --root "$root" --pam-edit "$stub" >/dev/null 2>&1
if grep -q 'ARGS:snapshot' "$log" 2>/dev/null; then
    ok "pre-upgrade calls 'namtor-pam-edit snapshot'"
else
    not_ok "pre-upgrade does not call snapshot"
fi

# post-upgrade → migrate
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc post-upgrade --root "$root" --pam-edit "$stub" >/dev/null 2>&1
if grep -q 'ARGS:migrate' "$log" 2>/dev/null; then
    ok "post-upgrade calls 'namtor-pam-edit migrate'"
else
    not_ok "post-upgrade does not call migrate"
fi

# pre-remove → disable-all
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc pre-remove --root "$root" --pam-edit "$stub" >/dev/null 2>&1
if grep -q 'ARGS:disable-all' "$log" 2>/dev/null; then
    ok "pre-remove calls 'namtor-pam-edit disable-all'"
else
    not_ok "pre-remove does not call disable-all"
fi

# post-remove → purge
root="$(mk_temp_dir)"
stub="$root/usr/lib/namtor/namtor-pam-edit"
log="$root/pam-edit-calls.log"
build_complete_root "$root" "$stub" "$log"
run_lc post-remove --root "$root" --pam-edit "$stub" >/dev/null 2>&1
if grep -q 'ARGS:purge' "$log" 2>/dev/null; then
    ok "post-remove calls 'namtor-pam-edit purge'"
else
    not_ok "post-remove does not call purge"
fi

# post-remove should also purge runtime dirs under the fake root.
if [[ ! -d "$root/var/lib/namtor/models" ]]; then
    ok "post-remove purges /var/lib/namtor under fake root"
else
    not_ok "post-remove leaves /var/lib/namtor/models behind"
fi

# --- (g) no nonexistent CLI -------------------------------------------------

echo "--- (g) no nonexistent CLI references ---"

if grep -qE 'models ensure|namtor-cli models' "$LIFECYCLE"; then
    not_ok "lifecycle.sh references nonexistent 'models ensure' or 'namtor-cli models'"
else
    ok "lifecycle.sh has no 'models ensure' / 'namtor-cli models' reference"
fi

# --- (h) no PAM editing in post-install phase body --------------------------

echo "--- (h) post-install phase never edits PAM ---"

# The lifecycle helper must not invoke pam-edit enable/disable/migrate in
# pre-install or post-install. Search the script source.
lc_body="$(sed -n '/^namtor_lc_phase_post_install/,/^}/p' "$LIFECYCLE")"
if printf '%s\n' "$lc_body" | grep -qE 'pam-edit.*enable|pam-edit.*disable|pam-edit.*migrate|pam-edit.*snapshot|pam-edit.*purge|/etc/pam\.d'; then
    not_ok "post-install phase body references PAM editing"
else
    ok "post-install phase body has no PAM editing"
fi

lc_pre="$(sed -n '/^namtor_lc_phase_pre_install/,/^}/p' "$LIFECYCLE")"
if printf '%s\n' "$lc_pre" | grep -qE 'pam-edit.*enable|pam-edit.*disable|pam-edit.*migrate|pam-edit.*purge|/etc/pam\.d'; then
    not_ok "pre-install phase body references PAM editing"
else
    ok "pre-install phase body has no PAM editing"
fi

# --- (i) no PAM default target ----------------------------------------------

echo "--- (i) no PAM default target ---"

# The helper must not name a specific PAM service as a default target.
if grep -qE 'gdm-password|system-auth|password-auth|common-auth' "$LIFECYCLE"; then
    not_ok "lifecycle.sh hardcodes a PAM default target"
else
    ok "lifecycle.sh has no hardcoded PAM default target"
fi

# --- (j) all distro hooks call the shared lifecycle helper ------------------

echo "--- (j) distro hooks invoke the shared lifecycle helper ---"

if grep -qE 'lifecycle\.sh (post-install|pre-install|post-upgrade|pre-upgrade|pre-remove|post-remove)' "$ARCH_INSTALL"; then
    ok "Arch .install invokes lifecycle.sh"
else
    not_ok "Arch .install does not invoke lifecycle.sh"
fi

if grep -qE 'lifecycle\.sh (post-install|pre-install|post-upgrade|pre-upgrade|pre-remove|post-remove)' "$FEDORA_SPEC"; then
    ok "Fedora .spec invokes lifecycle.sh"
else
    not_ok "Fedora .spec does not invoke lifecycle.sh"
fi

deb_any=0
for f in "$DEB_POSTINST" "$DEB_PRERM" "$DEB_POSTRM"; do
    if grep -qE 'lifecycle\.sh (post-install|pre-install|post-upgrade|pre-upgrade|pre-remove|post-remove)' "$f"; then
        deb_any=1
    fi
done
if [[ $deb_any -eq 1 ]]; then
    ok "Debian maintainer scripts invoke lifecycle.sh"
else
    not_ok "Debian maintainer scripts do not invoke lifecycle.sh"
fi

# --- distro hooks: no direct pam-edit calls (delegated to lifecycle) --------

echo "--- distro hooks delegate PAM editing to lifecycle helper ---"

# After delegation, the distro hooks must not call pam-edit directly for
# snapshot/migrate/disable-all/purge — those go through lifecycle.sh.
for f in "$ARCH_INSTALL" "$FEDORA_SPEC" "$DEB_POSTINST" "$DEB_PRERM" "$DEB_POSTRM"; do
    if grep -qE 'namtor-pam-edit (snapshot|migrate|disable-all|purge)' "$f"; then
        not_ok "$f calls pam-edit directly (should delegate to lifecycle.sh)"
    else
        ok "$f delegates PAM lifecycle to lifecycle.sh"
    fi
done

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
