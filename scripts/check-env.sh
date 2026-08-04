#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Pre-flight environment check for Nam-tor development.
#
# Run this before starting M0 or M1. It reports what is present, what is
# missing, and — importantly — which gaps block which milestone, so nothing is
# discovered halfway through a task.
set -uo pipefail

pass=0; warn=0; fail=0

ok()   { printf "  \033[32mOK\033[0m    %-26s %s\n" "$1" "${2:-}"; pass=$((pass+1)); }
note() { printf "  \033[33mNOTE\033[0m  %-26s %s\n" "$1" "${2:-}"; warn=$((warn+1)); }
bad()  { printf "  \033[31mMISS\033[0m  %-26s %s\n" "$1" "${2:-}"; fail=$((fail+1)); }

have() { command -v "$1" >/dev/null 2>&1; }

echo "=== Nam-tor environment check ==="
echo

echo "-- toolchain (required: M0) --"
if have cargo; then ok "cargo" "$(cargo --version 2>/dev/null)"; else bad "cargo" "install rust"; fi
if have rustc; then ok "rustc" "$(rustc --version 2>/dev/null)"; else bad "rustc" "install rust"; fi
if have rustfmt; then ok "rustfmt"; else note "rustfmt" "rustup component add rustfmt"; fi
if have clippy-driver; then ok "clippy"; else note "clippy" "rustup component add clippy"; fi
if have python3; then ok "python3" "$(python3 --version 2>&1)"; else bad "python3" "required for ml/eval"; fi
echo

echo "-- hardware tools (required: M1) --"
if have v4l2-ctl; then ok "v4l2-ctl" "$(v4l2-ctl --version 2>/dev/null | head -1)"; else bad "v4l2-ctl" "pkg: v4l-utils"; fi
if have lsusb; then ok "lsusb"; else bad "lsusb" "pkg: usbutils"; fi
echo

echo "-- TPM tools (required: M4) --"
if [ -e /dev/tpmrm0 ]; then ok "/dev/tpmrm0" "TPM resource manager present"; else note "/dev/tpmrm0" "no TPM; M4 falls back to file storage"; fi
if have tpm2_getcap; then ok "tpm2-tools"; else note "tpm2-tools" "pkg: tpm2-tools — needed for M4, not before"; fi
if ls /usr/lib/libtss2-esys.so* >/dev/null 2>&1; then ok "libtss2"; else note "libtss2" "pkg: tpm2-tss — needed for M4"; fi
echo

echo "-- contribution tools (required: M8) --"
if have git; then ok "git" "$(git --version)"; else bad "git"; fi
if have gh; then ok "gh"; else note "gh" "needed for quirks contribution flow"; fi
if have jq; then ok "jq"; else note "jq" "convenience only"; fi
echo

echo "-- camera access --"
ir_found=0
for node in /dev/video*; do
    [ -e "$node" ] || continue
    if v4l2-ctl -d "$node" --list-formats 2>/dev/null | grep -q "'GREY'"; then
        if [ -r "$node" ]; then
            ok "$node" "GREY format — candidate IR node, readable"
        else
            bad "$node" "GREY format but NOT readable by $(id -un)"
        fi
        ir_found=1
    fi
done
[ "$ir_found" -eq 1 ] || note "IR node" "no GREY-format node found; M1 needs one"

if id -nG | tr ' ' '\n' | grep -qx video; then
    ok "group: video" "member"
else
    note "group: video" "not a member — desktop access works via logind seat ACL,
                                but the 'namtor' system user in M3 will need real
                                group membership since daemons get no seat ACL"
fi
if id -nG | tr ' ' '\n' | grep -qx tss; then ok "group: tss" "member"; else note "group: tss" "needed by the daemon user in M4"; fi
echo

echo "-- inference runtime (required: M3) --"
if ls /usr/lib/libonnxruntime.so* >/dev/null 2>&1; then
    ok "onnxruntime" "system library present"
else
    note "onnxruntime" "not installed; the ort crate can download its own.
                                Arch: pacman -S onnxruntime-cpu"
fi
if grep -qw avx_vnni /proc/cpuinfo 2>/dev/null; then
    ok "avx_vnni" "INT8 acceleration available — the design depends on this"
else
    note "avx_vnni" "absent; INT8 gains will be smaller, revisit the latency budget"
fi
echo

echo "-- repository pre-flight --"
[ -f LICENSE-GPL-3.0 ]    && ok "LICENSE-GPL-3.0"    || note "LICENSE-GPL-3.0" "fetch from https://www.gnu.org/licenses/gpl-3.0.txt"
[ -f LICENSE-APACHE-2.0 ] && ok "LICENSE-APACHE-2.0" || note "LICENSE-APACHE-2.0" "fetch from https://www.apache.org/licenses/LICENSE-2.0.txt"
if [ -f fixtures/descriptors/3277-0018.bin ]; then
    sum=$(sha256sum fixtures/descriptors/3277-0018.bin | cut -c1-16)
    if [ "$sum" = "68936ace0b0d0428" ]; then
        ok "descriptor fixture" "matches the validated blob"
    else
        note "descriptor fixture" "present but differs from the validated blob ($sum);
                                M1 Task 4 expectations may not hold"
    fi
else
    note "descriptor fixture" "absent; M1 Task 4 Step 1 captures it"
fi
echo

echo "=== summary: $pass ok, $warn notes, $fail blocking ==="
if [ "$fail" -gt 0 ]; then
    echo "Blocking items must be resolved before starting."
    exit 1
fi
echo "Ready. Start with docs/superpowers/plans/ — read the roadmap first."
