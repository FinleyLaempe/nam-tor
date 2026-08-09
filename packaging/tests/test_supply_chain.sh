#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
#
# Task 24 — Supply-chain pass: SBOM, provenance, model cards, notices.
#
# Verifies:
#   (a) all three ONNX models present in SBOM (CycloneDX + SPDX)
#   (b) model sha256 in SBOM matches model-manifest.toml
#   (c) licence IDs correct (yunet=MIT, sface=Apache-2.0, minifasnet=Apache-2.0)
#   (d) no host paths (/home/, /Users/, host://) in any output
#   (e) deterministic — run generator twice, outputs byte-identical
#   (f) missing model archive makes provenance refuse (exit 1)
#   (g) provenance includes modelProvenance with all 3 models
#   (h) MiniFASNet PAD model card exists and has required sections
set -euo pipefail

SRCDIR="$(cd "$(dirname "$0")/../.." && pwd)"
EVIDENCE="${SRCDIR}/docs/evidence/packaging"
MANIFEST="${SRCDIR}/packaging/model-manifest.toml"
MODEL_CARD="${SRCDIR}/ml/reports/learned-pad.md"

pass=0
fail=0
assert() {
    local desc="$1" result="$2"
    if [ "$result" = "true" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL: $desc"
    fi
}

echo "=== Task 24: Supply-chain test ==="

# ---------------------------------------------------------------------------
# Gate 1: SBOM files exist
# ---------------------------------------------------------------------------
assert "CycloneDX SBOM exists" \
    "$([ -f "${EVIDENCE}/namtor.cyclonedx.json" ] && echo true || echo false)"
assert "SPDX SBOM exists" \
    "$([ -f "${EVIDENCE}/namtor.spdx.json" ] && echo true || echo false)"
assert "Provenance exists" \
    "$([ -f "${EVIDENCE}/namtor.provenance.json" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 2: All three models in CycloneDX SBOM
# ---------------------------------------------------------------------------
cdx_models=$(python3 -c "
import json
d = json.load(open('${EVIDIRESS:-${EVIDENCE}}/namtor.cyclonedx.json'))
models = [c for c in d.get('components', [])
          if c.get('type') == 'machine-learning-model']
names = sorted(c['name'] for c in models)
print(' '.join(names))
" 2>/dev/null || python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
models = [c for c in d.get('components', [])
          if c.get('type') == 'machine-learning-model']
names = sorted(c['name'] for c in models)
print(' '.join(names))
")

for mid in yunet_2023mar sface_2021dec minifasnet_pad; do
    case "$cdx_models" in
        *"$mid"*) assert "CycloneDX has model $mid" true ;;
        *) assert "CycloneDX has model $mid" false ;;
    esac
done

# ---------------------------------------------------------------------------
# Gate 3: All three models in SPDX SBOM
# ---------------------------------------------------------------------------
spdx_models=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.spdx.json'))
models = [p for p in d.get('packages', [])
          if p['name'] in ('yunet_2023mar', 'sface_2021dec', 'minifasnet_pad')]
names = sorted(p['name'] for p in models)
print(' '.join(names))
")

for mid in yunet_2023mar sface_2021dec minifasnet_pad; do
    case "$spdx_models" in
        *"$mid"*) assert "SPDX has model $mid" true ;;
        *) assert "SPDX has model $mid" false ;;
    esac
done

