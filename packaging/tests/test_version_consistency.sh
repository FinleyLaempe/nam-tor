#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
python3 - "$ROOT" "${1:-}" <<'PY'
import pathlib
import re
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
expected = sys.argv[2] or re.search(
    r'^pkgver=(\S+)$', (root / 'packaging/arch/PKGBUILD').read_text(), re.M)[1]
assert re.fullmatch(r'\d+\.\d+\.\d+', expected), expected
versions = {}
for manifest in sorted((root / 'crates').glob('*/Cargo.toml')):
    package = tomllib.loads(manifest.read_text())['package']
    versions[package['name']] = expected
    assert package['version'] == expected, f'{manifest}: {package["version"]} != {expected}'
lock = tomllib.loads((root / 'Cargo.lock').read_text())
for package in lock['package']:
    if package['name'] in versions and 'source' not in package:
        assert package['version'] == expected, f'Cargo.lock: {package["name"]} version drift'
arch = (root / 'packaging/arch/PKGBUILD').read_text()
assert f'pkgver={expected}\n' in arch
fedora = (root / 'packaging/fedora/namtor.spec').read_text()
assert re.search(r'^Version:\s+' + re.escape(expected) + r'$', fedora, re.M)
debian = (root / 'packaging/debian/changelog').read_text().splitlines()[0]
assert debian.startswith(f'namtor ({expected}-'), debian
print(f'PASS: {len(versions)} crates, lockfile and three distro recipes agree on {expected}')
PY
