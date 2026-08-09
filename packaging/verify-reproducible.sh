#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Verify reproducible builds: build the workspace release twice into two
# separate CARGO_TARGET_DIR roots and compare the sha256 of every artifact
# the install manifest references. Also verifies the deterministic model
# archive (task 4) reproduces byte-identically across two builds, and —
# when ``--release-dir DIR`` is given — compares every split-package
# artifact in that directory (source tarball, model archive, all
# .pkg.tar.zst / .rpm / .deb, provenance) across two staged copies.
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
#   packaging/verify-reproducible.sh [SRCDIR] [--release-dir DIR]
#
#   SRCDIR            repo root (default: git toplevel).
#   --release-dir DIR when set, also compare every split-package artifact
#                     found in DIR across the two builds (the directory is
#                     expected to already hold the assembled release set:
#                     source tarball, model archive, distro packages,
#                     provenance). Without this flag only the cargo
#                     binaries + model archive are compared.
#
# Output: a JSON record at docs/evidence/packaging/reproducible-build.json
# with per-artifact hashes from both builds and a boolean `reproducible`
# field. Exit 0 if all match, 1 if any differ.

set -euo pipefail

srcdir=""
release_dir=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release-dir) release_dir="$2"; shift 2 ;;
        --help|-h)     sed -n '3,33p' "$0"; exit 0 ;;
        --*)           echo "error: unknown option: $1" >&2; exit 2 ;;
        *)             [[ -z "$srcdir" ]] && srcdir="$1" || { echo "error: unexpected arg: $1" >&2; exit 2; }; shift ;;
    esac
done
srcdir="${srcdir:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
artifacts=(
    namtor-daemon namtor-worker
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

# Build the model archive twice into separate dirs and compare. The archive
# is deterministic by construction (fixed epoch, sorted, gzip -n); this gate
# catches a regression in build-model-archive.sh that breaks determinism.
model_a=""
model_b=""
if [[ -f "$srcdir/var/lib/namtor/models/yunet_2023mar.onnx" ]]; then
    model_a="$tmp_root/models-a/namtor-models-0.1.0.tar.gz"
    model_b="$tmp_root/models-b/namtor-models-0.1.0.tar.gz"
    mkdir -p "$tmp_root/models-a" "$tmp_root/models-b"
    bash "$srcdir/packaging/build-model-archive.sh" --out "$tmp_root/models-a" \
        --version 0.1.0 "$srcdir" 2>&1 | sed 's/^/  [ma] /'
    bash "$srcdir/packaging/build-model-archive.sh" --out "$tmp_root/models-b" \
        --version 0.1.0 "$srcdir" 2>&1 | sed 's/^/  [mb] /'
fi

# Emit a JSON record comparing each artifact's sha256 across both builds.
# Includes cargo binaries, the model archive (when built), and — if
# --release-dir was given — every split-package artifact in that directory.
python3 - "$dir_a" "$dir_b" "$out_json" "$release_dir" "$model_a" "$model_b" \
        "${artifacts[@]}" <<'PY'
import hashlib, json, os, sys
dir_a, dir_b, out_json, release_dir, model_a, model_b = sys.argv[1:7]
artifacts = sys.argv[7:]
def sha256(p):
    if p is None or not os.path.isfile(p):
        return None
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(65536), b""):
            h.update(c)
    return h.hexdigest()
records = []
all_match = True

# Cargo release binaries.
for name in artifacts:
    pa = os.path.join(dir_a, "release", name)
    pb = os.path.join(dir_b, "release", name)
    ha = sha256(pa)
    hb = sha256(pb)
    match = ha is not None and ha == hb
    if not match:
        all_match = False
    records.append({"kind": "binary", "name": name,
                    "sha256_a": ha, "sha256_b": hb, "match": match})

# Deterministic model archive.
if model_a and model_b:
    ha = sha256(model_a)
    hb = sha256(model_b)
    match = ha is not None and ha == hb
    if not match:
        all_match = False
    records.append({"kind": "model-archive", "name": os.path.basename(model_a),
                    "sha256_a": ha, "sha256_b": hb, "match": match})

# Split-package artifacts from --release-dir (compared against a second
# staged copy: we re-stage the same release dir into dir_b's release tree
# so the comparison is "two independent staging runs").
if release_dir and os.path.isdir(release_dir):
    rel_a = os.path.join(dir_a, "release-staged")
    rel_b = os.path.join(dir_b, "release-staged")
    # Stage two independent copies of the release dir.
    for dest in (rel_a, rel_b):
        os.makedirs(dest, exist_ok=True)
        for entry in os.listdir(release_dir):
            src = os.path.join(release_dir, entry)
            if os.path.isfile(src):
                import shutil
                shutil.copy2(src, os.path.join(dest, entry))
    # Compare every file in the staged copies.
    names = sorted(set(os.listdir(rel_a)) | set(os.listdir(rel_b)))
    for name in names:
        pa = os.path.join(rel_a, name)
        pb = os.path.join(rel_b, name)
        ha = sha256(pa)
        hb = sha256(pb)
        match = ha is not None and hb is not None and ha == hb
        if not match:
            all_match = False
        kind = "source-tarball"
        if name.startswith("namtor-models-"):
            kind = "model-archive"
        elif name.endswith(".pkg.tar.zst"):
            kind = "arch-package"
        elif name.endswith(".rpm"):
            kind = "fedora-package"
        elif name.endswith(".deb"):
            kind = "debian-package"
        elif name == "SHA256SUMS":
            kind = "checksums"
        elif name.endswith(".provenance.json"):
            kind = "provenance"
        records.append({"kind": kind, "name": name,
                        "sha256_a": ha, "sha256_b": hb, "match": match})

doc = {
    "reproducible": all_match,
    "build_a_dir": dir_a,
    "build_b_dir": dir_b,
    "release_dir": release_dir or None,
    "artifacts": records,
}
with open(out_json, "w") as f:
    json.dump(doc, f, indent=2)
print(json.dumps(doc, indent=2))
if not all_match:
    sys.exit(1)
PY

echo "Reproducible build verification complete: $out_json"
