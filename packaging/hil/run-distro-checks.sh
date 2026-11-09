#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Run the release workflow's Debian 13 and Fedora 41 cells locally in podman.
#
#   packaging/hil/run-distro-checks.sh [debian] [fedora]    (default: both)
#
# Builds the source tarball from the working tree, takes the model archive from
# the repo root (namtor-models-<ver>.tar.gz), and runs packaging/hil/
# distro-check-<distro>.sh in docker.io/library/{debian:13,fedora:41}.
# Logs and artifacts (packages, UI screenshots) land in $OUT (default
# ./target/distro-checks/<distro>/). Exit status is non-zero if a cell fails.
set -euo pipefail
cd "$(dirname "$0")/../.."

PKGVER="$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD | head -1)"
MODELVER="$(sed -n 's/^_modelver=//p' packaging/arch/PKGBUILD | head -1)"
[ -n "$PKGVER" ] && [ -n "$MODELVER" ] || { echo "cannot read versions from PKGBUILD" >&2; exit 1; }
[ -f "namtor-models-$MODELVER.tar.gz" ] || { echo "namtor-models-$MODELVER.tar.gz not in repo root" >&2; exit 1; }

OUT="${OUT:-$PWD/target/distro-checks}"
IN="$OUT/inputs"
mkdir -p "$IN"
tar --exclude=.git --exclude=./target --exclude=./ml/.venv --exclude=./var --exclude=./.codegraph \
    --transform "s,^\.,nam-tor-$PKGVER," -czf "$IN/namtor-$PKGVER.tar.gz" . 2>/dev/null || true
cp "namtor-models-$MODELVER.tar.gz" "$IN/"

declare -A IMAGE=( [debian]=docker.io/library/debian:13 [fedora]=docker.io/library/fedora:41 )
cells=("$@"); [ ${#cells[@]} -gt 0 ] || cells=(debian fedora)

pids=()
for c in "${cells[@]}"; do
  [ -n "${IMAGE[$c]:-}" ] || { echo "unknown cell: $c" >&2; exit 2; }
  rm -rf "$OUT/$c"; mkdir -p "$OUT/$c"
  podman run --rm -e PKGVER="$PKGVER" -e MODELVER="$MODELVER" \
    -v "$IN:/inputs:ro,Z" -v "$OUT/$c:/out:Z" \
    -v "$PWD/packaging/hil/distro-check-$c.sh:/run.sh:ro,Z" \
    "${IMAGE[$c]}" bash /run.sh > "$OUT/$c.log" 2>&1 &
  pids+=($!)
done

rc=0
for i in "${!pids[@]}"; do
  if wait "${pids[$i]}"; then echo "PASS ${cells[$i]}  ($OUT/${cells[$i]}.log)"
  else echo "FAIL ${cells[$i]}  ($OUT/${cells[$i]}.log)"; rc=1; fi
done
exit $rc
