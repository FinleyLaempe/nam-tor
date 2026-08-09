#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for the deterministic model archive builder.
#
# Verifies the three gates required by task 4:
#   1. Reproducibility  — building the archive twice yields identical SHA-256.
#   2. Tamper evidence  — flipping any byte in the archive fails --verify.
#   3. Manifest hygiene — model-manifest.toml has no SKIP / all-zero sha256
#                         on any release-path variant.
#
# Runs without network or root. Models must be present at
# <SRCDIR>/var/lib/namtor/models/ (the reference hardware deploys them; CI
# skips with exit 77 if absent so this is a hardware-gated test).
#
# Usage:
#   packaging/tests/test_model_archive.sh [SRCDIR]
#
# Exit codes: 0 = all pass, 1 = at least one failure, 77 = models absent.

set -euo pipefail

srcdir="${1:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
builder="$srcdir/packaging/build-model-archive.sh"
manifest="$srcdir/packaging/model-manifest.toml"
models_dir="$srcdir/var/lib/namtor/models"

pass=0
fail=0
failures=()

assert_ok() {
    local desc="$1"; shift
    if "$@"; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc")
        echo "  FAIL: $desc"
    fi
}

assert_eq() {
    local desc="$1" a="$2" b="$3"
    if [[ "$a" == "$b" ]]; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc ($a != $b)")
        echo "  FAIL: $desc ($a != $b)"
    fi
}

assert_ne() {
    local desc="$1" a="$2" b="$3"
    if [[ "$a" != "$b" ]]; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc (both = $a)")
        echo "  FAIL: $desc (both = $a)"
    fi
}

# Skip if models are absent (hardware-gated).
if [[ ! -f "$models_dir/yunet_2023mar.onnx" ]]; then
    echo "SKIP: models absent at $models_dir (hardware-gated; exit 77)"
    exit 77
fi

tmp="$(mktemp -d -t namtor-model-archive-test-XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

echo "## Gate 1: reproducibility (build twice, compare SHA-256)"
archive_a="$tmp/namtor-models-0.1.0.tar.gz"
archive_b="$tmp/b/namtor-models-0.1.0.tar.gz"
mkdir -p "$tmp/b"

bash "$builder" --out "$tmp"     --version 0.1.0 "$srcdir"
bash "$builder" --out "$tmp/b"   --version 0.1.0 "$srcdir"

hash_a=$(sha256sum "$archive_a" | awk '{print $1}')
hash_b=$(sha256sum "$archive_b" | awk '{print $1}')
assert_eq "two builds produce identical SHA-256" "$hash_a" "$hash_b"
assert_ne "archive is not empty" "$hash_a" "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

echo "## Gate 2: tamper evidence (flip one byte, --verify must fail)"
tampered="$tmp/tampered.tar.gz"
cp "$archive_a" "$tampered"
# Flip one byte in the middle of the archive (byte offset 1024).
printf '\x01' | dd of="$tampered" bs=1 seek=1024 count=1 conv=notrunc status=none
tamper_verify_rc=0
bash "$builder" --verify "$tampered" "$srcdir" || tamper_verify_rc=$?
assert_ne "tampered archive fails --verify" "$tamper_verify_rc" "0"

# A clean archive must verify.
clean_verify_rc=0
bash "$builder" --verify "$archive_a" "$srcdir" || clean_verify_rc=$?
assert_eq "clean archive passes --verify" "$clean_verify_rc" "0"

echo "## Gate 3: manifest hygiene (no SKIP, no all-zero sha256)"
python3 - "$manifest" <<'PY'
import re, sys, tomllib
manifest = sys.argv[1]
with open(manifest, "rb") as f:
    data = tomllib.load(f)
bad = []
zero = "0" * 64
for m in data.get("models", []):
    for v in m.get("variants", []):
        h = v.get("sha256", "")
        if not h or h == zero or h.upper() == "SKIP":
            bad.append(f"{m['id']}/{v.get('quant','?')}: {h!r}")
        if not re.fullmatch(r"[0-9a-f]{64}", h):
            bad.append(f"{m['id']}/{v.get('quant','?')}: not a 64-hex sha256: {h!r}")
    ls = m.get("licence_sha256", "")
    if not ls or ls == zero or not re.fullmatch(r"[0-9a-f]{64}", ls):
        bad.append(f"{m['id']}/licence: {ls!r}")
