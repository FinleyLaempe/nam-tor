#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Build, install and exercise the Fedora packages inside a Fedora 41 root —
# the same steps as the release workflow's fedora41 cell, runnable without
# GitHub Actions. Expects /inputs/namtor-<ver>.tar.gz and
# /inputs/namtor-models-<ver>.tar.gz, and writes results to /out.
set -euo pipefail
PKGVER="${PKGVER:?}"; MODELVER="${MODELVER:?}"
step() { printf '\n==== %s ====\n' "$*"; }

step "toolchain"
dnf -y -q install @development-tools rust cargo python3 \
    systemd-rpm-macros rpmlint rpm-build openssl-devel pam-devel \
    gtk4-devel libadwaita-devel libseccomp-devel gcc-c++ systemd-devel \
    xorg-x11-server-Xvfb xorg-x11-xauth which >/dev/null
rustc --version

step "unpack"
rm -rf /build && mkdir -p /build && tar -xzf "/inputs/namtor-$PKGVER.tar.gz" -C /build
SPEC="/build/nam-tor-$PKGVER/packaging/fedora/namtor.spec"

step "lint (rpmlint)"
rpmlint "$SPEC" || echo "rpmlint warnings (non-blocking, as in CI)"

step "build rpm"
mkdir -p ~/rpmbuild/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
cp "/inputs/namtor-$PKGVER.tar.gz" "/inputs/namtor-models-$MODELVER.tar.gz" ~/rpmbuild/SOURCES/
cp "$SPEC" ~/rpmbuild/SPECS/
(cd "/build/nam-tor-$PKGVER" && cargo fetch --locked)
rpmbuild -ba ~/rpmbuild/SPECS/namtor.spec 2>&1 | tail -40
find ~/rpmbuild/RPMS -name 'namtor-*.rpm' -exec cp {} /out/ \;
ls -la /out/*.rpm

step "install (dnf resolves real dependencies)"
dnf -y install /out/namtor-*.rpm 2>&1 | tail -6
rpm -q namtor namtor-ui
rpm -q --requires namtor | grep -iE 'onnx|lib64gcc' && { echo "FAIL: bogus requires"; exit 1; } || echo "no bogus requires"

step "smoke"
timeout -k 10 10 /usr/bin/namtor daemon --version
if ldd /usr/lib64/security/pam_namtor.so | grep 'not found'; then echo "FAIL: pam module unresolved"; exit 1; fi
if ldd /usr/bin/namtor-enroll-wizard | grep 'not found'; then echo "FAIL: wizard unresolved"; exit 1; fi
echo "binaries link"

step "UI test suite under Xvfb"
cd "/build/nam-tor-$PKGVER"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 GSK_RENDERER=cairo
xvfb-run -a cargo test -p namtor-ui -- --test-threads=1 2>&1 | grep -E "^test result|FAILED|panicked" | tee /out/ui-tests.txt

step "render the UI on this distro"
for st in angles review-ok error; do
  xvfb-run -a timeout 90 cargo run -q -p namtor-ui --example scan_page -- "$st" "/out/$st.png" 2>&1 | grep -E "wrote|failed" || true
done
NAMTOR_QUIRKS_DIR="$PWD/quirks/devices" xvfb-run -a timeout 120 cargo run -q -p namtor-ui --example app_pages -- /out 2>&1 | grep -E "wrote|failed" || true

step "uninstall"
dnf -y remove namtor-ui namtor 2>&1 | tail -3
test ! -e /usr/bin/namtor && echo "uninstall removed /usr/bin/namtor"
echo "FEDORA CHECK COMPLETE"
