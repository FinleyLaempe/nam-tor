#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Gate: the release and CI workflows must not contain known-broken shell
# idioms, assert results that never ran, or pin retired action majors.
#
# Every check below corresponds to a defect observed in a real release run —
# these are regression guards, not style preferences.
set -euo pipefail
cd "$(dirname "$0")/../.."

rc=0
R=.github/workflows/release.yml
C=.github/workflows/ci.yml

fail() { echo "::error::$*"; rc=1; }

# --- D5: the ldd link check must be able to fail the step -------------------
# Two plausible-looking forms are silently useless:
#   ldd X | grep -v 'not found'   -> `grep -v` exits 0 as soon as ANY line
#                                    does not match, which is always true for
#                                    a normal ldd listing.
#   ! ldd X | grep 'not found'    -> `!` puts the pipeline in a tested
#                                    context, which suppresses `set -e` for
#                                    the whole statement, so any command after
#                                    it swallows the failure. (Verified: a
#                                    step with a trailing command exits 0 even
#                                    when ldd reports "not found".)
# The only reliable form is an explicit `if ...; then ... exit 1; fi`.
while IFS= read -r line; do
    case "$line" in
        *"if ldd "*"| grep 'not found'; then"*) ;;
        *) fail "ldd link check cannot fail the step: ${line#*:}" ;;
    esac
done < <(grep -n 'ldd .*pam_namtor\.so' "$R" || true)
if ! grep -q "if ldd .*pam_namtor\.so | grep 'not found'; then" "$R"; then
    fail "no working ldd link check found in $R — this gate would be vacuous"
fi

# --- D8: workflow commands must use a real command name ---------------------
# `echo "::uninstall verified clean"` is not GitHub Actions syntax; the runner
# prints it literally and the intended annotation never appears.
while IFS= read -r line; do
    cmd=$(printf '%s' "$line" | sed -n 's/.*echo "::\([a-z-]*\).*/\1/p')
    case "$cmd" in
        error|warning|notice|debug|group|endgroup|add-mask|add-matcher|stop-commands|set-output|save-state) ;;
        *) fail "invalid workflow command in $R: ${line#*echo }" ;;
    esac
done < <(grep -n 'echo "::' "$R" || true)

# --- D4: the Arch uninstall must remove every split package -----------------
# The install step installs all three split packages, and the desktop ones
# declare depends=("namtor=$pkgver"). Removing only `namtor` makes pacman
# abort the transaction on the broken dependency. The expected package list
# is read from the PKGBUILD's pkgname array rather than hardcoded, so it stays
# correct when the split packages are renamed.
pkgnames=$(awk '/^pkgname=\(/{f=1;next} f&&/^\)/{f=0} f{gsub(/[ \t]/,"");if($0!="")print}' \
    packaging/arch/PKGBUILD)
if [ -z "$pkgnames" ]; then
    fail "could not parse pkgname=() from packaging/arch/PKGBUILD"
elif ! grep -q 'pacman -Rns --noconfirm' "$R"; then
    fail "no 'pacman -Rns --noconfirm' uninstall step found in $R"
else
    # Join shell line-continuations first, so the package list is found even
    # when it is wrapped across lines.
    rm_args=" $(sed -e ':a' -e '/\\$/N; s/\\\n//; ta' "$R" \
        | grep 'pacman -Rns --noconfirm' \
        | sed 's/.*--noconfirm//; s/||.*//' | tr -s '[:space:]' ' ') "
    for p in $pkgnames; do
        case "$rm_args" in
            *" $p "*) ;;
            *) fail "Arch uninstall does not remove split package '$p'; pacman aborts on the broken dependency" ;;
        esac
    done
fi

# --- D7: a plain-scalar `run:` must not end in a trailing backslash ---------
# YAML folds the newline in a plain scalar to a single space, so
#   run: cmd || \
#        echo "..."
# becomes `cmd || \ echo "..."`. The backslash escapes the SPACE, so the
# command word becomes " echo" and the shell exits 127. Use `run: |`.
if grep -nE '^[[:space:]]*run:[[:space:]]*[^|>].*\\$' "$R" "$C"; then
    fail "plain-scalar 'run:' ending in a backslash — YAML folds it to '\\ ' (exit 127); use 'run: |'"
fi

# --- D6: published evidence must not assert cells that never ran ------------
# The evidence job hardcoded pass results for fedora40 and debian12, which are
# not in the build matrix. A release that ships that JSON is asserting test
# results for platforms nobody tested.
matrix_distros=$(grep -oE '^[[:space:]]*- distro:[[:space:]]*[a-z0-9]+' "$R" \
    | awk '{print $NF}' | sort -u)
evidence_distros=$(grep -oE '^[[:space:]]*\("[a-z0-9]+",[[:space:]]*"[a-z0-9_]+"\),' "$R" \
    | sed -E 's/^[^"]*"([a-z0-9]+)".*/\1/' | sort -u)
for d in $evidence_distros; do
    if ! printf '%s\n' $matrix_distros | grep -qx "$d"; then
        fail "evidence job publishes results for '$d', which is not in the build matrix"
    fi
done
if [ -z "$evidence_distros" ]; then
    fail "could not parse the evidence cell list in $R — this gate would be vacuous"
fi

# --- P3: retired action majors ---------------------------------------------
# checkout@v4 and setup-python@v5 run on the retired Node 20 runtime and are
# already emitting deprecation warnings.
for w in "$R" "$C"; do
    if grep -nE 'actions/checkout@v[1-4]\b' "$w"; then
        fail "$w pins a retired actions/checkout major; use @v5"
    fi
    if grep -nE 'actions/setup-python@v[1-5]\b' "$w"; then
        fail "$w pins a retired actions/setup-python major; use @v6"
    fi
done

exit $rc
