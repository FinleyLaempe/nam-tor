#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for Task 13: keep every PAM service disabled at package
# installation.
#
# Acceptance criteria (from the task spec):
#   (a) Fresh install has ZERO Nam-tor PAM blocks — no install hook calls
#       pam-install.sh or edits /etc/pam.d.
#   (b) Socket/D-Bus/model validation succeeds when all mandatory pieces
#       are present.
#   (c) Mandatory setup failure (missing model, missing sysuser, missing
#       service) returns nonzero.
#   (d) Documentation says: open Settings, enroll, choose targets — no
#       CLI/root shell setup.
#   (e) pam-install.sh is NOT called during install (it remains available
#       for manual/explicit use only).
#
# Usage: bash packaging/tests/test_install_default_off.sh
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

# Extract a section from the Fedora spec between two markers (exclusive).
# Usage: extract_spec_section spec_file section_name
extract_spec_section() {
    local file="$1" section="$2"
    awk -v sec="$section" '
        $0 ~ "^%" sec "($| )" { in_sec = 1; next }
        in_sec && $0 ~ "^%[a-z]" { exit }
        in_sec { print }
    ' "$file"
}

# --- install hook files under test ------------------------------------------
ARCH_INSTALL="$PACKAGING_DIR/arch/namtor.install"
FEDORA_SPEC="$PACKAGING_DIR/fedora/namtor.spec"
DEB_POSTINST="$PACKAGING_DIR/debian/namtor.postinst"
README="$PACKAGING_DIR/README.md"
PAM_INSTALL_SH="$PACKAGING_DIR/pam-install.sh"

echo "== Task 13: install default-off =="

# --- (a) + (e) No PAM editing in install hooks ------------------------------
#
# The install hooks (Arch post_install, Fedora %post, Debian postinst
# configure) must NOT call pam-install.sh, must NOT reference pam_namtor.so
# insertion, and must NOT write to /etc/pam.d. pam-install.sh itself remains
# on disk for manual use but is never invoked during install.

echo "--- (a/e) no PAM editing in install hooks ---"

# Arch: post_install body (between 'post_install()' and the next 'post_').
arch_post_install_body="$(awk '/^post_install\(\)/{f=1;next} f&&/^[a-z_]+\(\)/{exit} f{print}' "$ARCH_INSTALL")"
if printf '%s\n' "$arch_post_install_body" | grep_all -E 'pam-install\.sh|pam_namtor|/etc/pam\.d'; then
    not_ok "Arch post_install references PAM editing"
else
    ok "Arch post_install has no PAM editing"
fi

# Fedora: %post body.
fedora_post_body="$(extract_spec_section "$FEDORA_SPEC" post)"
if printf '%s\n' "$fedora_post_body" | grep_all -E 'pam-install\.sh|pam_namtor|/etc/pam\.d'; then
    not_ok "Fedora %post references PAM editing"
else
    ok "Fedora %post has no PAM editing"
fi

# Debian: configure body.
deb_configure_body="$(awk '/case "\$1" in/{f=1} f&&/configure\)/{c=1} c&&/;;/{if(f&&c){exit}} c{print}' "$DEB_POSTINST")"
if printf '%s\n' "$deb_configure_body" | grep_all -E 'pam-install\.sh|pam_namtor|/etc/pam\.d'; then
    not_ok "Debian postinst configure references PAM editing"
else
    ok "Debian postinst configure has no PAM editing"
fi

# --- no `models ensure` in install hooks ------------------------------------
#
# `namtor-cli models ensure` does not exist (models are bundled in the
# package). Install hooks must not reference it.

echo "--- no 'models ensure' in install hooks ---"

for hook in "$ARCH_INSTALL" "$FEDORA_SPEC" "$DEB_POSTINST"; do
    if grep -qE 'models ensure' "$hook"; then
        not_ok "$hook references nonexistent 'models ensure'"
    else
        ok "$hook has no 'models ensure'"
    fi
done

# --- no GDM-user assumption in install hooks --------------------------------
#
# The install hooks must NOT add display-manager users (gdm/sddm/etc.) to
# namtor-auth. That group membership is added when the user enables a PAM
# service via Settings (D-Bus), not at install time.

echo "--- no DM-group assumption in install hooks ---"

for hook in "$ARCH_INSTALL" "$FEDORA_SPEC" "$DEB_POSTINST"; do
    if grep -qE 'gpasswd.*namtor-auth|usermod.*namtor-auth' "$hook"; then
        not_ok "$hook assumes DM group membership at install"
    else
        ok "$hook has no DM-group assumption"
    fi
done

# --- no unverified success messages -----------------------------------------
#
# The hooks must not claim "installed and running" or similar before
# validation confirms the system is operational.

echo "--- no unverified success messages ---"

if printf '%s\n' "$arch_post_install_body" | grep_all -i 'installed and running'; then
    not_ok "Arch post_install has unverified 'installed and running' message"
else
    ok "Arch post_install has no unverified success message"
fi

# --- (b)(c) Validation function ---------------------------------------------
#
# Task 19 centralised validation in packaging/lifecycle.sh. The helper's
# post-install phase validates the mandatory payload (models, sysuser
# config, units, D-Bus service, PAM module, runtime dirs) and is DESTDIR-
# aware via --root. We exercise it directly against fake roots.

