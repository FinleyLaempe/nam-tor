#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Gate: every `Documentation=man:` URI in a shipped systemd unit resolves to a
# man page source in the repo AND to an entry in the install manifest.
#
# Why this is a release blocker: `systemd-analyze verify` shells out to `man(1)`
# to resolve every Documentation= URI. When the referenced page does not exist
# it reports `Protocol error` and exits 1, which fails the release smoke step.
# A dangling Documentation= URI is also a user-facing defect — `systemctl help
# namtor-daemon` tells the admin to read a page that was never shipped.
set -euo pipefail
cd "$(dirname "$0")/../.."

rc=0
found_any=0

for unit in packaging/systemd/*.service packaging/systemd/*.socket; do
    [ -e "$unit" ] || continue
    # `|| true`: a unit with no Documentation=man: URI is fine, but grep exits 1
    # on no match and `set -o pipefail` would abort the whole gate.
    refs=$(grep -oP 'man:\K[a-z0-9_.-]+\([0-9]\)' "$unit" || true)
    [ -n "$refs" ] || continue
    while read -r ref; do
        [ -n "$ref" ] || continue
        found_any=1
        page="${ref%(*}"
        sec="${ref##*(}"
        sec="${sec%)}"
        src="docs/man/${page}.${sec}"
        if [ ! -f "$src" ]; then
            echo "::error::$unit references man:$ref but $src does not exist"
            rc=1
            continue
        fi
        if ! grep -q "$(basename "$src")" packaging/install-manifest.toml; then
            echo "::error::$src is not in packaging/install-manifest.toml"
            rc=1
            continue
        fi
        # The manifest must install it into the matching section directory,
        # or `man namtor-daemon` will not find it on the installed system.
        if ! grep -q "^dest = \"/usr/share/man/man${sec}/${page}.${sec}\"$" \
                packaging/install-manifest.toml; then
            echo "::error::$src is not installed to /usr/share/man/man${sec}/"
            rc=1
        fi
    done <<< "$refs"
done

if [ "$found_any" = 0 ]; then
    echo "::error::no Documentation=man: URI found in any shipped unit — gate is vacuous"
    rc=1
fi

# Every man page source in the repo must be installed by some package, or it is
# dead weight that never reaches a user.
for src in docs/man/*.[0-9]; do
    [ -e "$src" ] || continue
    if ! grep -q "^src = \"$src\"$" packaging/install-manifest.toml; then
        echo "::error::$src exists but no install-manifest entry sources it"
        rc=1
    fi
done

exit $rc
