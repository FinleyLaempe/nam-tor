#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Build a deterministic, reproducible model source archive for Nam-tor.
#
# Produces ``namtor-models-<version>.tar.gz`` plus a ``.sha256`` sidecar. The
# archive is the signed source of truth for every distro recipe: no package
# may build without it (task 4). Reproducible by construction:
#
#   - tar entries sorted by name (``--sort=name``)
#   - fixed mtime epoch 0 (``--mtime=@0``)
#   - fixed owner/group 0 (``--owner=0 --group=0 --numeric-owner``)
#   - gzip without original timestamp/name (``gzip -n``)
#
# Archive layout (every path deterministic):
#
#   models/yunet_2023mar.onnx          — face detector weights (MIT)
#   models/sface_2021dec.onnx          — face embedding weights (Apache-2.0)
#   models/minifasnet_pad.onnx         — strobe-differential PAD (self-trained)
#   licences/MIT.txt                   — canonical MIT text
#   licences/APACHE-2.0.txt            — canonical Apache-2.0 text
#   licences/yunet_2023mar.LICENSE     — YuNet model-specific MIT (Shiqi Yu)
#   licences/sface_2021dec.LICENSE     — SFace model-specific Apache-2.0
#   licences/minifasnet_pad.NOTICE     — MiniFASNet self-trained provenance
#   PROVENANCE.txt                     — upstream commit, source URLs,
#                                        source-availability statement,
#                                        fixed epoch declaration
#   MANIFEST.json                      — sha256 of every file in the archive
#                                        (MANIFEST.json's own entry excluded;
#                                        the .sha256 sidecar covers the whole
#                                        archive)
#
# Usage::
#
#   packaging/build-model-archive.sh [--out DIR] [--version VER] [SRCDIR]
#   packaging/build-model-archive.sh --verify ARCHIVE [SRCDIR]
#
#   --out        output directory (default: SRCDIR).
#   --version    archive version tag (default: 0.1.0).
#   --verify     verify an existing archive: recompute every hash in
#                MANIFEST.json, check the .sha256 sidecar (if present),
#                and check each model hash against model-manifest.toml.
#                Exits non-zero on any mismatch (tamper-evident).
#   SRCDIR       repo root (default: git toplevel).
#
# Environment:
#   NAMTOR_MODELS_DIR  override the models source directory
#                      (default: <SRCDIR>/var/lib/namtor/models).
#   NAMTOR_EPOCH       fixed mtime epoch (default: 0).
#
# Exit codes: 0 = ok, 1 = verification failed, 2 = bad args,
#             3 = a required source file was missing.

set -euo pipefail

out_dir=""
version="0.1.0"
verify_mode=0
verify_archive=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out)     out_dir="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        --verify)  verify_mode=1; verify_archive="$2"; shift 2 ;;
        --help|-h) sed -n '3,52p' "$0"; exit 0 ;;
        --*)       echo "error: unknown option: $1" >&2; exit 2 ;;
        *)         break ;;
    esac
done

srcdir="${1:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
models_dir="${NAMTOR_MODELS_DIR:-$srcdir/var/lib/namtor/models}"
licences_dir="$srcdir/packaging/licences"
manifest="$srcdir/packaging/model-manifest.toml"
epoch="${NAMTOR_EPOCH:-0}"

# ----- verify mode -----------------------------------------------------------
if [[ "$verify_mode" -eq 1 ]]; then
    [[ -f "$verify_archive" ]] || { echo "error: archive not found: $verify_archive" >&2; exit 3; }
    python3 - "$verify_archive" "$manifest" "$verify_archive.sha256" <<'PY'
import hashlib, json, os, sys, tarfile, tomllib
archive, manifest, sidecar = sys.argv[1], sys.argv[2], sys.argv[3]
errors = []
# 1. Whole-archive sha256 vs sidecar (if sidecar present).
actual = hashlib.sha256(open(archive, "rb").read()).hexdigest()
if os.path.isfile(sidecar):
    with open(sidecar) as f:
        want = f.read().split()[0]
    if actual != want:
        errors.append(f"archive sha256 mismatch: sidecar={want[:12]} actual={actual[:12]}")
# 2. MANIFEST.json per-file hashes.
with tarfile.open(archive, "r:gz") as tf:
    members = {m.name: m for m in tf.getmembers()}
    mj = members.get("MANIFEST.json")
    if mj is None:
        errors.append("MANIFEST.json missing from archive")
    else:
        doc = json.load(tf.extractfile(mj))
        for name, info in doc.get("files", {}).items():
            m = members.get(name)
            if m is None:
                errors.append(f"{name}: listed in MANIFEST but absent from archive")
                continue
            got = hashlib.sha256(tf.extractfile(m).read()).hexdigest()
            if got != info.get("sha256"):
                errors.append(f"{name}: hash mismatch manifest={info.get('sha256')[:12]} actual={got[:12]}")
# 3. Model hashes vs model-manifest.toml.
with open(manifest, "rb") as f:
    data = tomllib.load(f)
with tarfile.open(archive, "r:gz") as tf:
    members = {m.name: m for m in tf.getmembers()}
    for m in data.get("models", []):
        for v in m.get("variants", []):
            path = f"models/{m['id']}.onnx"
            mm = members.get(path)
            if mm is None:
                errors.append(f"{path}: model missing from archive")
                continue
            got = hashlib.sha256(tf.extractfile(mm).read()).hexdigest()
            if got != v["sha256"]:
                errors.append(f"{path}: model hash mismatch manifest={v['sha256'][:12]} actual={got[:12]}")
if errors:
    for e in errors:
        print(f"  VERIFY FAIL: {e}", file=sys.stderr)
    sys.exit(1)
print(f"verify ok: {archive} ({actual[:12]})")
sys.exit(0)
PY
    exit $?
