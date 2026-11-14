#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Build, install and exercise the Debian package inside a Debian 13 root —
# the same steps as the release workflow's debian13 cell, runnable without
# GitHub Actions. Expects /inputs/namtor-<ver>.tar.gz and
# /inputs/namtor-models-<ver>.tar.gz, and writes results to /out.
set -euo pipefail
PKGVER="${PKGVER:?}"; MODELVER="${MODELVER:?}"
export DEBIAN_FRONTEND=noninteractive
step() { printf '\n==== %s ====\n' "$*"; }

step "toolchain"
apt-get update -qq
apt-get install -y -qq --no-install-recommends build-essential cargo rustc \
    python3 debhelper-compat dh-cargo libpam0g-dev openssl devscripts \
    libgtk-4-dev libadwaita-1-dev curl ca-certificates libseccomp-dev \
    libudev-dev xvfb xauth >/dev/null
if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null 2>&1 || true
fi
export PATH="/root/.cargo/bin:$PATH"
rustc --version

step "unpack"
rm -rf /build && mkdir -p /build && cd /build
tar -xzf "/inputs/namtor-$PKGVER.tar.gz"
cd "nam-tor-$PKGVER"

step "lint (dpkg-checkbuilddeps)"
cp -r packaging/debian ./debian
dpkg-checkbuilddeps debian/control

step "build .deb"
cp "/inputs/namtor-models-$MODELVER.tar.gz" .
cargo fetch --locked
dpkg-buildpackage -us -uc -b 2>&1 | tail -40
ls -la ../*.deb
cp ../*.deb /out/

step "install (apt resolves real dependencies)"
apt-get install -y --no-install-recommends ../namtor_*.deb ../namtor-ui_*.deb 2>&1 | tail -5
dpkg -s namtor | grep -E '^(Status|Version|Depends):'

step "smoke"
timeout -k 10 10 /usr/bin/namtor daemon --version
if ldd /usr/lib/*/security/pam_namtor.so | grep 'not found'; then echo "FAIL: pam module unresolved"; exit 1; fi
systemd-analyze verify --man=no /usr/lib/systemd/system/namtor-daemon.service || true
test -x /usr/bin/namtor-enroll-wizard && echo "wizard installed"
ldd /usr/bin/namtor-enroll-wizard | grep 'not found' && { echo "FAIL: wizard unresolved"; exit 1; } || echo "wizard links"

step "UI test suite under Xvfb"
cargo build -q -p namtor-ui --examples
xvfb-run -a cargo test -p namtor-ui -- --test-threads=1 2>&1 | grep -E "^test result|FAILED|panicked" | tee /out/ui-tests.txt

step "render the UI on this distro"
for st in angles review-ok error; do
  xvfb-run -a timeout 90 cargo run -q -p namtor-ui --example scan_page -- "$st" "/out/$st.png" 2>&1 | grep -E "wrote|failed" || true
done
NAMTOR_QUIRKS_DIR="$PWD/quirks/devices" xvfb-run -a timeout 120 cargo run -q -p namtor-ui --example app_pages -- /out 2>&1 | grep -E "wrote|failed" || true

step "uninstall"
apt-get purge -y namtor-ui namtor 2>&1 | tail -3
test ! -e /usr/bin/namtor && echo "uninstall removed /usr/bin/namtor"
echo "DEBIAN CHECK COMPLETE"