# ---------------------------------------------------------------------------
# Gate 4: Model sha256 in SBOM matches model-manifest.toml
# ---------------------------------------------------------------------------
manifest_hashes=$(python3 -c "
import tomllib
d = tomllib.load(open('${MANIFEST}', 'rb'))
for m in d['models']:
    v = m['variants'][0]
    print(f\"{m['id']} {v['sha256']}\")
")

while IFS=' ' read -r mid mhash; do
    sbom_hash=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
for c in d.get('components', []):
    if c.get('name') == '${mid}':
        print(c['hashes'][0]['content'])
        break
")
    assert "SBOM sha256 matches manifest for $mid" \
        "$([ "$sbom_hash" = "$mhash" ] && echo true || echo false)"
done <<< "$manifest_hashes"

# ---------------------------------------------------------------------------
# Gate 5: Licence IDs correct
# ---------------------------------------------------------------------------
expected_licences="yunet_2023mar:MIT sface_2021dec:Apache-2.0 minifasnet_pad:Apache-2.0"
for pair in $expected_licences; do
    mid="${pair%%:*}"
    expected_lic="${pair##*:}"
    actual_lic=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
for c in d.get('components', []):
    if c.get('name') == '${mid}':
        print(c['licenses'][0]['license']['id'])
        break
")
    assert "Licence for $mid = $expected_lic" \
        "$([ "$actual_lic" = "$expected_lic" ] && echo true || echo false)"
done

# ---------------------------------------------------------------------------
# Gate 6: No host paths in any output
# ---------------------------------------------------------------------------
for f in namtor.cyclonedx.json namtor.spdx.json namtor.provenance.json; do
    host_hits=$(grep -cE '/home/|/Users/|host://' "${EVIDENCE}/${f}" || true)
    assert "No host paths in $f" \
        "$([ "$host_hits" = "0" ] && echo true || echo false)"
done

# ---------------------------------------------------------------------------
# Gate 7: Deterministic — regenerate twice, compare
# ---------------------------------------------------------------------------
tmp1=$(mktemp -d)
tmp2=$(mktemp -d)
python3 "${SRCDIR}/packaging/generate-sbom.py" --out "$tmp1" >/dev/null 2>&1
python3 "${SRCDIR}/packaging/generate-sbom.py" --out "$tmp2" >/dev/null 2>&1
assert "CycloneDX deterministic (two runs identical)" \
    "$(diff -q "$tmp1/namtor.cyclonedx.json" "$tmp2/namtor.cyclonedx.json" >/dev/null && echo true || echo false)"
assert "SPDX deterministic (two runs identical)" \
    "$(diff -q "$tmp1/namtor.spdx.json" "$tmp2/namtor.spdx.json" >/dev/null && echo true || echo false)"

python3 "${SRCDIR}/packaging/generate-provenance.py" --out "$tmp1/prov.json" >/dev/null 2>&1
python3 "${SRCDIR}/packaging/generate-provenance.py" --out "$tmp2/prov.json" >/dev/null 2>&1
assert "Provenance deterministic (two runs identical)" \
    "$(diff -q "$tmp1/prov.json" "$tmp2/prov.json" >/dev/null && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 8: Fixed epoch timestamp (not wall clock)
# ---------------------------------------------------------------------------
cdx_ts=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
print(d['metadata']['timestamp'])
")
assert "CycloneDX timestamp is fixed epoch" \
    "$([ "$cdx_ts" = "2026-01-01T00:00:00Z" ] && echo true || echo false)"

prov_ts=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.provenance.json'))
print(d['predicate']['runDetails']['metadata']['startedOn'])
")
assert "Provenance timestamp is fixed epoch" \
    "$([ "$prov_ts" = "2026-01-01T00:00:00Z" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 9: Missing model archive makes provenance refuse (exit 1)
# ---------------------------------------------------------------------------
prov_rc=0
python3 "${SRCDIR}/packaging/generate-provenance.py" \
    --model-archive "/nonexistent/models.tar.gz" \
    --out "$tmp1/should_fail.json" >/dev/null 2>&1 || prov_rc=$?
assert "Provenance refuses without model archive (exit 1)" \
    "$([ "$prov_rc" = "1" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 10: --allow-no-models override works (exit 0)
# ---------------------------------------------------------------------------
prov_rc=0
python3 "${SRCDIR}/packaging/generate-provenance.py" \
    --model-archive "/nonexistent/models.tar.gz" \
    --allow-no-models \
    --out "$tmp1/no_models.json" >/dev/null 2>&1 || prov_rc=$?
assert "Provenance --allow-no-models succeeds (exit 0)" \
    "$([ "$prov_rc" = "0" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 11: Provenance includes modelProvenance with all 3 models
# ---------------------------------------------------------------------------
mp_count=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.provenance.json'))
mp = d.get('predicate', {}).get('modelProvenance', [])
print(len(mp))
")
assert "Provenance modelProvenance has 3 models" \
    "$([ "$mp_count" = "3" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 12: Provenance modelProvenance has correct fields
# ---------------------------------------------------------------------------
mp_check=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.provenance.json'))
mp = d.get('predicate', {}).get('modelProvenance', [])
ok = True
for m in mp:
    for field in ('modelId', 'sha256', 'licence', 'licenceSha256',
                  'upstreamUrl', 'builtFromCommit'):
        if field not in m:
            ok = False
    if m['modelId'] == 'minifasnet_pad':
        if 'modelCard' not in m or 'trainingRecipe' not in m:
            ok = False
print('true' if ok else 'false')
")
assert "modelProvenance fields complete (incl. PAD card/recipe)" \
    "$mp_check"

# ---------------------------------------------------------------------------
# Gate 13: Release binaries in SBOM
# ---------------------------------------------------------------------------
expected_bins="namtor-daemon namtor-worker namtor-cli pam_namtor.so"
for bname in $expected_bins; do
    found=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
for c in d.get('components', []):
    if c.get('name') == '${bname}':
        print('true')
        break
else:
    print('false')
")
    assert "CycloneDX has binary $bname" "$found"
done

# ---------------------------------------------------------------------------
# Gate 14: Model card exists and has required sections
# ---------------------------------------------------------------------------
assert "Model card file exists" \
    "$([ -f "${MODEL_CARD}" ] && echo true || echo false)"
card_sections=$(grep -cE '^## Model Card|^### (Training Recipe|Dataset Description|Performance Metrics|Limitations)' "${MODEL_CARD}" || true)
assert "Model card has 5 required sections (card + 4 subsections)" \
    "$([ "$card_sections" -ge "5" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Gate 15: SBOM has no uuid4 (must be deterministic uuid5)
# ---------------------------------------------------------------------------
# uuid4 is random; uuid5 is deterministic. Check serial number is stable.
serial1=$(python3 -c "
import json
d = json.load(open('${EVIDENCE}/namtor.cyclonedx.json'))
print(d['serialNumber'])
")
serial2=$(python3 -c "
import json
d = json.load(open('$tmp1/namtor.cyclonedx.json'))
print(d['serialNumber'])
")
assert "CycloneDX serial number is deterministic (same across runs)" \
    "$([ "$serial1" = "$serial2" ] && echo true || echo false)"

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
rm -rf "$tmp1" "$tmp2"

echo ""
echo "=== Task 24: $pass passed, $fail failed ==="
exit $([ "$fail" = "0" ] && echo 0 || echo 1)
