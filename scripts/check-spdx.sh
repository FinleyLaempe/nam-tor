#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Asserts every Rust source file carries an SPDX license identifier.
set -euo pipefail

fail=0
while IFS= read -r -d '' f; do
    if ! head -n 3 "$f" | grep -q 'SPDX-License-Identifier:'; then
        echo "missing SPDX header: $f" >&2
        fail=1
    fi
done < <(find crates -name '*.rs' -print0)

if [ "$fail" -ne 0 ]; then
    echo "SPDX check FAILED" >&2
    exit 1
fi
echo "SPDX check passed"