fi

# ----- build mode ------------------------------------------------------------
[[ -f "$manifest" ]] || { echo "error: model-manifest.toml not found: $manifest" >&2; exit 3; }

# Resolve output directory (absolute — the tar pipeline cds into a temp
# stage, so a relative out_dir would write the archive into the stage and
# tar would archive its own output).
[[ -n "$out_dir" ]] || out_dir="$srcdir"
out_dir="$(cd "$out_dir" 2>/dev/null && pwd)" || {
    echo "error: output directory does not exist: $out_dir" >&2; exit 2; }
mkdir -p "$out_dir"

archive_name="namtor-models-${version}.tar.gz"
archive_path="$out_dir/$archive_name"

# Required source files.
declare -a model_files=(
    "$models_dir/yunet_2023mar.onnx"
    "$models_dir/sface_2021dec.onnx"
    "$models_dir/minifasnet_pad.onnx"
)
declare -a licence_files=(
    "$licences_dir/MIT.txt"
    "$licences_dir/APACHE-2.0.txt"
    "$licences_dir/yunet_2023mar.LICENSE"
    "$licences_dir/sface_2021dec.LICENSE"
    "$licences_dir/minifasnet_pad.NOTICE"
)
for f in "${model_files[@]}" "${licence_files[@]}"; do
    [[ -f "$f" ]] || { echo "error: required source file missing: $f" >&2; exit 3; }
done

# Stage into a deterministic scratch tree, then tar with fixed metadata.
stage="$(mktemp -d -t namtor-model-archive-XXXXXX)"
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage/models" "$stage/licences"

cp "${model_files[@]}" "$stage/models/"
cp "${licence_files[@]}" "$stage/licences/"

# Verify staged model hashes against the manifest before archiving.
python3 - "$stage/models" "$manifest" <<'PY'
import hashlib, sys, tomllib
models_dir, manifest = sys.argv[1], sys.argv[2]
with open(manifest, "rb") as f:
    data = tomllib.load(f)
for m in data.get("models", []):
    for v in m.get("variants", []):
        path = f"{models_dir}/{m['id']}.onnx"
        got = hashlib.sha256(open(path, "rb").read()).hexdigest()
        if got != v["sha256"]:
            sys.stderr.write(f"error: {m['id']} hash mismatch (expected {v['sha256'][:12]}, got {got[:12]})\n")
            sys.exit(3)
PY

# PROVENANCE.txt — upstream commit, source URLs, source-availability statement,
# fixed epoch declaration. All deterministic (no build host, no timestamps).
commit="$(git -C "$srcdir" rev-parse HEAD 2>/dev/null || echo UNKNOWN)"
cat > "$stage/PROVENANCE.txt" <<EOF
# Nam-tor model source archive — provenance
# SPDX-License-Identifier: GPL-3.0-only
archive_version = ${version}
built_from_commit = ${commit}
fixed_epoch = ${epoch}
build_host = redacted-for-reproducibility

[source availability]
All three models in this archive are redistributable under permissive
licences (MIT or Apache-2.0). The weights are NOT committed to the Nam-tor
git repository; they are assembled into this deterministic archive by
packaging/build-model-archive.sh from a deployed model store. Rebuilding
the archive from the same deployed weights yields a byte-identical tarball
(fixed epoch, sorted entries, gzip -n).

[upstream provenance]
yunet_2023mar:
  upstream_url = https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx
  licence      = MIT (packaging/licences/yunet_2023mar.LICENSE)
sface_2021dec:
  upstream_url = https://github.com/opencv/opencv_zoo/raw/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx
  licence      = Apache-2.0 (packaging/licences/sface_2021dec.LICENSE)
minifasnet_pad:
  upstream_url = self-trained on physics-based synthetic strobe-differential data
  licence      = Apache-2.0 (packaging/licences/minifasnet_pad.NOTICE)

[offline build contract]
No network access is required to install or run Nam-tor from this archive.
The distro recipes (Arch PKGBUILD, Fedora namtor.spec, Debian rules) bundle
these models and pre-write the licence acceptance records. The daemon starts
with a valid offline cache and never fetches weights at install time.
EOF

# MANIFEST.json — sha256 of every file EXCEPT MANIFEST.json itself (computed
# after staging, before tarring, so the manifest is part of the archive).
python3 - "$stage" <<'PY'
import hashlib, json, os, sys
stage = sys.argv[1]
files = {}
for root, _, names in os.walk(stage):
    for n in sorted(names):
        full = os.path.join(root, n)
        rel = os.path.relpath(full, stage)
        if rel == "MANIFEST.json":
            continue
        h = hashlib.sha256(open(full, "rb").read()).hexdigest()
        files[rel] = {"sha256": h}
doc = {
    "archive_version": os.environ.get("NAMTOR_ARCHIVE_VERSION", "0.1.0"),
    "files": dict(sorted(files.items())),
}
with open(os.path.join(stage, "MANIFEST.json"), "w") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
    f.write("\n")
PY

# Deterministic tar: sorted names, fixed mtime/owner/group, gzip -n.
export NAMTOR_ARCHIVE_VERSION="$version"
(
    cd "$stage"
    find . -type f -printf '%P\n' | LC_ALL=C sort \
        | tar --sort=name --mtime="@${epoch}" --owner=0 --group=0 \
              --numeric-owner --no-recursion -T - -cf - \
        | gzip -n -9 > "$archive_path"
)

# sha256 sidecar (deterministic: just the hash + filename).
archive_hash=$(sha256sum "$archive_path" | awk '{print $1}')
echo "$archive_hash  $archive_name" > "$archive_path.sha256"

echo "Built $archive_path ($archive_hash)"
echo "Sidecar $archive_path.sha256"
