#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Verify reproducible builds: build the workspace release twice into two
# separate CARGO_TARGET_DIR roots and compare the sha256 of every artifact
# the install manifest references.
#
# Rust release builds are deterministic when:
#   - `strip = true` (no embedded absolute build paths in debug info),
#   - `lto = true` + `codegen-units = 1` (no parallel codegen noise),
#   - `panic = "abort"` (no unwinding metadata variance),
#   - same rustc version + same host.
# All four hold for this workspace (see Cargo.toml [profile.release]).
#
# This script does NOT require network. It uses two fresh target dirs to
# rule out incremental-cache contamination. A mismatch is reported per-file
# with both hashes; the script exits non-zero if any artifact differs.
#
# Usage::
#
#   packaging/verify-reproducible.sh [SRCDIR]
#
#   SRCDIR  repo root (default: git toplevel).
#
# Output: a JSON record at docs/evidence/packaging/reproducible-build.json
# with per-artifact hashes from both builds and a boolean `reproducible`
# field. Exit 0 if all match, 1 if any differ.

set -euo pipefail

srcdir="${1:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
artifacts=(
    namtor-daemon namtor-worker namtor-worker-spawn
    namtor-camera-emitter-off namtor-cli namtor-enroll-wizard
    libnamtor_pam.so
)

tmp_root="$(mktemp -d -t namtor-repro-XXXXXX)"
dir_a="$tmp_root/build-a"
dir_b="$tmp_root/build-b"
trap 'rm -rf "$tmp_root"' EXIT

echo "Build 1 -> $dir_a"
CARGO_TARGET_DIR="$dir_a" cargo build --release --workspace \
    --manifest-path "$srcdir/Cargo.toml" 2>&1 | sed 's/^/  [a] /'

echo "Build 2 -> $dir_b"
CARGO_TARGET_DIR="$dir_b" cargo build --release --workspace \
    --manifest-path "$srcdir/Cargo.toml" 2>&1 | sed 's/^/  [b] /'

out_json="$srcdir/docs/evidence/packaging/reproducible-build.json"
mkdir -p "$(dirname "$out_json")"

# Emit a JSON record comparing each artifact's sha256 across both builds.
python3 - "$dir_a" "$dir_b" "$out_json" "${artifacts[@]}" <<'PY'
import hashlib, json, os, sys
dir_a, dir_b, out_json = sys.argv[1], sys.argv[2], sys.argv[3]
artifacts = sys.argv[4:]
def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(65536), b""):
            h.update(c)
    return h.hexdigest()
records = []
all_match = True
for name in artifacts:
    pa = os.path.join(dir_a, "release", name)
    pb = os.path.join(dir_b, "release", name)
    ha = sha256(pa) if os.path.isfile(pa) else None
    hb = sha256(pb) if os.path.isfile(pb) else None
    match = ha is not None and ha == hb
    if not match:
        all_match = False
    records.append({"name": name, "sha256_a": ha, "sha256_b": hb,
                    "match": match})
doc = {
    "reproducible": all_match,
    "build_a_dir": dir_a,
    "build_b_dir": dir_b,
    "artifacts": records,
}
with open(out_json, "w") as f:
    json.dump(doc, f, indent=2)
print(json.dumps(doc, indent=2))
if not all_match:
    sys.exit(1)
PY

echo "Reproducible build verification complete: $out_json"