if bad:
    print("  FAIL: manifest hygiene: " + "; ".join(bad))
    sys.exit(1)
print("  PASS: manifest hygiene (no SKIP, no zeros, all 64-hex)")
sys.exit(0)
PY
rc=$?
if [[ $rc -eq 0 ]]; then
    pass=$((pass + 1))
else
    fail=$((fail + 1))
    failures+=("manifest hygiene")
fi

echo "## Gate 4: archive contents (licences + provenance + MANIFEST.json)"
contents=$(tar tzf "$archive_a")
for required in \
    models/yunet_2023mar.onnx \
    models/sface_2021dec.onnx \
    models/minifasnet_pad.onnx \
    licences/MIT.txt \
    licences/APACHE-2.0.txt \
    licences/yunet_2023mar.LICENSE \
    licences/sface_2021dec.LICENSE \
    licences/minifasnet_pad.NOTICE \
    PROVENANCE.txt \
    MANIFEST.json; do
    if echo "$contents" | grep -qxF "$required"; then
        pass=$((pass + 1))
        echo "  PASS: archive contains $required"
    else
        fail=$((fail + 1))
        failures+=("archive missing $required")
        echo "  FAIL: archive missing $required"
    fi
done

echo "## Gate 5: MANIFEST.json hashes match the archive bytes"
python3 - "$archive_a" "$tmp" <<'PY'
import hashlib, json, sys, tarfile, tempfile
archive = sys.argv[1]
with tarfile.open(archive, "r:gz") as tf:
    members = {m.name: m for m in tf.getmembers()}
    manifest_entry = members.get("MANIFEST.json")
    if manifest_entry is None:
        print("  FAIL: MANIFEST.json not found in archive")
        sys.exit(1)
    manifest = json.load(tf.extractfile(manifest_entry))
    bad = []
    for name, info in manifest.get("files", {}).items():
        if name == "MANIFEST.json":
            continue
        m = members.get(name)
        if m is None:
            bad.append(f"{name}: listed in MANIFEST but not in archive")
            continue
        data = tf.extractfile(m).read()
        actual = hashlib.sha256(data).hexdigest()
        if actual != info.get("sha256"):
            bad.append(f"{name}: hash mismatch (manifest={info.get('sha256')[:12]} actual={actual[:12]})")
    if bad:
        print("  FAIL: " + "; ".join(bad))
        sys.exit(1)
    print("  PASS: MANIFEST.json hashes match archive bytes")
    sys.exit(0)
PY
rc=$?
if [[ $rc -eq 0 ]]; then pass=$((pass + 1)); else fail=$((fail + 1)); failures+=("MANIFEST.json hash match"); fi

echo "## Gate 6: archive model hashes match model-manifest.toml"
python3 - "$archive_a" "$manifest" <<'PY'
import hashlib, sys, tarfile, tomllib
archive, manifest = sys.argv[1], sys.argv[2]
with open(manifest, "rb") as f:
    data = tomllib.load(f)
expected = {}
for m in data.get("models", []):
    for v in m.get("variants", []):
        expected[f"models/{m['id']}.onnx"] = v["sha256"]
bad = []
with tarfile.open(archive, "r:gz") as tf:
    members = {m.name: m for m in tf.getmembers()}
    for name, want in expected.items():
        mm = members.get(name)
        if mm is None:
            bad.append(f"{name}: not in archive")
            continue
        got = hashlib.sha256(tf.extractfile(mm).read()).hexdigest()
        if got != want:
            bad.append(f"{name}: archive={got[:12]} manifest={want[:12]}")
if bad:
    print("  FAIL: " + "; ".join(bad))
    sys.exit(1)
print("  PASS: archive model hashes match model-manifest.toml")
sys.exit(0)
PY
rc=$?
if [[ $rc -eq 0 ]]; then pass=$((pass + 1)); else fail=$((fail + 1)); failures+=("archive model hashes match manifest"); fi

echo ""
echo "Results: $pass passed, $fail failed"
if [[ $fail -gt 0 ]]; then
    echo "Failures:"
    for f in "${failures[@]}"; do echo "  - $f"; done
    exit 1
fi
exit 0