echo "--- (b/c) validation function ---"

LIFECYCLE="$PACKAGING_DIR/lifecycle.sh"

# Build a complete fake root (all mandatory pieces present).
build_complete_root() {
    local root="$1"
    mkdir -p "$root/var/lib/namtor/models/yunet_2023mar"
    mkdir -p "$root/var/lib/namtor/models/sface_2021dec"
    mkdir -p "$root/var/lib/namtor/templates"
    mkdir -p "$root/run/namtor"
    mkdir -p "$root/usr/lib/systemd/system"
    mkdir -p "$root/usr/lib/sysusers.d"
    mkdir -p "$root/usr/lib/tmpfiles.d"
    mkdir -p "$root/usr/share/dbus-1/system-services"
    mkdir -p "$root/usr/lib/security"
    mkdir -p "$root/usr/lib/namtor"
    : >"$root/var/lib/namtor/models/yunet_2023mar/fp32.onnx"
    : >"$root/var/lib/namtor/models/sface_2021dec/fp32.onnx"
    : >"$root/usr/lib/systemd/system/namtor-daemon.socket"
    : >"$root/usr/lib/systemd/system/namtor-daemon.service"
    : >"$root/usr/lib/sysusers.d/namtor.conf"
    : >"$root/usr/lib/tmpfiles.d/namtor.conf"
    : >"$root/usr/share/dbus-1/system-services/org.namtor.Manager.service"
    : >"$root/usr/lib/security/pam_namtor.so"
    : >"$root/usr/lib/namtor/namtor-daemon"
}

# validate_root <root> — runs the lifecycle post-install validation (exit
# code only, output silenced). Returns 0 if the payload is complete.
validate_root() {
    "$LIFECYCLE" post-install --root "$1" --pam-edit /nonexistent >/dev/null 2>&1
}

# (b) Validation succeeds when all present.
root_ok="$(mk_temp_dir)"
build_complete_root "$root_ok"
if validate_root "$root_ok"; then
    ok "validation succeeds when all mandatory pieces present"
else
    not_ok "validation fails when all mandatory pieces present (should succeed)"
fi

# (c) Validation fails nonzero when model missing.
root_no_model="$(mk_temp_dir)"
build_complete_root "$root_no_model"
rm -rf "$root_no_model/var/lib/namtor/models"
mkdir -p "$root_no_model/var/lib/namtor/models"
if validate_root "$root_no_model"; then
    not_ok "validation succeeds when models missing (should fail nonzero)"
else
    ok "validation fails nonzero when models missing"
fi

# (c) Validation fails nonzero when sysuser config missing.
root_no_user="$(mk_temp_dir)"
build_complete_root "$root_no_user"
rm -f "$root_no_user/usr/lib/sysusers.d/namtor.conf"
if validate_root "$root_no_user"; then
    not_ok "validation succeeds when sysusers.d missing (should fail nonzero)"
else
    ok "validation fails nonzero when sysusers.d missing"
fi

# (c) Validation fails nonzero when systemd unit missing.
root_no_unit="$(mk_temp_dir)"
build_complete_root "$root_no_unit"
rm -f "$root_no_unit/usr/lib/systemd/system/namtor-daemon.socket"
if validate_root "$root_no_unit"; then
    not_ok "validation succeeds when socket unit missing (should fail nonzero)"
else
    ok "validation fails nonzero when socket unit missing"
fi

# (c) Validation fails nonzero when D-Bus service missing.
root_no_dbus="$(mk_temp_dir)"
build_complete_root "$root_no_dbus"
rm -f "$root_no_dbus/usr/share/dbus-1/system-services/org.namtor.Manager.service"
if validate_root "$root_no_dbus"; then
    not_ok "validation succeeds when D-Bus service missing (should fail nonzero)"
else
    ok "validation fails nonzero when D-Bus service missing"
fi

# (c) Validation fails nonzero when PAM module missing.
root_no_pam="$(mk_temp_dir)"
build_complete_root "$root_no_pam"
rm -f "$root_no_pam/usr/lib/security/pam_namtor.so"
if validate_root "$root_no_pam"; then
    not_ok "validation succeeds when pam_namtor.so missing (should fail nonzero)"
else
    ok "validation fails nonzero when pam_namtor.so missing"
fi

# --- (d) Documentation flow -------------------------------------------------

echo "--- (d) README documents Settings flow ---"

if grep -qi 'open Settings\|Settings app' "$README"; then
    ok "README references Settings app"
else
    not_ok "README does not reference Settings app"
fi

if grep -qi 'enroll' "$README"; then
    ok "README references enrollment"
else
    not_ok "README does not reference enrollment"
fi

# The README install section must not instruct the user to run
# pam-install.sh as part of the standard setup flow. (It may still document
# the script as a manual/explicit-only tool.)
readme_install_section="$(awk '/^## Install/{f=1} f&&/^## /{if(NR>1&&$0!~/^## Install/)exit} f{print}' "$README")"
if printf '%s\n' "$readme_install_section" | grep_all -E 'sudo.*pam-install\.sh'; then
    not_ok "README install section instructs pam-install.sh as setup step"
else
    ok "README install section does not instruct pam-install.sh"
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
